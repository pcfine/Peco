// ============================================================================
// WorkflowHandle — 外部控制句柄
// ============================================================================

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::engine::SharedWorkflowTask;
use super::events::{ApprovalResponse, WorkflowEvent};

/// Workflow 执行的外部控制句柄。
///
/// 创建自 `WorkflowEngine::spawn()`。通过 `recv_event()` 消费事件流，
/// 通过 `approve()` 响应 Pause 状态，通过 `wait()` 等待完成。
///
/// `Listener` 是独占的（不可 Clone），因此 `WorkflowHandle` 也不可 Clone。
/// 如需多消费者，由调用方在外面包 `Arc<Mutex<...>>`。
pub struct WorkflowHandle {
    pub(crate) run_id: String,
    pub(crate) cancel_flag: Arc<AtomicBool>,
    pub(crate) event_rx: tokio::sync::mpsc::Receiver<WorkflowEvent>,
    pub(crate) approval_tx: tokio::sync::mpsc::Sender<ApprovalResponse>,
    pub(crate) join_handle: SharedWorkflowTask,
}

impl WorkflowHandle {
    /// 返回本次执行的唯一 ID（UUID v4）。
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// 请求取消 Workflow 执行。
    ///
    /// 引擎在每层开始前检查取消标志。已启动的步骤会通过 JoinHandle::abort() 终止。
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
    }

    /// 接收下一个事件。返回 None 表示引擎已停止发送事件（正常结束）。
    pub async fn recv_event(&mut self) -> Option<WorkflowEvent> {
        self.event_rx.recv().await
    }

    /// 向暂停的引擎发送审批决策。
    ///
    /// 仅在收到 `WorkflowEvent::Paused` 后调用有效。
    pub async fn approve(
        &self,
        decision: ApprovalResponse,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<ApprovalResponse>> {
        self.approval_tx.send(decision).await
    }

    /// 等待 Workflow 执行完成（阻塞）。
    ///
    /// 消费内部的 JoinHandle（一次性）。第二次调用返回错误。
    pub async fn wait(&self) -> Result<(), String> {
        let handle = self
            .join_handle
            .lock()
            .await
            .take()
            .ok_or_else(|| "workflow task already consumed".to_string())?;

        match handle.await {
            Ok(()) => Ok(()),
            Err(join_err) => Err(format!("workflow task panicked: {join_err}")),
        }
    }

    /// 检查是否已被取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }
}

impl Drop for WorkflowHandle {
    fn drop(&mut self) {
        // 最后一个引用被丢弃时，自动设置取消标志
        if Arc::strong_count(&self.join_handle) == 1 {
            self.cancel_flag.store(true, Ordering::Release);
        }
    }
}
