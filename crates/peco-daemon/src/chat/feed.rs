// ============================================================================
// TurnFeed — 一轮对话的帧扇出与在途快照
// ============================================================================
//
// 这一轮的生命周期归 daemon，不归任何一条 SSE 连接：run 任务把事件 `publish`
// 到这里，订阅者可来可走；走了再回来，凭首帧快照接上（[`TurnFeed::attach`]）。
//
// 三条不变量（订阅侧「不重不漏」全靠它们）：
//
// * I1 `publish` 在同一把锁内完成「分配 seq / 应用快照 / 广播」；
// * I2 终态（`done` 已发布）之后 `attach` 返回 `None` —— 调用方据此回 204，
//   「历史已是权威」于是有了确定信号；
// * I3 `attach` 先 `subscribe` 再取快照（同一把锁内）。反过来会丢掉「取快照
//   之后、订阅之前」发布的帧：既不在快照里，也不在流里。
//
// 快照是「显示的权威」，`messages` 表是「历史的权威」；两者只在 `done` 之后
// 交接 —— 落库严格早于 `done`，见 `chat::run` 的收尾顺序。

use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::response::sse::Event;
use futures::stream::Stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::warn;

use super::sse::{ChatSseEvent, UsageData};

/// 广播缓冲容量。慢订阅者落后超过这个帧数就拿到 `Lagged`，随后由
/// [`frame_stream`] 补发一帧新快照（不重放旧帧）。
const FEED_CAPACITY: usize = 256;

/// 一轮对话的出口：有没有订阅者是 feed 内部的事。
pub(crate) struct TurnFeed {
    /// 锁内只有内存操作 —— 不 await、不 IO，故用 `std::sync::Mutex`。
    inner: Mutex<FeedState>,
    tx: broadcast::Sender<Arc<Frame>>,
}

struct FeedState {
    /// 这一轮此刻的样子；`seq` 是已经发布出去的最后一帧序号。
    snapshot: TurnSnapshot,
    /// 终态帧（`done`）已发布 —— 此后不再接受订阅。
    finished: bool,
}

/// 一帧：分配的序号 + 事件载荷。订阅端据此给 SSE 帧加 `id:`。
#[derive(Debug)]
pub(crate) struct Frame {
    pub(crate) seq: u64,
    pub(crate) event: ChatSseEvent,
}

/// 订阅首帧 `turn_snapshot` 的载荷 = 「这一轮此刻的样子」。
///
/// 它同时是 wire 载荷（`ChatSseEvent::TurnSnapshot` 的内容），故与事件枚举同
/// 可见性；模块本身是 crate 内部模块，对外仍是私有。
#[derive(Debug, Clone, Serialize)]
pub struct TurnSnapshot {
    pub(crate) conversation_id: String,
    /// 本快照已覆盖到的最后一帧序号；客户端丢弃 `id <= seq` 的后续帧。
    pub(crate) seq: u64,
    pub(crate) phase: Phase,
    /// 本轮**已产出**的 assistant 正文（不是历史库存量）。
    pub(crate) text: String,
    pub(crate) reasoning: String,
    pub(crate) tool_calls: Vec<ToolCallCard>,
    pub(crate) agent_calls: Vec<AgentCallCard>,
    /// 最近一次模型调用的用量；没有则 0/0。
    pub(crate) usage: UsageData,
}

/// 服务端权威的运行阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Paused,
    /// 已收到 cancel、尚未收尾。
    Cancelling,
}

/// 工具卡状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CardStatus {
    Running,
    Done,
}

/// 在途/已完成的工具卡。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallCard {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
    status: CardStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

/// 子 Agent 卡。
#[derive(Debug, Clone, Serialize)]
pub struct AgentCallCard {
    pub(crate) call_id: String,
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    status: CardStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

/// 一次订阅拿到的三件东西。
///
/// `feed` 一并给出，是因为落后（`Lagged`）时要补发一帧**新快照**：那需要再读
/// 一次当前状态，而不是重放旧帧。
pub(crate) struct LiveFrames {
    pub(crate) feed: Arc<TurnFeed>,
    pub(crate) rx: broadcast::Receiver<Arc<Frame>>,
    pub(crate) snapshot: TurnSnapshot,
}

impl TurnFeed {
    pub(crate) fn new(conversation_id: String) -> Self {
        // 广播通道在第一个接收者出现前不需要保活：`send` 失败就是「没人看」。
        let (tx, _rx) = broadcast::channel(FEED_CAPACITY);
        Self {
            inner: Mutex::new(FeedState {
                snapshot: TurnSnapshot {
                    conversation_id,
                    seq: 0,
                    phase: Phase::Running,
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    agent_calls: Vec::new(),
                    usage: UsageData {
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                },
                finished: false,
            }),
            tx,
        }
    }

    pub(crate) fn conversation_id(&self) -> String {
        self.lock().snapshot.conversation_id.clone()
    }

    /// 分配 seq、应用到快照、广播；三件事必须在同一把锁内完成（I1）。
    ///
    /// 返回分配的序号。没有订阅者时广播失败是常态 —— detach 之后前端可能谁都
    /// 不在，这一轮照旧跑完并落库。
    pub(crate) fn publish(&self, event: ChatSseEvent) -> u64 {
        let (seq, frame) = {
            let mut state = self.lock();
            let seq = state.snapshot.seq + 1;
            state.snapshot.seq = seq;
            state.apply(&event);
            (seq, Arc::new(Frame { seq, event }))
        };
        let _ = self.tx.send(frame);
        seq
    }

    /// 标记「已收到取消、尚未收尾」。取消来自控制通道而不是 looper 事件，故不能
    /// 由 [`Self::publish`] 的快照规则推导。
    ///
    /// 只改快照、不发帧：`turn_snapshot` 是订阅首帧专用的载荷，中途插一帧会让
    /// 「客户端按 seq 去重」之外多出一条重置路径；在看的订阅者已经由自己的
    /// `POST .../cancel` 得知进入取消态。
    pub(crate) fn mark_cancelling(&self) {
        self.lock().snapshot.phase = Phase::Cancelling;
    }

    /// 起始订阅端：feed 刚建、还没有任何帧时用（`chat::run` 在 spawn 之前调用）。
    ///
    /// 顺序同 [`Self::attach`]：先订阅再取快照。
    pub(crate) fn start(self: &Arc<Self>) -> LiveFrames {
        let rx = self.tx.subscribe();
        let snapshot = self.lock().snapshot.clone();
        LiveFrames {
            feed: Arc::clone(self),
            rx,
            snapshot,
        }
    }

    /// 加入一条在途轮：`finished` 之后返回 `None`（对应 HTTP 204，见 `I2`）。
    ///
    /// 实现顺序固定 —— 先 `subscribe`，再在同一把锁内 clone 快照（见 `I3`）。
    pub(crate) fn attach(self: &Arc<Self>) -> Option<LiveFrames> {
        let rx = self.tx.subscribe();
        let snapshot = {
            let state = self.lock();
            if state.finished {
                return None;
            }
            state.snapshot.clone()
        };
        Some(LiveFrames {
            feed: Arc::clone(self),
            rx,
            snapshot,
        })
    }

    /// 此刻还有没有订阅者在看这一轮。
    ///
    /// 数的是**握在手里的出口**而不是「曾经来过」：`start()` / `attach()` 各持一个
    /// `Receiver`，随 SSE 响应结束 drop，全部 drop 之后才为 false。
    ///
    /// 空闲看门狗据此区分「有人看着的慢轮」与「没人看着的死轮」—— 前者掐断等于掐断
    /// 用户正在读的一段对话，后者不掐会永久占住该会话的运行条目。
    pub(crate) fn has_subscribers(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    /// 当前快照 —— 落后补发用（`Lagged` 之后不重放旧帧）。
    pub(crate) fn snapshot(&self) -> TurnSnapshot {
        self.lock().snapshot.clone()
    }

    /// 保证 feed 已进入终态：已发过 `done` 就什么都不做，否则补一条。
    ///
    /// 供任务收尾的 Drop 守卫调用（不能 await），覆盖「早退与 panic 展开跳过
    /// 循环末尾收尾」的路径 —— 否则订阅者会一直等一个永远不来的 `done`。
    pub(crate) fn finish(&self) {
        if self.lock().finished {
            return;
        }
        let (usage, conversation_id) = {
            let state = self.lock();
            (
                state.snapshot.usage.clone(),
                state.snapshot.conversation_id.clone(),
            )
        };
        self.publish(ChatSseEvent::Done {
            usage,
            conversation_id,
        });
    }

    fn lock(&self) -> MutexGuard<'_, FeedState> {
        // 锁内只有内存操作，毒化只可能来自 panic 展开的写者：继续用旧值比连带
        // panic 掉整轮更好。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl FeedState {
    /// 把一条事件应用到「这一轮的样子」上。
    fn apply(&mut self, event: &ChatSseEvent) {
        let snapshot = &mut self.snapshot;
        match event {
            ChatSseEvent::TextDelta { content, .. } => snapshot.text.push_str(content),
            ChatSseEvent::ReasoningDelta { content, .. } => snapshot.reasoning.push_str(content),
            ChatSseEvent::ToolCallStart {
                id,
                name,
                arguments,
                ..
            } => snapshot.tool_calls.push(ToolCallCard {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                status: CardStatus::Running,
                result: None,
            }),
            ChatSseEvent::ToolResult { id, result, .. } => {
                if let Some(card) = snapshot.tool_calls.iter_mut().find(|card| &card.id == id) {
                    card.status = CardStatus::Done;
                    card.result = Some(result.clone());
                }
            }
            ChatSseEvent::AgentCallStart {
                call_id,
                agent_id,
                agent_name,
                ..
            } => snapshot.agent_calls.push(AgentCallCard {
                call_id: call_id.clone(),
                agent_id: agent_id.clone(),
                agent_name: agent_name.clone(),
                status: CardStatus::Running,
                result: None,
            }),
            ChatSseEvent::AgentCallEnd {
                call_id, result, ..
            } => {
                if let Some(card) = snapshot
                    .agent_calls
                    .iter_mut()
                    .find(|card| &card.call_id == call_id)
                {
                    card.status = CardStatus::Done;
                    card.result = Some(result.clone());
                }
            }
            ChatSseEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                snapshot.usage = UsageData {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                }
            }
            ChatSseEvent::LooperState { paused, .. } => {
                snapshot.phase = if *paused {
                    Phase::Paused
                } else {
                    Phase::Running
                };
            }
            // 取消的收尾在即：阶段交回 running，由紧随其后的 `done` 结束这一轮。
            ChatSseEvent::Error { .. } => snapshot.phase = Phase::Running,
            ChatSseEvent::Done { .. } => self.finished = true,
            // 其余帧不改变「这一轮的样子」。
            ChatSseEvent::TurnComplete { .. }
            | ChatSseEvent::ContextCompacted { .. }
            | ChatSseEvent::TruncationRetry { .. }
            | ChatSseEvent::UserSteer { .. }
            | ChatSseEvent::TurnSnapshot(_) => {}
        }
    }
}

/// 订阅端 → SSE 帧流：首帧快照，之后逐帧送出，`done` 之后结束。
///
/// 结束判据是 `done` 而不是「广播通道关闭」：订阅端自己也持着 `Arc<TurnFeed>`，
/// 只要它还在，广播发送端就不会 drop —— 等关闭等于永远等下去。
pub(crate) fn frame_stream(frames: LiveFrames) -> impl Stream<Item = Result<Event, Infallible>> {
    futures::stream::unfold(Some(Cursor::Snapshot(frames)), |state| async move {
        let state = state?;
        match state {
            Cursor::Snapshot(LiveFrames { feed, rx, snapshot }) => Some((
                Ok(to_wire(snapshot.seq, &ChatSseEvent::TurnSnapshot(snapshot))),
                Some(Cursor::Live { feed, rx }),
            )),
            Cursor::Live { feed, mut rx } => match rx.recv().await {
                Ok(frame) => {
                    let last = matches!(&frame.event, ChatSseEvent::Done { .. });
                    let event = to_wire(frame.seq, &frame.event);
                    let next = (!last).then_some(Cursor::Live { feed, rx });
                    Some((Ok(event), next))
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // 落后于广播缓冲：补发一帧**新快照**而不是重放旧帧 ——
                    // 旧帧的正文已经包含在快照里，重放只会让客户端重复一遍。
                    warn!(
                        conversation_id = %feed.conversation_id(),
                        skipped,
                        "SSE subscriber lagged behind the feed; resending a snapshot"
                    );
                    let snapshot = feed.snapshot();
                    Some((
                        Ok(to_wire(snapshot.seq, &ChatSseEvent::TurnSnapshot(snapshot))),
                        Some(Cursor::Live { feed, rx }),
                    ))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            },
        }
    })
}

enum Cursor {
    Snapshot(LiveFrames),
    Live {
        feed: Arc<TurnFeed>,
        rx: broadcast::Receiver<Arc<Frame>>,
    },
}

/// 唯一的「帧 → wire」转换点：`event` / `id` / `data` 在这里一次拼好。
fn to_wire(seq: u64, event: &ChatSseEvent) -> Event {
    event.to_sse_event(seq).unwrap_or_else(|e| {
        tracing::error!(error = %e, "SSE frame serialization failed");
        Event::default().comment("frame serialization failed")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use futures::StreamExt;
    use serde_json::Value;

    use super::*;

    fn delta(content: &str) -> ChatSseEvent {
        ChatSseEvent::TextDelta {
            content: content.to_string(),
            conversation_id: "conv-1".to_string(),
        }
    }

    fn done() -> ChatSseEvent {
        ChatSseEvent::Done {
            usage: UsageData {
                input_tokens: 0,
                output_tokens: 0,
            },
            conversation_id: "conv-1".to_string(),
        }
    }

    /// 帧流里的一帧：`(id, 事件名, 载荷)`。
    #[derive(Debug)]
    struct Wire {
        id: Option<u64>,
        event: String,
        data: Value,
    }

    async fn collect(frames: LiveFrames) -> Vec<Wire> {
        frame_stream(frames)
            .map(|item| {
                let event = item.expect("帧流不会产出错误项");
                let wire = wire_of(event);
                Wire {
                    id: sse_id(&wire),
                    event: event_name(&wire),
                    data: sse_data(&wire),
                }
            })
            .collect()
            .await
    }

    /// `Event` 没有公开读接口，故把它重新走一遍 SSE 序列化拿到报文字节 ——
    /// 「帧长什么样」也就一并被测到了。
    fn wire_of(event: Event) -> String {
        use axum::response::IntoResponse;

        futures::executor::block_on(async {
            let body = axum::response::Sse::new(futures::stream::once(async move {
                Ok::<_, Infallible>(event)
            }))
            .into_response()
            .into_body();
            let bytes = axum::body::to_bytes(body, usize::MAX)
                .await
                .expect("读取 SSE 报文失败");
            String::from_utf8(bytes.into()).expect("SSE 报文必须是 UTF-8")
        })
    }

    fn event_name(wire: &str) -> String {
        wire.lines()
            .find_map(|line| line.strip_prefix("event: "))
            .unwrap_or_default()
            .to_string()
    }

    fn sse_id(wire: &str) -> Option<u64> {
        wire.lines()
            .find_map(|line| line.strip_prefix("id: "))
            .and_then(|value| value.trim().parse().ok())
    }

    fn sse_data(wire: &str) -> Value {
        let data = wire
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str(&data).unwrap_or_else(|e| panic!("data 不是合法 JSON: {e}\n{wire}"))
    }

    /// 「先订阅后取快照」不可反：反过来，两次操作之间发布的帧既不在快照里，也不在
    /// 订阅端的流里 —— 客户端既没有那段文字（快照），也永远收不到那几帧。
    ///
    /// 判据只看向订阅端拿到的**第一帧**：
    ///
    /// * `seq <= 快照水位` ⇒ 重叠。这**只有**「订阅早于取快照」才能发生（订阅端先
    ///   建好，随后发布的那一帧同时被快照吸收），是顺序正确的正面证据；
    /// * `seq > 水位 + 1` ⇒ 空洞。中间那一帧在两次操作之间发布，两边都没拿到。
    ///
    /// 单线程构造不出这个窗口，故发布端跑在另一条线程上连续发布，主循环反复
    /// `attach`。先等发布端真的跑起来：否则主循环可能在它被调度之前就结束，这条
    /// 用例会静默地什么都没测到。
    ///
    /// 变异方法：把 `attach` 改成先 `snapshot()` 后 `subscribe()` → 重叠永远发生
    /// 不了（断言变红），同时开始出现空洞。
    #[test]
    fn attach_subscribes_before_it_takes_the_snapshot() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let published = Arc::new(AtomicU64::new(0));

        let publisher = {
            let feed = Arc::clone(&feed);
            let stop = Arc::clone(&stop);
            let published = Arc::clone(&published);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    feed.publish(delta("x"));
                    published.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        let mut waited = 0u32;
        while published.load(Ordering::Relaxed) < 100 {
            std::thread::yield_now();
            waited += 1;
            assert!(waited < 1_000_000, "发布线程没跑起来，这条用例测不到窗口");
        }

        let mut overlaps = 0usize;
        let mut gaps: Vec<(u64, u64)> = Vec::new();
        for _ in 0..5_000 {
            let frames = feed.attach().expect("未 finished 的 feed 必可订阅");
            let watermark = frames.snapshot.seq;
            let mut rx = frames.rx;
            match rx.try_recv() {
                Ok(frame) if frame.seq <= watermark => overlaps += 1,
                Ok(frame) if frame.seq > watermark + 1 => gaps.push((watermark, frame.seq)),
                // 恰好在窗口外发布（`水位 + 1`）或这一轮还没发布：没有判据，跳过。
                Ok(_) | Err(_) => {}
            }
        }

        stop.store(true, Ordering::Relaxed);
        publisher.join().expect("发布线程不得 panic");

        assert!(gaps.is_empty(), "快照与帧流之间出现空洞: {gaps:?}");
        assert!(
            overlaps >= 20,
            "没有观察到「订阅早于快照」的证据（overlaps={overlaps}），顺序可能被写反了"
        );
    }

    /// 落后于广播缓冲时补发**新快照**（不重放旧帧）：快照覆盖到全部已发布内容，
    /// 之后拿到的帧都落在 `id <= snapshot.seq` 里（客户端按规则丢弃）。
    ///
    /// 变异方法：删掉 `Lagged` 分支（或改成返回 `None` 结束流）→ 首帧之后直接是
    /// 缓冲里最旧的一帧，已产出的正文凭空少一段，本用例的覆盖率断言变红。
    #[tokio::test]
    async fn a_lagging_subscriber_gets_a_snapshot_that_covers_the_skipped_frames() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        let frames = feed.start();

        // 一直不读，读到时必然已经落后于广播缓冲。
        let published = FEED_CAPACITY + 50;
        for _ in 0..published {
            feed.publish(delta("x"));
        }
        feed.publish(done());

        let events = collect(frames).await;

        assert_eq!(events[0].event, "turn_snapshot", "首帧必须是快照");
        assert_eq!(events[0].id, Some(0), "首发订阅的快照序号是 0");
        assert_eq!(events[0].data["data"]["text"], "", "订阅早于任何产出");

        let resend = events
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, event)| event.event == "turn_snapshot")
            .map(|(index, event)| (index, event.id, event.data.clone()))
            .expect("落后之后必须补发一帧 turn_snapshot");
        let (index, resend_id, resend_data) = resend;
        assert_eq!(
            resend_data["data"]["text"],
            "x".repeat(published),
            "补发的快照必须覆盖全部已发布正文"
        );

        // 补发之后的帧都是「客户端该按水位丢掉」的重叠帧，没有新的缺口。
        for event in &events[index + 1..] {
            assert!(
                event.id.is_some_and(|id| id <= resend_id.unwrap_or(0)),
                "补发之后的帧不得越过水位: {event:?}"
            );
        }
        assert_eq!(
            events.last().map(|event| event.event.as_str()),
            Some("done"),
            "帧流靠 done 结束（不要等广播通道关闭：订阅端自己也持着 feed）"
        );
    }

    /// 帧流的帧序：首帧快照 + 之后逐帧递增，`done` 之后收尾。
    #[tokio::test]
    async fn the_frame_stream_starts_with_a_snapshot_and_ends_at_done() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        let frames = feed.start();
        feed.publish(delta("你"));
        feed.publish(delta("好"));
        feed.publish(done());
        // done 之后的帧不该出现（事件循环已退出），这里多发一条验证「收到 done 即收尾」。
        feed.publish(delta("不该被读到"));

        let events = collect(frames).await;
        let ids: Vec<Option<u64>> = events.iter().map(|event| event.id).collect();
        assert_eq!(
            ids,
            vec![Some(0), Some(1), Some(2), Some(3)],
            "每帧都要带 id"
        );
        let names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
        assert_eq!(
            names,
            vec!["turn_snapshot", "text_delta", "text_delta", "done"]
        );
        assert_eq!(events[0].data["data"]["seq"], 0);
        assert_eq!(events[0].data["data"]["phase"], "running");
    }

    /// `publish` 的每条应用规则：快照是「这一轮此刻的样子」，供重连替换气泡。
    #[test]
    fn publish_applies_the_snapshot_rules() {
        let feed = TurnFeed::new("conv-1".to_string());
        feed.publish(delta("正文"));
        feed.publish(ChatSseEvent::ReasoningDelta {
            content: "推理".to_string(),
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::ToolCallStart {
            id: "t1".to_string(),
            name: "show_workspace".to_string(),
            arguments: "{}".to_string(),
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::AgentCallStart {
            call_id: "c1".to_string(),
            agent_id: "a1".to_string(),
            agent_name: "@coder".to_string(),
            task: "do it".to_string(),
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::Usage {
            input_tokens: 7,
            output_tokens: 3,
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::LooperState {
            paused: true,
            conversation_id: "conv-1".to_string(),
        });

        let snapshot = serde_json::to_value(feed.snapshot()).expect("快照必须可序列化");
        assert_eq!(snapshot["text"], "正文");
        assert_eq!(snapshot["reasoning"], "推理");
        assert_eq!(snapshot["phase"], "paused");
        assert_eq!(snapshot["usage"]["input_tokens"], 7);
        assert_eq!(snapshot["usage"]["output_tokens"], 3);
        assert_eq!(snapshot["tool_calls"][0]["id"], "t1");
        assert_eq!(snapshot["tool_calls"][0]["status"], "running");
        assert!(
            snapshot["tool_calls"][0].get("result").is_none(),
            "还没结果就别下发 result 字段: {snapshot}"
        );
        assert_eq!(snapshot["agent_calls"][0]["call_id"], "c1");
        assert_eq!(snapshot["agent_calls"][0]["status"], "running");
        assert_eq!(snapshot["conversation_id"], "conv-1");

        feed.publish(ChatSseEvent::ToolResult {
            id: "t1".to_string(),
            name: "show_workspace".to_string(),
            result: "结果".to_string(),
            images: Vec::new(),
            artifacts: Vec::new(),
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::AgentCallEnd {
            call_id: "c1".to_string(),
            agent_id: "a1".to_string(),
            agent_name: "@coder".to_string(),
            result: "子结果".to_string(),
            conversation_id: "conv-1".to_string(),
        });
        feed.publish(ChatSseEvent::LooperState {
            paused: false,
            conversation_id: "conv-1".to_string(),
        });

        let snapshot = serde_json::to_value(feed.snapshot()).expect("快照必须可序列化");
        assert_eq!(snapshot["tool_calls"][0]["status"], "done");
        assert_eq!(snapshot["tool_calls"][0]["result"], "结果");
        assert_eq!(snapshot["agent_calls"][0]["status"], "done");
        assert_eq!(snapshot["agent_calls"][0]["result"], "子结果");
        assert_eq!(
            snapshot["phase"], "running",
            "取消/失败之后阶段交回 running"
        );
    }

    /// 「有没有订阅者」跟着出口的实际握持走：订阅端一 drop 就立刻回到「没人看」，
    /// 否则看门狗会把一条已经 detach 的轮当成有人看着而永不收尾。
    ///
    /// 变异方法：把判据改成「曾经订阅过」（如置位标志位永不回落）→ 最后一条断言变红。
    #[test]
    fn subscriber_presence_tracks_the_held_outlets() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        assert!(!feed.has_subscribers(), "刚建好的 feed 还没有任何出口");

        let started = feed.start();
        assert!(feed.has_subscribers(), "起始订阅端握着时有人在看");

        let attached = feed.attach().expect("未 finished 的 feed 必可订阅");
        assert!(feed.has_subscribers());

        drop(started);
        assert!(feed.has_subscribers(), "还剩一个出口：不能提前判成没人看");

        drop(attached);
        assert!(!feed.has_subscribers(), "出口全部 drop 之后才回到没人看");
    }

    /// 终态之后不再接受订阅（HTTP 回 204 的判据）；快照本身仍可读（补发用）。
    #[test]
    fn a_finished_turn_refuses_new_subscribers() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        assert!(feed.attach().is_some(), "在途时可订阅");

        feed.publish(delta("答"));
        feed.publish(done());

        assert!(
            feed.attach().is_none(),
            "已 finished 的 feed 不得再接受订阅：否则客户端会等一个永远不会来的 done"
        );
        assert_eq!(feed.snapshot().text, "答", "快照仍可读（落后补发要用）");
    }

    /// 取消的可见性由快照承载，不额外占一帧：`turn_snapshot` 是订阅首帧专用载荷，
    /// 中途插一帧等于给客户端多一条重置路径。
    #[test]
    fn cancelling_shows_up_in_the_snapshot_without_a_frame() {
        let feed = TurnFeed::new("conv-1".to_string());
        feed.publish(delta("半段"));
        feed.mark_cancelling();

        let snapshot = feed.snapshot();
        assert_eq!(snapshot.phase, Phase::Cancelling);
        assert_eq!(snapshot.seq, 1, "阶段是状态不是事件，不占帧序号");
    }
}
