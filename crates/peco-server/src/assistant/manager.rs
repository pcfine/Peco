// ============================================================================
// PersonalAssistantManager — 个人助理生命周期管理器
// ============================================================================
//
// 职责：
//   1. 确保个人助理 agent.md 存在于用户 Workspace 中（首次访问时从模版安装）
//   2. 加载 Agent 实例（从 agent.md）
//   3. 构建 LooperConfig（注入 PPA 读/写路径 + MessageFilter）
//   4. 提供 stream_chat() → SSE 流式对话
//
// 核心保证：
//   - 每次用户 query 前，PpaDynamicContext 自动检索并注入用户 Profile 和相关记忆
//   - 每次对话轮次完成后，PpaMemoryHook 自动提取新记忆并持久化
//   - MessageFilter 区分当前轮/历史轮，减少 token 消耗但不丢失当前 ReAct 上下文

use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{KeepAlive, Sse};
use futures::stream::Stream;
use model_provider::Message;
use peco_core::agent::{AgentLooper, LooperConfig, LooperEvent, MessageFilter};
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use tokio::sync::mpsc;

use crate::chat::sse::{ChatSseEvent, UsageData, map_looper_event};
use crate::error::ApiError;
use crate::personal_assistant::{PpaComponents, build_ppa_components};
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

/// 个人助理的固定标识，用于 Session identity 和 conversation_id。
pub const PERSONAL_ASSISTANT_ID: &str = "personal_assistant";
/// 个人助理的 Agent 名称（与 agent.md 中 `agent.name` 一致）。
pub const PERSONAL_ASSISTANT_AGENT_NAME: &str = "个人助理";

// ============================================================================
// PersonalAssistantManager
// ============================================================================

/// 个人助理管理器。
///
/// 每个用户拥有一个独立的 Agent 实例（来自其 Workspace 中的 agent.md），
/// PPA 组件（记忆读写）和 MessageFilter 在每次 `stream_chat` 时注入 LooperConfig。
///
/// # 使用方式
///
/// ```ignore
/// let pa = PersonalAssistantManager::new(&state, &user_id).await?;
/// let sse = pa.stream_chat(&state, "帮我查看今天的日志").await?;
/// ```
pub struct PersonalAssistantManager {
    /// 用户 ID
    user_id: String,
    /// 已加载的个人助理 Agent（Arc 共享，多个请求可复用）
    agent: Arc<peco_core::agent::Agent>,
    /// PPA 组件（DynamicContext + MemoryHook）
    ppa_components: PpaComponents,
}

impl PersonalAssistantManager {
    /// 创建新的 PersonalAssistantManager。
    ///
    /// 首次调用时会将随代码提交的 agent.md 模版安装到用户 Workspace 中
    /// （通过 `include_str!` 编译期嵌入）。
    /// 后续调用直接从 Workspace 加载已有 Agent。
    pub async fn new(state: &AppState, user_id: &str) -> Result<Self, ApiError> {
        // ── 1. 确保 agent.md 存在于用户 Workspace ──────────────────────────
        let ws = state.workspace_manager.get(user_id)?;
        Self::ensure_agent_installed(&ws).await?;

        // ── 2. 加载 Agent ─────────────────────────────────────────────────
        let agent = ws
            .load_agent_cached(PERSONAL_ASSISTANT_AGENT_NAME)
            .map_err(|e| {
                ApiError::Internal(format!("failed to load personal assistant agent: {e}"))
            })?;

        // ── 3. 构建 PPA 组件（读: DynamicContext → Profile注入 + 记忆检索
        //                      写: MemoryHook → 对话分析 + 记忆持久化）───
        let ppa_components = build_ppa_components(state, user_id).await;

        tracing::info!(
            user_id = %user_id,
            "PersonalAssistantManager initialized"
        );

        Ok(Self {
            user_id: user_id.to_string(),
            agent,
            ppa_components,
        })
    }

    /// 启动流式对话，返回 SSE 响应。
    ///
    /// # 核心流程
    ///
    /// 1. 加载/创建 Perpetual Session（identity = `"personal_assistant"`）
    /// 2. 构建 LooperConfig：
    ///    - `dynamic_context` → PpaDynamicContext
    ///      ★ 每次用户 query 前，自动检索并注入：
    ///        - 用户 Profile（姓名、角色、技术栈偏好、回复风格偏好）
    ///        - 与当前 query 相关的 Semantic/Episodic 记忆
    ///    - `hooks` → PpaMemoryHook（每次 turn 完成后提取新记忆）
    ///    - `message_filter` → PersonalAssistantMessageFilter（当前轮全保留 / 历史轮去 tool）
    /// 3. 创建 AgentLooper → send_query → 事件循环 → SSE 输出
    pub async fn stream_chat(
        &self,
        state: &AppState,
        message: &str,
    ) -> Result<
        Sse<impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>,
        ApiError,
    > {
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err(ApiError::BadRequest("message is required".into()));
        }

        let conv_id = PERSONAL_ASSISTANT_ID.to_string();

        // ── 1. 加载或创建 Perpetual Session ──────────────────────────────
        let persister = SqliteSessionPersister::new(state.db.clone());
        let session: Box<Session> = match persister.load(&conv_id).await {
            Ok(Some((snapshot, _meta))) => {
                tracing::info!(
                    user_id = %self.user_id,
                    turns = snapshot.committed_turns.len(),
                    "Personal assistant session restored"
                );
                let created_at = snapshot
                    .committed_turns
                    .first()
                    .and_then(|t| t.first())
                    .map(|m| m.timestamp_ms / 1000)
                    .unwrap_or_else(|| {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    });
                Box::new(Session::from_snapshot(
                    conv_id.clone(),
                    "个人助理".to_string(),
                    created_at,
                    snapshot,
                ))
            }
            _ => {
                tracing::info!(
                    user_id = %self.user_id,
                    "Creating new personal assistant session"
                );
                Box::new(Session::new(conv_id.clone(), "个人助理".to_string()))
            }
        };

        // ── 2. 创建 SSE channel ─────────────────────────────────────────
        let (sse_tx, sse_rx) =
            mpsc::channel::<Result<axum::response::sse::Event, std::convert::Infallible>>(256);

        // ── 3. 克隆需要的值给后台任务 ──────────────────────────────────
        let agent = Arc::clone(&self.agent);
        let conv_id_bg = conv_id.clone();
        let message_bg = message.clone();
        let db_bg = state.db.clone();

        // PPA 组件（Arc clone）
        let dynamic_context = self.ppa_components.dynamic_context.clone();
        let hooks = self.ppa_components.hooks.clone();

        tokio::spawn(async move {
            let persister: Arc<dyn SessionPersister> =
                Arc::new(SqliteSessionPersister::new(db_bg.clone()));

            // ── 3a. 构建 LooperConfig ──────────────────────────────────
            let config = LooperConfig {
                event_buffer: 256,
                per_turn_timeout: Some(Duration::from_secs(300)),
                total_timeout: Some(Duration::from_secs(1800)),
                persist_on_failure: true,

                // ★ 读路径：每次用户 query 前自动检索 Profile + 相关记忆
                //   PpaDynamicContext::query() 被 AgentLooper 在 PreparingRequest 阶段调用:
                //     1. 加载 UserProfile（姓名、角色、技术栈、风格偏好）
                //     2. 检索相关 Semantic 记忆
                //     3. PersonalQuery 时额外检索 Episodic 记忆
                //     4. 格式化后注入到 system prompt 的 [Dynamic Context] 段
                dynamic_context,

                // ★ 写路径：每次 turn 完成后自动提取新记忆
                hooks,

                // ★ MessageFilter：区分当前轮/历史轮
                message_filter: Some(Arc::new(PersonalAssistantMessageFilter::new(20))),

                ..LooperConfig::default()
            };

            let handle = AgentLooper::spawn(agent, session, config, persister.clone());

            // ── 3b. 发送用户消息 ──────────────────────────────────────
            if let Err(e) = handle.send_query(message_bg.clone()).await {
                let err_event = ChatSseEvent::Error {
                    message: format!("Failed to send message: {e}"),
                    conversation_id: conv_id_bg.clone(),
                };
                if let Ok(ev) = err_event.to_sse_event() {
                    let _ = sse_tx.send(Ok(ev)).await;
                }
                return;
            }

            // ── 3c. 事件循环：LooperEvent → SSE ───────────────────────
            loop {
                match handle.recv_event().await {
                    Some(LooperEvent::Shutdown { total_usage, .. }) => {
                        let done_ev = ChatSseEvent::Done {
                            usage: UsageData::from(total_usage),
                            conversation_id: conv_id_bg.clone(),
                        };
                        if let Ok(ev) = done_ev.to_sse_event() {
                            let _ = sse_tx.send(Ok(ev)).await;
                        }
                        break;
                    }

                    Some(event) => {
                        if let Some(sse_ev) = map_looper_event(event, &conv_id_bg)
                            && let Ok(ev) = sse_ev.to_sse_event()
                            && sse_tx.send(Ok(ev)).await.is_err()
                        {
                            break;
                        }
                    }

                    None => break,
                }
            }

            drop(handle);
        });

        // ── 4. 返回 SSE 响应 ─────────────────────────────────────────
        let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
        Ok(Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        ))
    }

    // ── 私有方法 ──────────────────────────────────────────────────────

    /// 确保个人助理 agent.md 存在于用户 Workspace 中。
    ///
    /// 使用 `include_str!` 将随代码提交的模版编译进二进制，
    /// 首次访问时写入用户 Workspace 的 `agents/个人助理/agent.md`。
    async fn ensure_agent_installed(ws: &peco_core::workspace::Workspace) -> Result<(), ApiError> {
        // `save_agent` is idempotent: create_dir_all is a no-op on existing dirs,
        // fs::write is atomic, and the content from include_str! is always identical.
        let content = include_str!("personal_assistant_agent.md");

        ws.save_agent(PERSONAL_ASSISTANT_AGENT_NAME, content)
            .map_err(|e| {
                ApiError::Internal(format!(
                    "failed to install personal assistant agent.md: {e}"
                ))
            })?;

        tracing::debug!(
            agent_name = PERSONAL_ASSISTANT_AGENT_NAME,
            "Personal assistant agent.md ensured in workspace"
        );
        Ok(())
    }
}

// ============================================================================
// PersonalAssistantMessageFilter — 当前轮/历史轮区分过滤
// ============================================================================

/// 个人助理专用消息过滤器。
///
/// 区分当前轮与历史轮，减少历史 tool_call/tool_result 的 token 消耗：
/// - 当前轮（最后一个 User 消息起）：保留全部消息（含 tool_call / tool_result）
/// - 历史轮：只保留 User + 有内容的 Assistant 消息
/// - 历史轮施加滑动窗口（最近 N 条）
/// - System 消息始终保留
pub struct PersonalAssistantMessageFilter {
    /// 历史轮保留的最大消息条数（默认 20，约 10 轮对话）
    max_history_messages: usize,
}

impl PersonalAssistantMessageFilter {
    pub fn new(max_history_messages: usize) -> Self {
        Self {
            max_history_messages,
        }
    }
}

impl MessageFilter for PersonalAssistantMessageFilter {
    fn filter(&self, messages: Vec<Arc<Message>>) -> Vec<Arc<Message>> {
        if messages.is_empty() {
            return messages;
        }

        // ── 1. 定位当前轮起点：最后一个 User 消息的索引 ──────────────
        let last_user_idx = messages
            .iter()
            .rposition(|m| matches!(m.as_ref(), Message::User { .. }));

        // 无 User 消息 → 全部视为当前轮，原样返回
        let split_idx = match last_user_idx {
            Some(i) => i,
            None => return messages,
        };

        // 检查首条是否为 System 消息
        let system_msg: Option<&Arc<Message>> = messages
            .first()
            .filter(|m| matches!(m.as_ref(), Message::System { .. }));

        let system_offset = if system_msg.is_some() { 1 } else { 0 };

        // ── 2. 切分：当前轮 [split_idx..] vs 历史轮 [system_offset..split_idx)
        let current_turn: Vec<Arc<Message>> = messages[split_idx..].to_vec();

        // ── 3. 过滤历史轮 ────────────────────────────────────────────
        let filtered_history: Vec<Arc<Message>> = messages[system_offset..split_idx]
            .iter()
            .filter(|m| {
                match m.as_ref() {
                    Message::User { .. } => true,
                    // 保留有文本内容的 Assistant 消息
                    // 注意：同时含 content + tool_calls 的消息也被保留，
                    // 因为 content 中通常包含对用户 query 的直接回复
                    Message::Assistant { content, .. } => content.is_some(),
                    // Tool / 其他 → 丢弃（历史的 tool 过程已被最终回复总结）
                    _ => false,
                }
            })
            .cloned()
            .collect();

        // ── 4. 滑动窗口：历史轮只保留最近 N 条 ───────────────────────
        let recent_history = if filtered_history.len() > self.max_history_messages {
            let start = filtered_history.len() - self.max_history_messages;
            filtered_history[start..].to_vec()
        } else {
            filtered_history
        };

        // ── 5. 组装：System + 截断后的历史 + 完整当前轮 ──────────────
        let mut result: Vec<Arc<Message>> =
            Vec::with_capacity(1 + recent_history.len() + current_turn.len());

        if let Some(sys) = system_msg {
            result.push(Arc::clone(sys));
        }
        result.extend(recent_history);
        result.extend(current_turn);

        result
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{Message, ToolCall, ToolCallFunction};

    fn make_user(content: &str) -> Arc<Message> {
        Arc::new(Message::User {
            content: content.to_string(),
        })
    }

    fn make_assistant_text(content: &str) -> Arc<Message> {
        Arc::new(Message::Assistant {
            content: Some(content.to_string()),
            tool_calls: None,
            reasoning_content: None,
        })
    }

    fn make_assistant_tool_call(id: &str, name: &str, args: &str) -> Arc<Message> {
        Arc::new(Message::Assistant {
            content: Some("Let me run a command.".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            }]),
            reasoning_content: None,
        })
    }

    fn make_tool(tool_call_id: &str, content: &str) -> Arc<Message> {
        Arc::new(Message::Tool {
            tool_call_id: tool_call_id.to_string(),
            content: content.to_string(),
        })
    }

    fn make_system(content: &str) -> Arc<Message> {
        Arc::new(Message::System {
            content: content.to_string(),
        })
    }

    #[test]
    fn test_current_turn_preserved_with_tool_calls() {
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_system("System prompt"),
            make_user("帮我修复编译错误"),
            make_assistant_tool_call("1", "shell_exec", "cargo build"),
            make_tool("1", "error[E0425]..."),
            make_assistant_text("编译报错是因为缺少 use 语句"),
        ];

        let result = filter.filter(messages);

        // System + 当前轮全部保留（含 tool_call/tool_result）
        assert_eq!(result.len(), 5);
        assert!(matches!(result[0].as_ref(), Message::System { .. }));
        assert!(matches!(result[1].as_ref(), Message::User { .. }));
        assert!(matches!(result[2].as_ref(), Message::Assistant { .. }));
        assert!(matches!(result[3].as_ref(), Message::Tool { .. }));
        assert!(matches!(result[4].as_ref(), Message::Assistant { .. }));
    }

    #[test]
    fn test_history_tool_calls_filtered() {
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_system("System prompt"),                               // 0: System
            make_user("历史问题1"),                                     // 1: User → 保留
            make_assistant_tool_call("1", "shell_exec", "ls"), // 2: 含 tool_calls + content → 保留
            make_tool("1", "..."),                             // 3: Tool → 丢弃
            make_assistant_text("历史回答1"),                  // 4: 纯文本 → 保留
            make_user("当前问题"),                             // 5: ★ 当前轮起点
            make_assistant_tool_call("2", "shell_exec", "cargo build"), // 6: 当前轮 → 保留
            make_tool("2", "..."),                             // 7: 当前轮 → 保留
        ];

        let result = filter.filter(messages);

        // 期望: [System] [历史User] [历史Assistant(含content)] [历史纯文本Assistant] [当前User] [当前Assistant(tool)] [当前Tool]
        assert_eq!(result.len(), 7);
        assert!(matches!(result[0].as_ref(), Message::System { .. }));
        assert!(matches!(result[1].as_ref(), Message::User { .. }));
        assert!(matches!(result[2].as_ref(), Message::Assistant { .. }));
        assert!(matches!(result[3].as_ref(), Message::Assistant { .. }));
        assert!(matches!(result[4].as_ref(), Message::User { .. })); // 当前 User
        assert!(matches!(result[5].as_ref(), Message::Assistant { .. })); // 当前 tool_call
        assert!(matches!(result[6].as_ref(), Message::Tool { .. })); // 当前 tool_result
    }

    #[test]
    fn test_sliding_window() {
        let filter = PersonalAssistantMessageFilter::new(4); // 只保留最近 4 条历史
        let mut messages = vec![make_system("System prompt")];

        // 添加 5 轮历史对话
        for i in 0..5 {
            messages.push(make_user(&format!("问题{i}")));
            messages.push(make_assistant_text(&format!("回答{i}")));
        }
        // 添加当前轮
        messages.push(make_user("当前问题"));

        let result = filter.filter(messages);

        // 历史 10 条 → 滑动窗口保留最近 4 条（问题3,回答3,问题4,回答4）
        // + System(1) + 当前轮 User(1) = 6
        assert_eq!(result.len(), 6);
        assert!(matches!(result[0].as_ref(), Message::System { .. }));
        // 前4条历史
        assert!(matches!(result[1].as_ref(), Message::User { .. }));
        assert!(matches!(result[2].as_ref(), Message::Assistant { .. }));
        assert!(matches!(result[3].as_ref(), Message::User { .. }));
        assert!(matches!(result[4].as_ref(), Message::Assistant { .. }));
        // 当前轮
        assert!(matches!(result[5].as_ref(), Message::User { .. }));
    }

    #[test]
    fn test_empty_messages() {
        let filter = PersonalAssistantMessageFilter::new(20);
        let result = filter.filter(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_no_user_messages() {
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_system("System prompt"),
            make_assistant_text("No user message"),
        ];
        // 无 User 消息 → 全部原样返回
        let result = filter.filter(messages);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_assistant_no_content_tool_calls_filtered_in_history() {
        // Assistant { content: None, tool_calls: Some } in history → dropped
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_system("System prompt"),
            make_user("历史问题"),
            Arc::new(Message::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "t1".to_string(),
                    call_type: "function".to_string(),
                    function: ToolCallFunction {
                        name: "shell_exec".to_string(),
                        arguments: "ls".to_string(),
                    },
                }]),
                reasoning_content: None,
            }),
            make_tool("t1", "output"),
            make_assistant_text("历史回答"),
            make_user("当前问题"),
        ];

        let result = filter.filter(messages);

        // Expected: System + 历史User + 历史纯文本Assistant + 当前User
        // Assistant(content=None, tool_calls=Some) and its Tool are dropped
        assert_eq!(result.len(), 4);
        assert!(matches!(result[0].as_ref(), Message::System { .. }));
        assert!(matches!(result[1].as_ref(), Message::User { .. }));
        assert!(matches!(result[2].as_ref(), Message::Assistant { .. }));
        assert!(matches!(result[3].as_ref(), Message::User { .. }));
    }

    #[test]
    fn test_no_system_message() {
        // No System message → system_offset=0, filter starts from index 0
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_user("历史问题1"),
            make_assistant_text("历史回答1"),
            make_user("历史问题2"),
            make_assistant_text("历史回答2"),
            make_user("当前问题"),
        ];

        let result = filter.filter(messages);

        assert_eq!(result.len(), 5);
        assert!(matches!(result[0].as_ref(), Message::User { .. }));
        assert!(matches!(
            result[result.len() - 1].as_ref(),
            Message::User { .. }
        ));
    }

    #[test]
    fn test_mixed_content_types_in_history() {
        // History Tool messages dropped, Assistant with content+tool_calls kept,
        // current turn Tools preserved.
        let filter = PersonalAssistantMessageFilter::new(20);
        let messages = vec![
            make_system("System prompt"),
            // Turn 1 (history): User + tool-call Assistant(content+tool_calls) + Tool + text Assistant
            make_user("问题1"),
            make_assistant_tool_call("c1", "shell_exec", "cargo build"),
            make_tool("c1", "compilation error..."),
            make_assistant_text("修复完成"),
            // Turn 2 (history): User + text-only Assistant
            make_user("问题2"),
            make_assistant_text("回答2"),
            // Turn 3 (current): User + tool-call Assistant + Tool
            make_user("当前问题"),
            make_assistant_tool_call("c2", "shell_exec", "cargo test"),
            make_tool("c2", "tests passed"),
        ];

        let result = filter.filter(messages);

        // 10 input messages, 1 history Tool dropped → 9 output messages
        // [System][User"问题1"][Asst(tool)][Asst"修复完成"][User"问题2"][Asst"回答2"][User"当前"][Asst(tool)][Tool]
        assert_eq!(result.len(), 9);
        assert!(matches!(result[0].as_ref(), Message::System { .. }));
        assert!(matches!(result[1].as_ref(), Message::User { .. })); // "问题1"
        assert!(matches!(result[2].as_ref(), Message::Assistant { .. })); // tool_call with content
        assert!(matches!(result[3].as_ref(), Message::Assistant { .. })); // "修复完成"
        assert!(matches!(result[4].as_ref(), Message::User { .. })); // "问题2"
        assert!(matches!(result[5].as_ref(), Message::Assistant { .. })); // "回答2"
        assert!(matches!(result[6].as_ref(), Message::User { .. })); // "当前问题" (current)
        assert!(matches!(result[7].as_ref(), Message::Assistant { .. })); // current tool_call
        assert!(matches!(result[8].as_ref(), Message::Tool { .. })); // current tool_result
    }
}
