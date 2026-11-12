//! 派发正文的持久化与跨重启查阅：写入走 `TaskObserver`，读取走 `inspect_workspace_task`。
//!
//! 「重启」用「这个进程的 TaskHub 从来没接纳过这个 task_id」表达：那正是重启后
//! Hub 的状态，而台账行还在。

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use peco_core::agent::hooks::{DispatchObservation, DispatchOrigin, TaskObservationSink};
use peco_core::machine::{
    OutputView, Progress, TaggedJson, TaskId, TaskOutcome, TaskSnapshot, TaskSource, TaskState,
};
use peco_core::tools::{Content, InspectWorkspaceTask, ToolDyn};
use serde_json::{Value, json};
use support::TestEnv;
use support::new_env;
use uuid::Uuid;

/// 正文三段唯一标记：尾窗实现只能拿到尾段，中间与头段都取不到。
const HEAD_MARK: &str = "HEAD-MARK-ONLY-IN-THE-FULL-BODY";
const MID_MARK: &str = "MID-MARK-ONLY-IN-THE-FULL-BODY";
const TAIL_MARK: &str = "TAIL-MARK-ONLY-IN-THE-FULL-BODY";

/// 一段远超台账摘要上限（512 字）的正文。
fn long_body() -> String {
    format!("{}{TAIL_MARK}", "正".repeat(3000))
}

/// 一段远超单页上限（32_000 字符）的正文，三处标记各占一格。
fn paged_body() -> String {
    format!(
        "{HEAD_MARK}{}{MID_MARK}{}{TAIL_MARK}",
        "正".repeat(20_000),
        "文".repeat(20_000)
    )
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
        output: OutputView {
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

fn observation(target_dir: PathBuf, task_id: TaskId) -> DispatchObservation {
    DispatchObservation {
        origin: DispatchOrigin {
            ws_id: "peco".into(),
            principal: "local".into(),
            origin_dir: PathBuf::from("/tmp/peco-origin"),
        },
        target_dir,
        task_id,
        dispatch_id: Some("disp-1".into()),
        origin_kind: "dispatch_to_workspace".into(),
        agent_name: "@master".into(),
        prompt_digest: None,
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

async fn inspect(env: &TestEnv, dir: &Path, task_id: &str, action: &str) -> Result<Value, String> {
    inspect_args(env, dir, task_id, json!({ "action": action })).await
}

/// 同一个工具、同一份身份参数，只换 action 之外的入参（`offset` 等）。
async fn inspect_args(
    env: &TestEnv,
    dir: &Path,
    task_id: &str,
    extra: Value,
) -> Result<Value, String> {
    let tool = InspectWorkspaceTask::new(Arc::downgrade(&env.hub));
    let mut args = json!({
        "dir": dir.display().to_string(),
        "task_id": task_id
    });
    let (Some(base), Some(extra)) = (args.as_object_mut(), extra.as_object()) else {
        panic!("arguments must be JSON objects");
    };
    for (key, value) in extra {
        base.insert(key.clone(), value.clone());
    }
    let content = ToolDyn::call(&tool, args.to_string())
        .await
        .map_err(|err| err.to_string())?;
    let Content::Text(text) = content else {
        panic!("inspect_workspace_task must return plain text");
    };
    Ok(serde_json::from_str(&text).expect("the report body must be JSON"))
}

/// 直接写一条台账行：只用于「历史行」与「在飞行」这两类不该由写入侧产生的形状。
/// 一条直接写进台账的行：写入侧只产得出「终态 + 正文」，其余形状只能手造。
struct SeededRow<'a> {
    state: &'a str,
    digest: Option<&'a str>,
    output: Option<&'a str>,
    terminal: bool,
}

async fn seed_row(env: &TestEnv, ws_id: &str, dir: &Path, task_id: &str, row: SeededRow<'_>) {
    let SeededRow {
        state,
        digest,
        output,
        terminal,
    } = row;
    sqlx::query(
        "INSERT INTO d_dispatches (id, task_id, target_ws_id, target_work_dir, target_team_id, \
         origin_ws_id, agent_name, origin_kind, task_state, wake_state, result_digest, \
         output, terminal_at) VALUES (?, ?, ?, ?, 'research', 'peco', '@master', \
         'dispatch_to_workspace', ?, 'skipped', ?, ?, \
         CASE WHEN ? THEN datetime('now') END)",
    )
    .bind(format!("d_{task_id}"))
    .bind(task_id)
    .bind(ws_id)
    .bind(dir.display().to_string())
    .bind(state)
    .bind(digest)
    .bind(output)
    .bind(terminal)
    .execute(&env.pool)
    .await
    .unwrap();
}

/// 写入侧：终态落库的正文长于 512 字摘要，且尾部与原文一致。
///
/// 变异点：`finish` 不写 `output` 列 ⇒ 长度与尾部两条断言红。
#[tokio::test]
async fn the_terminal_body_is_stored_beyond_the_digest_cap() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "store-body").await;
    let task = TaskId(Uuid::new_v4());
    let body = long_body();
    let obs = observation(target_dir, task);
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();

    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, Some(&body)))
        .await;

    let (output, truncated, digest) = sqlx::query_as::<_, (Option<String>, i64, Option<String>)>(
        "SELECT output, output_truncated, result_digest FROM d_dispatches \
         WHERE target_ws_id = ? AND task_id = ?",
    )
    .bind(&ws_id)
    .bind(task.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();

    let output = output.expect("终态必须落正文");
    assert_eq!(output, body, "落库的正文必须与原文一致");
    assert!(
        output.chars().count() > 512,
        "正文必须长于摘要上限，实际 {} 字",
        output.chars().count()
    );
    assert!(
        output.ends_with(TAIL_MARK),
        "尾部必须在：{:?}",
        &output[output.len() - TAIL_MARK.len()..]
    );
    assert_eq!(truncated, 0, "没超上限不该置截断标志");
    assert_eq!(
        digest.map(|text| text.chars().count()),
        Some(512),
        "摘要仍是 512 字，两者不是一回事"
    );
}

/// 读取侧：本进程的 Hub 不认识这条任务时，从台账取回全文 —— 分页取完再拼回来。
///
/// 判别力来自三处：三段标记分别落在头 / 中 / 尾（8KB 尾窗只拿得到尾段）、按
/// 字符拼回来的结果与原文逐字相等、`total_chars` 与原文的字符数一致。
///
/// 变异点：分页退化成「一次返整段」⇒ 第一页之后 `next_offset` 为 null，头 / 中
/// 两段虽然都在，`limit=32_000` 与「页数 > 1」的断言红；台账取法改成先取快照再
/// 截 8KB 尾窗 ⇒ 头 / 中两段标记双双缺席。
#[tokio::test]
async fn a_restarted_process_reads_the_full_body_from_the_ledger() {
    let env = new_env(&["research"]).await;
    let (_ws_id, target_dir) = open_space(&env, "read-after-restart").await;
    let task = TaskId(Uuid::new_v4());
    let body = paged_body();
    let obs = observation(target_dir.clone(), task);
    let sink: &dyn TaskObservationSink = env.tasks.as_ref();
    sink.on_registered(&obs, &snapshot(task, TaskState::Working, None))
        .await;
    sink.on_terminal(&obs, &snapshot(task, TaskState::Completed, Some(&body)))
        .await;

    // 先证明这确实是回退：打开着的空间里的 Hub 根本没有这个任务。
    let handle = env.hub.get(&target_dir).expect("空间应当是打开的");
    let runtime = handle.machine_runtime().upgrade().expect("runtime");
    assert!(
        runtime.hub().expect("hub").snapshot(&task).await.is_err(),
        "Hub 不认识这个任务，才谈得上回退"
    );

    // status 只给 8KB 尾窗：缺口与取法必须写在 note 里，否则模型知道缺了也取不回。
    let report = inspect(&env, &target_dir, &task.to_string(), "status")
        .await
        .expect("台账命中就不该报错");
    let task_json = &report["task"];
    assert_eq!(task_json["state"], json!("completed"));
    assert_eq!(
        task_json["source"],
        json!("ledger"),
        "正文来自台账，必须自报"
    );
    let preview = task_json["output"].as_str().expect("预览必须有正文");
    assert!(preview.contains(TAIL_MARK), "尾窗拿到的是尾段");
    assert!(
        !preview.contains(HEAD_MARK),
        "status 不该给全文：{preview:?}"
    );
    assert_eq!(task_json["output_total_bytes"], json!(body.len()));
    assert!(
        task_json["output_omitted_head_bytes"]
            .as_u64()
            .is_some_and(|omitted| omitted > 0),
        "缺口必须报出来：{task_json}"
    );
    let note = task_json["note"].as_str().expect("缺口必须写进取法");
    assert!(note.contains("action=output"), "{note}");

    // 全文出口：分页取完、按字符拼回来，必须与原文逐字相等。
    let expected_chars = body.chars().count() as u64;
    let mut offset = 0usize;
    let mut assembled = String::new();
    let mut pages = 0usize;
    loop {
        let page = inspect_args(
            &env,
            &target_dir,
            &task.to_string(),
            json!({ "action": "output", "offset": offset }),
        )
        .await
        .expect("action=output 必须可读");
        pages += 1;
        assert_eq!(page["action"], json!("output"));
        assert_eq!(page["source"], json!("ledger"));
        assert_eq!(page["offset"], json!(offset));
        assert_eq!(page["limit"], json!(32_000), "默认一页 32k 字符");
        assert_eq!(
            page["total_chars"],
            json!(expected_chars),
            "total_chars 必须是正文字符数（不是字节数）"
        );
        let chunk = page["body"].as_str().expect("body 必须是原样正文");
        assert!(
            !chunk.is_empty(),
            "有 next_offset 的页不能是空页：offset={offset}"
        );
        assembled.push_str(chunk);
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset as u64, "next_offset 必须前进");
                offset = next as usize;
            }
            None => break,
        }
    }
    assert!(pages > 1, "正文长于单页上限，必须真的分页：{pages}");
    assert!(assembled.contains(HEAD_MARK), "头段标记必须在");
    assert!(assembled.contains(MID_MARK), "中段标记必须在");
    assert!(assembled.contains(TAIL_MARK), "尾段标记必须在");
    assert_eq!(assembled, body, "分页拼回来必须与原文逐字相等");

    let awaited = inspect(&env, &target_dir, &task.to_string(), "await")
        .await
        .expect("action=await 同样走台账");
    assert_eq!(awaited["timed_out"], json!(false), "台账命中即视为终态");
    assert_eq!(awaited["task"]["source"], json!("ledger"));
}

/// 历史行（`output IS NULL`）只剩摘要：必须如实标注，不能用摘要冒充全文。
///
/// 变异点：回退不看 `output` 有没有值、把摘要当正文报 ⇒ `note` 与长度断言红。
#[tokio::test]
async fn a_legacy_row_without_a_body_is_reported_as_a_digest() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "legacy-row").await;
    let task = TaskId(Uuid::new_v4());
    let digest = "摘".repeat(512);
    seed_row(
        &env,
        &ws_id,
        &target_dir,
        &task.to_string(),
        SeededRow {
            state: "completed",
            digest: Some(&digest),
            output: None,
            terminal: true,
        },
    )
    .await;

    let report = inspect(&env, &target_dir, &task.to_string(), "status")
        .await
        .expect("历史行仍可查");
    let task_json = &report["task"];
    assert_eq!(task_json["source"], json!("ledger"));
    assert_eq!(task_json["output"], json!(digest), "正文位给的就是摘要");
    assert_eq!(task_json["output_truncated"], json!(true));
    assert_eq!(task_json["output_lossy"], json!(true));
    assert_eq!(
        task_json["output_total_bytes"],
        Value::Null,
        "原文总长不可知"
    );
    let note = task_json["note"].as_str().expect("必须自带提示");
    assert!(note.contains("全文未留存"), "{note}");
    assert!(note.contains("512"), "{note}");
    assert!(
        note.contains("action=output"),
        "只剩摘要时也要给出取法：{note}"
    );

    let output = inspect_args(
        &env,
        &target_dir,
        &task.to_string(),
        json!({ "action": "output" }),
    )
    .await
    .expect("摘要可读");
    assert_eq!(output["body"], Value::Null, "摘要不是正文");
    assert_eq!(output["digest"], json!(digest));
    assert_eq!(output["total_chars"], json!(512), "摘要长度是已知的");
    let note = output["note"].as_str().expect("必须说明原文长度不可考");
    assert!(note.contains("原文长度不可考"), "{note}");
}

/// `failed` 终态同样要能读正文：可读性只取决于正文有没有落库，与哪种终态无关。
///
/// 变异点：取正文时加 `state == Completed` 门控 ⇒ 这里从有正文退化成
/// "not found in workspace"（评审实测把门控加回去全绿，说明此前没有保护）。
#[tokio::test]
async fn a_failed_row_still_serves_its_body() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "failed-row").await;
    let task = TaskId(Uuid::new_v4());
    let body = long_body();
    let digest: String = body.chars().take(512).collect();
    seed_row(
        &env,
        &ws_id,
        &target_dir,
        &task.to_string(),
        SeededRow {
            state: "failed",
            digest: Some(&digest),
            output: Some(&body),
            terminal: true,
        },
    )
    .await;

    let report = inspect(&env, &target_dir, &task.to_string(), "status")
        .await
        .expect("failed 行也要可查");
    let task_json = &report["task"];
    assert_eq!(task_json["state"], json!("failed"));
    assert_eq!(task_json["source"], json!("ledger"));
    let preview = task_json["output"].as_str().expect("正文必须可读");
    assert!(preview.ends_with(TAIL_MARK), "尾窗给的是尾段");

    let output = inspect_args(
        &env,
        &target_dir,
        &task.to_string(),
        json!({ "action": "output" }),
    )
    .await
    .expect("failed 行的全文同样可读");
    assert_eq!(output["source"], json!("ledger"));
    assert_eq!(output["total_chars"], json!(body.chars().count()));
    assert_eq!(output["body"], json!(body), "正文必须是原样");
}

/// 运行中的行不得回退：否则「本进程没有句柄」会被谎报成「有」。
///
/// 变异点：台账查询去掉 `terminal_at IS NOT NULL` ⇒ 这条从 not found 变成一个
/// 编出来的 working 快照。
#[tokio::test]
async fn a_running_row_is_still_not_found() {
    let env = new_env(&["research"]).await;
    let (ws_id, target_dir) = open_space(&env, "running-row").await;
    let task = TaskId(Uuid::new_v4());
    seed_row(
        &env,
        &ws_id,
        &target_dir,
        &task.to_string(),
        SeededRow {
            state: "working",
            digest: Some("还没跑完"),
            output: Some("半截正文"),
            terminal: false,
        },
    )
    .await;

    let err = inspect(&env, &target_dir, &task.to_string(), "status")
        .await
        .expect_err("在飞的行不得回退");
    assert!(
        err.contains("not found in workspace"),
        "必须仍报不存在：{err}"
    );

    // 全文出口不是绕过这条纪律的后门：本进程没有句柄，就没有正文可给。
    let err = inspect_args(
        &env,
        &target_dir,
        &task.to_string(),
        json!({ "action": "output" }),
    )
    .await
    .expect_err("在飞的行同样不得给出正文");
    assert!(
        err.contains("not found in workspace"),
        "必须仍报不存在：{err}"
    );
}
