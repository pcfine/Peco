//! 派发任务的报告落盘：任务在哪个空间跑，那个空间 work 根下的 `dispatch/` 里就该
//! 有一份 markdown。
//!
//! 终态有两条独立路径（运行期的 sink、重启对账），两条都要留下文件 —— 只测一条
//! 等于把重启期间补终态的任务交给空白目录。

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{
    EchoMachine, Machine, MachineDescriptor, MachineError, Progress, TaggedJson, Task, TaskContext,
    TaskId, TaskOutcome, TaskSnapshot, TaskSource, TaskState,
};
use peco_daemon::db::dispatches::{self, NewDispatch, WakeState};
use peco_daemon::space::close::close_workspace;
use peco_daemon::space::reconcile::reconcile_ledger;
use peco_daemon::space::report::{self, ReportBody, ReportError, ReportMeta};
use peco_daemon::space::tasks::TaskEvent;
use serde_json::{Value, json};
use support::TestEnv;
use support::new_env;
use tokio::sync::broadcast;
use uuid::Uuid;

const TAIL_MARK: &str = "TAIL-MARK-ONLY-IN-THE-FULL-BODY";

/// 远超台账摘要上限（512 字）的正文：报告里的全文与摘要必须能分开。
fn long_body() -> String {
    format!("{}{TAIL_MARK}", "正".repeat(3000))
}

fn snapshot(task_id: TaskId, state: TaskState, output: Option<&str>) -> TaskSnapshot {
    TaskSnapshot {
        id: task_id,
        kind: "agent".into(),
        state,
        state_detail: None,
        progress: Progress {
            phase: None,
            current: None,
            total: None,
        },
        output: peco_core::machine::OutputView {
            tail: Vec::new(),
            start_offset: 0,
            end_offset: 0,
        },
        outcome: output.map(|text| TaskOutcome {
            output: Some(TaggedJson {
                tag: "text".into(),
                payload: json!(text),
            }),
            error: None,
        }),
        artifacts: Vec::new(),
        deadline: None,
        source: TaskSource::External {
            principal: "local".into(),
        },
        labels: BTreeMap::new(),
        scope: Some("research".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_seq: 0,
    }
}

fn observation(
    target_dir: PathBuf,
    task_id: TaskId,
    prompt_digest: Option<&str>,
) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: "peco".into(),
            principal: "local".into(),
            origin_dir: PathBuf::from("/tmp/peco-origin"),
            conversation_id: None,
        },
        target_dir,
        task_id,
        dispatch_id: Some("disp-1".into()),
        origin_kind: "dispatch_to_workspace".into(),
        agent_name: "@master".into(),
        prompt_digest: prompt_digest.map(str::to_string),
    }
}

/// 打开一个已索引的空间，返回 (ws_id, canonical work_dir)。
async fn open_space(env: &TestEnv, name: &str) -> (String, PathBuf) {
    let work_dir = env.work_dir(name);
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;
    env.open(&ws_id).await.unwrap();
    (ws_id, target_dir)
}

fn report_dir(target_dir: &Path) -> PathBuf {
    target_dir.join("dispatch")
}

/// 空间目录下的报告文件，按名字排序；目录不存在时是空（「一个都没写」）。
fn reports(target_dir: &Path) -> Vec<PathBuf> {
    let dir = report_dir(target_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{} 读不出来: {e}", path.display()))
}

fn one_report(target_dir: &Path) -> String {
    let files = reports(target_dir);
    assert_eq!(files.len(), 1, "报告应当恰好一份：{files:?}");
    read(&files[0])
}

/// 目标空间里的每一个条目名：临时文件与隐藏文件都在内，用于证明没有残留。
fn entry_names(target_dir: &Path) -> Vec<String> {
    let dir = report_dir(target_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

async fn seed_row(
    env: &TestEnv,
    ws_id: &str,
    dir: &Path,
    task_id: &str,
    task_state: &str,
    result_digest: Option<&str>,
) {
    dispatches::insert(
        &env.pool,
        &NewDispatch {
            id: &format!("d_{task_id}"),
            task_id,
            target_ws_id: ws_id,
            target_work_dir: &dir.to_string_lossy(),
            target_team_id: "research",
            origin_ws_id: "peco",
            origin_conversation_id: None,
            agent_name: "@master",
            origin_kind: "dispatch_to_workspace",
            task_state,
            wake_state: WakeState::Pending,
            wake_skip_reason: None,
            prompt_digest: Some("补写一份报告"),
            dispatch_id: Some("disp-1"),
        },
    )
    .await
    .expect("播种台账行");
    // `result_digest` 是 `finish` 才写的列：旧行（正文未落库那一代）的摘要只能手造。
    if let Some(digest) = result_digest {
        sqlx::query("UPDATE d_dispatches SET result_digest = ? WHERE id = ?")
            .bind(digest)
            .bind(format!("d_{task_id}"))
            .execute(&env.pool)
            .await
            .expect("补上摘要列");
    }
}

/// 台账上的三列：终态、唤醒态、终态时刻。报告写不出去时它们必须照旧。
async fn ledger_fields(env: &TestEnv, task_id: &str) -> (String, String, Option<String>) {
    sqlx::query_as("SELECT task_state, wake_state, terminal_at FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(&env.pool)
        .await
        .expect("台账里应当有这一行")
}

async fn ledger_created_at(env: &TestEnv, task_id: &str) -> String {
    sqlx::query_scalar("SELECT created_at FROM d_dispatches WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(&env.pool)
        .await
        .expect("台账里应当有这一行")
}

/// 等到指定 task 的某类事件：广播是异步的，卡着等会把「没发」误判成「慢」。
async fn wait_for_event(
    rx: &mut broadcast::Receiver<TaskEvent>,
    task_id: &str,
    kind: &str,
) -> TaskEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(left, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("{task_id} 的 {kind} 事件没有送达"))
            .expect("广播不得关闭");
        if event.task_id == task_id && event.kind == kind {
            return event;
        }
    }
}

fn text_task(text: &str) -> Task {
    Task {
        // Hub 会覆盖 id 与 kind。
        id: TaskId(Uuid::nil()),
        kind: String::new(),
        input: TaggedJson {
            tag: "text".to_string(),
            payload: Value::String(text.to_string()),
        },
        context: TaskContext {
            scope: None,
            data: None,
        },
        source: TaskSource::External {
            principal: "local".to_string(),
        },
        deadline: None,
        labels: BTreeMap::new(),
        created_at: Utc::now(),
    }
}

/// 把目标任务提交进空间自己的 Hub 并等它终态：对账要看到的是真快照。
async fn run_to_terminal(
    env: &TestEnv,
    target_dir: &Path,
    machine: Arc<dyn Machine>,
    text: &str,
) -> TaskId {
    let handle = env.hub.get(target_dir).expect("空间应当是打开的");
    let runtime = handle
        .machine_runtime()
        .upgrade()
        .expect("runtime 必须活着");
    let hub = runtime.hub().expect("task hub");
    let kind = machine.descriptor().kind;
    match hub.register(machine) {
        Ok(()) | Err(MachineError::Illegal { .. }) => {}
        Err(err) => panic!("machine 注册失败：{err}"),
    }
    let id = hub.submit(&kind, text_task(text)).await.expect("提交任务");
    hub.await_terminal(&id, Some(Duration::from_secs(10)))
        .await
        .expect("任务必须收敛到终态");
    id
}

/// 只交付一个终态、不带正文的 machine：用来制造「没有正文、台账只剩摘要」的旧行。
struct SilentMachine;

#[async_trait]
impl Machine for SilentMachine {
    fn descriptor(&self) -> MachineDescriptor {
        MachineDescriptor {
            kind: "silent".to_string(),
            state_tag: "silent".to_string(),
            input_tag: "text".to_string(),
            description: "test double: finishes without a body".to_string(),
        }
    }

    async fn submit(
        &self,
        _task: Task,
        sink: peco_core::machine::TaskSink,
    ) -> Result<(), MachineError> {
        sink.begin().await.map_err(unavailable)?;
        sink.finish(
            TaskState::Completed,
            TaskOutcome {
                output: None,
                error: None,
            },
        )
        .await
        .map_err(unavailable)
    }

    async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
        Ok(())
    }

    async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        Err(MachineError::NotFound { id: *id })
    }
}

fn unavailable(err: peco_core::machine::SinkError) -> MachineError {
    MachineError::Unavailable {
        reason: err.to_string(),
    }
}

// ============================================================================
// 运行期：sink 的两条路径
// ============================================================================

/// 正常终态：文件落在目标空间、正文是全文、身份从目标目录反查得到。
///
/// 变异点：`path_of` 不进 `dispatch/` ⇒ 落点断言红；正文改用摘要 ⇒
/// 尾段标记与字数断言红。
#[tokio::test]
async fn the_terminal_report_lands_in_the_target_space() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "report-terminal").await;
    let task = TaskId(Uuid::new_v4());
    let body = long_body();
    let obs = observation(
        target_dir.clone(),
        task,
        Some("重构登录流程\n细节不进文件名"),
    );
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, Some(&body)))
        .await;

    let files = reports(&target_dir);
    assert_eq!(files.len(), 1, "终态必须留下一份报告：{files:?}");
    assert_eq!(
        files[0].file_name().unwrap().to_string_lossy(),
        format!("重构登录流程__{}.md", &task.to_string()[..8]),
        "文件名由任务描述与 task_id 决定"
    );
    assert!(
        entry_names(&target_dir)
            .iter()
            .all(|name| !name.contains(".tmp-")),
        "原子写不得留下临时文件"
    );

    let text = read(&files[0]);
    assert!(text.starts_with("---\n"), "frontmatter 必须在最前");
    assert!(text.contains("state: \"completed\""));
    assert!(
        text.contains(&format!("space: \"{ws_id}\"")),
        "空间身份按目标目录反查：{text}"
    );
    assert!(text.contains("team: \"research\""));
    assert!(text.contains("origin_space: \"peco\""));
    assert!(text.contains("agent: \"@master\""));
    assert!(text.contains("retention: full"));
    assert!(text.contains("output_truncated: false"));
    assert!(text.contains("派发摘要（最多 512 字）"), "派发内容要能追溯");
    assert!(text.contains(&body), "正文必须是全文，不是摘要");
    assert!(text.ends_with('\n'));
}

/// 行已 orphan 时 `finish` 返回 0：晚到的终态不得把失联说明改写成 completed。
///
/// 变异点：去掉 `changed > 0` 门控 ⇒ 报告被改写成 completed 且带着晚到的正文，
/// 两条断言同时红。
#[tokio::test]
async fn a_late_terminal_does_not_rewrite_an_orphan_report() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-orphan-gate").await;
    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir.clone(), task, Some("跑一下测试"));
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_orphaned(&obs, "target space closed").await;
    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("LATE-OUTPUT")),
    )
    .await;

    let text = one_report(&target_dir);
    assert!(
        text.contains("state: \"orphan\""),
        "台账已 orphan，报告不得改写：{text}"
    );
    assert!(
        !text.contains("LATE-OUTPUT"),
        "晚到的终态正文不得进报告：{text}"
    );
}

/// 同一任务两次终态：一个路径、一份文件，内容是最新那次。
///
/// 变异点：文件名带上时间戳 ⇒ 两次终态留下两份文件，数量断言红。
#[tokio::test]
async fn a_repeated_terminal_keeps_one_report_with_the_last_body() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-repeat").await;
    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir.clone(), task, Some("同一个任务"));
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("FIRST-BODY")),
    )
    .await;
    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("SECOND-BODY")),
    )
    .await;

    let text = one_report(&target_dir);
    assert!(text.contains("SECOND-BODY"), "{text}");
    assert!(!text.contains("FIRST-BODY"), "旧正文必须被覆盖：{text}");
}

/// 失联：报告是这条任务唯一的交付物，必须写明原因与「没有结果可回捞」。
#[tokio::test]
async fn an_orphan_report_says_the_result_cannot_be_recovered() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-orphan").await;
    let task = TaskId(Uuid::new_v4());
    let obs = observation(target_dir.clone(), task, Some("跑一下测试"));
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_orphaned(&obs, "target hub does not know this task")
        .await;

    let text = one_report(&target_dir);
    assert!(text.contains("state: \"orphan\""));
    assert!(
        text.contains("terminal_at_estimated: true"),
        "失联时刻是估计的"
    );
    assert!(text.contains("无结果可回捞"), "{text}");
    assert!(
        text.contains("target hub does not know this task"),
        "{text}"
    );
}

/// 第二次终态的快照可以没有正文（`finish` 用 `COALESCE` 保住了台账里那份）：
/// 报告必须回读台账，不能跟着快照退化成「无输出」。
///
/// 变异点：报告正文取快照而不读台账 ⇒ 第二次写盘把全文抹成「无输出」，
/// `retention` 与正文断言同时红。
#[tokio::test]
async fn a_repeated_terminal_does_not_degrade_the_report() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-repeat-degrade").await;
    let task = TaskId(Uuid::new_v4());
    let body = long_body();
    let obs = observation(target_dir.clone(), task, Some("重跑一次"));
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, Some(&body)))
        .await;
    // 台账里已有全文；这一次快照什么都没带。
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, None))
        .await;

    let text = one_report(&target_dir);
    assert!(text.contains("state: \"completed\""), "{text}");
    assert!(text.contains(&body), "台账里还在的全文不得被抹掉：{text}");
    assert!(!text.contains("无输出"), "报告不该说自己没有产出：{text}");
    assert!(
        text.contains(&format!("output_chars: {}", body.chars().count())),
        "{text}"
    );
    assert!(text.contains("retention: full"), "{text}");
    assert!(
        !text.contains("created_at: \"未知\""),
        "运行期同样能从台账拿到创建时间：{text}"
    );
}

/// 文件名里的描述段只认台账：重订阅后的观察不再带 `prompt_digest`，若据观察写，
/// 同一个任务会在同一目录里留下 `task__<id8>.md` 与真正的报告两份。
///
/// 变异点：`write_for_observation` 不回读台账 ⇒ 第二次写盘换了文件名，条数断言红。
#[tokio::test]
async fn the_report_path_comes_from_the_ledger_not_the_observation() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-path-source").await;
    let task = TaskId(Uuid::new_v4());
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(
        &observation(target_dir.clone(), task, Some("同一个任务")),
        &snapshot(task, TaskState::Working, None),
    )
    .await;
    sink.on_terminal(
        &observation(target_dir.clone(), task, Some("同一个任务")),
        &snapshot(task, TaskState::Completed, Some("FIRST-BODY")),
    )
    .await;
    // 重订阅拿不到入参摘要 —— 台账里那句还在。
    sink.on_terminal(
        &observation(target_dir.clone(), task, None),
        &snapshot(task, TaskState::Completed, Some("SECOND-BODY")),
    )
    .await;

    let files = reports(&target_dir);
    assert_eq!(files.len(), 1, "同一任务只许一份报告：{files:?}");
    assert_eq!(
        files[0].file_name().unwrap().to_string_lossy(),
        format!("同一个任务__{}.md", &task.to_string()[..8]),
        "描述段来自台账，不随观察丢失"
    );
    let text = read(&files[0]);
    assert!(text.contains("SECOND-BODY"), "{text}");
}

/// 报告写不出去（这里让 `dispatch` 是个同名文件）只许 warn：台账、唤醒
/// 队列与事件广播照旧。磁盘满了不能把派发链路一起拖垮。
#[tokio::test]
async fn a_report_that_cannot_be_written_leaves_the_ledger_and_the_wake_queue_alone() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-unwritable").await;
    std::fs::write(report_dir(&target_dir), "not a directory").expect("用同名文件占住落点");

    let task = TaskId(Uuid::new_v4());
    let mut obs = observation(target_dir.clone(), task, Some("写不出去也要记账"));
    // 必须算「Peco 的待办」，否则这一行注册时就被跳过，唤醒态断言失去意义。
    obs.origin.ws_id = env.env.peco.ws_id.clone();
    let mut events = env.tasks.subscribe();
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(
        &obs,
        &snapshot(task, TaskState::Completed, Some("BODY-THAT-CANNOT-LAND")),
    )
    .await;

    let (task_state, wake_state, terminal_at) = ledger_fields(&env, &task.to_string()).await;
    assert_eq!(task_state, "completed", "写盘失败不得回退台账");
    assert!(
        terminal_at.is_some(),
        "终态时刻必须已经落库，写盘失败不影响它"
    );
    assert_eq!(wake_state, "pending", "该投的唤醒仍要入队");
    let event = wait_for_event(&mut events, &task.to_string(), "terminal").await;
    assert_eq!(event.state, "completed", "事件仍要广播：{event:?}");
    assert!(
        reports(&target_dir).is_empty(),
        "落点是个文件，不该有报告写出来"
    );
}

// ============================================================================
// 重启对账：完全绕过 sink 的那条路径
// ============================================================================

/// 对账补终态：`reconcile_row` 直接调 `finish`，报告必须在这里另写一次。
///
/// 变异点：只在 `on_terminal` 写报告 ⇒ 本用例的目录一直是空的。
#[tokio::test]
async fn reconcile_finishing_writes_the_report_too() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "report-reconcile-terminal").await;
    let body = long_body();
    let task = run_to_terminal(&env, &target_dir, Arc::new(EchoMachine::new()), &body).await;
    // 上个进程留下的在飞行：本进程的 Hub 认识这个任务，但台账还不知道它跑完了。
    seed_row(
        &env,
        &ws_id,
        &target_dir,
        &task.to_string(),
        "working",
        None,
    )
    .await;

    let created_at = ledger_created_at(&env, &task.to_string()).await;
    let outcome = reconcile_ledger(&env.app_state()).await;
    assert_eq!(outcome.finished, 1, "{outcome:?}");

    let text = one_report(&target_dir);
    assert!(text.contains("state: \"completed\""));
    assert!(text.contains(&body), "对账补写的报告同样要带全文");
    assert!(text.contains(&format!("space: \"{ws_id}\"")));
    // 只断「有一个时间」抓不到退化：写成「未知」也满足。要对着台账那一行的实际值断。
    assert!(
        text.contains(&format!("created_at: \"{created_at}\"")),
        "台账行的创建时间要原样落进 frontmatter：{text}"
    );
    assert!(
        !text.contains("created_at: \"未知\""),
        "台账里有时间，报告就不许写「未知」：{text}"
    );
}

/// 先运行期写、再重启对账写：两条路径对同一任务必须落到同一个文件名上，否则
/// 目录里会并排躺着两份内容不同的报告。
///
/// 第二段把台账退回在飞状态，模拟「终态还没来得及落库进程就没了」—— 对账扫的
/// 正是在飞行，只有它能把这个任务补完。
#[tokio::test]
async fn runtime_and_restart_reconcile_land_on_the_same_single_report() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "report-runtime-then-reconcile").await;
    let body = long_body();
    let task = run_to_terminal(&env, &target_dir, Arc::new(EchoMachine::new()), &body).await;
    let obs = observation(target_dir.clone(), task, Some("重启前后同一份"));
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    // 第一段：运行期终态。
    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, Some(&body)))
        .await;
    let files = reports(&target_dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let written_at_runtime = files[0].file_name().unwrap().to_string_lossy().into_owned();

    // 第二段：重启对账补写。
    sqlx::query(
        "UPDATE d_dispatches SET task_state = 'working', terminal_at = NULL WHERE task_id = ?",
    )
    .bind(task.to_string())
    .execute(&env.pool)
    .await
    .expect("把台账退回在飞");
    let outcome = reconcile_ledger(&env.app_state()).await;
    assert_eq!(outcome.finished, 1, "{outcome:?}");

    let files = reports(&target_dir);
    assert_eq!(files.len(), 1, "两条路径写的是同一份：{files:?}");
    assert_eq!(
        files[0].file_name().unwrap().to_string_lossy(),
        written_at_runtime,
        "文件名只由 task_id 与台账摘要决定，两条路径必须一致"
    );
    assert!(read(&files[0]).contains(&body));
}

/// force 关闭是第五条路径：这些行一离开在飞状态就再没有对账会扫到它们，失联
/// 说明只能在这里写；目标目录已经不在时的那些行只 warn、不 panic。
#[tokio::test]
async fn force_close_writes_an_orphan_report_for_every_inflight_row() {
    let env = new_env(&["research"]).await;
    let dir = env.work_dir("report-force-close");
    let ws_id = env.resolve(&dir, "research").await.unwrap();
    env.open(&ws_id).await.unwrap();
    let target_dir = env.work_dir_of(&ws_id).await;

    let landed = Uuid::new_v4().to_string();
    let gone = Uuid::new_v4().to_string();
    seed_row(&env, &ws_id, &target_dir, &landed, "working", None).await;
    let gone_dir = env.home.join("spaces").join("gone").join("work");
    seed_row(&env, &ws_id, &gone_dir, &gone, "working", None).await;

    let outcome = close_workspace(&env.env, &env.hub, &ws_id, true)
        .await
        .expect("force 关闭应当成功");
    assert_eq!(outcome.orphaned, 2, "两行都该被 orphan 化");

    let text = one_report(&target_dir);
    assert!(text.contains("state: \"orphan\""), "{text}");
    assert!(text.contains("无结果可回捞"), "{text}");
    assert!(
        text.contains("空间被强制关闭"),
        "原因要写清是人为决定：{text}"
    );
    assert!(
        !gone_dir.exists(),
        "目标目录都不在了，不得为了写报告把它建出来"
    );
}

/// 对账判孤儿：目标空间没恢复，报告写明原因而不是留空白。
#[tokio::test]
async fn reconcile_orphaning_writes_the_report_too() {
    let env = new_env(&["research"]).await;
    let dir = env.work_dir("report-reconcile-orphan");
    let ws_id = env.resolve(&dir, "research").await.unwrap();
    assert!(!env.is_open(&ws_id).await, "本用例的前提是目标空间没有实例");
    let task = Uuid::new_v4().to_string();
    seed_row(&env, &ws_id, &dir, &task, "working", None).await;

    let outcome = reconcile_ledger(&env.app_state()).await;
    assert_eq!(outcome.orphaned, 1, "{outcome:?}");

    let text = one_report(&dir);
    assert!(text.contains("state: \"orphan\""));
    assert!(text.contains("无结果可回捞"), "{text}");
    assert!(
        text.contains("target space is not open in this process"),
        "原因必须写清楚：{text}"
    );
}

/// 只剩摘要的旧行：报告必须声明「全文未留存」，不得拿摘要冒充全文。
///
/// 变异点：无正文时把摘要当结果并标 `retention: full` ⇒ 两条断言红。
#[tokio::test]
async fn a_digest_only_row_admits_the_full_body_is_gone() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "report-digest-only").await;
    let task = run_to_terminal(&env, &target_dir, Arc::new(SilentMachine), "无正文").await;
    let digest = "摘要".repeat(100);
    seed_row(
        &env,
        &ws_id,
        &target_dir,
        &task.to_string(),
        "working",
        Some(&digest),
    )
    .await;

    let outcome = reconcile_ledger(&env.app_state()).await;
    assert_eq!(outcome.finished, 1, "{outcome:?}");

    let text = one_report(&target_dir);
    assert!(text.contains("retention: digest_only"), "{text}");
    assert!(text.contains("全文未留存"), "{text}");
    assert!(text.contains(&digest), "摘要本身仍要留下");
}

// ============================================================================
// 落点边界
// ============================================================================

/// 目标空间目录已不在：返回 `Err`，既不 panic 也不把目录建回来。
#[tokio::test]
async fn a_missing_target_directory_is_an_error() {
    let env = new_env(&["research"]).await;
    let gone = env.home.join("spaces").join("gone").join("work");
    let meta = ReportMeta {
        task_id: "aabbccdd-1111-2222-3333-444455556666".to_string(),
        dispatch_id: None,
        target_dir: gone.clone(),
        target_ws_id: "ws_gone".to_string(),
        team_id: "research".to_string(),
        origin_ws_id: "peco".to_string(),
        agent: "@master".to_string(),
        prompt_digest: None,
        created_at: None,
    };

    let err = report::write(
        &meta,
        &ReportBody::Orphan {
            reason: "target space closed",
            terminal_at: None,
        },
    )
    .expect_err("目录不在时必须报错");

    assert!(
        matches!(err, ReportError::TargetMissing(ref path) if path == &gone),
        "{err}"
    );
    assert!(!gone.exists(), "不得凭空建出目标空间目录");
}
