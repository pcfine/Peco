//! 单轮记忆提取：提示词、JSON 契约与解析容错。
//!
//! 提示词与 JSON 契约沿用 `peco-server` 的提取器 —— 那套措辞经过实测调过，
//! 换一份只会让同一个模型在两处给出不同质量的提取结果。这里去掉的是与
//! 取代/图事实相关的部分（没有候选集、没有三元组，写路径只做追加）。
//!
//! 调用范式与 `ModelSummarizer`（压缩摘要）一致：复用主 Agent 的 provider +
//! Flash 档模型 + 关闭 thinking。

use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{ContentBlock, GenerateRequest, InputItem, ReasoningConfig, Role};
use serde::Deserialize;

/// 记忆类别。以 `ppa_{category}` 作为 KB 文档的 source 标签存储，
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
    /// 一句话事实描述（中文）。
    pub text: String,
    pub category: MemoryCategory,
}

/// 单轮提取器抽象 —— 测试替换成假实现，不必打真实模型。
#[async_trait]
pub trait TurnAnalyzer: Send + Sync {
    /// 从本轮对话的纯文本转录中提取记忆；空 `Vec` 表示本轮无可记之事。
    async fn analyze(&self, turn_dialogue: &str) -> Result<Vec<MemoryFact>, String>;
}

/// 提取器的系统提示词。
///
/// 只要求 `category` 与 `content` 两个字段：写路径不做取代、不写图，
/// 多要的字段只会让模型把注意力从「这一轮有什么值得记」上移开。
const ANALYZER_PROMPT: &str = r#"You are the personal assistant's memory extractor. Analyze one turn of dialogue and decide whether it contains new information worth remembering long-term.

Three memory categories:
- profile: user identity and preferences (how to address the user, language, communication style, long-term preferences)
- semantic: discrete facts (tech stack, work background, project environment, knowledge background)
- episodic: events and ongoing matters (tasks in progress, commitments, todos)

Rules:
1. Extract only information that is new in this turn; skip anything the dialogue already established earlier.
2. Each memory is one sentence, in Chinese, stating the fact rather than quoting the original text.
3. Never extract one-off technical Q&A, pure knowledge questions, chitchat, or procedural narration.
4. When there is nothing worth extracting, output {"facts": []}.
5. Output JSON only, with no other text or markdown code fence markers.

Output format:
{"facts": [{"category": "profile|semantic|episodic", "content": "one-sentence fact"}]}"#;

/// 单轮输出预算。
///
/// 一条 fact 约 30 token；给 2048 是为了多事实轮不撞顶 —— JSON 被截断会让
/// 整轮提取静默归零（见 [`parse_facts`] 的容错口径）。
const MAX_OUTPUT_TOKENS: u32 = 2048;

/// 基于 [`model_provider::ModelProvider`] 的提取器。
pub struct ModelTurnAnalyzer {
    provider: Arc<dyn model_provider::ModelProvider>,
    model: String,
}

impl ModelTurnAnalyzer {
    pub fn new(provider: Arc<dyn model_provider::ModelProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

#[async_trait]
impl TurnAnalyzer for ModelTurnAnalyzer {
    async fn analyze(&self, turn_dialogue: &str) -> Result<Vec<MemoryFact>, String> {
        let request = GenerateRequest {
            model: self.model.clone(),
            instructions: Some(ANALYZER_PROMPT.to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: format!("[Current turn dialogue]\n{turn_dialogue}").into(),
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            // 记忆提取不需要推理
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
/// 畸形 JSON、未知类别、空正文与「本轮无新信息」同归空 `Vec`：调用方按
/// 非致命处理，区分错误类型只会让写路径多一条没人看的告警。
fn parse_facts(raw: &str) -> Vec<MemoryFact> {
    // 剥 markdown 代码块包裹（模型偶尔无视「只输出 JSON」，大小写不定）
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

    let Ok(extraction) = serde_json::from_str::<RawExtraction>(stripped) else {
        return Vec::new();
    };

    extraction
        .facts
        .into_iter()
        .filter_map(|fact| {
            let text = fact.content.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let category = MemoryCategory::from_raw(&fact.category)?;
            Some(MemoryFact { text, category })
        })
        .collect()
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
    fn parses_clean_json() {
        let facts = parse_facts(
            r#"{"facts": [
                {"category": "profile", "content": "用户偏好中文交流"},
                {"category": "semantic", "content": "用户使用 Rust 开发"}
            ]}"#,
        );
        assert_eq!(facts.len(), 2);
        assert_eq!(
            facts[0],
            MemoryFact {
                text: "用户偏好中文交流".into(),
                category: MemoryCategory::Profile
            }
        );
        assert_eq!(facts[1].category, MemoryCategory::Semantic);
    }

    #[test]
    fn strips_markdown_fences() {
        for fence in ["```json", "```JSON", "```"] {
            let facts = parse_facts(&format!(
                "{fence}\n{{\"facts\": [{{\"category\": \"episodic\", \"content\": \"正在开发 peco\"}}]}}\n```",
            ));
            assert_eq!(facts.len(), 1, "fence {fence} 应被剥除");
        }
    }

    /// 未知类别、空正文与畸形 JSON 一律归零：写路径不写半条记忆。
    #[test]
    fn malformed_and_unknown_input_yields_nothing() {
        assert!(parse_facts("这不是 JSON").is_empty());
        assert!(parse_facts("").is_empty());
        assert!(parse_facts(r#"{"facts": [{"category": "unknown", "content": "x"}]}"#).is_empty());
        assert!(
            parse_facts(r#"{"facts": [{"category": "profile", "content": "   "}]}"#).is_empty()
        );
        // 字段缺失
        assert!(parse_facts(r#"{"facts": [{"category": "profile"}]}"#).is_empty());
    }

    /// 模型偶尔多给字段（旧契约的 topic / supersedes）：多出来的键不得让整轮解析失败。
    #[test]
    fn extra_fields_are_ignored() {
        let facts = parse_facts(
            r#"{"facts": [{"category": "semantic", "content": "部署状态已切换到灰度",
                 "topic": "deployment_status", "supersedes": ["doc-abc"]}]}"#,
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].text, "部署状态已切换到灰度");
    }

    #[test]
    fn category_as_str_roundtrip() {
        for (raw, category) in [
            ("profile", MemoryCategory::Profile),
            ("semantic", MemoryCategory::Semantic),
            ("episodic", MemoryCategory::Episodic),
        ] {
            assert_eq!(MemoryCategory::from_raw(raw), Some(category));
            assert_eq!(category.as_str(), raw);
        }
    }
}
