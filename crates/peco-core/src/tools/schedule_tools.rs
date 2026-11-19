// ============================================================================
// Scheme tools — 定时任务（save_schedule）
//   save_schedule — 按 cron 把一段话投进目标空间的 @master 会话
// ============================================================================
//
// peco-core 只认窄接口：cron 怎么算、目标空间怎么解析、排期落哪张表都在宿主
// （daemon）里。这样同一份工具在 CLI 里只是「没有排期存储」而自动跳过，不会把
// SQLite 或 `workspaces` 表的知识拖进 core。

use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::Future;
use model_provider::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::tool_factory::StringError;
use super::{Content, ToolDyn, ToolError};

/// 一条待建的排期。
#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub name: String,
    /// 目标空间：ws_id 或 label（daemon 侧解析）。
    pub target: String,
    /// 到点投进会话的正文。
    pub prompt: String,
    /// 5 或 6 字段的 cron 表达式。
    pub cron_expr: String,
    pub enabled: bool,
}

/// 建好之后的回执：只有人能核对的字段，不含内部列。
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleInfo {
    pub id: String,
    pub name: String,
    pub target_ws_id: String,
    pub target_label: String,
    pub cron_expr: String,
    pub enabled: bool,
    /// 接下来几次触发时刻（UTC 文本），让模型能当场确认「我理解的时间对不对」。
    pub next: Vec<String>,
}

/// 建排期失败的原因。`code` 是稳定分类，`message` 给人读。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ScheduleError {
    pub code: &'static str,
    pub message: String,
    /// `ambiguous_target` 时的候选 ws_id；其余错误为空。
    pub candidates: Vec<String>,
}

impl ScheduleError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            candidates: Vec::new(),
        }
    }

    pub fn with_candidates(
        code: &'static str,
        message: impl Into<String>,
        candidates: Vec<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            candidates,
        }
    }

    /// 给模型看的错误文本：code 必须在里面，否则它只能靠猜来决定是改名还是改 cron。
    fn tool_message(&self) -> String {
        if self.candidates.is_empty() {
            format!("save_schedule failed ({}): {}", self.code, self.message)
        } else {
            format!(
                "save_schedule failed ({}): {}; candidates: {}",
                self.code,
                self.message,
                self.candidates.join(", ")
            )
        }
    }
}

/// 排期存储。实现方（daemon）负责 cron 校验、目标解析与落库。
#[async_trait]
pub trait ScheduleAccess: Send + Sync {
    async fn save(&self, request: NewSchedule) -> Result<ScheduleInfo, ScheduleError>;
}

static SCHEDULE_ACCESS: OnceLock<Arc<dyn ScheduleAccess>> = OnceLock::new();

/// 安装进程级排期存储。只能成功一次：已初始化时返回被拒的 `Arc`。
///
/// 与 `install_workspace_hub` 同一手法：agent 在请求期构造，注入点必须早于任何
/// agent 加载。
pub fn install_schedule_access(
    access: Arc<dyn ScheduleAccess>,
) -> Result<(), Arc<dyn ScheduleAccess>> {
    SCHEDULE_ACCESS.set(access)
}

/// 进程级排期存储；未安装时为 `None`（`save_schedule` 因此 warn + skip）。
pub fn schedule_access() -> Option<Arc<dyn ScheduleAccess>> {
    SCHEDULE_ACCESS.get().cloned()
}

// ── SaveSchedule ─────────────────────────────────────────────────────────────

pub struct SaveSchedule {
    access: Arc<dyn ScheduleAccess>,
}

impl SaveSchedule {
    pub fn new(access: Arc<dyn ScheduleAccess>) -> Self {
        Self { access }
    }
}

#[derive(Debug, Deserialize)]
struct SaveScheduleArgs {
    name: String,
    target: String,
    prompt: String,
    cron_expr: String,
    /// 省略即启用：建一条排好就暂停的任务没有意义。
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl ToolDyn for SaveSchedule {
    fn name(&self) -> String {
        "save_schedule".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_schedule".to_string(),
            description: "Create a scheduled task: at each cron occurrence the prompt is \
                delivered as one user message into the target space's @master conversation, \
                and that space runs one turn on it.\n\
                \n\
                target accepts either the space's ws_id or its label (e.g. '研发'). If a label \
                matches more than one space the call fails with 'ambiguous_target' and lists the \
                candidate ws_ids — retry with the ws_id.\n\
                \n\
                cron_expr uses the standard 5-field form 'minute hour day-of-month month \
                day-of-week' (e.g. '0 9 * * *' for 09:00 every day, local time); a 6-field form \
                with a leading seconds field is also accepted. Firing times are computed in the \
                daemon's local timezone.\n\
                \n\
                The reply reports the stored schedule with the next three firing times, so you \
                can confirm the time interpretation with the user before it runs."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short human-readable name of the task (shown in the UI and in the notification)."
                    },
                    "target": {
                        "type": "string",
                        "description": "Target space, as its ws_id or its label."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The user message delivered into the target space at each occurrence."
                    },
                    "cron_expr": {
                        "type": "string",
                        "description": "Cron expression: 5 fields (minute hour day month weekday) or 6 fields (with leading seconds)."
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "Whether the schedule starts enabled. Defaults to true."
                    }
                },
                "required": ["name", "target", "prompt", "cron_expr"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed: SaveScheduleArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let info = self
                .access
                .save(NewSchedule {
                    name: parsed.name,
                    target: parsed.target,
                    prompt: parsed.prompt,
                    cron_expr: parsed.cron_expr,
                    enabled: parsed.enabled,
                })
                .await
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.tool_message()))))?;
            let text = serde_json::to_string_pretty(&info).map_err(ToolError::JsonError)?;
            Ok(Content::Text(text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 记录最后一次请求的假存储：工具层只做参数搬运与结果序列化，落库行为在
    /// daemon 侧验证。
    struct FakeAccess {
        result: Result<ScheduleInfo, ScheduleError>,
        seen: Mutex<Option<NewSchedule>>,
    }

    impl FakeAccess {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                result: Ok(ScheduleInfo {
                    id: "s-1".into(),
                    name: "每日站会".into(),
                    target_ws_id: "ws_a".into(),
                    target_label: "研发".into(),
                    cron_expr: "0 0 9 * * *".into(),
                    enabled: true,
                    next: vec!["2026-03-05 01:00:00".into()],
                }),
                seen: Mutex::new(None),
            })
        }

        fn failing(error: ScheduleError) -> Arc<Self> {
            Arc::new(Self {
                result: Err(error),
                seen: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl ScheduleAccess for FakeAccess {
        async fn save(&self, request: NewSchedule) -> Result<ScheduleInfo, ScheduleError> {
            *self.seen.lock().unwrap() = Some(request);
            self.result.clone()
        }
    }

    fn tool(access: Arc<FakeAccess>) -> SaveSchedule {
        SaveSchedule::new(access)
    }

    async fn call(tool: &SaveSchedule, args: serde_json::Value) -> Result<String, String> {
        tool.call(args.to_string())
            .await
            .map(|content| content.text_view().into_owned())
            .map_err(|e| e.to_string())
    }

    /// 参数名是模型唯一能看到的契约：改名等于换了工具。
    #[test]
    fn the_schema_requires_the_four_core_arguments() {
        let definition = tool(FakeAccess::ok()).definition();
        assert_eq!(definition.name, "save_schedule");
        let required: Vec<&str> = definition.parameters["required"]
            .as_array()
            .expect("required 必须存在")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(required, vec!["name", "target", "prompt", "cron_expr"]);
        assert_eq!(
            definition.parameters["properties"]["enabled"]["type"],
            json!("boolean")
        );
    }

    /// 省略 `enabled` 视为启用：建一条「排好就暂停」的任务没有意义。
    #[tokio::test]
    async fn enabled_defaults_to_true() {
        let access = FakeAccess::ok();
        let output = call(
            &tool(access.clone()),
            json!({
                "name": "每日站会",
                "target": "研发",
                "prompt": "汇报进展",
                "cron_expr": "0 9 * * *"
            }),
        )
        .await
        .expect("fake 存储总是成功");

        let seen = access
            .seen
            .lock()
            .unwrap()
            .clone()
            .expect("必须真的调用存储");
        assert!(seen.enabled);
        assert_eq!(seen.target, "研发", "target 原样交给宿主解析");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["target_label"], json!("研发"));
        assert_eq!(parsed["next"][0], json!("2026-03-05 01:00:00"));
    }

    #[tokio::test]
    async fn an_explicit_false_is_honoured() {
        let access = FakeAccess::ok();
        call(
            &tool(access.clone()),
            json!({
                "name": "n",
                "target": "ws_a",
                "prompt": "p",
                "cron_expr": "0 9 * * *",
                "enabled": false
            }),
        )
        .await
        .unwrap();
        assert!(!access.seen.lock().unwrap().clone().unwrap().enabled);
    }

    /// 失败要带 code 与候选：模型据此决定是换 ws_id 还是重写 cron。
    #[tokio::test]
    async fn failures_carry_their_code() {
        let error = call(
            &tool(FakeAccess::failing(ScheduleError::with_candidates(
                "ambiguous_target",
                "'研发' matches 2 workspaces",
                vec!["ws_a".into(), "ws_b".into()],
            ))),
            json!({
                "name": "n",
                "target": "研发",
                "prompt": "p",
                "cron_expr": "0 9 * * *"
            }),
        )
        .await
        .expect_err("存储拒绝即失败");

        assert!(error.contains("ambiguous_target"), "{error}");
        assert!(error.contains("ws_a, ws_b"), "候选要列出来：{error}");
    }

    /// 缺必填参数是 serde 层的错误，不落到存储。
    #[tokio::test]
    async fn missing_arguments_never_reach_the_store() {
        let access = FakeAccess::ok();
        let error = call(&tool(access.clone()), json!({"name": "n"})).await;
        assert!(error.is_err());
        assert!(access.seen.lock().unwrap().is_none());
    }

    /// 进程级注入只成功一次：第二次拿回被拒的那一份，调用方据此报装配错误。
    #[test]
    fn installing_twice_is_refused() {
        let first: Arc<dyn ScheduleAccess> = FakeAccess::ok();
        // 进程级槽只装一次；别的用例先装过时同样必须能取回同一份。
        if install_schedule_access(first.clone()).is_ok() {
            assert!(schedule_access().is_some(), "装好之后必须能取回");
            assert!(
                install_schedule_access(first).is_err(),
                "第二次安装必须被拒"
            );
        } else {
            assert!(schedule_access().is_some(), "已被别的用例装过");
        }
    }
}
