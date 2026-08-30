use sha2::{Digest, Sha256};

use crate::traits::Chunker;
use crate::traits::chunker::ChunkingStrategy;
use crate::types::{Chunk, ChunkMetadata, Document};

// ---------------------------------------------------------------------------
// 辅助函数 — 确定性分块 ID
// ---------------------------------------------------------------------------

/// 格式：`{doc_id}-{seq:04}-{content_sha256_hex:08}`
fn make_chunk_id(doc_id: &str, seq: u32, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    let hash_hex = hex::encode(&hash[..4]); // 前 4 字节 → 8 个十六进制字符
    format!("{doc_id}-{seq:04}-{hash_hex}")
}

// ---------------------------------------------------------------------------
// OverlappingWindowChunker
// ---------------------------------------------------------------------------

/// 可配置重叠的滑动窗口分块器。推荐的默认选择。
pub struct OverlappingWindowChunker {
    size: usize,
    overlap: usize,
}

impl OverlappingWindowChunker {
    pub fn new(size: usize, overlap: usize) -> Self {
        assert!(size > overlap, "分块大小必须大于重叠大小");
        Self { size, overlap }
    }
}

impl Chunker for OverlappingWindowChunker {
    fn chunk(&self, doc: &Document) -> Vec<Chunk> {
        let text = &doc.content;
        if text.is_empty() {
            return vec![];
        }

        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        let mut chunks = Vec::new();
        let mut start = 0usize;
        let mut seq: u32 = 0;

        while start < total {
            let end = (start + self.size).min(total);

            // 尝试将 end 对齐到句子边界。
            let raw_aligned = align_end_to_sentence(&chars, start, end, total);

            // 验证：如果对齐后的 end 会使我们卡住（后退 `overlap`
            // 不会使 `start` 前进），则回退到未对齐的 `end`。
            // 这可以防止句子边界位于重叠区域内时产生 1 字符的分块。
            let aligned_end = {
                let retreated = raw_aligned.saturating_sub(self.overlap);
                if raw_aligned < total && retreated <= start {
                    // 对齐后退过了 start → 无意义，使用未对齐的。
                    end
                } else {
                    raw_aligned
                }
            };

            let slice: String = chars[start..aligned_end].iter().collect();

            // 不产生过小的尾部片段；将其吸收到前一个分块中。
            if aligned_end >= total && slice.chars().count() < self.size / 4 && !chunks.is_empty() {
                break;
            }

            let id = make_chunk_id(&doc.id, seq, &slice);
            chunks.push(Chunk {
                id,
                document_id: doc.id.clone(),
                text: slice,
                sequence_index: seq,
                page_number: None,
                embedding: Vec::new(),
                metadata: ChunkMetadata {
                    start_char: Some(start),
                    end_char: Some(aligned_end),
                    heading_path: None,
                },
            });

            seq += 1;
            start = if aligned_end >= total {
                total // 完成
            } else {
                aligned_end.saturating_sub(self.overlap)
            };
        }

        chunks
    }

    fn strategy_name(&self) -> &'static str {
        "overlapping_window"
    }
}

/// 尝试将 `end` 向前扩展到下一个句子边界（在合理范围内），
/// 或回退到前一个句子边界。
fn align_end_to_sentence(chars: &[char], start: usize, end: usize, total: usize) -> usize {
    let sentence_end = |c: char| matches!(c, '.' | '!' | '?' | '\n' | '。' | '！' | '？');
    let max_lookahead = 100;

    // 向前最多查找 max_lookahead 个字符以寻找句子边界。
    let forward = (end..(end + max_lookahead).min(total)).find(|&i| sentence_end(chars[i]));
    if let Some(i) = forward {
        return (i + 1).min(total); // 包含标点符号
    }

    // 向后查找最近的句子边界。
    let backward = (start..end).rev().find(|&i| sentence_end(chars[i]));
    if let Some(i) = backward {
        return (i + 1).min(total);
    }

    end
}

// ---------------------------------------------------------------------------
// FixedSizeChunker
// ---------------------------------------------------------------------------

pub struct FixedSizeChunker {
    size: usize,
}

impl FixedSizeChunker {
    pub fn new(size: usize) -> Self {
        Self { size }
    }
}

impl Chunker for FixedSizeChunker {
    fn chunk(&self, doc: &Document) -> Vec<Chunk> {
        let text = &doc.content;
        if text.is_empty() {
            return vec![];
        }

        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        let mut chunks = Vec::new();
        let mut start = 0usize;
        let mut seq: u32 = 0;

        while start < total {
            let end = (start + self.size).min(total);
            let slice: String = chars[start..end].iter().collect();
            let id = make_chunk_id(&doc.id, seq, &slice);
            chunks.push(Chunk {
                id,
                document_id: doc.id.clone(),
                text: slice,
                sequence_index: seq,
                page_number: None,
                embedding: Vec::new(),
                metadata: ChunkMetadata {
                    start_char: Some(start),
                    end_char: Some(end),
                    heading_path: None,
                },
            });
            seq += 1;
            start = end;
        }

        chunks
    }

    fn strategy_name(&self) -> &'static str {
        "fixed_size"
    }
}

// ---------------------------------------------------------------------------
// SentenceBasedChunker
// ---------------------------------------------------------------------------

pub struct SentenceBasedChunker {
    max_chars: usize,
}

impl SentenceBasedChunker {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Chunker for SentenceBasedChunker {
    fn chunk(&self, doc: &Document) -> Vec<Chunk> {
        let text = &doc.content;
        if text.is_empty() {
            return vec![];
        }

        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        let is_boundary =
            |c: char| matches!(c, '.' | '!' | '?' | '\n' | '。' | '！' | '？' | '；' | ';');

        let mut chunks = Vec::new();
        let mut start = 0usize;
        let mut seq: u32 = 0;

        while start < total {
            let mut end = (start + self.max_chars).min(total);

            // 向前扩展到下一个句子边界。
            if end < total {
                while end < total && !is_boundary(chars[end]) {
                    end += 1;
                }
                if end < total {
                    end += 1; // 包含标点符号
                }
            }

            let slice: String = chars[start..end].iter().collect();
            let id = make_chunk_id(&doc.id, seq, &slice);
            chunks.push(Chunk {
                id,
                document_id: doc.id.clone(),
                text: slice,
                sequence_index: seq,
                page_number: None,
                embedding: Vec::new(),
                metadata: ChunkMetadata {
                    start_char: Some(start),
                    end_char: Some(end),
                    heading_path: None,
                },
            });

            seq += 1;
            start = end;
        }

        chunks
    }

    fn strategy_name(&self) -> &'static str {
        "sentence_based"
    }
}

// ---------------------------------------------------------------------------
// 工厂函数
// ---------------------------------------------------------------------------

/// 从 `ChunkingStrategy` 创建分块器。
pub fn make_chunker(strategy: ChunkingStrategy) -> Box<dyn Chunker> {
    match strategy {
        ChunkingStrategy::OverlappingWindow { size, overlap } => {
            Box::new(OverlappingWindowChunker::new(size, overlap))
        }
        ChunkingStrategy::SentenceBased { max_chars } => {
            Box::new(SentenceBasedChunker::new(max_chars))
        }
        ChunkingStrategy::FixedSize { size } => Box::new(FixedSizeChunker::new(size)),
        ChunkingStrategy::MarkdownHeading { max_chars: _ } => {
            // 暂时回退到滑动窗口 — Markdown 标题分块器是未来的增强功能。
            Box::new(OverlappingWindowChunker::new(800, 200))
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DocumentMetadata;

    fn test_doc(content: &str) -> Document {
        Document {
            kb_id: None,
            id: "test-doc".into(),
            title: "Test".into(),
            source_path: "/tmp/test.md".into(),
            content: content.into(),
            metadata: DocumentMetadata::default(),
        }
    }

    #[test]
    fn overlapping_window_basic() {
        let chunker = OverlappingWindowChunker::new(50, 10);
        let text = "Rust is a systems programming language. It guarantees memory safety without a garbage collector. It has excellent concurrency support.";
        let doc = test_doc(text);
        let chunks = chunker.chunk(&doc);
        assert!(!chunks.is_empty());
        // 每个分块应有确定性 ID。
        for c in &chunks {
            assert!(c.id.starts_with("test-doc-"), "意外的 id: {}", c.id);
        }
    }

    #[test]
    fn overlapping_window_empty_doc() {
        let chunker = OverlappingWindowChunker::new(50, 10);
        let chunks = chunker.chunk(&test_doc(""));
        assert!(chunks.is_empty());
    }

    #[test]
    fn fixed_size_basic() {
        let chunker = FixedSizeChunker::new(20);
        let text = "abcdefghijklmnopqrstuvwxyz";
        let chunks = chunker.chunk(&test_doc(text));
        assert!(!chunks.is_empty());
    }

    #[test]
    fn sentence_based_basic() {
        let chunker = SentenceBasedChunker::new(100);
        let text = "First sentence. Second sentence! Third sentence? Fourth. End.";
        let chunks = chunker.chunk(&test_doc(text));
        assert!(!chunks.is_empty());
    }

    #[test]
    fn deterministic_chunk_id() {
        let id1 = make_chunk_id("doc", 0, "hello world");
        let id2 = make_chunk_id("doc", 0, "hello world");
        assert_eq!(id1, id2);

        let id3 = make_chunk_id("doc", 0, "different text");
        assert_ne!(id1, id3);

        let id4 = make_chunk_id("doc", 1, "hello world");
        assert_ne!(id1, id4);
    }

    #[test]
    fn chunker_factory() {
        let c = make_chunker(ChunkingStrategy::default());
        assert_eq!(c.strategy_name(), "overlapping_window");

        let c = make_chunker(ChunkingStrategy::FixedSize { size: 100 });
        assert_eq!(c.strategy_name(), "fixed_size");
    }
}
