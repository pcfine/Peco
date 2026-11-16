//! 一轮 chat turn 与 SSE 连接解绑（阶段一）的端到端语义。
//!
//! 判据全部落在「真实路由 + 真实 SQLite + 假上游」上：走生产同一条
//! `provider_type = "openai"` 适配器路径，断连用「丢弃未消费的响应体」模拟。
//!
//! 覆盖设计文档的 V1–V6：
//!
//! * V1 断连不取消，这一轮照旧跑完并落库；
//! * V2 重新订阅拿到的快照 + 后续增量拼回落库正文，不重不漏；
//! * V3 轮结束后 `/stream/live` 是 204，且行已在库里（先落库、后收尾）；
//! * V4 cancel 只投递命令，收尾帧与正文都要在；
//! * V5 在途时再发一轮是 409，原轮不受影响；
//! * V6 同秒的多轮按插入顺序上屏，重复请求行序一致。

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::Method;
use futures::StreamExt;
use peco_daemon::chat::ChatActiveRuns;
use peco_daemon::db::messages::MessageRow;
use peco_daemon::state::AppState;
use serde_json::{Value, json};
use support::upstream::{self, FakeUpstream, Reply};
use support::{HttpReply, TestEnv, call_in_space, fixture, install_team, new_env, stream_in_space};
use tokio::sync::oneshot;

/// 单帧/单轮的等待上限。夹具是本地进程内的假上游，正常都在毫秒级；到点即失败，
/// 而不是让用例挂住。
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

// ============================================================================
// 夹具
// ============================================================================

/// 一次用例的路由与它背后的注册表。
///
/// 必须共用一个 router：`TestEnv::router()` 每次新建一份 `ChatActiveRuns`，分开建
/// 就变成「SSE 注册在 A、订阅/取消查的是 B」—— 204/409 全都会失真。
struct Harness {
    env: TestEnv,
    runs: Arc<ChatActiveRuns>,
    router: axum::Router,
    ws_id: String,
}

impl Harness {
    async fn new(upstream: &FakeUpstream, tools: &[&str]) -> Self {
        let env = new_env(&["research"]).await;
        install_team(
            &env,
            "research",
            upstream,
            &[fixture("@master", "master", tools)],
        );
        let ws_id = env.open_space("alpha", "research").await;
        let runs = Arc::new(ChatActiveRuns::new());
        let router = peco_daemon::http::router(AppState {
            chat_runs: Arc::clone(&runs),
            ..env.app_state()
        });
        Self {
            env,
            runs,
            router,
            ws_id,
        }
    }

    async fn create_conversation(&self) -> String {
        let created = call_in_space(
            &self.router,
            Method::POST,
            "/api/chat/@master/conversations",
            Some(json!({})),
            Some(&self.ws_id),
        )
        .await;
        assert_eq!(created.status, 201, "建会话失败: {}", created.body);
        created.body["id"]
            .as_str()
            .expect("会话必须带 id")
            .to_string()
    }

    fn stream_uri(&self, conv_id: &str, message: &str) -> String {
        format!("/api/chat/@master/conversations/{conv_id}/stream?message={message}")
    }

    fn live_uri(&self, conv_id: &str) -> String {
        format!("/api/chat/@master/conversations/{conv_id}/stream/live")
    }

    fn cancel_uri(&self, conv_id: &str) -> String {
        format!("/api/chat/@master/conversations/{conv_id}/cancel")
    }

    /// 普通请求（响应体读到底）。
    async fn call(&self, method: Method, uri: &str) -> HttpReply {
        call_in_space(&self.router, method, uri, None, Some(&self.ws_id)).await
    }

    /// 起一条 SSE 流，返回未消费的帧读取器。
    async fn open_stream(&self, uri: &str) -> SseReader {
        let response = stream_in_space(&self.router, Method::GET, uri, Some(&self.ws_id)).await;
        assert_eq!(response.status(), 200, "SSE 握手失败: {uri}");
        SseReader::new(response)
    }

    async fn stored(&self, conv_id: &str) -> Vec<MessageRow> {
        peco_daemon::db::messages::list_by_conversation(&self.env.pool, conv_id, 0, 200)
            .await
            .expect("读 messages 不得失败")
    }

    /// 等到该轮 assistant 行出现。等不到即失败（而不是返回 `None` —— 那会让
    /// 「什么都没写」也变成可接受的结果）。
    async fn wait_for_assistant(&self, conv_id: &str) -> MessageRow {
        let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(row) = self
                .stored(conv_id)
                .await
                .into_iter()
                .find(|row| row.role == "assistant")
            {
                return row;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{STEP_TIMEOUT:?} 内 assistant 行必须落库，实际 {}",
                self.describe(conv_id).await
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 等到该会话的注册条目摘除：一轮跑完的收尾信号。
    async fn wait_until_idle(&self, conv_id: &str) {
        let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
        while self.runs.is_active(conv_id) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{STEP_TIMEOUT:?} 内这一轮必须收尾（会把整条流的出口与唤醒队列一起堵住）"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn describe(&self, conv_id: &str) -> String {
        let rows = self.stored(conv_id).await;
        rows.iter()
            .map(|row| format!("{}={}", row.role, row.content))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// 一帧：事件名 + `id:` + 载荷（`payload()` 是 `data` 里的内容）。
#[derive(Debug, Clone)]
struct Frame {
    event: String,
    id: Option<u64>,
    data: Value,
}

impl Frame {
    fn payload(&self) -> &Value {
        &self.data["data"]
    }
}

/// SSE 帧读取器：把响应体的字节流切成帧。
struct SseReader {
    stream: axum::body::BodyDataStream,
    buffer: String,
}

impl SseReader {
    fn new(response: axum::response::Response) -> Self {
        Self {
            stream: response.into_body().into_data_stream(),
            buffer: String::new(),
        }
    }

    /// 下一帧；流正常结束返回 `None`。超时直接 panic（用例里没有「等这么久」的
    /// 合法场景）。
    async fn next_frame(&mut self, what: &str) -> Option<Frame> {
        loop {
            if let Some(block) = self.take_block() {
                // keep-alive 是注释行（`: keep-alive`），解析不出帧就继续。
                if let Some(frame) = parse_block(&block) {
                    return Some(frame);
                }
                continue;
            }
            match tokio::time::timeout(STEP_TIMEOUT, self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(Some(Err(e))) => panic!("{what}: SSE 帧读取失败: {e}"),
                Ok(None) => return None,
                Err(_) => panic!(
                    "{what}: {STEP_TIMEOUT:?} 内没有等到下一帧，已收到的残片 = {:?}",
                    self.buffer
                ),
            }
        }
    }

    /// 读到 `done` 为止，返回途经的全部帧（含 `done`）。
    async fn drain_to_done(&mut self, what: &str) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some(frame) = self.next_frame(what).await {
            let last = frame.event == "done";
            frames.push(frame);
            if last {
                return frames;
            }
        }
        panic!("{what}: 帧流在没有 done 的情况下结束，已收到 {frames:?}");
    }

    fn take_block(&mut self) -> Option<String> {
        let index = self.buffer.find("\n\n")?;
        Some(self.buffer.drain(..index + 2).collect())
    }
}

fn parse_block(block: &str) -> Option<Frame> {
    let mut event = None;
    let mut id = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("id: ") {
            id = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("data: ") {
            data.push_str(value);
        }
    }
    Some(Frame {
        event: event?,
        id,
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
    })
}

/// 假上游的「放行」闩：responder 在它上面等，测试决定何时返回。
type Gate = Arc<Mutex<Option<oneshot::Receiver<()>>>>;

fn gate() -> (Gate, oneshot::Sender<()>) {
    let (tx, rx) = oneshot::channel();
    (Arc::new(Mutex::new(Some(rx))), tx)
}

/// 等闩放行（已经取走闩的后续调用直接返回）。
async fn wait_for_gate(gate: &Gate) {
    let rx = { gate.lock().unwrap().take() };
    if let Some(rx) = rx {
        let _ = rx.await;
    }
}

// ============================================================================
// V1：断连不取消
// ============================================================================

/// 读 1 帧后丢弃响应体（连接消失），再放行假上游：这一轮必须跑完并把 assistant
/// 正文写进 `messages`。
///
/// 变异方法：把断连（订阅者为 0）加回成一个显式 break 分支 → 断连后这一轮随即
/// 退出，正文为空、兜底落库什么都不写，本用例在等 assistant 行处超时变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnected_stream_does_not_cancel_the_turn() {
    let (gate, release) = gate();
    let upstream = FakeUpstream::start(move |request: Value| {
        let gate = Arc::clone(&gate);
        async move {
            if upstream::model_of(&request) != "master" {
                return Reply::Text(String::new());
            }
            wait_for_gate(&gate).await;
            Reply::Text("整段回答".to_string())
        }
    })
    .await;
    let harness = Harness::new(&upstream, &[]).await;
    let conv_id = harness.create_conversation().await;

    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    let first = reader.next_frame("V1 首帧").await.expect("首帧必须到");
    assert_eq!(first.event, "turn_snapshot", "首帧必须是快照");
    assert!(harness.runs.is_active(&conv_id), "这一轮必须真的跑起来了");
    // 模拟关页面：响应体被丢弃。
    drop(reader);

    // 断连之后才放行假上游：这一轮应当照旧跑完。
    release.send(()).expect("放行闩必须还在");

    let row = harness.wait_for_assistant(&conv_id).await;
    assert_eq!(row.content, "整段回答", "断连不得截断正文");
    harness.wait_until_idle(&conv_id).await;

    let rows = harness.stored(&conv_id).await;
    assert_eq!(
        rows.iter().filter(|row| row.role == "assistant").count(),
        1,
        "同一轮只落一行 assistant，实际 {}",
        harness.describe(&conv_id).await
    );
    assert_eq!(
        rows[0].role,
        "user",
        "user 行在前，实际 {}",
        harness.describe(&conv_id).await
    );
}

// ============================================================================
// V2：重新订阅不重不漏
// ============================================================================

/// 断连之后重订阅：首帧快照是最终正文的**前缀**，快照 + 后续增量恰好拼回落库
/// 正文，且没有 `id <= snapshot.seq` 的帧。
///
/// 夹具用「正文 + 工具调用」让这一轮在真正产出正文之后停住（纯文本回复是这一轮
/// 的最后一步，构造不出半程），于是快照必须非空才有判别力。
///
/// 变异方法：`attach` 改成先取快照后订阅 → 两次操作之间的帧两边都没有，拼接结果
/// 与库内容不相等（`chat::feed` 的单测还有一条更直接的连续性用例）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resubscribing_rebuilds_the_turn_without_gaps_or_duplicates() {
    let (gate, release) = gate();
    let upstream = FakeUpstream::start(move |request: Value| {
        let gate = Arc::clone(&gate);
        async move {
            if upstream::model_of(&request) != "master" {
                return Reply::Text(String::new());
            }
            if upstream::has_tool_result(&request) {
                // 工具执行后的续轮：停在这里等测试放行。
                wait_for_gate(&gate).await;
                Reply::Text("后半".to_string())
            } else {
                Reply::text_and_tool_call("半段", "show_workspace", json!({}))
            }
        }
    })
    .await;
    let harness = Harness::new(&upstream, &["show_workspace"]).await;
    let conv_id = harness.create_conversation().await;

    // 1. 发一轮，读到「半段」这个 text_delta 再断连（此时快照必须非空）。
    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    loop {
        let frame = reader
            .next_frame("V2 首段")
            .await
            .expect("帧流不得提前结束");
        if frame.event == "text_delta" && frame.payload()["content"] == "半段" {
            break;
        }
    }
    drop(reader);

    // 2. 重新订阅：首帧快照。
    let mut live = harness.open_stream(&harness.live_uri(&conv_id)).await;
    let snapshot = live.next_frame("V2 快照").await.expect("在途轮必须能订阅");
    assert_eq!(snapshot.event, "turn_snapshot", "首帧必须是快照");
    let watermark = snapshot.payload()["seq"].as_u64().expect("快照必须带 seq");
    let snapshot_text = snapshot.payload()["text"]
        .as_str()
        .expect("快照必须带 text")
        .to_string();
    assert!(
        snapshot_text.starts_with("半段"),
        "断连前已产出的正文必须进快照，实际 {snapshot_text:?}"
    );

    // 3. 放行续轮，收完剩余帧。
    release.send(()).expect("放行闩必须还在");
    let frames = live.drain_to_done("V2 收尾").await;

    for frame in &frames {
        assert!(
            frame.id.is_some_and(|id| id > watermark),
            "快照水位之后不得再出现 id <= {watermark} 的帧: {frame:?}"
        );
    }
    let deltas: String = frames
        .iter()
        .filter(|frame| frame.event == "text_delta")
        .map(|frame| {
            frame.payload()["content"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    let row = harness.wait_for_assistant(&conv_id).await;
    assert!(
        row.content.starts_with(&snapshot_text),
        "快照必须是落库正文的前缀: snapshot={snapshot_text:?} db={:?}",
        row.content
    );
    assert_eq!(
        format!("{snapshot_text}{deltas}"),
        row.content,
        "快照 + 后续增量必须拼回库里的正文（不重不漏）"
    );
}

// ============================================================================
// V3：204 是「历史已是权威」的信号
// ============================================================================

/// 轮结束后 `GET .../stream/live` 必须 204，且此刻库里已经有该轮 assistant 行。
///
/// 变异方法：把 assistant 落库挪到 Idle 收尾之后 → 204 先到、行还没写，第二条
/// 断言变红（客户端会拿「历史即权威」去 refetch 到一个缺芯的历史）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settled_turn_answers_204_and_the_row_is_already_stored() {
    let upstream = FakeUpstream::start(|request: Value| async move {
        if upstream::model_of(&request) != "master" {
            return Reply::Text(String::new());
        }
        Reply::Text("答完了".to_string())
    })
    .await;
    let harness = Harness::new(&upstream, &[]).await;
    let conv_id = harness.create_conversation().await;

    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    let frames = reader.drain_to_done("V3 一轮").await;
    assert_eq!(
        frames.last().map(|frame| frame.event.as_str()),
        Some("done"),
        "正常收尾必须以 done 结束"
    );

    // 收到 done 之后立刻问：204 必须与「行已落库」同时成立。
    let live = harness.call(Method::GET, &harness.live_uri(&conv_id)).await;
    assert_eq!(
        live.status, 204,
        "已结束的轮不得再接受订阅，body={}",
        live.body
    );

    let rows = harness.stored(&conv_id).await;
    let Some(assistant) = rows.iter().find(|row| row.role == "assistant") else {
        panic!(
            "204 时库里必须已有 assistant 行，实际 {}",
            harness.describe(&conv_id).await
        );
    };
    assert_eq!(assistant.content, "答完了");
    harness.wait_until_idle(&conv_id).await;
}

// ============================================================================
// V4：cancel 对齐
// ============================================================================

/// 在途时 `POST .../cancel` 回 200（只表示命令已投递），随后同一条流上收到
/// `error{cancelled:true}` 与 `done`，且这一轮已产出的正文落库。
///
/// 注意：失败轮不发 `turn_complete`（`chat::sse` 把 `TurnComplete{Failed}` 映射成
/// `error`），故判据是「error.cancelled + done」，与设计文档 V4 的帧序列描述略异。
///
/// 变异方法：`Cancel` 分支改调 `handle.shutdown()`（即弃，不记账）→ 冻结轮次被
/// 丢弃，库里没有该轮 assistant 行，第二条断言变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_running_turn_is_delivered_and_keeps_the_partial_answer() {
    let (gate, release) = gate();
    let upstream = FakeUpstream::start(move |request: Value| {
        let gate = Arc::clone(&gate);
        async move {
            if upstream::model_of(&request) != "master" {
                return Reply::Text(String::new());
            }
            if upstream::has_tool_result(&request) {
                wait_for_gate(&gate).await;
                Reply::Text("后半".to_string())
            } else {
                Reply::text_and_tool_call("半段", "show_workspace", json!({}))
            }
        }
    })
    .await;
    let harness = Harness::new(&upstream, &["show_workspace"]).await;
    let conv_id = harness.create_conversation().await;

    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    loop {
        let frame = reader
            .next_frame("V4 首段")
            .await
            .expect("帧流不得提前结束");
        if frame.event == "text_delta" && frame.payload()["content"] == "半段" {
            break;
        }
    }

    let cancelled = harness
        .call(Method::POST, &harness.cancel_uri(&conv_id))
        .await;
    assert_eq!(cancelled.status, 200, "body={}", cancelled.body);
    assert_eq!(cancelled.body["success"], json!(true));

    // 取消在步进边界生效：放行在途的模型调用，收尾才开始。
    release.send(()).expect("放行闩必须还在");
    let frames = reader.drain_to_done("V4 收尾").await;

    let error = frames
        .iter()
        .find(|frame| frame.event == "error")
        .unwrap_or_else(|| panic!("取消必须下发 error 帧，实际 {frames:?}"));
    assert_eq!(
        error.payload()["cancelled"],
        json!(true),
        "取消与真故障必须可区分: {:?}",
        error.payload()
    );

    let row = harness.wait_for_assistant(&conv_id).await;
    assert!(
        row.content.starts_with("半段"),
        "取消不得丢弃已产出的正文，实际 {:?}",
        row.content
    );

    // 轮已收尾：再取消是 409 `no_running_task`（命令没有对象，不能被伪装成成功）。
    harness.wait_until_idle(&conv_id).await;
    let again = harness
        .call(Method::POST, &harness.cancel_uri(&conv_id))
        .await;
    assert_eq!(again.status, 409, "body={}", again.body);
    assert_eq!(again.code(), "no_running_task", "body={}", again.body);
}

// ============================================================================
// V5：busy 409 且不伤原轮
// ============================================================================

/// 在途时再发一轮必须 409 `conversation_busy`；原轮照旧跑完，表里只有第一条
/// 提问的 user 行。
///
/// 变异方法：注册改回顶替语义（`register`）→ 第二次请求返回 200，原轮被挤掉
/// （缺 assistant 行），并且表里多出 `hi2` 的 user 行，三条断言全红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_turn_is_refused_while_the_first_still_runs() {
    let (gate, release) = gate();
    let upstream = FakeUpstream::start(move |request: Value| {
        let gate = Arc::clone(&gate);
        async move {
            if upstream::model_of(&request) != "master" {
                return Reply::Text(String::new());
            }
            wait_for_gate(&gate).await;
            Reply::Text("第一条的答复".to_string())
        }
    })
    .await;
    let harness = Harness::new(&upstream, &[]).await;
    let conv_id = harness.create_conversation().await;

    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    let first = reader.next_frame("V5 首帧").await.expect("首帧必须到");
    assert_eq!(first.event, "turn_snapshot");

    let busy = harness
        .call(Method::GET, &harness.stream_uri(&conv_id, "hi2"))
        .await;
    assert_eq!(busy.status, 409, "body={}", busy.body);
    assert_eq!(busy.code(), "conversation_busy", "body={}", busy.body);

    // 原轮不受影响：放行后照旧走到 done 并落库。
    release.send(()).expect("放行闩必须还在");
    let frames = reader.drain_to_done("V5 原轮").await;
    assert_eq!(
        frames.last().map(|frame| frame.event.as_str()),
        Some("done")
    );

    let row = harness.wait_for_assistant(&conv_id).await;
    assert_eq!(row.content, "第一条的答复");

    let rows = harness.stored(&conv_id).await;
    let users: Vec<&str> = rows
        .iter()
        .filter(|row| row.role == "user")
        .map(|row| row.content.as_str())
        .collect();
    assert_eq!(
        users,
        vec!["hi"],
        "被拒的那一轮不得写 user 行，实际 {}",
        harness.describe(&conv_id).await
    );
}

// ============================================================================
// V6：同秒不乱序
// ============================================================================

/// 连续多轮（假上游立即返回，天然同秒）之后：role 序列恒为 `user, assistant, ...`
/// 且与发送顺序一致；同一响应重复请求 5 次，id 序列逐字相同。
///
/// 变异方法：去掉 `ORDER BY` 里的 `rowid ASC` → 排序退化为按秒，同秒行的先后由
/// SQLite 的偶然返回顺序决定，重复请求的 id 序列随分页/扫描计划抖动（本机 SQLite
/// 可能仍按 rowid 返回，故这条更像「锁住不变量」而非严格判红，见交付说明）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turns_sharing_one_second_keep_their_order() {
    let upstream = FakeUpstream::start(|request: Value| async move {
        if upstream::model_of(&request) != "master" {
            return Reply::Text(String::new());
        }
        Reply::Text("收到".to_string())
    })
    .await;
    let harness = Harness::new(&upstream, &[]).await;
    let conv_id = harness.create_conversation().await;

    const TURNS: usize = 20;
    for turn in 0..TURNS {
        let uri = harness.stream_uri(&conv_id, &format!("q{turn}"));
        // 读完整个响应体：done 之后帧流结束，于是这就是「这一轮跑完了」。
        let reply = call_in_space(
            &harness.router,
            Method::GET,
            &uri,
            Some(json!({})),
            Some(&harness.ws_id),
        )
        .await;
        // `call_in_space` 把 SSE 体按 JSON 解析（失败即 Null），这里只关心状态码：
        // busy 说明上一轮刚发完 done、条目还没摘除，重试一次即可（不是本用例的判据）。
        if reply.status == 409 {
            harness.wait_until_idle(&conv_id).await;
            let retried = call_in_space(
                &harness.router,
                Method::GET,
                &uri,
                Some(json!({})),
                Some(&harness.ws_id),
            )
            .await;
            assert_eq!(retried.status, 200, "重试后必须能发, body={}", retried.body);
        } else {
            assert_eq!(reply.status, 200, "发一轮失败: {}", reply.body);
        }
    }

    let uri = format!("/api/chat/@master/conversations/{conv_id}/messages?limit=100&offset=0");
    let listed = harness.call(Method::GET, &uri).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let items = listed.body.as_array().expect("消息列表必须是数组");
    assert_eq!(items.len(), TURNS * 2, "每轮各一行 user 与 assistant");

    let roles: Vec<&str> = items
        .iter()
        .map(|item| item["role"].as_str().unwrap_or_default())
        .collect();
    for (index, role) in roles.iter().enumerate() {
        let expected = if index % 2 == 0 { "user" } else { "assistant" };
        assert_eq!(*role, expected, "第 {index} 行的角色错了: {roles:?}");
    }
    let contents: Vec<&str> = items
        .iter()
        .filter(|item| item["role"] == json!("user"))
        .map(|item| item["content"].as_str().unwrap_or_default())
        .collect();
    let expected: Vec<String> = (0..TURNS).map(|turn| format!("q{turn}")).collect();
    assert_eq!(contents, expected, "user 行必须按发送顺序排列");

    let reference: Vec<String> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap_or_default().to_string())
        .collect();
    for _ in 0..5 {
        let again = harness.call(Method::GET, &uri).await;
        let ids: Vec<String> = again
            .body
            .as_array()
            .expect("消息列表必须是数组")
            .iter()
            .map(|item| item["id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(ids, reference, "同一响应重复请求的 id 序列必须一致");
    }
}

// ============================================================================
// 兜底落库：中途退出（这里用「被顶替」触发）也要保住已产出的正文
// ============================================================================

/// 这一轮的控制端被摘除（顶替）时，run 任务必须收敛：订阅者拿到 `error` + `done`，
/// 已产出的正文由兜底落库写进 `messages`。
///
/// 为什么用 `register` 造这个场景：`control_rx` 关闭只发生在条目被移除/顶替时，
/// 而没有真实客户端会走到那一步 —— 注册表的顶替原语是这里唯一可控的触发点。
///
/// 变异方法：删掉 `chat/run.rs` 收尾处的兜底落库 → 这一轮的正文只活在内存里，
/// 用例在等 assistant 行处超时变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superseded_run_salvages_its_partial_answer() {
    let (gate, _release) = gate();
    let upstream = FakeUpstream::start(move |request: Value| {
        let gate = Arc::clone(&gate);
        async move {
            if upstream::model_of(&request) != "master" {
                return Reply::Text(String::new());
            }
            if upstream::has_tool_result(&request) {
                // 停在续轮上：这一轮不会自己结束，只能由「控制端消失」收尾。
                wait_for_gate(&gate).await;
                Reply::Text("不该走到这里".to_string())
            } else {
                Reply::text_and_tool_call("半段", "show_workspace", json!({}))
            }
        }
    })
    .await;
    let harness = Harness::new(&upstream, &["show_workspace"]).await;
    let conv_id = harness.create_conversation().await;

    let mut reader = harness
        .open_stream(&harness.stream_uri(&conv_id, "hi"))
        .await;
    loop {
        let frame = reader
            .next_frame("兜底落库 首段")
            .await
            .expect("帧流不得提前结束");
        if frame.event == "text_delta" && frame.payload()["content"] == "半段" {
            break;
        }
    }

    // 顶替：旧条目连同它的控制发送端一起丢弃 ⇒ 旧任务从 control_rx 收到 None 并收尾。
    // 旧接收端在 run 任务手里，测试这边只能从「帧流有没有收敛」看出来。
    let (_new_rx, _new_guard) = harness.runs.register(&conv_id);

    let frames = reader.drain_to_done("兜底落库 收尾").await;
    assert!(
        frames.iter().any(|frame| frame.event == "error"),
        "非正常退出必须解释原因，实际 {frames:?}"
    );

    let row = harness.wait_for_assistant(&conv_id).await;
    assert_eq!(row.content, "半段", "兜底落库必须保住已产出的正文");
}
