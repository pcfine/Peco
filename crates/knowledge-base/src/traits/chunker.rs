use crate::types::{Chunk, Document};

/// 分块策略选择器（用于配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkingStrategy {
    /// 带重叠的滑动窗口（推荐的默认值）。
    OverlappingWindow { size: usize, overlap: usize },
    /// 按句子边界分割。
    SentenceBased { max_chars: usize },
    /// 按 Markdown 标题边界分割。
    MarkdownHeading { max_chars: usize },
    /// 固定大小，无重叠。
    FixedSize { size: usize },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        Self::OverlappingWindow {
            size: 800,
            overlap: 200,
        }
    }
}

/// 文本分块抽象 — 纯逻辑，无 I/O。
pub trait Chunker: Send + Sync {
    /// 将文档内容分割为 `Chunk` 列表。
    ///
    /// 分块的 `id` 字段由分块器确定性计算。
    /// `embedding` 字段留空 — 由摄入管道填充。
    fn chunk(&self, doc: &Document) -> Vec<Chunk>;

    /// 人类可读的策略名称（用于日志/调试）。
    fn strategy_name(&self) -> &'static str;
}
