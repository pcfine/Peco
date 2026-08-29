// ============================================================================
// 记忆提取器 — Flash 模型从单轮对话中提取结构化记忆
// ============================================================================
//
// 写路径的 LLM 环节。调用范式与 peco-core 的 ModelSummarizer（compaction）
// 一致：复用主 Agent 的 provider + Flash 档模型 + 关闭 reasoning。
//
// V1 极简语义：自动路径只做 **add**（提取新信息）——记忆的更新/删除由
// `@memory` 子 agent 的显式工具路径负责，两条路径职责正交。

use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{ContentBlock, GenerateRequest, InputItem, ReasoningConfig, Role};
use serde::Deserialize;

/// 记忆类别。
///
/// 以 `ppa_{category}` 作为 KB 文档的 source 标签存储，
/// 读路径据此归类展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCategory {
    /// 用户身份与偏好（长期稳定）
    Profile,
    /// 离散事实（技术栈、项目背景等）
    Semantic,
    /// 事件与进行中的事项
    Episodic,
}

impl MemoryCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryCategory::Profile => "profile",
            MemoryCategory::Semantic => "semantic",
            MemoryCategory::Episodic => "episodic",
        }
    }

    fn from_raw(s: &str) -> Option<Self> {
        match s {
            "profile" => Some(MemoryCategory::Profile),
            "semantic" => Some(MemoryCategory::Semantic),
            "episodic" => Some(MemoryCategory::Episodic),
            _ => None,
        }
    }
}

/// 一条提取出的记忆。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryFact {
    pub category: MemoryCategory,
    /// 一句话事实描述（中文）。
    pub content: String,
}

/// 单轮记忆提取器抽象 — 便于测试时 mock。
#[async_trait]
pub trait TurnAnalyzer: Send + Sync {
    /// 从一轮对话中提取记忆。
    ///
    /// * `turn_dialogue` — 本轮对话的纯文本转录（"用户: ...\n助手: ..."）
    /// * `existing_memories` — 检索到的既有相关记忆（供模型判断"是否为新信息"）
    ///
    /// 返回空 Vec 表示本轮无可提取的新信息。
    async fn analyze(
        &self,
        turn_dialogue: &str,
        existing_memories: &[String],
    ) -> Result<Vec<MemoryFact>, String>;
}

/// 提取器的系统提示词。
const ANALYZER_SYSTEM_PROMPT: &str = r#"你是个人助理的记忆提取器。分析一轮对话，判断是否包含值得长期记住的**新**信息。

记忆分三类：
- profile：用户身份与偏好（称呼、语言、沟通风格、长期偏好）
- semantic：离散事实（技术栈、工作背景、项目环境、知识背景）
- episodic：事件与进行中的事项（正在做的任务、约定、待办）

规则：
1. 只提取**新**信息 — 「既有记忆」中已包含的内容不要重复输出；
2. 每条记忆一句话，中文，陈述事实而非引用原文；
3. 一次性技术问答、纯知识问答、寒暄、过程性描述一律不提取；
4. 没有值得提取的内容时输出 {"facts": []}；
5. 只输出 JSON，不要任何其他文字或 markdown 代码块标记。

输出格式：
{"facts": [{"category": "profile|semantic|episodic", "content": "一句话事实"}]}"#;

/// 基于 [`model_provider::ModelProvider`] 的提取器。
pub struct ModelTurnAnalyzer {
    provider: Arc<dyn model_provider::ModelProvider>,
    model: String,
    max_output_tokens: u32,
}

impl ModelTurnAnalyzer {
    pub fn new(provider: Arc<dyn model_provider::ModelProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            max_output_tokens: 512,
        }
    }
}

#[async_trait]
impl TurnAnalyzer for ModelTurnAnalyzer {
    async fn analyze(
        &self,
        turn_dialogue: &str,
        existing_memories: &[String],
    ) -> Result<Vec<MemoryFact>, String> {
        let mut user_content = String::from("【既有记忆】\n");
        if existing_memories.is_empty() {
            user_content.push_str("（无）\n\n");
        } else {
            for m in existing_memories {
                user_content.push_str("- ");
                user_content.push_str(m);
                user_content.push('\n');
            }
            user_content.push('\n');
        }
        user_content.push_str("【本轮对话】\n");
        user_content.push_str(turn_dialogue);

        let request = GenerateRequest {
            model: self.model.clone(),
            instructions: Some(ANALYZER_SYSTEM_PROMPT.to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: user_content,
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: Some(self.max_output_tokens),
            // 记忆提取不需要推理 — 关闭 thinking 降低延迟与成本
            reasoning: Some(ReasoningConfig {
                enabled: false,
                effort: None,
            }),
            text: None,
            additional_params: None,
        };

        let result = self
            .provider
            .generate_full(&request)
            .await
            .map_err(|e| format!("analyzer model call failed: {e}"))?;

        if result.status != model_provider::ResponseStatus::Completed {
            return Err(format!(
                "analyzer generation incomplete: status={:?}, error={:?}",
                result.status, result.error
            ));
        }

        let text: String = result
            .output
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(parse_facts(&text))
    }
}

/// 解析提取器输出为记忆列表。
///
/// 提取失败（畸形 JSON、未知类别）与"无新信息"同归为空 Vec —
/// 由调用方按非致命处理，不区分错误类型。
fn parse_facts(raw: &str) -> Vec<MemoryFact> {
    // 剥 markdown 代码块包裹（模型偶尔无视"只输出 JSON"的指示，大小写不定）
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

    let parsed: Result<RawExtraction, _> = serde_json::from_str(stripped);
    match parsed {
        Ok(extraction) => extraction
            .facts
            .into_iter()
            .filter_map(|f| {
                let content = f.content.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                MemoryCategory::from_raw(&f.category)
                    .map(|category| MemoryFact { category, content })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[derive(Debug, Deserialize)]
struct RawExtraction {
    #[serde(default)]
    facts: Vec<RawFact>,
}

#[derive(Debug, Deserialize)]
struct RawFact {
    category: String,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_json() {
        let facts = parse_facts(
            r#"{"facts": [
                {"category": "profile", "content": "用户偏好中文交流"},
                {"category": "semantic", "content": "用户使用 Rust 开发"}
            ]}"#,
        );
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].category, MemoryCategory::Profile);
        assert_eq!(facts[0].content, "用户偏好中文交流");
        assert_eq!(facts[1].category, MemoryCategory::Semantic);
    }

    #[test]
    fn test_parse_fenced_json() {
        for fence in ["```json", "```JSON", "```"] {
            let facts = parse_facts(&format!(
                "{fence}\n{{\"facts\": [{{\"category\": \"episodic\", \"content\": \"正在开发 peco\"}}]}}\n```",
            ));
            assert_eq!(facts.len(), 1, "fence {fence} 应被剥除");
            assert_eq!(facts[0].category, MemoryCategory::Episodic);
        }
    }

    #[test]
    fn test_parse_malformed_returns_empty() {
        assert!(parse_facts("这不是 JSON").is_empty());
        assert!(parse_facts("").is_empty());
        assert!(
            parse_facts("{\"facts\": [{\"category\": \"unknown\", \"content\": \"x\"}]}")
                .is_empty()
        );
        assert!(
            parse_facts("{\"facts\": [{\"category\": \"profile\", \"content\": \"   \"}]}")
                .is_empty()
        );
    }

    #[test]
    fn test_category_as_str_roundtrip() {
        for (s, cat) in [
            ("profile", MemoryCategory::Profile),
            ("semantic", MemoryCategory::Semantic),
            ("episodic", MemoryCategory::Episodic),
        ] {
            assert_eq!(MemoryCategory::from_raw(s), Some(cat));
            assert_eq!(cat.as_str(), s);
        }
    }
}
