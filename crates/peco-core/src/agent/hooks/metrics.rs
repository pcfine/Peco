// ============================================================================
// MetricsCollector — Token 用量收集钩子
// ============================================================================
//
// 实现 LooperHook，在每轮模型调用后收集 Usage 数据。
// 通过 mpsc channel 发送给 consumer（在 peco-server handler 中消费写入 DB）。

use async_trait::async_trait;
use model_provider::GenerateResult;
use tokio::sync::mpsc;

use super::{HookAction, LooperHook};

/// Token 用量记录。
#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub turn_index: usize,
}

/// 用量收集钩子。
///
/// 在 `on_after_response` 中提取 Usage 并通过 channel 发送。
/// Channel receiver 端在 handler 中消费并写入 usage_logs 表。
pub struct MetricsCollector {
    tx: mpsc::UnboundedSender<UsageRecord>,
}

impl MetricsCollector {
    /// 创建新的 MetricsCollector 和对应的 receiver。
    pub fn new() -> (Self, mpsc::UnboundedReceiver<UsageRecord>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// 使用已有 sender 创建（多个 collector 共享同一 channel）。
    pub fn with_sender(tx: mpsc::UnboundedSender<UsageRecord>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl LooperHook for MetricsCollector {
    async fn on_after_response(&self, turn_index: usize, response: &GenerateResult) -> HookAction {
        if response.usage.total_tokens > 0 {
            let record = UsageRecord {
                input_tokens: response.usage.input_tokens,
                output_tokens: response.usage.output_tokens,
                turn_index,
            };
            let _ = self.tx.send(record);
        }
        HookAction::Continue
    }
}
