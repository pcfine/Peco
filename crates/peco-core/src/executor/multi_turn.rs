// ============================================================================
// MultiTurnExecutor — 多轮对话
// ============================================================================
//
// 跨多次 execute() 复用同一个 LooperHandle + Session。
// 首次 execute() 时 spawn looper，后续调用复用。
// 每次 execute() 等待一个 TurnComplete 后返回，通过 turn 序号精确匹配事件归属。
//
// **重要**：MultiTurnExecutor 是 LooperHandle 的排他性消费者。
// 调用方不应同时通过其他途径操作同一 looper。

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{AgentExecutor, ExecutorError, ExecutorInput, ExecutorOutput, ExecutorType};
use crate::agent::agent::Agent;
use crate::agent::agent_looper::{AgentLooper, LooperConfig, LooperEvent, LooperHandle};
use crate::session::Session;

/// 多轮对话执行器。
///
/// # 示例
///
/// ```ignore
/// let agent = Arc::new(Agent::from_file("agents/tutor.md").await?);
/// let session = Arc::new(Session::new(SessionMeta::new("math-001", "")));
/// let executor = MultiTurnExecutor::new(agent, session, LooperConfig::default());
///
/// let r1 = executor.execute(ExecutorInput::new("Teach derivatives")).await?;
/// let r2 = executor.execute(ExecutorInput::new("Show an example")).await?;
/// ```
pub struct MultiTurnExecutor {
    /// 首次 execute() 创建，后续复用（Mutex 支持内部可变性）
    handle: Mutex<Option<LooperHandle>>,
    /// 共享的 Session（跨 turn 保留历史）。
    ///
    /// Some = 尚未传给 AgentLooper（首次 execute 前），
    /// None = session 已移入 AgentLooper（looper 内部管理），
    /// 后续通过 looper 的持久化状态恢复。
    session: Mutex<Option<Box<Session>>>,
    /// Agent 实例
    agent: Arc<Agent>,
    /// Looper 配置
    config: LooperConfig,
    /// 当前 turn 序号（每次 execute() 递增，用于匹配事件归属）
    current_turn: Mutex<usize>,
}

impl MultiTurnExecutor {
    /// 创建新的 MultiTurnExecutor。
    ///
    /// session 所有权移入 executor，首次 execute() 时传给 AgentLooper。
    pub fn new(agent: Arc<Agent>, session: Box<Session>, config: LooperConfig) -> Self {
        Self {
            handle: Mutex::new(None),
            session: Mutex::new(Some(session)),
            agent,
            config,
            current_turn: Mutex::new(0),
        }
    }

    /// 创建新的 MultiTurnExecutor 并自动创建 Session。
    pub fn with_new_session(agent: Arc<Agent>, description: &str, config: LooperConfig) -> Self {
        let session = Box::new(Session::new(
            uuid::Uuid::new_v4().to_string(),
            description.to_string(),
        ));
        Self::new(agent, session, config)
    }

    /// 获取 Session 引用（仅在未 spawn 前可用）。
    ///
    /// 返回 `None` 表示 session 已移入 AgentLooper。
    pub fn session(&self) -> Option<&Session> {
        // 注意：此方法需要 &self 但 session 在 Mutex 中。
        // 调用方应确保在单线程上下文中使用，或改用 try_lock。
        // 简化：返回 None（session 在创建 looper 后不可访问）。
        None
    }

    /// 取消当前执行（透传到 LooperHandle）。
    pub async fn cancel(&self) {
        let guard = self.handle.lock().await;
        if let Some(ref h) = *guard {
            h.cancel();
        }
    }

    /// 暂停 looper。
    pub async fn pause(&self) {
        let guard = self.handle.lock().await;
        if let Some(ref h) = *guard {
            h.pause();
        }
    }

    /// 恢复 looper。
    pub async fn resume(&self) {
        let guard = self.handle.lock().await;
        if let Some(ref h) = *guard {
            h.resume();
        }
    }

    /// 关闭 looper 并获取最终结果。
    ///
    /// 调用后不可再执行 execute()。
    pub async fn shutdown(&self) -> Result<crate::agent::agent::ModelResponse, ExecutorError> {
        let mut guard = self.handle.lock().await;
        if let Some(h) = guard.take() {
            Ok(h.shutdown().await?)
        } else {
            Err(ExecutorError::LooperExited("no active looper".into()))
        }
    }

    /// 确保 looper 已创建（首次调用时 spawn）。
    async fn ensure_looper(&self) -> Result<(), ExecutorError> {
        let mut guard = self.handle.lock().await;
        if guard.is_none() {
            let session =
                self.session.lock().await.take().ok_or_else(|| {
                    ExecutorError::LooperExited("session already consumed".into())
                })?;
            let h = AgentLooper::spawn(
                self.agent.clone(),
                session,
                self.config.clone(),
                Arc::new(crate::persistence::NullSessionPersister),
            );
            *guard = Some(h);
        }
        Ok(())
    }
}

#[async_trait]
impl AgentExecutor for MultiTurnExecutor {
    fn name(&self) -> &str {
        "multi_turn"
    }

    fn executor_type(&self) -> ExecutorType {
        ExecutorType::MultiTurn
    }

    async fn execute(&self, input: ExecutorInput) -> Result<ExecutorOutput, ExecutorError> {
        // 1. 确保 looper 已创建
        self.ensure_looper().await?;

        // 2. 递增 turn 序号
        let mut turn_guard = self.current_turn.lock().await;
        *turn_guard += 1;
        let expected_turn = *turn_guard;
        drop(turn_guard);

        // 3. 发送查询
        {
            let guard = self.handle.lock().await;
            if let Some(ref h) = *guard {
                h.send_query(input.prompt.clone()).await.map_err(|_| {
                    ExecutorError::LooperExited("send_query failed: looper exited".into())
                })?;
            } else {
                return Err(ExecutorError::LooperExited("looper handle consumed".into()));
            }
        }

        // 4. 收集事件直到匹配的 TurnComplete
        let mut events = Vec::new();
        let mut turn_complete_found = false;

        loop {
            let event = {
                let guard = self.handle.lock().await;
                if let Some(ref h) = *guard {
                    h.recv_event().await
                } else {
                    return Err(ExecutorError::LooperExited("looper handle consumed".into()));
                }
            };

            match event {
                Some(event) => {
                    // 检查是否为目标 turn 的 TurnComplete
                    if let LooperEvent::TurnComplete { turn_index, .. } = &event
                        && *turn_index == expected_turn
                    {
                        turn_complete_found = true;
                        events.push(event);
                        break;
                    }
                    // 也处理 Shutdown（looper 意外退出）
                    if matches!(event, LooperEvent::Shutdown { .. }) {
                        events.push(event);
                        break;
                    }
                    events.push(event);
                }
                None => {
                    // 通道关闭，looper 意外退出
                    return Err(ExecutorError::LooperExited(
                        "event channel closed unexpectedly".into(),
                    ));
                }
            }
        }

        if !turn_complete_found {
            return Err(ExecutorError::LooperExited(
                "looper shut down before TurnComplete".into(),
            ));
        }

        // 5. 从 TurnComplete 事件中提取文本和 usage
        let (content, usage) = events
            .iter()
            .find_map(|e| {
                if let LooperEvent::TurnComplete { outcome, usage, .. } = e {
                    Some((
                        outcome.text().unwrap_or_default().to_string(),
                        usage.clone(),
                    ))
                } else {
                    None
                }
            })
            .unwrap_or_default();

        Ok(ExecutorOutput {
            content,
            usage,
            structured_data: None,
            turns: 1,
            success: true,
        })
    }
}
