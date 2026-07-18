// ============================================================================
// MemoryAnalyzer — LLM 驱动的记忆提取与冲突检测
// ============================================================================
//
// 使用独立轻量模型（如 deepseek-v4-flash）分析对话内容，
// 提取值得保留的用户信息，不消耗主对话推理预算。
//
// 工作方式：
//   1. 构建 System Prompt（指导 LLM 提取特定类型的信息）
//   2. 发送 TurnContext 给 LLM
//   3. 解析 JSON 响应 → Vec<MemoryFact>

use std::sync::Arc;

use model_provider::{ChatRequest, ChatResponse, Message, ModelProvider};
use serde::Deserialize;
use tracing::warn;

use super::config::AnalyzerConfig;
use super::types::{Importance, MemoryCategory, MemoryFact, TurnContext};

/// LLM 返回的记忆提取 JSON 结构。
#[derive(Debug, Deserialize)]
struct MemoryExtractionResponse {
    #[serde(default)]
    facts: Vec<MemoryExtractionFact>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MemoryExtractionFact {
    category: String,
    importance: String,
    content: String,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    conflicts_with: Option<String>,
}

/// 记忆分析器。
///
/// 使用独立模型分析对话，提取用户偏好、项目上下文、重要决策等信息。
pub struct MemoryAnalyzer {
    /// 独立模型提供者（如 DeepSeek Flash）。
    model: Arc<dyn ModelProvider>,
    /// 分析器配置。
    config: AnalyzerConfig,
}

impl MemoryAnalyzer {
    /// 创建新的 MemoryAnalyzer。
    pub fn new(model: Arc<dyn ModelProvider>, config: AnalyzerConfig) -> Self {
        Self { model, config }
    }

    /// 分析本轮对话，提取需要保留的记忆事实。
    ///
    /// # Arguments
    ///
    /// * `turn_context` - 本轮对话的 User + Assistant 消息
    ///
    /// # Returns
    ///
    /// 返回 `Vec<MemoryFact>`，空向量表示无需保留。
    pub async fn analyze(&self, turn_context: &TurnContext) -> Result<Vec<MemoryFact>, String> {
        let conversation_text = turn_context.format_for_analysis();

        // 构建请求
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                Message::system(&build_analyzer_system_prompt()).into(),
                Message::user(&conversation_text).into(),
            ],
            tools: vec![],
            temperature: Some(0.1), // 低温度，追求一致性
            max_tokens: Some(1024),
            additional_params: None,
        };

        // 调用模型
        let response = self
            .model
            .chat(&request)
            .await
            .map_err(|e| format!("Memory analysis LLM call failed: {e}"))?;

        // 解析响应
        let facts = self.parse_response(&response)?;

        // 限制数量
        let facts: Vec<MemoryFact> = facts
            .into_iter()
            .take(self.config.max_facts_per_turn)
            .collect();

        if !facts.is_empty() {
            tracing::info!(count = facts.len(), "Memory facts extracted");
        }

        Ok(facts)
    }

    /// 解析 LLM 响应，提取 MemoryFact 列表。
    fn parse_response(&self, response: &ChatResponse) -> Result<Vec<MemoryFact>, String> {
        let text = response
            .message
            .content()
            .unwrap_or_default()
            .trim()
            .to_string();

        if text.is_empty() || text == "{}" || text == r#"{"facts":[]}"# {
            return Ok(Vec::new());
        }

        // 尝试提取 JSON 块（可能被 markdown 代码块包裹）
        let json_text = extract_json_from_text(&text);

        match serde_json::from_str::<MemoryExtractionResponse>(&json_text) {
            Ok(extraction) => {
                let facts: Vec<MemoryFact> = extraction
                    .facts
                    .into_iter()
                    .map(|f| MemoryFact::new(
                        parse_category(&f.category),
                        parse_importance(&f.importance),
                        f.content,
                    ))
                    .collect();
                Ok(facts)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    raw_response = %text,
                    "Failed to parse memory extraction JSON"
                );
                // 返回空 — 单次解析失败不应阻断对话
                Ok(Vec::new())
            }
        }
    }
}

/// 构建分析器的 System Prompt。
fn build_analyzer_system_prompt() -> String {
    r#"你是私人助理记忆分析器。从以下对话中提取需要保留的用户信息。

## 需要记住的信息类型

- **用户偏好 (profile)**: 姓名、角色、技能水平、行业背景、回复风格偏好
- **技术偏好 (semantic)**: 技术栈、编码风格、工具选择、沟通习惯
- **项目上下文 (episodic)**: 正在做的工作、使用的技术、项目阶段、重要决策、架构选择、方案取舍、约束条件

## 不需要记住的信息

- 临时的、一次性的技术问答（如"这段代码怎么报错？"）
- 纯知识性问答（除非涉及用户的具体项目场景）
- 闲聊、问候、感谢

## 输出格式

严格输出 JSON，格式如下：

```json
{
  "facts": [
    {
      "category": "profile|semantic|episodic",
      "importance": "high|medium|low",
      "content": "简洁的事实陈述（一句话）",
      "operation": "add|update|noop",
      "conflicts_with": null
    }
  ]
}
```

## 规则

1. 每条 fact 的 content 用中文，一句话说清
2. 如果没有需要保留的信息，返回 {"facts": []}
3. 只提取新信息，不要重复已有的
4. importance 判断标准：
   - high: 用户身份、核心偏好、重大决策
   - medium: 技术偏好、项目上下文
   - low: 临时偏好、一次性需求
"#
    .to_string()
}

/// 从可能被 markdown 代码块包裹的文本中提取 JSON。
fn extract_json_from_text(text: &str) -> String {
    let text = text.trim();

    // 尝试去掉 ```json ... ``` 包裹
    if let Some(inner) = text
        .strip_prefix("```json")
        .and_then(|t| t.strip_suffix("```"))
    {
        return inner.trim().to_string();
    }
    if let Some(inner) = text
        .strip_prefix("```")
        .and_then(|t| t.strip_suffix("```"))
    {
        return inner.trim().to_string();
    }

    text.to_string()
}

/// 解析 category 字符串。
fn parse_category(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "profile" => MemoryCategory::Profile,
        "episodic" => MemoryCategory::Episodic,
        _ => MemoryCategory::Semantic, // 默认 semantic
    }
}

/// 解析 importance 字符串。
fn parse_importance(s: &str) -> Importance {
    match s.to_lowercase().as_str() {
        "high" => Importance::High,
        "low" => Importance::Low,
        _ => Importance::Medium, // 默认 medium
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_category() {
        assert_eq!(parse_category("profile"), MemoryCategory::Profile);
        assert_eq!(parse_category("episodic"), MemoryCategory::Episodic);
        assert_eq!(parse_category("semantic"), MemoryCategory::Semantic);
        assert_eq!(parse_category("unknown"), MemoryCategory::Semantic); // default
    }

    #[test]
    fn test_parse_importance() {
        assert_eq!(parse_importance("high"), Importance::High);
        assert_eq!(parse_importance("low"), Importance::Low);
        assert_eq!(parse_importance("medium"), Importance::Medium);
        assert_eq!(parse_importance("unknown"), Importance::Medium); // default
    }

    #[test]
    fn test_extract_json_plain() {
        let input = r#"{"facts":[{"category":"semantic","importance":"medium","content":"用户偏好 Rust","operation":"add","conflicts_with":null}]}"#;
        let result = extract_json_from_text(input);
        assert!(result.contains("\"facts\""));
    }

    #[test]
    fn test_extract_json_code_block() {
        let input = "```json\n{\"facts\":[]}\n```";
        let result = extract_json_from_text(input);
        assert_eq!(result, "{\"facts\":[]}");
    }

    #[test]
    fn test_extract_json_code_block_no_lang() {
        let input = "```\n{\"facts\":[]}\n```";
        let result = extract_json_from_text(input);
        assert_eq!(result, "{\"facts\":[]}");
    }

    #[test]
    fn test_parse_empty_facts() {
        let response_text = r#"{"facts":[]}"#;
        let extraction: MemoryExtractionResponse =
            serde_json::from_str(response_text).unwrap();
        assert!(extraction.facts.is_empty());
    }

    #[test]
    fn test_parse_memory_extraction() {
        let response_text = r#"{"facts":[{"category":"semantic","importance":"medium","content":"用户偏好 Axum 框架","operation":"add","conflicts_with":null}]}"#;
        let extraction: MemoryExtractionResponse =
            serde_json::from_str(response_text).unwrap();
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].content, "用户偏好 Axum 框架");
    }
}
