use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project_lite::pin_project;

use crate::ProviderError;
use crate::response::StreamChunk;

pin_project! {
    /// 中立内容块流。
    ///
    /// 实现 [`futures::Stream`]，产出 [`StreamChunk`] 项。
    /// 是 [`crate::ModelProvider::generate_stream`] 的返回类型，
    /// 引擎用 [`crate::BlockAssembler`] 折叠为 `Vec<ContentBlock>`。
    pub struct GenerateStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
        // 仅在每次 `poll_next` 期间进入、poll 返回即退出。
        //
        // 这是 async 场景下唯一正确的 span 用法。**不要**改回在流体内
        // `let _guard = span.enter();` 跨 await 持有：`Entered<'_>` 只装了 `&Span`，
        // 是 `Send` 的（带 `PhantomNotSend` 的是 owned 的 `EnteredSpan`），所以那样写
        // 能通过编译；但在多线程 runtime 上任务会在 A 线程 enter、让出、在 B 线程恢复并
        // exit —— A 的 thread-local span 栈残留本 span，期间同线程其它任务的事件会被
        // 挂到它下面。
        span: tracing::Span,
    }
}

impl GenerateStream {
    /// 从装箱的流创建新的 `GenerateStream`（不关联 span）。
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
    ) -> Self {
        Self {
            inner,
            span: tracing::Span::none(),
        }
    }

    /// 同 [`Self::new`]，但把 `span` 关联到该流的每次 poll 上。
    ///
    /// 流内部（含 [`crate::streaming`] 的 SSE 层）发出的事件因此都带上调用方的
    /// `request_id` / `model` / `endpoint` 等字段。
    pub fn new_instrumented(
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
        span: tracing::Span,
    ) -> Self {
        Self { inner, span }
    }

    /// 从流中拉取下一个 chunk。
    ///
    /// 当流耗尽时返回 `None`。
    pub async fn next_chunk(&mut self) -> Option<Result<StreamChunk, ProviderError>> {
        futures::StreamExt::next(self).await
    }

    /// 消费流并返回内部的装箱流。
    pub fn into_inner(
        self,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>> {
        self.inner
    }
}

impl Stream for GenerateStream {
    type Item = Result<StreamChunk, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let _enter = this.span.enter();
        // `project()` 返回的 `inner` 是 `Pin<&mut _>`，直接 `poll_next` 即可。
        this.inner.poll_next(cx)
    }
}
