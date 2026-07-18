use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project_lite::pin_project;

use crate::{ProviderError, ToolCall, types::Usage};

/// 流式聊天补全过程中发出的事件。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 助手的文本内容增量。
    TextDelta(String),
    /// 推理/思考内容增量（例如 DeepSeek-R1 思考模式）。
    ReasoningDelta(String),
    /// 工具调用增量。`name` 为 `Some` 表示函数名称首次出现；
    /// 同一调用的后续增量为 `name: None`，并追加到 `arguments` 中。
    ToolCallDelta {
        /// 工具调用标识符。
        id: String,
        /// 函数名称（仅在同一调用的第一个数据块中出现）。
        name: Option<String>,
        /// 增量的 JSON 编码参数。
        ///
        /// - 流式构建过程中，部分片段为 [`serde_json::Value::String`]
        ///   （例如 `"{\"city\""`）。
        /// - 当参数构成完整的、已解析的 JSON 对象时，为
        ///   [`serde_json::Value::Object`]（单数据块补全或最终刷新时）。
        arguments: serde_json::Value,
    },
    /// 已完整组装、可执行的工具调用。
    ///
    /// 当流式管线从流式增量中完成组装完整工具调用时发出
    /// （在结束原因刷新或流结束时）。使用者应使用此事件来执行，
    /// 而不是从单个 [`ToolCallDelta`] 片段中重建调用。
    ToolCallComplete(ToolCall),
    /// 流已结束，附带最终的用量信息。
    End {
        /// 整个补全过程的 token 用量。
        usage: Usage,
    },
}

pin_project! {
    /// 聊天补全事件流。
    ///
    /// 实现 [`futures::Stream`]，产出 [`StreamEvent`] 项。
    /// 可直接作为流使用，也可通过便捷方法使用。
    pub struct ChatStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
    }
}

impl ChatStream {
    /// 从装箱的流创建新的 `ChatStream`。
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
    ) -> Self {
        Self { inner }
    }

    /// 从流中拉取下一个事件。
    ///
    /// 当流耗尽时返回 `None`。
    pub async fn next_event(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        futures::StreamExt::next(self).await
    }

    /// 将所有文本增量收集为单个字符串。
    ///
    /// 消费整个流，并将所有 `TextDelta` 事件拼接起来。
    /// 其他事件类型在收集过程中被忽略。
    pub async fn collect_text(&mut self) -> Result<String, ProviderError> {
        let mut text = String::new();
        while let Some(event) = self.next_event().await {
            if let StreamEvent::TextDelta(delta) = event? {
                text.push_str(&delta)
            }
        }
        Ok(text)
    }

    /// 消费流并返回内部的装箱流。
    pub fn into_inner(
        self,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> {
        self.inner
    }
}

impl Stream for ChatStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.as_mut().poll_next(cx)
    }
}
