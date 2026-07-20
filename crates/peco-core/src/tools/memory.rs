// ============================================================================
// PPA Memory Tools — remember / recall / forget
// ============================================================================
//
// 基于 `MemoryStore` trait 的依赖注入实现。
// CLI 和 Web 使用同一个工具实现 — 差异仅在 Workspace 传入的 MemoryStore 实现。

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::personal_memory::{Importance, MemoryCategory, MemoryFact};
use crate::workspace::MemoryStore;

use super::{StringError, ToolDyn, ToolError};

fn string_err(msg: impl ToString) -> ToolError {
    ToolError::ToolCallError(Box::new(StringError(msg.to_string())))
}

// ============================================================================
// RememberTool
// ============================================================================

pub struct RememberTool {
    store: Arc<dyn MemoryStore>,
}

impl RememberTool {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

// ============================================================================
// RememberTool
// ============================================================================

impl ToolDyn for RememberTool {
    fn name(&self) -> String {
        "remember".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "remember".to_string(),
            description: "保存一条关于当前用户的个人记忆。当用户明确要求记住某事、或你发现需要记住的重要用户信息时使用。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "enum": ["profile", "semantic", "episodic"],
                        "description": "记忆分类：profile(个人资料), semantic(偏好/知识), episodic(项目/事件)"
                    },
                    "content": {
                        "type": "string",
                        "description": "要保存的记忆内容（简洁的一句话）"
                    },
                    "importance": {
                        "type": "string",
                        "enum": ["high", "medium", "low"],
                        "description": "重要性：high(核心), medium(一般), low(临时)",
                        "default": "medium"
                    }
                },
                "required": ["category", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct RememberArgs {
                category: String,
                content: String,
                #[serde(default = "default_importance")]
                importance: String,
            }
            fn default_importance() -> String {
                "medium".to_string()
            }

            let parsed: RememberArgs = serde_json::from_str(&args)
                .map_err(|e| string_err(format!("参数解析失败: {e}")))?;

            let category = match parsed.category.as_str() {
                "profile" => MemoryCategory::Profile,
                "episodic" => MemoryCategory::Episodic,
                _ => MemoryCategory::Semantic,
            };
            let importance = match parsed.importance.as_str() {
                "high" => Importance::High,
                "low" => Importance::Low,
                _ => Importance::Medium,
            };

            let fact = MemoryFact::new(category, importance, parsed.content.clone());
            self.store
                .save_or_update_fact(&fact)
                .await
                .map_err(|e| string_err(format!("保存记忆失败: {e}")))?;

            Ok(format!("已保存记忆 [{}]: {}", fact.id, parsed.content))
        })
    }
}

// ============================================================================
// RecallTool
// ============================================================================

pub struct RecallTool {
    store: Arc<dyn MemoryStore>,
}

impl RecallTool {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

impl ToolDyn for RecallTool {
    fn name(&self) -> String {
        "recall".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "recall".to_string(),
            description:
                "搜索关于当前用户的个人记忆。当需要回顾用户偏好、历史决策或之前提到过的信息时使用。"
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索查询，例如 '用户之前提到的项目名称'、'用户的编码风格偏好'"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "返回的记忆条数",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct RecallArgs {
                query: String,
                #[serde(default = "default_top_k")]
                top_k: usize,
            }
            fn default_top_k() -> usize {
                5
            }

            let parsed: RecallArgs = serde_json::from_str(&args)
                .map_err(|e| string_err(format!("参数解析失败: {e}")))?;

            let semantic = self
                .store
                .search_semantic(&parsed.query, parsed.top_k, 0.5)
                .await
                .map_err(|e| string_err(format!("语义记忆搜索失败: {e}")))?;

            let episodic = self
                .store
                .search_episodic(&parsed.query, parsed.top_k, 0.5)
                .await
                .map_err(|e| string_err(format!("历史记忆搜索失败: {e}")))?;

            if semantic.is_empty() && episodic.is_empty() {
                return Ok("未找到相关记忆。".to_string());
            }

            let mut lines = vec!["找到以下相关记忆:".to_string(), String::new()];

            if !semantic.is_empty() {
                lines.push("[语义记忆]".to_string());
                for f in &semantic {
                    lines.push(format!("- {}", f.content));
                }
                lines.push(String::new());
            }
            if !episodic.is_empty() {
                lines.push("[历史上下文]".to_string());
                for f in &episodic {
                    lines.push(format!("- {}", f.content));
                }
            }

            Ok(lines.join("\n"))
        })
    }
}

// ============================================================================
// ForgetTool
// ============================================================================

pub struct ForgetTool {
    store: Arc<dyn MemoryStore>,
}

impl ForgetTool {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

impl ToolDyn for ForgetTool {
    fn name(&self) -> String {
        "forget".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "forget".to_string(),
            description: "删除一条关于当前用户的个人记忆。当记忆已过时或不正确时使用。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "memory_id": {
                        "type": "string",
                        "description": "要删除的记忆 ID"
                    }
                },
                "required": ["memory_id"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct ForgetArgs {
                memory_id: String,
            }

            let parsed: ForgetArgs = serde_json::from_str(&args)
                .map_err(|e| string_err(format!("参数解析失败: {e}")))?;

            let fact = MemoryFact::with_id(
                parsed.memory_id.clone(),
                MemoryCategory::Semantic,
                Importance::Low,
                String::new(),
            );
            self.store
                .invalidate_fact(&fact)
                .await
                .map_err(|e| string_err(format!("删除记忆失败: {e}")))?;

            Ok(format!("已删除记忆: {}", parsed.memory_id))
        })
    }
}
