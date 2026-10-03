// ============================================================================
// 记忆提取器 — Flash 模型从单轮对话中提取结构化记忆
// ============================================================================
//
// 写路径的 LLM 环节。调用范式与 peco-core 的 ModelSummarizer（compaction）
// 一致：复用主 Agent 的 provider + Flash 档模型 + 关闭 reasoning。
//
// 自动路径的 KB 写入只做 **add**：提取 schema v2 额外产出 topic/supersedes
// 取代指针，但只落 shadow 观测表（见 hook.rs），不执行任何删除 ——
// 记忆的更新/删除仍由 `@memory` 子 agent 的显式工具路径负责。

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
    /// 该事实「当前值槽」的稳定短键（取代时必填；模型可留空）。
    pub topic: Option<String>,
    /// 候选集中被本事实更正/推进的既有记忆 id（**至多 1 个**，
    /// `parse_facts` 只保留第一个非空 id）。
    pub supersedes: Vec<String>,
}

/// 供提取 prompt 展示的既有记忆候选。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCandidate {
    pub id: String,
    /// KB source 标签，如 `ppa_semantic`
    pub source: String,
    /// 展示文本（snippet 或截断后的正文）
    pub text: String,
}

/// 单轮记忆提取器抽象 — 便于测试时 mock。
#[async_trait]
pub trait TurnAnalyzer: Send + Sync {
    /// 从一轮对话中提取记忆。
    ///
    /// * `turn_dialogue` — 本轮对话的纯文本转录（"用户: ...\n助手: ..."）
    /// * `candidates` — 候选集既有记忆（带 id/source，供模型指 `supersedes`）
    ///
    /// 返回空 Vec 表示本轮无可提取的新信息。
    async fn analyze(
        &self,
        turn_dialogue: &str,
        candidates: &[MemoryCandidate],
    ) -> Result<Vec<MemoryFact>, String>;
}

/// 提取器的系统提示词。
const ANALYZER_SYSTEM_PROMPT: &str = r#"You are the personal assistant's memory extractor. Analyze one turn of dialogue and decide whether it contains new or updated information worth remembering long-term.

Three memory categories:
- profile: user identity and preferences (how to address the user, language, communication style, long-term preferences)
- semantic: discrete facts (tech stack, work background, project environment, knowledge background)
- episodic: events and ongoing matters (tasks in progress, commitments, todos)

Rules:
1. Extract new information AND updated/corrected information. If this turn corrects or advances an existing memory listed below, you MUST still extract it and put that memory's id in "supersedes". Only skip information that is exactly identical to an existing memory.
2. Each memory is one sentence, in Chinese, stating the fact rather than quoting the original text.
3. Never extract one-off technical Q&A, pure knowledge questions, chitchat, or procedural narration.
4. "topic": a short, stable key naming the "current-value slot" this fact belongs to (for example deployment_status, project_phase, user_timezone). Fill it whenever the fact updates or could later be updated; leave it empty if the fact is a one-off statement.
5. "supersedes": at most ONE id from the existing memories listed below, and ONLY when this turn clearly corrects or advances that memory. When unsure, leave it empty — we prefer appending over deleting.
6. For profile memories be strict: only supersede an existing profile memory when this turn directly contradicts it (not merely supplements it).
7. When there is nothing worth extracting, output {"facts": []}.
8. Output JSON only, with no other text or markdown code fence markers.

Output format:
{"facts": [{"category": "profile|semantic|episodic", "content": "one-sentence fact", "topic": "short_key", "supersedes": ["existing_memory_id"]}]}"#;

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
        candidates: &[MemoryCandidate],
    ) -> Result<Vec<MemoryFact>, String> {
        let mut user_content = String::from("[Existing memories]\n");
        if candidates.is_empty() {
            user_content.push_str("(none)\n\n");
        } else {
            for c in candidates {
                user_content.push_str(&format!("- [{}] ({}) {}\n", c.id, c.source, c.text));
            }
            user_content.push('\n');
        }
        user_content.push_str("[Current turn dialogue]\n");
        user_content.push_str(turn_dialogue);

        let request = GenerateRequest {
            model: self.model.clone(),
            instructions: Some(ANALYZER_SYSTEM_PROMPT.to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: user_content.into(),
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
                let category = MemoryCategory::from_raw(&f.category)?;
                // topic 空白 → None；supersedes 设计上限 1 条 —— 只取第一个非空 id
                let topic = f
                    .topic
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                let supersedes = f
                    .supersedes
                    .into_iter()
                    .find(|id| !id.trim().is_empty())
                    .map(|id| id.trim().to_string())
                    .into_iter()
                    .collect();
                Some(MemoryFact {
                    category,
                    content,
                    topic,
                    supersedes,
                })
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
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    supersedes: Vec<String>,
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
        // 旧格式（无 topic/supersedes 字段）→ 默认空
        assert!(facts[0].topic.is_none());
        assert!(facts[0].supersedes.is_empty());
        assert_eq!(facts[1].category, MemoryCategory::Semantic);
    }

    #[test]
    fn test_parse_topic_and_supersedes() {
        let facts = parse_facts(
            r#"{"facts": [
                {"category": "semantic", "content": "部署状态已切换到灰度",
                 "topic": "deployment_status", "supersedes": ["doc-abc123"]}
            ]}"#,
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].topic.as_deref(), Some("deployment_status"));
        assert_eq!(facts[0].supersedes, vec!["doc-abc123"]);
    }

    #[test]
    fn test_parse_supersedes_takes_first_non_empty() {
        // 2 个 id → 只取第 1 个（设计上限 1 条）
        let facts = parse_facts(
            r#"{"facts": [{"category": "semantic", "content": "x",
                 "supersedes": ["doc-a", "doc-b"]}]}"#,
        );
        assert_eq!(facts[0].supersedes, vec!["doc-a"]);

        // [""] → 空
        let facts = parse_facts(
            r#"{"facts": [{"category": "semantic", "content": "x", "supersedes": [""]}]}"#,
        );
        assert!(facts[0].supersedes.is_empty());

        // 字段缺失 → 空
        let facts = parse_facts(r#"{"facts": [{"category": "semantic", "content": "x"}]}"#);
        assert!(facts[0].supersedes.is_empty());
        assert!(facts[0].topic.is_none());
    }

    #[test]
    fn test_parse_blank_topic_is_none() {
        let facts =
            parse_facts(r#"{"facts": [{"category": "profile", "content": "x", "topic": "  "}]}"#);
        assert!(facts[0].topic.is_none());
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
