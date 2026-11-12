// ============================================================================
// machine_tool — 把工作空间的 TaskHub 暴露为「提交 / 观察-取消」两个工具
// ============================================================================
//
// Hub 的快照是任务的唯一权威副本：工具不缓存状态、不枚举任务，只按 id 读写。
// 两个工具都只持 `Weak<MachineRuntime>`，否则 agent ↔ 工具 ↔ Hub 会闭合。

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::{debug, warn};
use uuid::Uuid;

use super::deps::AgentAccess;
use super::{Content, StringError, ToolDyn, ToolError};
use crate::agent::hooks::MAX_DIGEST_CHARS;
use crate::artifacts::{
    ArtifactDecl, render_artifact_lines, strip_artifact_lines, with_artifact_lines,
};
use crate::machine::agent::ARTIFACT_DECL_TAG;
use crate::machine::runtime::{RuntimeError, validate_agent_name};
use crate::machine::{
    MachineError, MachineRuntime, TaggedJson, Task, TaskContext, TaskError, TaskHub, TaskId,
    TaskSnapshot, TaskSource, TaskState,
};

/// 单次调用最多提交的任务数：再多就该分多次调用，模型预算才可见。
const MAX_TASKS: usize = 16;
const MAX_WAIT_SECS: u64 = 3600;
/// 缺省等待上界：父回合的退出边界恒为有限值，不存在无限阻塞的路径。
const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 300;
const MAX_DEADLINE_SECS: u64 = 86400;

/// 单任务正文上限：全文可达数 MiB，16 条会撞调用方 agent 的上下文。
const MAX_OUTPUT_PREVIEW_BYTES: usize = 8 * 1024;
/// 单任务 prompt 回显上限：任务是描述性的，意图在开头。
const MAX_PROMPT_ECHO_BYTES: usize = 512;
/// 单条 `error.message` 上限：文案由 provider/panic payload 决定，长度不受本工具约束；
/// 与 `prompt` 回显同档，`kind` 与 `attempts` 才是模型的决策依据。
const MAX_ERROR_MESSAGE_BYTES: usize = 512;
/// 整份结果上限：声明块与正文共用这一份额度。
const MAX_RESULT_BYTES: usize = 32 * 1024;

/// 声明块条数上限：跨任务累计，与每任务的上限相乘会放大到上千行。
const MAX_DECLARATIONS_TOTAL: usize = 64;
/// 声明块字节上限（含 marker 与换行）：块本身也必须落在 `MAX_RESULT_BYTES` 之内。
const MAX_DECLARATIONS_BYTES: usize = 8 * 1024;
/// 单条 `path` 上限：由子 agent 写，长度无天然上界。
const MAX_DECL_PATH_BYTES: usize = 512;
/// 单条 `title` 上限：展示用短串，与 `path` 分档。
const MAX_DECL_TITLE_BYTES: usize = 128;

/// `TaskOutcome::output` 正文的 tag，与 `AgentMachine` 落终态时写的一致。
const TEXT_OUTPUT_TAG: &str = "text";
const PROMPT_TAG: &str = "prompt";
const ORIGIN_LABEL: &str = "run_machine_task";
/// 标明这次提交没有等待期；同步提交不带该键。
const ASYNC_LABEL: &str = "async";
const CANCEL_NOTE: &str = "cancellation requested; poll action=status to confirm";
/// 终态任务上 `cancel` 什么都没做：`accepted` 恒 `true`，只能靠 `note` 说清。
const CANCEL_INERT_NOTE: &str = "task is already terminal; cancel has no effect";

/// 快照来源标签：缺键即本进程的活任务，`ledger` 表示正文来自台账。
pub(crate) const LABEL_SOURCE: &str = "source";
/// 正文留存口径：`full` 是全文，`digest_only` 只剩台账摘要。
pub(crate) const LABEL_OUTPUT_RETENTION: &str = "output_retention";
/// 台账里的正文自己就被落库上限截断过：它的长度不是原文总长。
pub(crate) const LABEL_OUTPUT_TRUNCATED: &str = "output_truncated";
pub(crate) const SOURCE_LEDGER: &str = "ledger";
pub(crate) const RETENTION_FULL: &str = "full";
pub(crate) const RETENTION_DIGEST_ONLY: &str = "digest_only";
/// 台账里既没有正文也没有摘要：什么都没留下来。
pub(crate) const RETENTION_NONE: &str = "none";
/// [`LABEL_OUTPUT_TRUNCATED`] 唯一的真值：写入侧与读取侧各拼一次会漂移。
pub(crate) const TRUNCATED_LABEL: &str = "true";
pub(crate) const SOURCE_LIVE: &str = "live";

fn tool_error(message: String) -> ToolError {
    ToolError::ToolCallError(Box::new(StringError(message)))
}

// ============================================================================
// run_machine_task
// ============================================================================

pub struct RunMachineTask {
    agent_access: Arc<dyn AgentAccess>,
    /// 提交人标签：Hub 不校验、也不据此过滤，只进 `source.principal`。
    user_id: String,
    runtime: Weak<MachineRuntime>,
}

impl RunMachineTask {
    pub fn new(
        agent_access: Arc<dyn AgentAccess>,
        user_id: String,
        runtime: Weak<MachineRuntime>,
    ) -> Self {
        Self {
            agent_access,
            user_id,
            runtime,
        }
    }

    /// 已提交项按下标填回快照：`await_all` 的返回顺序与 `ids` 一致，故位置即答案。
    async fn submit_all(
        &self,
        runtime: &MachineRuntime,
        hub: &TaskHub,
        prepared: &[PreparedTask],
        wait: bool,
        timeout: Duration,
    ) -> String {
        let mut items: Vec<RunItem> = Vec::with_capacity(prepared.len());
        let mut submitted: Vec<(usize, TaskId)> = Vec::new();

        for (index, task) in prepared.iter().enumerate() {
            let kind = match runtime.ensure_machine(&task.agent_name, || {
                self.agent_access.load_agent(&task.agent_name)
            }) {
                Ok(kind) => kind,
                Err(err) => {
                    items.push(RunItem::rejected(index, task, item_runtime_error(err)));
                    continue;
                }
            };
            let built = build_task(
                &kind,
                &task.prompt,
                task.deadline_secs,
                &self.user_id,
                !wait,
                ORIGIN_LABEL,
            );
            match hub.submit(&kind, built).await {
                Ok(id) => {
                    submitted.push((index, id));
                    items.push(RunItem::submitted(index, task, kind, id));
                }
                // 一条失败只影响自己那一项：其余条目继续提交。
                Err(err) => items.push(RunItem::rejected(index, task, submit_error(&err))),
            }
        }

        let ids: Vec<TaskId> = submitted.iter().map(|(_, id)| *id).collect();
        let mut outcomes: Vec<Result<TaskSnapshot, ItemError>> = Vec::with_capacity(ids.len());
        if wait {
            let awaited = hub.await_all(&ids, Some(timeout)).await;
            for (id, result) in ids.iter().zip(awaited) {
                match result {
                    Ok(snapshot) => outcomes.push(Ok(snapshot)),
                    // 超时（或多次订阅仍无终态）不是失败：补取一次快照判真实状态。
                    Err(err) => {
                        debug!(
                            task_id = %id,
                            error = %err,
                            "wait phase ended without a terminal snapshot"
                        );
                        outcomes.push(hub.snapshot(id).await.map_err(hub_unavailable));
                    }
                }
            }
        } else {
            for id in &ids {
                outcomes.push(hub.snapshot(id).await.map_err(hub_unavailable));
            }
        }

        let mut unresolved = false;
        for ((index, _), outcome) in submitted.iter().zip(outcomes) {
            match outcome {
                Ok(snapshot) => {
                    if !snapshot.state.is_terminal() {
                        unresolved = true;
                    }
                    items[*index].report = Some(TaskReport::new(&snapshot));
                }
                // 补取也失败时无从判断是否收敛，按「到点未收敛」计。
                Err(err) => {
                    unresolved = true;
                    items[*index].error = Some(err);
                }
            }
        }

        render_run(wait, wait && unresolved, &items)
    }
}

impl ToolDyn for RunMachineTask {
    fn name(&self) -> String {
        "run_machine_task".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_machine_task".to_string(),
            description: "Submit one or more tasks to other agents and run them concurrently. \
                Each task is one agent turn (model -> tools -> ... -> final answer) running in \
                the background in this workspace. By default this call waits until every task is \
                terminal or timeout_secs elapses (default 300s) and returns each result. Set \
                wait=false to get task ids back immediately and poll them later with \
                inspect_machine_task. A wait timeout never cancels anything: unfinished tasks \
                keep running, their current state is returned, so you can inspect or cancel them \
                afterwards. You cannot choose the machine kind; agent_name selects the agent."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["tasks"],
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 16,
                        "description": "Tasks to submit. They run concurrently; completion order is not the array order.",
                        "items": {
                            "type": "object",
                            "required": ["agent_name", "prompt"],
                            "properties": {
                                "agent_name": {
                                    "type": "string",
                                    "description": "Existing agent name: ASCII letters, digits, '_' or '-', at most 64 bytes."
                                },
                                "prompt": {
                                    "type": "string",
                                    "description": "The complete task description handed to that agent as one user message."
                                },
                                "deadline_secs": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "maximum": 86400,
                                    "description": "Optional wall-clock budget; on expiry the task becomes TimedOut and keeps what it produced. Omit for no wall-clock deadline (only the agent's own iteration limit applies)."
                                }
                            }
                        }
                    },
                    "wait": {
                        "type": "boolean",
                        "default": true,
                        "description": "true: block until every task is terminal or timeout_secs elapses. false: return right after submission."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 3600,
                        "default": 300,
                        "description": "Upper bound for the wait phase when wait=true; omit to use 300. Expiry is not a cancellation."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed: RunArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            // 全量校验先做完：任何一条不合法都不得先提交一半。
            let (wait, timeout, prepared) = prepare(parsed)?;

            let runtime = match self.runtime.upgrade() {
                Some(runtime) => runtime,
                None => {
                    return Ok(Content::Text(render_rejected(
                        &prepared,
                        wait,
                        runtime_gone(),
                    )));
                }
            };
            let hub = match runtime.hub() {
                Ok(hub) => hub,
                Err(err) => {
                    return Ok(Content::Text(render_rejected(
                        &prepared,
                        wait,
                        no_runtime(&err),
                    )));
                }
            };
            Ok(Content::Text(
                self.submit_all(&runtime, &hub, &prepared, wait, timeout)
                    .await,
            ))
        })
    }
}

fn wait_by_default() -> bool {
    true
}

#[derive(Deserialize)]
struct RunArgs {
    tasks: Vec<TaskArgs>,
    #[serde(default = "wait_by_default")]
    wait: bool,
    timeout_secs: Option<u64>,
}

impl RunArgs {
    fn resolved_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS))
    }
}

#[derive(Deserialize)]
struct TaskArgs {
    agent_name: String,
    prompt: String,
    deadline_secs: Option<u64>,
}

/// 校验通过的一条任务：`agent_name` 已是 trim 后的值，拼接 kind 与回显同源。
struct PreparedTask {
    agent_name: String,
    prompt: String,
    deadline_secs: Option<u64>,
}

fn prepare(parsed: RunArgs) -> Result<(bool, Duration, Vec<PreparedTask>), ToolError> {
    if parsed.tasks.is_empty() || parsed.tasks.len() > MAX_TASKS {
        return Err(tool_error(format!(
            "tasks must contain between 1 and {MAX_TASKS} items"
        )));
    }
    if let Some(secs) = parsed.timeout_secs
        && !(1..=MAX_WAIT_SECS).contains(&secs)
    {
        return Err(tool_error(format!(
            "timeout_secs must be in 1..={MAX_WAIT_SECS}, got {secs}"
        )));
    }

    let wait = parsed.wait;
    let timeout = parsed.resolved_timeout();
    let mut prepared = Vec::with_capacity(parsed.tasks.len());
    for task in parsed.tasks {
        let name = validate_agent_name(&task.agent_name)
            .map_err(|err| invalid_name_error(&task.agent_name, err))?;
        if let Some(secs) = task.deadline_secs
            && !(1..=MAX_DEADLINE_SECS).contains(&secs)
        {
            return Err(tool_error(format!(
                "deadline_secs must be in 1..={MAX_DEADLINE_SECS}, got {secs}"
            )));
        }
        prepared.push(PreparedTask {
            agent_name: name.to_string(),
            prompt: task.prompt,
            deadline_secs: task.deadline_secs,
        });
    }
    Ok((wait, timeout, prepared))
}

fn invalid_name_error(raw: &str, err: RuntimeError) -> ToolError {
    match err {
        RuntimeError::InvalidAgentName { reason, .. } => {
            tool_error(format!("invalid agent_name \"{raw}\": {reason}"))
        }
        // `validate_agent_name` 只产上面那一个变体。
        other => tool_error(other.to_string()),
    }
}

/// 任务构造的唯一实现点：字段取值即 Hub 与 `AgentMachine` 的入参契约。
///
/// `is_async` 只影响 `labels`：异步提交（`wait=false`）没有等待期，结果只能靠
/// 台账扫描发现并唤醒提交人，扫描侧需要这个键才能把同步调用排除掉。
pub(crate) fn build_task(
    kind: &str,
    prompt: &str,
    deadline_secs: Option<u64>,
    principal: &str,
    is_async: bool,
    origin: &str,
) -> Task {
    Task {
        // Hub 会用自己分配的 id 与 kind 覆盖这两个字段。
        id: TaskId(Uuid::nil()),
        kind: kind.to_string(),
        input: TaggedJson {
            tag: PROMPT_TAG.to_string(),
            payload: Value::String(prompt.to_string()),
        },
        context: TaskContext {
            scope: None,
            data: None,
        },
        source: TaskSource::External {
            principal: principal.to_string(),
        },
        deadline: deadline_secs.map(|secs| Utc::now() + chrono::Duration::seconds(secs as i64)),
        labels: {
            let mut labels = BTreeMap::from([("origin".to_string(), origin.to_string())]);
            if is_async {
                labels.insert(ASYNC_LABEL.to_string(), "true".to_string());
            }
            labels
        },
        created_at: Utc::now(),
    }
}

// ============================================================================
// inspect_machine_task
// ============================================================================

pub struct InspectMachineTask {
    runtime: Weak<MachineRuntime>,
}

impl InspectMachineTask {
    pub fn new(runtime: Weak<MachineRuntime>) -> Self {
        Self { runtime }
    }
}

impl ToolDyn for InspectMachineTask {
    fn name(&self) -> String {
        "inspect_machine_task".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "inspect_machine_task".to_string(),
            description: "Observe tasks previously submitted with run_machine_task in this \
                workspace. action=status: read one task's current state and, once terminal, its \
                final answer. action=cancel: request cancellation of one task. Idempotent; \
                returns once the request is recorded, not when the task converged, so poll status \
                to confirm. Task state is hosted by this workspace's hub: an id stays observable \
                until the workspace is destroyed, and state is not persisted across restarts. \
                Cancelling the parent turn does not cancel tasks already submitted here."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["action", "task_id"],
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "cancel"],
                        "description": "status: read the task's current state and its final answer once terminal. cancel: request cancellation; idempotent."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "The task id returned by run_machine_task. Required for both actions."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed: InspectArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let action = parsed.action.as_str();
            if !matches!(action, "status" | "cancel") {
                return Err(tool_error(
                    "action must be one of: status, cancel".to_string(),
                ));
            }
            let raw = parsed
                .task_id
                .ok_or_else(|| tool_error(format!("task_id is required for action={action}")))?;
            let id = Uuid::parse_str(&raw).map_err(|_| {
                tool_error(format!(
                    "invalid task_id \"{raw}\": expected the uuid returned by run_machine_task"
                ))
            })?;
            let id = TaskId(id);

            let runtime = self.runtime.upgrade().ok_or_else(|| {
                tool_error("machine runtime is gone (workspace dropped)".to_string())
            })?;
            let hub = runtime.hub().map_err(|err| tool_error(err.to_string()))?;

            if action == "status" {
                let snapshot = hub
                    .snapshot(&id)
                    .await
                    .map_err(|err| inspect_hub_error(id, err))?;
                return Ok(Content::Text(render_status(&snapshot)));
            }

            // 请求前快照是 `already_terminal` 的唯一判据：`cancel` 对终态直接返回 Ok。
            let before = hub
                .snapshot(&id)
                .await
                .map_err(|err| inspect_hub_error(id, err))?;
            hub.cancel(&id)
                .await
                .map_err(|err| inspect_hub_error(id, err))?;
            Ok(Content::Text(render_cancel(id, before.state)))
        })
    }
}

#[derive(Deserialize)]
struct InspectArgs {
    action: String,
    task_id: Option<String>,
}

fn inspect_hub_error(id: TaskId, err: MachineError) -> ToolError {
    match err {
        MachineError::NotFound { .. } => {
            tool_error(format!("task '{id}' not found in this workspace"))
        }
        other => tool_error(format!("machine hub is unavailable: {other}")),
    }
}

/// `status` 与 `run` 共用同一份额度：声明块先占，正文只拿剩下的；正文超预算时
/// 先剥 `task.artifacts`（与声明块重复）。`status` 没有 `prompt` 回显可剥。
fn render_status(snapshot: &TaskSnapshot) -> String {
    render_status_with(snapshot, "status", None)
}

/// 供跨空间 `inspect_workspace_task` 复用同一份额度与裁剪逻辑。
pub(crate) fn render_task_status(snapshot: &TaskSnapshot) -> String {
    render_status(snapshot)
}

/// `await` 与 `status` 同形状，只多一个 `timed_out`。
pub(crate) fn render_task_await(snapshot: &TaskSnapshot, timed_out: bool) -> String {
    render_status_with(snapshot, "await", Some(timed_out))
}

fn render_status_with(snapshot: &TaskSnapshot, action: &str, timed_out: Option<bool>) -> String {
    let report = TaskReport::new(snapshot);
    let (decls, artifacts_truncated) = collect_declarations([report.artifacts.as_slice()]);
    let body_budget = remaining_body_budget(&decls);

    let mut task = report.core();
    task.insert("deadline".to_string(), json!(snapshot.deadline));
    task.insert("created_at".to_string(), json!(snapshot.created_at));
    task.insert("updated_at".to_string(), json!(snapshot.updated_at));
    // 按 id 复查才需要「这份正文从哪来、缺了多少」：`run` 的每条结果共用一份
    // 紧预算，同一个形状在那边只是白占额度。
    task.insert("source".to_string(), json!(report.source));
    task.insert(
        "output_retention".to_string(),
        json!(report.output_retention),
    );
    task.insert("output_bytes".to_string(), json!(report.output_bytes()));
    task.insert(
        "output_total_bytes".to_string(),
        json!(report.output_total_bytes),
    );
    task.insert(
        "output_omitted_head_bytes".to_string(),
        json!(report.output_omitted_head_bytes),
    );
    task.insert("note".to_string(), report.note());
    let mut rows = vec![Value::Object(task)];

    finish_report(
        &decls,
        body_budget,
        &mut rows,
        |rows| status_body(rows, action, timed_out, artifacts_truncated),
        &[reclaim_artifacts],
    )
}

fn status_body(
    rows: &[Value],
    action: &str,
    timed_out: Option<bool>,
    artifacts_truncated: bool,
) -> Value {
    let mut fields = Map::new();
    fields.insert("action".to_string(), json!(action));
    fields.insert("found".to_string(), json!(true));
    fields.insert(
        "task".to_string(),
        rows.first().cloned().unwrap_or(Value::Null),
    );
    if let Some(timed_out) = timed_out {
        fields.insert("timed_out".to_string(), json!(timed_out));
    }
    insert_artifacts_truncated(&mut fields, artifacts_truncated);
    Value::Object(fields)
}

/// `accepted` 恒 `true`（请求本身就是成功的），所以「什么都没做」只能由 `note` 表达。
pub(crate) fn render_cancel(task_id: TaskId, state: TaskState) -> String {
    let note = if state.is_terminal() {
        CANCEL_INERT_NOTE
    } else {
        CANCEL_NOTE
    };
    let body = json!({
        "action": "cancel",
        "task_id": task_id.to_string(),
        "accepted": true,
        "already_terminal": state.is_terminal(),
        "state": state,
        "note": note,
    });
    serde_json::to_string_pretty(&body).expect("a cancel report is always serializable")
}

// ============================================================================
// 渲染：快照 → 结果条目
// ============================================================================

/// 一条已提交任务的呈现素材：正文已按预算裁剪，产物已还原成声明。
struct TaskReport {
    task_id: TaskId,
    kind: String,
    state: TaskState,
    /// 正文来源：`live` 是本进程的 Hub，`ledger` 是重启后从台账里捞回来的。
    source: String,
    /// 正文留存口径，作为一等字段外发：调用方据它决定该不该去 `action=output` 取全文，
    /// 而不是从 `note` 文案与会为 `null` 的正文长度反推。无从判定时为 `None`。
    output_retention: Option<&'static str>,
    output: Option<String>,
    output_truncated: bool,
    output_lossy: bool,
    /// 正文总字节数；拿到的不保证是原文的一份预览时为 `None`。
    output_total_bytes: Option<u64>,
    /// `preview` 丢掉的头部长；同上。
    output_omitted_head_bytes: Option<u64>,
    /// 手上这份正文的字符数（`digest_only` 时是摘要长度）；没有正文时为 `None`。
    total_chars: Option<usize>,
    /// 只剩台账摘要：这份正文不是原文，长度与缺口都不可知。
    digest_only: bool,
    /// 台账正文在落库时就被上限截断过：连它的长度都不是原文总长。
    persisted_prefix: bool,
    /// 台账里既无正文又无摘要：`output_truncated` / `output_lossy` 都不该为真。
    retention_none: bool,
    error: Option<TaskError>,
    artifacts: Vec<ArtifactDecl>,
}

impl TaskReport {
    fn new(snapshot: &TaskSnapshot) -> Self {
        let (body, body_decls) = snapshot_text(snapshot);
        let retention = snapshot.labels.get(LABEL_OUTPUT_RETENTION);
        let digest_only = retention.map(String::as_str) == Some(RETENTION_DIGEST_ONLY);
        let retention_none = retention.map(String::as_str) == Some(RETENTION_NONE);
        let persisted_prefix = snapshot
            .labels
            .get(LABEL_OUTPUT_TRUNCATED)
            .is_some_and(|flag| flag == TRUNCATED_LABEL);
        // 摘要代人原文、或台账正文自己就被截断过：两种情况下长度都不是原文的，
        // 报一个数只会让「缺了多少」看起来可算。
        let lengths_unknown = digest_only || persisted_prefix;
        // 原文全长的字符数：`action=output` 按字符分页，提示里的 N 必须同口径。
        // 摘要代原文时 `text_output` 给的就是摘要，N 自然是摘要长。
        let total_chars = text_output(snapshot).map(|text| text.chars().count());
        let source = snapshot
            .labels
            .get(LABEL_SOURCE)
            .cloned()
            .unwrap_or_else(|| SOURCE_LIVE.to_string());
        // 台账的留存口径是写入侧的事实，读取侧只照抄；认不出的值给 `null` 而不是
        // 当成 `full`。活快照没有留存策略可查，只能按手上有没有正文分两档 ——
        // `digest_only` 是台账独有的形态，在这里凭空造出来就是撒谎。
        let output_retention = if source == SOURCE_LEDGER {
            match retention.map(String::as_str) {
                Some(RETENTION_FULL) => Some(RETENTION_FULL),
                Some(RETENTION_DIGEST_ONLY) => Some(RETENTION_DIGEST_ONLY),
                Some(RETENTION_NONE) => Some(RETENTION_NONE),
                _ => None,
            }
        } else {
            Some(if body.text.is_some() {
                RETENTION_FULL
            } else {
                RETENTION_NONE
            })
        };
        Self {
            task_id: snapshot.id,
            kind: snapshot.kind.clone(),
            state: snapshot.state,
            source,
            output_retention,
            // 摘要冒充全文是这里最坏的错误：`digest_only` 时它必须同时可信地
            // 声明「丢了字节」与「给的是摘要」。
            output_truncated: body.truncated || lengths_unknown,
            output_lossy: body.lossy || digest_only,
            output_total_bytes: if lengths_unknown {
                None
            } else {
                body.total_bytes
            },
            output_omitted_head_bytes: if lengths_unknown {
                None
            } else {
                body.omitted_head_bytes
            },
            output: body.text,
            total_chars,
            digest_only,
            persisted_prefix,
            retention_none,
            error: snapshot
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.error.clone()),
            artifacts: decls_from_snapshot(snapshot, body_decls),
        }
    }

    /// 交给调用方的正文字节数（已被预览裁剪）：0 表示这段正文是空的。
    fn output_bytes(&self) -> usize {
        self.output.as_ref().map_or(0, String::len)
    }

    /// 摘要替不了原文、或预览只给了尾部时，必须由本字段说清缺口与取法，
    /// 而不是让读者从长度里猜。
    fn note(&self) -> Value {
        match self.note_text() {
            Some(text) => Value::String(text),
            None => Value::Null,
        }
    }

    fn note_text(&self) -> Option<String> {
        // 没到终态就没有全文可取：这时指向 `action=output` 只会让调用方撞空。
        if !self.state.is_terminal() {
            return None;
        }
        if self.retention_none {
            return Some("全文与摘要都未留存，这次任务的输出无法取回".to_string());
        }
        if self.digest_only {
            let chars = self.total_chars.unwrap_or(MAX_DIGEST_CHARS);
            return Some(format!(
                "全文未留存（任务早于 v11）：完整正文共 {chars} 字符，这只是 {chars} 字摘要的长度，\
                 原文长度不可考；用 action=output 读取这段摘要"
            ));
        }
        if self.persisted_prefix {
            return Some(
                "台账正文在落库时已按上限截断，原文总长不可考；用 action=output 分页读取已留存部分（带 offset 续读）"
                    .to_string(),
            );
        }
        match (self.output_omitted_head_bytes, self.total_chars) {
            (Some(omitted), Some(chars)) if omitted > 0 => Some(format!(
                "完整正文共 {chars} 字符，这里只是尾部预览；用 action=output 读取（带 offset 续读后一页）"
            )),
            _ => None,
        }
    }

    /// `run` 与 `inspect status` 共用的字段集合；两者只在此基础上加减字段。
    fn core(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        fields.insert("task_id".to_string(), json!(self.task_id.to_string()));
        // 台账不存 machine `kind`，空串是「不知道」：给 `null`，不给一个空名字。
        let kind = if self.kind.is_empty() {
            Value::Null
        } else {
            Value::String(self.kind.clone())
        };
        fields.insert("kind".to_string(), kind);
        fields.insert("state".to_string(), json!(self.state));
        fields.insert("terminal".to_string(), json!(self.state.is_terminal()));
        fields.insert("output".to_string(), json!(self.output));
        fields.insert("output_truncated".to_string(), json!(self.output_truncated));
        fields.insert("output_lossy".to_string(), json!(self.output_lossy));
        fields.insert(
            "error".to_string(),
            self.error.as_ref().map_or(Value::Null, task_error_json),
        );
        fields.insert("artifacts".to_string(), json!(self.artifacts));
        fields
    }
}

/// 没有快照时的同键空壳：提交失败（或读快照失败）的条目也要形状一致。
fn empty_core() -> Map<String, Value> {
    let mut fields = Map::new();
    for key in ["task_id", "kind", "state", "output", "error"] {
        fields.insert(key.to_string(), Value::Null);
    }
    fields.insert("terminal".to_string(), json!(false));
    fields.insert("output_truncated".to_string(), json!(false));
    fields.insert("output_lossy".to_string(), json!(false));
    fields.insert("artifacts".to_string(), json!([]));
    fields
}

fn task_error_json(error: &TaskError) -> Value {
    let mut fields = Map::new();
    fields.insert("kind".to_string(), json!(error.kind));
    fields.insert("message".to_string(), json!(error.message));
    // 「重试了几次」是模型自纠最需要的字段，其余 detail 属宿主内部信息。
    if matches!(error.kind.as_str(), "rate_limited" | "model_unavailable")
        && let Some(detail) = &error.detail
        && detail.tag == "attempts"
        && let Some(attempts) = detail.payload.as_u64()
    {
        fields.insert("attempts".to_string(), json!(attempts));
    }
    Value::Object(fields)
}

/// item 级失败：`kind` 是模型据以决策的稳定字符串，`message` 直接来自错误源。
#[derive(Clone)]
struct ItemError {
    kind: &'static str,
    message: String,
}

impl ItemError {
    fn json(&self) -> Value {
        json!({ "kind": self.kind, "message": self.message })
    }
}

fn runtime_gone() -> ItemError {
    ItemError {
        kind: "runtime_unavailable",
        message: "machine runtime is gone (workspace dropped)".to_string(),
    }
}

fn no_runtime(err: &RuntimeError) -> ItemError {
    ItemError {
        kind: "no_runtime",
        message: err.to_string(),
    }
}

/// `RuntimeError::Display` 就是对外文案，这里只补上机器可读的 `kind`。
fn item_runtime_error(err: RuntimeError) -> ItemError {
    let kind = match &err {
        RuntimeError::NoRuntime => "no_runtime",
        RuntimeError::InvalidAgentName { .. } => "invalid_agent_name",
        RuntimeError::AgentLoad { .. } => "agent_load_failed",
        RuntimeError::Register { .. } => "register_failed",
    };
    ItemError {
        kind,
        message: err.to_string(),
    }
}

fn submit_error(err: &MachineError) -> ItemError {
    match err {
        MachineError::Illegal { reason } => ItemError {
            kind: "hub_rejected",
            message: format!("hub rejected the request: {reason}"),
        },
        MachineError::Unavailable { reason } => ItemError {
            kind: "hub_unavailable",
            message: format!("machine hub is unavailable: {reason}"),
        },
        // 提交路径上 `NotFound` / `UnknownKind` 不可达（machine 刚由 `ensure_machine` 注册）。
        other => ItemError {
            kind: "hub_rejected",
            message: format!("hub rejected the request: {other}"),
        },
    }
}

fn hub_unavailable(err: MachineError) -> ItemError {
    let reason = match &err {
        MachineError::Unavailable { reason } | MachineError::Illegal { reason } => reason.clone(),
        other => other.to_string(),
    };
    ItemError {
        kind: "hub_unavailable",
        message: format!("machine hub is unavailable: {reason}"),
    }
}

struct RunItem {
    index: usize,
    agent_name: String,
    prompt: String,
    kind: Option<String>,
    task_id: Option<TaskId>,
    /// 提交成功且读到快照才有；读不到时由 `error` 说明原因。
    report: Option<TaskReport>,
    error: Option<ItemError>,
}

impl RunItem {
    fn rejected(index: usize, task: &PreparedTask, error: ItemError) -> Self {
        Self {
            index,
            agent_name: task.agent_name.clone(),
            prompt: prompt_echo(&task.prompt),
            kind: None,
            task_id: None,
            report: None,
            error: Some(error),
        }
    }

    fn submitted(index: usize, task: &PreparedTask, kind: String, task_id: TaskId) -> Self {
        Self {
            index,
            agent_name: task.agent_name.clone(),
            prompt: prompt_echo(&task.prompt),
            kind: Some(kind),
            task_id: Some(task_id),
            report: None,
            error: None,
        }
    }

    fn to_json(&self) -> Value {
        let mut fields = match &self.report {
            Some(report) => report.core(),
            None => empty_core(),
        };
        fields.insert("index".to_string(), json!(self.index));
        fields.insert("agent_name".to_string(), json!(self.agent_name));
        fields.insert("prompt".to_string(), json!(self.prompt));
        fields.insert("truncated".to_string(), json!(false));
        if self.report.is_none() {
            fields.insert(
                "task_id".to_string(),
                self.task_id.map_or(Value::Null, |id| json!(id.to_string())),
            );
            fields.insert(
                "kind".to_string(),
                self.kind.clone().map_or(Value::Null, Value::String),
            );
        }
        if let Some(error) = &self.error {
            fields.insert("error".to_string(), error.json());
        }
        Value::Object(fields)
    }
}

fn render_rejected(prepared: &[PreparedTask], waited: bool, error: ItemError) -> String {
    let items: Vec<RunItem> = prepared
        .iter()
        .enumerate()
        .map(|(index, task)| RunItem::rejected(index, task, error.clone()))
        .collect();
    render_run(waited, false, &items)
}

/// 正文主体按 `index` 升序逐条插入并重新量长，一旦超预算就把本条正文置空，此后不再
/// 分配，因此永远不会出现「半条被截断」；声明块与后续回剥交给 `finish_report`。
fn render_run(waited: bool, timed_out: bool, items: &[RunItem]) -> String {
    let sources = items
        .iter()
        .filter_map(|item| item.report.as_ref())
        .map(|report| report.artifacts.as_slice());
    let (decls, artifacts_truncated) = collect_declarations(sources);
    let body_budget = remaining_body_budget(&decls);
    let build = |results: &[Value]| run_body(waited, timed_out, results, artifacts_truncated);

    let mut results: Vec<Value> = Vec::with_capacity(items.len());
    let mut exhausted = false;
    for item in items {
        let mut value = item.to_json();
        if exhausted {
            drop_output(&mut value);
        }
        results.push(value);
        if !exhausted && serialized_len(&build(&results)) > body_budget {
            if let Some(last) = results.last_mut() {
                drop_output(last);
            }
            exhausted = true;
        }
    }

    // 逐条 `artifacts` 与声明块内容重复，先剥；`prompt` 是调用方自己写过的输入的回显；
    // `error.message` 是本工具唯一没有夹长点的字段（长度由错误源决定），留到最后夹长。
    finish_report(
        &decls,
        body_budget,
        &mut results,
        build,
        &[
            reclaim_artifacts,
            reclaim_prompt_echo,
            reclaim_error_message,
        ],
    )
}

/// 声明块与正文共用 `MAX_RESULT_BYTES`：块先按自己的上限收口并实测字节数，正文只拿剩下的。
fn remaining_body_budget(decls: &[ArtifactDecl]) -> usize {
    MAX_RESULT_BYTES.saturating_sub(render_artifact_lines(decls).len())
}

/// 结果文本 = 声明块 + JSON 正文，两者合计不超过 `MAX_RESULT_BYTES`。
///
/// `reclaims` 是预算不够时的回剥顺序（从最可替代的内容排到最不可替代的）；`rows` 是正文里
/// 可被整体剥空的那几个对象，`build` 负责把当前 `rows` 装成完整正文。剥遍全部 `reclaims`
/// 仍超说明骨架自己越界，已无字段可剥，只能如实记账。
fn finish_report(
    decls: &[ArtifactDecl],
    body_budget: usize,
    rows: &mut [Value],
    build: impl Fn(&[Value]) -> Value,
    reclaims: &[fn(&mut Value)],
) -> String {
    let mut body = build(rows);
    for reclaim in reclaims {
        if serialized_len(&body) <= body_budget {
            break;
        }
        for row in rows.iter_mut() {
            reclaim(row);
        }
        body = build(rows);
    }

    let body = serde_json::to_string_pretty(&body)
        .expect("a JSON object of task reports is always serializable");
    if body.len() > body_budget {
        // 剥遍阶梯后每条只剩定长字段与已夹长的 `error.message`（≤ MAX_ERROR_MESSAGE_BYTES
        // 加省略号）；再越界只可能是宿主侧字段（如 `kind`）异常长，已无字段可剥。
        warn!(
            body = body.len(),
            budget = body_budget,
            "task report skeleton alone exceeds the result budget"
        );
    }
    with_artifact_lines(&body, decls)
}

/// 声明块的收集顺序是来源顺序 → 来源内快照顺序，与结果条目的呈现顺序一致；
/// `run` 的每个来源是一个任务，`status` 只有一个任务。
///
/// 条数或字节数任一达上限即停，后续声明一律丢弃 —— 只有丢弃时才置
/// `artifacts_truncated`，模型据此知道「还有产物没给」。
fn collect_declarations<'a>(
    sources: impl IntoIterator<Item = &'a [ArtifactDecl]>,
) -> (Vec<ArtifactDecl>, bool) {
    let mut decls: Vec<ArtifactDecl> = Vec::new();
    let mut bytes = 0usize;
    let mut dropped = false;

    'sources: for source in sources {
        for decl in source {
            let decl = bounded_decl(decl);
            let line_bytes = render_artifact_lines(std::slice::from_ref(&decl)).len();
            if decls.len() >= MAX_DECLARATIONS_TOTAL || bytes + line_bytes > MAX_DECLARATIONS_BYTES
            {
                dropped = true;
                break 'sources;
            }
            bytes += line_bytes;
            decls.push(decl);
        }
    }

    (decls, dropped)
}

/// 单条声明的夹长：`path` 保留尾部（文件名在尾部，模型靠它认产物），`title` 保留头部。
fn bounded_decl(decl: &ArtifactDecl) -> ArtifactDecl {
    ArtifactDecl {
        path: tail_within(&decl.path, MAX_DECL_PATH_BYTES),
        title: decl
            .title
            .as_deref()
            .map(|title| head_within(title, MAX_DECL_TITLE_BYTES)),
        mime: decl.mime.clone(),
    }
}

/// 保留尾部并让省略号落在字符边界上：先按字节切，再前移到边界。
fn tail_within(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

/// 保留头部并退到字符边界：截断标记必须落在合法 UTF-8 边界上。
fn head_within(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn run_body(waited: bool, timed_out: bool, results: &[Value], artifacts_truncated: bool) -> Value {
    let mut fields = Map::new();
    fields.insert("waited".to_string(), json!(waited));
    fields.insert("timed_out".to_string(), json!(timed_out));
    fields.insert("results".to_string(), json!(results));
    insert_artifacts_truncated(&mut fields, artifacts_truncated);
    Value::Object(fields)
}

/// `artifacts_truncated` 恒在整份结果的顶层（与前置声明块同级），`run` 与 `status` 一致；
/// 缺键即「声明一条没丢」，模型不必区分 `false` 与不存在。
fn insert_artifacts_truncated(fields: &mut Map<String, Value>, truncated: bool) {
    if truncated {
        fields.insert("artifacts_truncated".to_string(), json!(true));
    }
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_string_pretty(value)
        .expect("a JSON object of task reports is always serializable")
        .len()
}

/// 预算裁剪先保 `task_id` 与 `error`：它们才是模型能据以决策的字段。
fn drop_output(value: &mut Value) {
    if let Value::Object(fields) = value {
        fields.insert("output".to_string(), Value::Null);
        fields.insert("truncated".to_string(), Value::Bool(true));
    }
}

/// 逐条 `artifacts` 与前置声明块重复：预算不够时留空，声明仍从块里交付
/// （超出块上限的那部分还能由 `inspect status` 取回）。
fn reclaim_artifacts(value: &mut Value) {
    if let Value::Object(fields) = value {
        fields.insert("artifacts".to_string(), Value::Array(Vec::new()));
        fields.insert("truncated".to_string(), Value::Bool(true));
    }
}

/// `prompt` 是调用方自己写过的输入的回显：预算不够时留空串，形状不变。
fn reclaim_prompt_echo(value: &mut Value) {
    if let Value::Object(fields) = value {
        fields.insert("prompt".to_string(), Value::String(String::new()));
        fields.insert("truncated".to_string(), Value::Bool(true));
    }
}

/// 无预算压力时 `error.message` 原样透出；越界时才夹到头部，`kind`/`attempts` 不动。
fn reclaim_error_message(value: &mut Value) {
    let Value::Object(fields) = value else {
        return;
    };
    let clamped = fields
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| message.len() > MAX_ERROR_MESSAGE_BYTES)
        .map(|message| head_within(message, MAX_ERROR_MESSAGE_BYTES));
    let Some(clamped) = clamped else {
        return;
    };
    if let Some(Value::Object(error)) = fields.get_mut("error") {
        error.insert("message".to_string(), Value::String(clamped));
    }
    fields.insert("truncated".to_string(), Value::Bool(true));
}

fn prompt_echo(prompt: &str) -> String {
    head_within(prompt, MAX_PROMPT_ECHO_BYTES)
}

/// 一次有界预览：文本、缺了多少、能不能解码，以及找回缺失所需的两个数。
///
/// 「有字节没给你」与「给你的字节里有不可解码的」互相独立，故是两个布尔。
struct Preview {
    text: String,
    truncated: bool,
    lossy: bool,
    /// 原文总字节数（Hub 窗口起点 + 窗口长度）。
    total_bytes: u64,
    /// 被丢掉的头部长。
    omitted_head_bytes: u64,
}

fn preview(bytes: &[u8], hub_start_offset: u64) -> Preview {
    let dropped_head = bytes.len().saturating_sub(MAX_OUTPUT_PREVIEW_BYTES);
    let window = &bytes[dropped_head..];
    // 窗口起点可能切在多字节字符中间：最多前跳 3 个续字节（0b10xx_xxxx）。
    let mut start = 0;
    while start < window.len().min(3) && (window[start] & 0xC0) == 0x80 {
        start += 1;
    }
    // 从尾部保留、只丢头部：模型要的是「最后发生了什么」。
    let rest = &window[start..];
    let lossy = std::str::from_utf8(rest).is_err();
    if lossy {
        warn!(
            bytes = rest.len(),
            "task output preview contains invalid UTF-8; undecodable bytes replaced"
        );
    }
    Preview {
        text: String::from_utf8_lossy(rest).into_owned(),
        truncated: dropped_head > 0 || start > 0 || hub_start_offset > 0,
        lossy,
        total_bytes: hub_start_offset.saturating_add(bytes.len() as u64),
        omitted_head_bytes: dropped_head as u64,
    }
}

/// 呈现用正文与「缺了多少」的补偿量。
struct OutputBody {
    text: Option<String>,
    truncated: bool,
    lossy: bool,
    /// 原文总字节数；无从得知时为 `None`。
    total_bytes: Option<u64>,
    /// 被丢掉的头部长；无从得知时为 `None`。
    omitted_head_bytes: Option<u64>,
}

impl OutputBody {
    fn from_preview(preview: Preview) -> Self {
        Self {
            text: non_empty(preview.text, preview.truncated || preview.lossy),
            truncated: preview.truncated,
            lossy: preview.lossy,
            total_bytes: Some(preview.total_bytes),
            omitted_head_bytes: Some(preview.omitted_head_bytes),
        }
    }

    /// 没有正文可给：两个长度也一并是未知，而不是 0。
    fn empty() -> Self {
        Self {
            text: None,
            truncated: false,
            lossy: false,
            total_bytes: None,
            omitted_head_bytes: None,
        }
    }
}

/// 空结果给 `null` 而不是空串：模型据此区分「没有正文」与「正文是空的」。
fn non_empty(text: String, incomplete: bool) -> Option<String> {
    if text.is_empty() && !incomplete {
        None
    } else {
        Some(text)
    }
}

/// 正文权威：`outcome.output` 的文本载荷，否则看有界的有损尾窗。
///
/// 同时返回正文里自带的声明：`outcome.output` 可能含子 agent 自己写的声明行，
/// 不剥离就会漏进调用方上下文并诱导它照抄。
fn snapshot_text(snapshot: &TaskSnapshot) -> (OutputBody, Vec<ArtifactDecl>) {
    if let Some(text) = text_output(snapshot) {
        let (clean, decls) = strip_artifact_lines(text);
        return (
            OutputBody::from_preview(preview(clean.as_bytes(), 0)),
            decls,
        );
    }
    // 跑完却没有文本载荷：这份任务没有正文可给，尾窗里的半截流不是结果。
    if snapshot.state == TaskState::Completed {
        return (OutputBody::empty(), Vec::new());
    }

    (
        OutputBody::from_preview(preview(&snapshot.output.tail, snapshot.output.start_offset)),
        Vec::new(),
    )
}

/// 终态正文的文本载荷；非文本（结构化载荷）没有可展示的原文。
pub(crate) fn text_output(snapshot: &TaskSnapshot) -> Option<&str> {
    let output = snapshot.outcome.as_ref()?.output.as_ref()?;
    if output.tag != TEXT_OUTPUT_TAG {
        return None;
    }
    output.payload.as_str()
}

/// 快照是产物权威：正文里重复声明的 `path` 不再补一次，正文内部冲突时后写获胜
/// （与 Hub 的覆盖语义一致）。
fn decls_from_snapshot(snapshot: &TaskSnapshot, body: Vec<ArtifactDecl>) -> Vec<ArtifactDecl> {
    let mut decls: Vec<ArtifactDecl> = Vec::with_capacity(snapshot.artifacts.len());
    for artifact in &snapshot.artifacts {
        if artifact.payload.tag != ARTIFACT_DECL_TAG {
            continue;
        }
        match serde_json::from_value::<ArtifactDecl>(artifact.payload.payload.clone()) {
            Ok(decl) => decls.push(decl),
            Err(err) => warn!(
                artifact = %artifact.id.0,
                error = %err,
                "artifact payload is not a declaration; skipped"
            ),
        }
    }

    let mut from_body: Vec<ArtifactDecl> = Vec::new();
    for decl in body {
        if decls.iter().any(|known| known.path == decl.path) {
            continue;
        }
        match from_body.iter_mut().find(|known| known.path == decl.path) {
            Some(slot) => *slot = decl,
            None => from_body.push(decl),
        }
    }
    decls.extend(from_body);
    decls
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::Notify;

    use super::*;
    use crate::agent::Agent;
    use crate::agent::test_support::{
        GateTool, MapAgentAccess, StaticTool, completed, scripted_agent, scripted_agent_with_tools,
        tool_call_response,
    };
    use crate::artifacts::ARTIFACT_MARKER;
    use crate::machine::{Artifact, ArtifactId, HubConfig, OutputView, Progress, TaskFilter};

    const BUDGET: Duration = Duration::from_secs(5);

    async fn within_budget<F: Future<Output = ()>>(body: F) {
        tokio::time::timeout(BUDGET, body)
            .await
            .expect("the test body must finish within the budget");
    }

    fn test_runtime() -> Arc<MachineRuntime> {
        Arc::new(MachineRuntime::with_hub_config(HubConfig {
            emit_min_interval: Duration::ZERO,
            scan_interval: Duration::from_millis(10),
            terminal_grace: Duration::from_millis(50),
            cancel_grace: Duration::from_millis(50),
            ..HubConfig::default()
        }))
    }

    fn runner(pairs: &[(&str, Arc<Agent>)], runtime: &Arc<MachineRuntime>) -> RunMachineTask {
        let mut agents = HashMap::new();
        for (name, agent) in pairs {
            agents.insert(name.to_string(), Arc::clone(agent));
        }
        RunMachineTask::new(
            Arc::new(MapAgentAccess(agents)),
            "test-user".to_string(),
            Arc::downgrade(runtime),
        )
    }

    /// 每个 agent 必须持有自己的 provider：共用脚本队列时并发执行会互相抢响应。
    fn scripted_agent_named(output: &str) -> Arc<Agent> {
        let (agent, _provider) = scripted_agent(vec![completed(output)]);
        agent
    }

    /// 卡在第一次工具调用上的 agent：`release` 之前任务停在 `Working`。
    fn gated_agent() -> (Arc<Agent>, Arc<Notify>, Arc<Notify>) {
        let gate = GateTool::new("gate", Content::Text("ok".to_string()));
        let entered = Arc::clone(&gate.entered);
        let release = Arc::clone(&gate.release);
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-1", "gate", json!({})),
                completed("done"),
            ],
            vec![Box::new(gate)],
        );
        (agent, entered, release)
    }

    fn empty_filter() -> TaskFilter {
        TaskFilter {
            kind: None,
            state: None,
            labels: BTreeMap::new(),
            scope: None,
            created_after: None,
        }
    }

    fn task_id_of(value: &Value) -> TaskId {
        let raw = value.as_str().expect("task_id must be a string");
        TaskId(Uuid::parse_str(raw).expect("task_id must be a uuid"))
    }

    fn parse(text: &str) -> (Value, Vec<ArtifactDecl>) {
        let (body, decls) = strip_artifact_lines(text);
        (
            serde_json::from_str(&body).expect("the stripped body must stay valid JSON"),
            decls,
        )
    }

    async fn run_result(
        tool: &RunMachineTask,
        args: Value,
    ) -> Result<(Value, Vec<ArtifactDecl>), ToolError> {
        let content = tool.call(args.to_string()).await?;
        let Content::Text(text) = content else {
            panic!("run_machine_task must return plain text, got {content:?}");
        };
        Ok(parse(&text))
    }

    async fn run_json(tool: &RunMachineTask, args: Value) -> (Value, Vec<ArtifactDecl>) {
        run_result(tool, args)
            .await
            .expect("run_machine_task must succeed")
    }

    async fn inspect_json(
        tool: &InspectMachineTask,
        args: Value,
    ) -> Result<(Value, Vec<ArtifactDecl>), ToolError> {
        let content = tool.call(args.to_string()).await?;
        let Content::Text(text) = content else {
            panic!("inspect_machine_task must return plain text, got {content:?}");
        };
        Ok(parse(&text))
    }

    fn one_task(agent_name: &str) -> Value {
        json!({ "tasks": [{ "agent_name": agent_name, "prompt": "p" }] })
    }

    #[tokio::test]
    async fn a_single_task_returns_the_finished_output() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("hello"))], &runtime);

            let (value, decls) = run_json(
                &tool,
                json!({ "tasks": [{ "agent_name": "worker", "prompt": "say hi" }] }),
            )
            .await;

            assert_eq!(value["waited"], json!(true));
            assert_eq!(value["timed_out"], json!(false));
            let item = &value["results"][0];
            assert_eq!(item["index"], json!(0));
            assert_eq!(item["agent_name"], json!("worker"));
            assert_eq!(item["prompt"], json!("say hi"));
            assert_eq!(item["kind"], json!("agent:worker"));
            assert_eq!(item["state"], json!("completed"));
            assert_eq!(item["terminal"], json!(true));
            assert_eq!(item["output"], json!("hello"));
            assert_eq!(item["output_truncated"], json!(false));
            assert_eq!(item["output_lossy"], json!(false));
            assert_eq!(item["error"], Value::Null);
            assert_eq!(item["truncated"], json!(false));
            assert!(decls.is_empty());

            // Hub 是唯一权威：拿返回的 id 立刻可读。
            let id = task_id_of(&item["task_id"]);
            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .snapshot(&id)
                .await
                .expect("the finished task is observable");
            assert_eq!(snapshot.state, TaskState::Completed);
        })
        .await;
    }

    #[tokio::test]
    async fn batch_results_follow_the_input_order() {
        within_budget(async {
            let runtime = test_runtime();
            // 交换入参顺序各跑一次：结果与入参逐项对齐，而不是按完成顺序排列。
            for (order, expected) in [(["a", "b"], ["a", "b"]), (["b", "a"], ["b", "a"])] {
                let tool = runner(
                    &[
                        ("a", scripted_agent_named("a")),
                        ("b", scripted_agent_named("b")),
                    ],
                    &runtime,
                );
                let tasks: Vec<Value> = order
                    .iter()
                    .map(|name| json!({ "agent_name": name, "prompt": "p" }))
                    .collect();

                let (value, _) = run_json(&tool, json!({ "tasks": tasks })).await;

                let results = value["results"].as_array().expect("results array");
                assert_eq!(results.len(), 2);
                for (index, name) in expected.iter().enumerate() {
                    assert_eq!(results[index]["agent_name"], json!(name));
                    assert_eq!(results[index]["output"], json!(name));
                }
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_missing_agent_does_not_poison_the_other_tasks() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("worker done"))], &runtime);

            let (value, _) = run_json(
                &tool,
                json!({
                    "tasks": [
                        { "agent_name": "worker", "prompt": "p" },
                        { "agent_name": "ghost", "prompt": "p" }
                    ]
                }),
            )
            .await;

            let results = value["results"].as_array().expect("results array");
            assert_eq!(results.len(), 2, "a failed submission keeps its slot");
            assert_eq!(results[0]["state"], json!("completed"));
            assert_eq!(results[0]["output"], json!("worker done"));
            assert_eq!(results[1]["task_id"], Value::Null);
            assert_eq!(results[1]["kind"], Value::Null);
            assert_eq!(results[1]["state"], Value::Null);
            assert_eq!(results[1]["error"]["kind"], json!("agent_load_failed"));
            assert_eq!(value["timed_out"], json!(false));

            // 不用 hub.list：它在终态瞬间有落地窗口。
            let id = task_id_of(&results[0]["task_id"]);
            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .snapshot(&id)
                .await
                .expect("the finished task is observable");
            assert_eq!(snapshot.state, TaskState::Completed);
        })
        .await;
    }

    /// 变异点：把加载失败分支的 `continue` 改成 `break` ⇒ 第 3 条任务不再提交。
    #[tokio::test]
    async fn a_failed_submission_in_the_middle_keeps_the_remaining_tasks() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(
                &[
                    ("worker", scripted_agent_named("worker done")),
                    ("helper", scripted_agent_named("helper done")),
                ],
                &runtime,
            );

            let (value, _) = run_json(
                &tool,
                json!({
                    "tasks": [
                        { "agent_name": "worker", "prompt": "first" },
                        { "agent_name": "ghost", "prompt": "second" },
                        { "agent_name": "helper", "prompt": "third" }
                    ]
                }),
            )
            .await;

            let results = value["results"].as_array().expect("results array");
            assert_eq!(
                results.len(),
                3,
                "a failed submission must not end the batch"
            );
            assert_eq!(results[0]["state"], json!("completed"));
            assert_eq!(results[0]["output"], json!("worker done"));
            assert_eq!(results[1]["error"]["kind"], json!("agent_load_failed"));
            assert_eq!(results[1]["task_id"], Value::Null);
            assert_eq!(
                results[2]["state"],
                json!("completed"),
                "the task after a failed one still runs: {}",
                results[2]
            );
            assert_eq!(results[2]["output"], json!("helper done"));
            assert!(results[2]["task_id"].is_string());
        })
        .await;
    }

    #[tokio::test]
    async fn wait_false_returns_the_id_before_the_task_finishes() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, entered, release) = gated_agent();
            let tool = runner(&[("worker", agent)], &runtime);

            let (value, _) = run_json(
                &tool,
                json!({
                    "tasks": [{ "agent_name": "worker", "prompt": "p" }],
                    "wait": false
                }),
            )
            .await;

            assert_eq!(value["waited"], json!(false));
            assert_eq!(value["timed_out"], json!(false));
            let item = &value["results"][0];
            let id = task_id_of(&item["task_id"]);
            let state = item["state"]
                .as_str()
                .expect("a submitted task reports a state");
            assert!(
                matches!(state, "submitted" | "working"),
                "a task returned by wait=false is not terminal yet, got {state}"
            );
            assert_eq!(item["output"], Value::Null);

            entered.notified().await;
            release.notify_one();
            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .await_terminal(&id, Some(Duration::from_secs(3)))
                .await
                .expect("the task converges once the gate opens");
            assert_eq!(snapshot.state, TaskState::Completed);
        })
        .await;
    }

    #[tokio::test]
    async fn a_wait_timeout_leaves_the_task_running_and_never_cancels_it() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, entered, release) = gated_agent();
            let tool = Arc::new(runner(&[("worker", agent)], &runtime));

            let call = {
                let tool = Arc::clone(&tool);
                tokio::spawn(async move {
                    let args = json!({
                        "tasks": [{ "agent_name": "worker", "prompt": "p" }],
                        "timeout_secs": 1
                    });
                    tool.call(args.to_string()).await
                })
            };
            entered.notified().await;

            let content = call
                .await
                .expect("the tool call task must not panic")
                .expect("run_machine_task must succeed");
            let Content::Text(text) = content else {
                panic!("run_machine_task must return plain text");
            };
            let (value, _) = parse(&text);

            assert_eq!(value["timed_out"], json!(true));
            let item = &value["results"][0];
            assert_eq!(
                item["state"],
                json!("working"),
                "a wait timeout is not a cancellation"
            );
            assert_eq!(item["terminal"], json!(false));
            assert_eq!(item["error"], Value::Null);
            assert_eq!(item["output"], Value::Null);
            assert_eq!(item["output_truncated"], json!(false));

            // 超时不取消：任务自行收敛到 Completed。
            let id = task_id_of(&item["task_id"]);
            release.notify_one();
            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .await_terminal(&id, Some(Duration::from_secs(3)))
                .await
                .expect("the task keeps running after the wait timed out");
            assert_eq!(snapshot.state, TaskState::Completed);
        })
        .await;
    }

    #[tokio::test]
    async fn inspect_status_reads_the_terminal_state_and_output() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("hello"))], &runtime);
            let (value, _) = run_json(&tool, one_task("worker")).await;
            let id = task_id_of(&value["results"][0]["task_id"]);

            let inspect = InspectMachineTask::new(Arc::downgrade(&runtime));
            let (status, decls) = inspect_json(
                &inspect,
                json!({ "action": "status", "task_id": id.to_string() }),
            )
            .await
            .expect("status must succeed");

            assert_eq!(status["action"], json!("status"));
            assert_eq!(status["found"], json!(true));
            assert_eq!(status["task"]["task_id"], json!(id.to_string()));
            assert_eq!(status["task"]["kind"], json!("agent:worker"));
            assert_eq!(status["task"]["state"], json!("completed"));
            assert_eq!(status["task"]["terminal"], json!(true));
            assert_eq!(status["task"]["output"], json!("hello"));
            assert_eq!(status["task"]["error"], Value::Null);
            assert_eq!(status["task"]["deadline"], Value::Null);
            assert!(status["task"]["created_at"].is_string());
            assert!(status["task"]["updated_at"].is_string());
            assert!(decls.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn cancel_is_idempotent_and_leaves_the_task_canceled() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, entered, _release) = gated_agent();
            let tool = runner(&[("worker", agent)], &runtime);
            let (value, _) = run_json(
                &tool,
                json!({
                    "tasks": [{ "agent_name": "worker", "prompt": "p" }],
                    "wait": false
                }),
            )
            .await;
            let id = task_id_of(&value["results"][0]["task_id"]);
            entered.notified().await;

            let inspect = InspectMachineTask::new(Arc::downgrade(&runtime));
            let args = json!({ "action": "cancel", "task_id": id.to_string() });

            let (first, _) = inspect_json(&inspect, args.clone())
                .await
                .expect("cancel must succeed");
            assert_eq!(first["accepted"], json!(true));
            assert_eq!(first["already_terminal"], json!(false));
            assert_eq!(first["note"], json!(CANCEL_NOTE));

            let (second, _) = inspect_json(&inspect, args.clone())
                .await
                .expect("a repeated cancel must not fail");
            assert_eq!(second["accepted"], json!(true));

            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .await_terminal(&id, Some(Duration::from_secs(3)))
                .await
                .expect("cancel converges");
            assert_eq!(snapshot.state, TaskState::Canceled);

            let (third, _) = inspect_json(&inspect, args)
                .await
                .expect("cancelling a terminal task must not fail");
            assert_eq!(third["accepted"], json!(true));
            assert_eq!(third["already_terminal"], json!(true));
            assert_eq!(third["state"], json!("canceled"));
        })
        .await;
    }

    /// 终态任务上 cancel 什么都没做：`note` 不得让模型以为「取消已受理」，`state` 如实。
    #[tokio::test]
    async fn cancelling_a_finished_task_reports_that_nothing_happened() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("done"))], &runtime);
            let (value, _) = run_json(&tool, one_task("worker")).await;
            assert_eq!(value["results"][0]["state"], json!("completed"));
            let id = task_id_of(&value["results"][0]["task_id"]);

            let inspect = InspectMachineTask::new(Arc::downgrade(&runtime));
            let (cancel, _) = inspect_json(
                &inspect,
                json!({ "action": "cancel", "task_id": id.to_string() }),
            )
            .await
            .expect("cancelling a finished task must not fail");

            assert_eq!(cancel["accepted"], json!(true));
            assert_eq!(cancel["already_terminal"], json!(true));
            assert_eq!(cancel["state"], json!("completed"));
            let note = cancel["note"].as_str().expect("note is a string");
            assert!(
                !note.contains("cancellation requested"),
                "a finished task must not be told a cancel was requested: {note}"
            );
            assert_eq!(note, CANCEL_INERT_NOTE);
        })
        .await;
    }

    #[tokio::test]
    async fn declarations_are_hoisted_above_the_json_body() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("call-1", "emit", json!({})),
                    completed("final"),
                ],
                vec![Box::new(StaticTool::new(
                    "emit",
                    Content::Text(
                        "ok\n@@PECO_ARTIFACT {\"path\":\"/ws/output/a.jpg\",\"title\":\"fox\"}\n"
                            .to_string(),
                    ),
                ))],
            );
            let tool = runner(&[("worker", agent)], &runtime);

            let content = tool
                .call(one_task("worker").to_string())
                .await
                .expect("run_machine_task must succeed");
            let Content::Text(text) = content else {
                panic!("run_machine_task must return plain text");
            };

            assert!(
                text.lines()
                    .next()
                    .unwrap_or_default()
                    .starts_with(ARTIFACT_MARKER),
                "the declaration must be the first line: {text:?}"
            );
            let (value, decls) = parse(&text);
            assert_eq!(
                decls,
                vec![ArtifactDecl {
                    path: "/ws/output/a.jpg".to_string(),
                    title: Some("fox".to_string()),
                    mime: None,
                }]
            );
            assert!(
                !value.to_string().contains(ARTIFACT_MARKER),
                "the marker must not appear inside the JSON body"
            );
            assert_eq!(value["results"][0]["output"], json!("final"));
            assert_eq!(
                value["results"][0]["artifacts"][0]["path"],
                json!("/ws/output/a.jpg")
            );
            assert_eq!(value["results"][0]["artifacts"][0]["title"], json!("fox"));
        })
        .await;
    }

    #[tokio::test]
    async fn the_last_declaration_for_the_same_path_wins() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("call-1", "emit", json!({})),
                    completed("final"),
                ],
                vec![Box::new(StaticTool::new(
                    "emit",
                    Content::Text(
                        "@@PECO_ARTIFACT {\"path\":\"/ws/output/a.jpg\",\"title\":\"first\"}\n\
                         @@PECO_ARTIFACT {\"path\":\"/ws/output/a.jpg\",\"title\":\"second\"}\n"
                            .to_string(),
                    ),
                ))],
            );
            let tool = runner(&[("worker", agent)], &runtime);

            let (value, decls) = run_json(&tool, one_task("worker")).await;

            let artifacts = value["results"][0]["artifacts"]
                .as_array()
                .expect("artifacts array");
            assert_eq!(
                artifacts.len(),
                1,
                "the hub merges declarations by path: {artifacts:?}"
            );
            assert_eq!(artifacts[0]["title"], json!("second"));
            assert_eq!(decls.len(), 1);
            assert_eq!(decls[0].title.as_deref(), Some("second"));

            let id = task_id_of(&value["results"][0]["task_id"]);
            let snapshot = runtime
                .hub()
                .expect("the hub is available inside a runtime")
                .snapshot(&id)
                .await
                .expect("the finished task is observable");
            assert_eq!(snapshot.artifacts.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn two_submissions_of_the_same_agent_both_succeed() {
        within_budget(async {
            let runtime = test_runtime();
            let (agent, _provider) = scripted_agent(vec![completed("one"), completed("two")]);
            let tool = runner(&[("worker", agent)], &runtime);

            let (first, _) = run_json(&tool, one_task("worker")).await;
            let (second, _) = run_json(&tool, one_task("worker")).await;

            for value in [&first, &second] {
                assert_eq!(value["results"][0]["state"], json!("completed"));
                assert_eq!(value["results"][0]["kind"], json!("agent:worker"));
                let id = task_id_of(&value["results"][0]["task_id"]);
                runtime
                    .hub()
                    .expect("the hub is available inside a runtime")
                    .snapshot(&id)
                    .await
                    .expect("both tasks stay observable");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn two_agents_get_distinct_kinds() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(
                &[
                    ("a", scripted_agent_named("a")),
                    ("b", scripted_agent_named("b")),
                ],
                &runtime,
            );

            let (value, _) = run_json(
                &tool,
                json!({
                    "tasks": [
                        { "agent_name": "a", "prompt": "p" },
                        { "agent_name": "b", "prompt": "p" }
                    ]
                }),
            )
            .await;

            assert_eq!(value["results"][0]["kind"], json!("agent:a"));
            assert_eq!(value["results"][1]["kind"], json!("agent:b"));
            assert_eq!(value["results"][0]["state"], json!("completed"));
            assert_eq!(value["results"][1]["state"], json!("completed"));
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_agent_names_are_rejected_before_anything_is_submitted() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("ok"))], &runtime);
            let too_long = "x".repeat(65);
            let mut rejected: Vec<&str> =
                vec!["", "  ", "..", ".hidden", "a/b", "a\\b", "a b", "a:b"];
            rejected.push(&too_long);

            for raw in rejected {
                let error = run_result(&tool, one_task(raw))
                    .await
                    .expect_err(&format!("agent name {raw:?} must be rejected"));
                assert!(
                    error.to_string().contains("invalid agent_name"),
                    "unexpected error for {raw:?}: {error}"
                );
            }
            let hub = runtime
                .hub()
                .expect("the hub is available inside a runtime");
            assert!(
                hub.list(&empty_filter()).await.expect("list").is_empty(),
                "a rejected name must not leave a task behind"
            );

            // trim 后合法：kind 与回显同源。
            let (value, _) = run_json(&tool, one_task(" worker ")).await;
            assert_eq!(value["results"][0]["agent_name"], json!("worker"));
            assert_eq!(value["results"][0]["kind"], json!("agent:worker"));

            // 字符集内的名字通过校验，失败只能来自加载。
            let (value, _) = run_result(&tool, one_task("ok_agent-1"))
                .await
                .expect("a valid name is accepted");
            assert_eq!(
                value["results"][0]["error"]["kind"],
                json!("agent_load_failed")
            );
        })
        .await;
    }

    #[tokio::test]
    async fn out_of_range_budgets_are_rejected_without_side_effects() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("ok"))], &runtime);
            let too_many: Vec<Value> = (0..MAX_TASKS + 1)
                .map(|_| json!({ "agent_name": "worker", "prompt": "p" }))
                .collect();

            let cases = [
                (
                    json!({ "tasks": [] }),
                    format!("tasks must contain between 1 and {MAX_TASKS} items"),
                ),
                (
                    json!({ "tasks": too_many }),
                    format!("tasks must contain between 1 and {MAX_TASKS} items"),
                ),
                (
                    json!({ "tasks": [{ "agent_name": "worker", "prompt": "p", "deadline_secs": 0 }] }),
                    "deadline_secs must be in 1..=86400, got 0".to_string(),
                ),
                (
                    json!({ "tasks": [{ "agent_name": "worker", "prompt": "p" }], "timeout_secs": 0 }),
                    "timeout_secs must be in 1..=3600, got 0".to_string(),
                ),
            ];

            for (args, expected) in cases {
                let error = run_result(&tool, args)
                    .await
                    .expect_err("an out-of-range argument is a tool error");
                assert_eq!(error.to_string(), expected);
            }
            assert!(
                runtime
                    .hub()
                    .expect("the hub is available inside a runtime")
                    .list(&empty_filter())
                    .await
                    .expect("list")
                    .is_empty(),
                "a rejected request must not submit anything"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_result_body_stays_within_the_budget() {
        within_budget(async {
            let runtime = test_runtime();
            let long = "x".repeat(200 * 1024);
            let mut agents = HashMap::new();
            for index in 0..MAX_TASKS {
                let (agent, _provider) = scripted_agent(vec![completed(&long)]);
                agents.insert(format!("w{index}"), agent);
            }
            let tool = RunMachineTask::new(
                Arc::new(MapAgentAccess(agents)),
                "test-user".to_string(),
                Arc::downgrade(&runtime),
            );
            let tasks: Vec<Value> = (0..MAX_TASKS)
                .map(|index| json!({ "agent_name": format!("w{index}"), "prompt": "p" }))
                .collect();

            let content = tool
                .call(json!({ "tasks": tasks }).to_string())
                .await
                .expect("run_machine_task must succeed");
            let Content::Text(text) = content else {
                panic!("run_machine_task must return plain text");
            };

            assert!(
                text.len() <= MAX_RESULT_BYTES,
                "the result body must stay within {MAX_RESULT_BYTES} bytes, got {}",
                text.len()
            );
            let (value, _) = parse(&text);
            let results = value["results"].as_array().expect("results array");
            assert_eq!(results.len(), MAX_TASKS);
            assert_eq!(
                results[0]["output_truncated"],
                json!(true),
                "the preview keeps only the tail of the answer"
            );
            assert_eq!(results[0]["truncated"], json!(false));

            let dropped: Vec<&Value> = results
                .iter()
                .filter(|item| item["output"].is_null())
                .collect();
            assert!(!dropped.is_empty(), "the budget must drop some outputs");
            for item in dropped {
                assert_eq!(item["truncated"], json!(true));
            }
            for item in results {
                assert!(
                    item["task_id"].is_string(),
                    "task_id must survive the budget: {item}"
                );
                assert!(
                    item.get("error").is_some(),
                    "error must survive the budget: {item}"
                );
            }
        })
        .await;
    }

    fn report_of(decls: Vec<ArtifactDecl>) -> TaskReport {
        TaskReport {
            task_id: TaskId(Uuid::new_v4()),
            kind: "agent:worker".to_string(),
            state: TaskState::Completed,
            output: Some("o".repeat(MAX_OUTPUT_PREVIEW_BYTES)),
            source: "live".to_string(),
            output_retention: Some(RETENTION_FULL),
            output_truncated: true,
            output_lossy: false,
            output_total_bytes: Some(MAX_OUTPUT_PREVIEW_BYTES as u64),
            output_omitted_head_bytes: Some(0),
            total_chars: Some(MAX_OUTPUT_PREVIEW_BYTES),
            digest_only: false,
            persisted_prefix: false,
            retention_none: false,
            error: None,
            artifacts: decls,
        }
    }

    fn run_item(index: usize, prompt_bytes: usize, decls: Vec<ArtifactDecl>) -> RunItem {
        RunItem {
            index,
            agent_name: "worker".to_string(),
            prompt: prompt_echo(&"p".repeat(prompt_bytes)),
            kind: Some("agent:worker".to_string()),
            task_id: Some(TaskId(Uuid::new_v4())),
            report: Some(report_of(decls)),
            error: None,
        }
    }

    fn long_decls(task: usize, count: usize) -> Vec<ArtifactDecl> {
        (0..count)
            .map(|index| ArtifactDecl {
                path: format!("/ws/output/t{task}/i{index}/{}.jpg", "p".repeat(180)),
                title: Some("t".repeat(90)),
                mime: None,
            })
            .collect()
    }

    fn wide_decls(task: usize, count: usize) -> Vec<ArtifactDecl> {
        (0..count)
            .map(|index| {
                let prefix = format!("/ws/output/t{task}/i{index}/");
                ArtifactDecl {
                    path: format!("{prefix}{}", "p".repeat(400 - prefix.len())),
                    title: None,
                    mime: None,
                }
            })
            .collect()
    }

    /// provider 文案的长度不受本工具约束：500 个汉字就是 1 500 B。
    fn rate_limited_error() -> TaskError {
        TaskError {
            kind: "rate_limited".to_string(),
            message: "中".repeat(500),
            detail: Some(TaggedJson {
                tag: "attempts".to_string(),
                payload: json!(3),
            }),
        }
    }

    fn failed_item(index: usize, error: TaskError, decls: Vec<ArtifactDecl>) -> RunItem {
        let mut item = run_item(index, MAX_PROMPT_ECHO_BYTES, decls);
        if let Some(report) = item.report.as_mut() {
            report.error = Some(error);
        }
        item
    }

    /// `status` 读到的快照：产物权威在 `artifacts`，正文与提示词回显都不参与。
    fn status_snapshot(artifact_count: usize, path_bytes: usize) -> TaskSnapshot {
        let artifacts = (0..artifact_count)
            .map(|index| Artifact {
                id: ArtifactId(format!("a{index}")),
                kind: ARTIFACT_DECL_TAG.to_string(),
                offset: 0,
                append: false,
                last_chunk: true,
                payload: TaggedJson {
                    tag: ARTIFACT_DECL_TAG.to_string(),
                    payload: json!({ "path": format!("/ws/output/{index}/{}.jpg", "p".repeat(path_bytes)) }),
                },
            })
            .collect();
        TaskSnapshot {
            id: TaskId(Uuid::new_v4()),
            kind: "agent:worker".to_string(),
            state: TaskState::Completed,
            state_detail: None,
            progress: Progress {
                phase: None,
                current: None,
                total: None,
            },
            output: OutputView {
                tail: Vec::new(),
                start_offset: 0,
                end_offset: 0,
            },
            outcome: None,
            artifacts,
            deadline: None,
            source: TaskSource::External {
                principal: "test-user".to_string(),
            },
            labels: BTreeMap::new(),
            scope: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_seq: 0,
        }
    }

    fn short_decls(task: usize, count: usize) -> Vec<ArtifactDecl> {
        (0..count)
            .map(|index| ArtifactDecl {
                path: format!("/ws/output/t{task}-{index}.jpg"),
                title: None,
                mime: None,
            })
            .collect()
    }

    /// 变异点：声明块字节不从正文预算里扣除 ⇒ 多留一条 8 KiB 正文，最终文本顶破上限。
    #[test]
    fn the_declaration_block_is_deducted_from_the_result_budget() {
        let items: Vec<RunItem> = (0..MAX_TASKS)
            .map(|index| run_item(index, 4, long_decls(index, 2)))
            .collect();

        let text = render_run(true, false, &items);

        assert!(
            text.len() <= MAX_RESULT_BYTES,
            "the whole result must stay within {MAX_RESULT_BYTES} bytes, got {}",
            text.len()
        );
        let (value, decls) = parse(&text);
        let block = render_artifact_lines(&decls);
        assert!(
            block.len() <= MAX_DECLARATIONS_BYTES,
            "the declaration block must stay within {MAX_DECLARATIONS_BYTES} bytes, got {}",
            block.len()
        );
        assert!(
            !decls.is_empty(),
            "the budget must still deliver declarations"
        );
        assert_eq!(
            value["artifacts_truncated"],
            json!(true),
            "dropped declarations must be announced"
        );
        // 骨架永远保留：预算再怎么紧也不动 task_id / state / error。
        let results = value["results"].as_array().expect("results array");
        assert_eq!(results.len(), MAX_TASKS);
        for item in results {
            assert!(
                item["task_id"].is_string(),
                "task_id must survive the budget: {item}"
            );
            assert_eq!(item["state"], json!("completed"));
            assert!(
                item.get("error").is_some(),
                "error must survive the budget: {item}"
            );
        }
    }

    /// 变异点：回剥阶梯删掉 `reclaim_error_message` ⇒ 16 条 1.5 KiB 的 provider 文案
    /// 把整份结果顶到预算之外。
    #[test]
    fn a_long_error_message_is_clamped_to_keep_the_result_within_the_budget() {
        let items: Vec<RunItem> = (0..MAX_TASKS)
            .map(|index| failed_item(index, rate_limited_error(), wide_decls(index, 64)))
            .collect();

        let text = render_run(true, false, &items);

        assert!(
            text.len() <= MAX_RESULT_BYTES,
            "the whole result must stay within {MAX_RESULT_BYTES} bytes, got {}",
            text.len()
        );
        let (value, _) = parse(&text);
        let results = value["results"].as_array().expect("results array");
        assert_eq!(results.len(), MAX_TASKS);
        for item in results {
            let error = &item["error"];
            assert_eq!(
                error["kind"],
                json!("rate_limited"),
                "kind must survive: {item}"
            );
            assert_eq!(error["attempts"], json!(3), "attempts must survive: {item}");
            let message = error["message"]
                .as_str()
                .expect("a task error keeps its message");
            assert!(
                message.ends_with('…'),
                "an over-long message is clamped with a visible cut: {item}"
            );
            assert!(
                message.len() <= MAX_ERROR_MESSAGE_BYTES + '…'.len_utf8(),
                "a clamped message must stay at its own cap, got {}",
                message.len()
            );
            assert_eq!(item["truncated"], json!(true));
        }
    }

    /// 变异点：回剥阶梯删掉 `reclaim_prompt_echo` ⇒ 正文与逐条 `artifacts` 剥空后仍越界。
    #[test]
    fn the_prompt_echo_is_reclaimed_when_the_body_still_overflows() {
        let items: Vec<RunItem> = (0..MAX_TASKS)
            .map(|index| run_item(index, MAX_PROMPT_ECHO_BYTES, wide_decls(index, 2)))
            .collect();

        let text = render_run(true, false, &items);

        assert!(
            text.len() <= MAX_RESULT_BYTES,
            "the whole result must stay within {MAX_RESULT_BYTES} bytes, got {}",
            text.len()
        );
        let (value, decls) = parse(&text);
        let block = render_artifact_lines(&decls);
        assert!(
            block.len() <= MAX_DECLARATIONS_BYTES && block.len() + 512 > MAX_DECLARATIONS_BYTES,
            "the block must sit close to its own cap, got {}",
            block.len()
        );

        let results = value["results"].as_array().expect("results array");
        assert_eq!(results.len(), MAX_TASKS);
        for item in results {
            assert_eq!(
                item["prompt"],
                json!(""),
                "an overflowing body gives up the prompt echo: {item}"
            );
            assert_eq!(item["truncated"], json!(true));
            assert!(item["task_id"].is_string(), "task_id must survive: {item}");
        }
    }

    /// 变异点：声明块字节不从 `status` 的正文预算里扣除 ⇒ 逐条 `artifacts` 数组整段
    /// 留在正文里，整份文本顶破上限。
    #[test]
    fn the_status_report_stays_within_the_result_budget() {
        let snapshot = status_snapshot(MAX_DECLARATIONS_TOTAL, 400);

        let text = render_status(&snapshot);

        assert!(
            text.len() <= MAX_RESULT_BYTES,
            "the status report must stay within {MAX_RESULT_BYTES} bytes, got {}",
            text.len()
        );
        let (value, decls) = parse(&text);
        let block = render_artifact_lines(&decls);
        assert!(
            block.len() <= MAX_DECLARATIONS_BYTES,
            "the declaration block must stay within {MAX_DECLARATIONS_BYTES} bytes, got {}",
            block.len()
        );
        assert!(
            !decls.is_empty(),
            "the budget must still deliver declarations"
        );
        assert!(
            text.starts_with(ARTIFACT_MARKER),
            "the declaration block must precede the body"
        );
        assert_eq!(
            value["artifacts_truncated"],
            json!(true),
            "dropped declarations must be announced"
        );
        assert_eq!(
            value["task"]["artifacts"],
            json!([]),
            "the per-task copy duplicates the block, so it is reclaimed first"
        );
        assert!(
            value["task"]["task_id"].is_string(),
            "task_id must survive the budget"
        );
    }

    /// 条数上限是跨任务的：每任务 8 条 × 16 条任务也只能给 64 条。
    #[test]
    fn the_declaration_count_is_capped_across_tasks() {
        let items: Vec<RunItem> = (0..MAX_TASKS)
            .map(|index| run_item(index, 4, short_decls(index, 8)))
            .collect();

        let text = render_run(true, false, &items);
        let (value, decls) = parse(&text);

        assert_eq!(decls.len(), MAX_DECLARATIONS_TOTAL);
        assert_eq!(value["artifacts_truncated"], json!(true));
    }

    /// 没有声明被丢时不加标记键：模型不必区分 `false` 与不存在。
    #[test]
    fn declarations_within_the_caps_are_not_flagged() {
        let text = render_run(true, false, &[run_item(0, 8, short_decls(0, 1))]);
        let (value, decls) = parse(&text);

        assert_eq!(decls.len(), 1);
        assert!(
            value.get("artifacts_truncated").is_none(),
            "no declaration was dropped, so no flag: {value}"
        );
    }

    /// 单条声明的两端截断：`path` 留尾部（文件名在后面）、`title` 留头部，且仍是合法 JSON。
    #[test]
    fn a_single_declaration_is_clamped_to_its_own_limit() {
        // 尾部留 7 B 而非 8 B，让 512 B 的尾窗起点落在 `深` 的三个字节中间。
        let decl = ArtifactDecl {
            path: format!("/out/{}/fox.jp", "深".repeat(200)),
            title: Some(format!("{}尾巴", "标".repeat(100))),
            mime: Some("image/jpeg".to_string()),
        };

        let bounded = bounded_decl(&decl);

        let kept_path = bounded.path.trim_start_matches('…');
        assert!(bounded.path.starts_with('…'), "the cut must be marked");
        assert!(
            bounded.path.ends_with("fox.jp"),
            "the file name identifies it"
        );
        assert!(
            kept_path.starts_with('深'),
            "the window must back off to a character boundary: {kept_path:?}"
        );
        assert!(kept_path.len() <= MAX_DECL_PATH_BYTES);
        assert!(decl.path.ends_with(kept_path));

        let title = bounded.title.clone().expect("the title is kept");
        let kept_title = title.trim_end_matches('…');
        assert!(title.ends_with('…'), "the cut must be marked");
        assert!(title.starts_with('标'), "the head of the title is kept");
        assert!(kept_title.len() <= MAX_DECL_TITLE_BYTES);
        assert!(
            decl.title
                .as_deref()
                .unwrap_or_default()
                .starts_with(kept_title)
        );

        assert_eq!(bounded.mime.as_deref(), Some("image/jpeg"));
        // 截断后的字段仍须是合法 JSON 字符串：声明行靠 serde 往返。
        let line = render_artifact_lines(std::slice::from_ref(&bounded));
        assert_eq!(strip_artifact_lines(&line).1, vec![bounded]);
    }

    #[tokio::test]
    async fn a_dropped_runtime_reports_runtime_unavailable_per_item() {
        within_budget(async {
            let runtime = test_runtime();
            let tool = runner(&[("worker", scripted_agent_named("ok"))], &runtime);
            drop(runtime);

            let (value, _) = run_json(&tool, one_task("worker")).await;

            assert_eq!(value["waited"], json!(true));
            assert_eq!(value["timed_out"], json!(false));
            let results = value["results"].as_array().expect("results array");
            assert_eq!(results.len(), 1, "every requested task keeps its slot");
            assert_eq!(results[0]["task_id"], Value::Null);
            assert_eq!(results[0]["error"]["kind"], json!("runtime_unavailable"));
            assert_eq!(
                results[0]["error"]["message"],
                json!("machine runtime is gone (workspace dropped)")
            );
        })
        .await;
    }

    /// 没有 tokio runtime 时 Hub 建不出来：必须逐条报 `no_runtime`，不能 panic。
    #[test]
    fn a_context_without_a_tokio_runtime_reports_no_runtime() {
        let runtime = test_runtime();
        let tool = runner(&[("worker", scripted_agent_named("ok"))], &runtime);

        let (value, _) = futures::executor::block_on(run_json(&tool, one_task("worker")));

        assert_eq!(value["results"][0]["error"]["kind"], json!("no_runtime"));
        assert_eq!(value["results"][0]["state"], Value::Null);
        assert_eq!(value["timed_out"], json!(false));
    }

    #[tokio::test]
    async fn inspect_rejects_unknown_ids_and_malformed_arguments() {
        within_budget(async {
            let runtime = test_runtime();
            let inspect = InspectMachineTask::new(Arc::downgrade(&runtime));
            let missing = Uuid::new_v4();

            let error = inspect_json(
                &inspect,
                json!({ "action": "status", "task_id": missing.to_string() }),
            )
            .await
            .expect_err("an id this workspace never issued is an error");
            assert_eq!(
                error.to_string(),
                format!("task '{missing}' not found in this workspace")
            );

            let error = inspect_json(&inspect, json!({ "action": "status" }))
                .await
                .expect_err("task_id is required");
            assert_eq!(error.to_string(), "task_id is required for action=status");

            let error = inspect_json(
                &inspect,
                json!({ "action": "list", "task_id": missing.to_string() }),
            )
            .await
            .expect_err("the tool exposes no task enumeration");
            assert_eq!(error.to_string(), "action must be one of: status, cancel");

            let error = inspect_json(&inspect, json!({ "action": "status", "task_id": "nope" }))
                .await
                .expect_err("a task id must be a uuid");
            assert_eq!(
                error.to_string(),
                "invalid task_id \"nope\": expected the uuid returned by run_machine_task"
            );
        })
        .await;
    }

    #[test]
    fn preview_keeps_the_tail_and_reports_lossiness_separately() {
        let body = preview(&[0x80, b'a', b'b'], 0);
        assert_eq!(body.text, "ab");
        assert!(
            body.truncated,
            "skipped continuation bytes are missing content"
        );
        assert!(!body.lossy);

        let body = preview(&[0xFF, b'a'], 0);
        assert_eq!(body.text, "\u{FFFD}a");
        assert!(
            body.lossy,
            "a replaced byte means the upstream wrote garbage"
        );

        // 头尾必须可区分：头部标记被丢、尾部标记保留，否则「保头部」的实现也能过。
        let mut long = vec![b'h'; MAX_OUTPUT_PREVIEW_BYTES + 808];
        long[..8].copy_from_slice(b"HEAD-MAR");
        let tail_at = long.len() - 16;
        long[tail_at..].copy_from_slice(b"TAIL-MARK-UNIQUE");
        let body = preview(&long, 0);
        assert_eq!(body.text.len(), MAX_OUTPUT_PREVIEW_BYTES);
        assert!(body.truncated);
        assert!(!body.lossy);
        assert!(
            body.text.contains("TAIL-MARK-UNIQUE"),
            "the newest bytes must survive: {:?}",
            body.text
        );
        assert!(
            !body.text.contains("HEAD-MAR"),
            "the oldest bytes must be dropped: {:?}",
            body.text
        );
        assert_eq!(
            body.text.as_bytes(),
            &long[long.len() - MAX_OUTPUT_PREVIEW_BYTES..],
            "the preview must be exactly the tail window"
        );
        // 补偿量必须对得上：原文总长是全长，缺的就是被丢掉的那 808 字节。
        assert_eq!(body.total_bytes, long.len() as u64);
        assert_eq!(body.omitted_head_bytes, 808);

        // 窗口起点切在多字节字符中间：只该丢掉那 3 个续字节，不得产出替换字符。
        let mut split = vec![0xF0, 0x9F, 0x98, 0x80];
        split.extend(std::iter::repeat_n(b'a', MAX_OUTPUT_PREVIEW_BYTES - 1));
        let body = preview(&split, 0);
        assert_eq!(body.text, "a".repeat(MAX_OUTPUT_PREVIEW_BYTES - 1));
        assert!(body.truncated);
        assert!(
            !body.lossy,
            "a cut multi-byte char must be skipped, not replaced"
        );

        // Hub 早已丢掉更早的字节：窗口本身完整也算「有内容没给你」，总长仍据偏移可算。
        let body = preview(b"abc", 10);
        assert!(body.truncated);
        assert_eq!(body.total_bytes, 13);
        assert_eq!(body.omitted_head_bytes, 0);
    }

    #[test]
    fn the_wait_timeout_defaults_to_300_seconds() {
        let defaults: RunArgs =
            serde_json::from_str(r#"{"tasks":[{"agent_name":"a","prompt":"p"}]}"#).expect("parse");
        assert_eq!(
            defaults.resolved_timeout(),
            Duration::from_secs(DEFAULT_WAIT_TIMEOUT_SECS)
        );
        assert!(defaults.wait, "wait defaults to true");

        let explicit: RunArgs =
            serde_json::from_str(r#"{"tasks":[{"agent_name":"a","prompt":"p"}],"timeout_secs":5}"#)
                .expect("parse");
        assert_eq!(explicit.resolved_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn build_task_maps_the_submission_contract() {
        let task = build_task(
            "agent:worker",
            "do it",
            Some(30),
            "user-7",
            false,
            ORIGIN_LABEL,
        );

        assert_eq!(task.input.tag, "prompt");
        assert_eq!(task.input.payload, json!("do it"));
        assert_eq!(
            task.source,
            TaskSource::External {
                principal: "user-7".to_string()
            }
        );
        assert!(task.parent().is_none(), "a tool submission has no parent");
        assert_eq!(task.context.scope, None);
        assert_eq!(task.context.data, None);
        assert_eq!(
            task.labels.get("origin").map(String::as_str),
            Some("run_machine_task")
        );
        let deadline = task.deadline.expect("a deadline is set");
        let remaining = deadline - Utc::now();
        assert!(remaining > chrono::Duration::seconds(20));
        assert!(remaining <= chrono::Duration::seconds(30));

        // 省略 deadline 时不写墙钟上界：父回合的退出边界只由 timeout_secs 决定。
        assert!(
            build_task("agent:worker", "p", None, "u", false, ORIGIN_LABEL)
                .deadline
                .is_none()
        );
    }

    /// 同步提交（默认 `wait=true`）不得带 `async` 键：台账扫描会把带键的任务
    /// 当成待唤醒项，同步调用当轮已拿到结果，补记只会造成冗余轮次。
    #[test]
    fn only_async_submissions_are_labelled() {
        let sync_task = build_task("agent:worker", "p", None, "u", false, ORIGIN_LABEL);
        assert_eq!(
            sync_task.labels.get("origin").map(String::as_str),
            Some("run_machine_task"),
            "the origin label must not change"
        );
        assert!(
            !sync_task.labels.contains_key("async"),
            "a waited submission carries no async key: {:?}",
            sync_task.labels
        );

        let async_task = build_task("agent:worker", "p", None, "u", true, ORIGIN_LABEL);
        assert_eq!(
            async_task.labels.get("async").map(String::as_str),
            Some("true")
        );
        assert_eq!(async_task.labels.len(), 2, "async 是唯一新增的 label");
    }

    /// 端到端：`wait=false` 落进 Hub 的任务带 `async` 键，`wait=true` 的不带 ——
    /// label 是扫描侧的判据，只在 build_task 上加对还不够，必须真的出现在快照里。
    #[tokio::test]
    async fn the_async_label_reaches_the_hub() {
        within_budget(async {
            let runtime = test_runtime();
            let hub = runtime
                .hub()
                .expect("the hub is available inside a runtime");

            let async_tool = runner(&[("worker", scripted_agent_named("bg"))], &runtime);
            let (value, _) = run_json(
                &async_tool,
                json!({
                    "tasks": [{ "agent_name": "worker", "prompt": "p" }],
                    "wait": false
                }),
            )
            .await;
            let async_id = task_id_of(&value["results"][0]["task_id"]);
            let async_labels = hub
                .snapshot(&async_id)
                .await
                .expect("the submitted task is observable")
                .labels;
            assert_eq!(
                async_labels.get("async").map(String::as_str),
                Some("true"),
                "wait=false must label the task as async: {async_labels:?}"
            );

            let sync_tool = runner(&[("worker", scripted_agent_named("done"))], &runtime);
            let (value, _) = run_json(
                &sync_tool,
                json!({ "tasks": [{ "agent_name": "worker", "prompt": "p" }] }),
            )
            .await;
            let sync_id = task_id_of(&value["results"][0]["task_id"]);
            let sync_labels = hub
                .snapshot(&sync_id)
                .await
                .expect("the waited task is observable")
                .labels;
            assert!(
                !sync_labels.contains_key("async"),
                "wait=true must not label the task as async: {sync_labels:?}"
            );
        })
        .await;
    }

    #[test]
    fn a_failed_item_shares_the_report_key_set() {
        let report = TaskReport {
            task_id: TaskId(Uuid::nil()),
            kind: "agent:worker".to_string(),
            state: TaskState::Completed,
            source: "live".to_string(),
            output_retention: Some(RETENTION_FULL),
            output: None,
            output_truncated: false,
            output_lossy: false,
            output_total_bytes: None,
            output_omitted_head_bytes: None,
            total_chars: None,
            digest_only: false,
            persisted_prefix: false,
            retention_none: false,
            error: None,
            artifacts: Vec::new(),
        };

        let mut with_report: Vec<String> = report.core().keys().cloned().collect();
        let mut without_report: Vec<String> = empty_core().keys().cloned().collect();
        with_report.sort();
        without_report.sort();
        assert_eq!(
            with_report, without_report,
            "both shapes must expose the same fields"
        );
    }

    #[test]
    fn the_tool_definitions_mirror_the_limits() {
        let runtime = test_runtime();
        let definition = runner(&[], &runtime).definition();
        let tasks = &definition.parameters["properties"]["tasks"];

        assert_eq!(tasks["minItems"], json!(1));
        assert_eq!(tasks["maxItems"], json!(MAX_TASKS));
        assert_eq!(
            tasks["items"]["properties"]["deadline_secs"]["maximum"],
            json!(MAX_DEADLINE_SECS)
        );
        assert_eq!(
            definition.parameters["properties"]["timeout_secs"]["maximum"],
            json!(MAX_WAIT_SECS)
        );
        assert_eq!(
            definition.parameters["properties"]["timeout_secs"]["default"],
            json!(DEFAULT_WAIT_TIMEOUT_SECS)
        );
    }
}
