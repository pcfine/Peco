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
    }
}

impl GenerateStream {
    /// 从装箱的流创建新的 `GenerateStream`。
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
    ) -> Self {
        Self { inner }
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
        self.project().inner.as_mut().poll_next(cx)
    }
}
