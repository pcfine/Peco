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
// MarkdownHeadingChunker
// ---------------------------------------------------------------------------

/// Markdown 结构感知分块器。
///
/// 以标题线开启小节，相邻小节累计到接近 `max_chars` 才切；单节超限时在段落
/// 边界切分。fenced code block 与 Markdown 表格块作为不可分割单元，即使自身
/// 超过 `max_chars` 也整块输出（宁超不裂）。
pub struct MarkdownHeadingChunker {
    max_chars: usize,
}

impl MarkdownHeadingChunker {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

/// 一条不可分割的 Markdown 块。
#[derive(Debug, Clone)]
enum Block {
    Heading(String),
    Para(String),
    Code(String),
    Table(String),
}

impl Block {
    fn text(&self) -> &str {
        match self {
            Block::Heading(s) | Block::Para(s) | Block::Code(s) | Block::Table(s) => s,
        }
    }

    fn len(&self) -> usize {
        self.text().chars().count()
    }

    fn is_heading(&self) -> bool {
        matches!(self, Block::Heading(_))
    }
}

/// `#`~`######` 加空格（或行尾）的 ATX 标题行。
fn is_heading_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return false;
    }
    let rest = &trimmed[hashes..];
    rest.is_empty() || rest.starts_with(' ')
}

/// 围栏代码块的围栏字符（``` 或 ~~~），返回其字符。
fn fence_char(line: &str) -> Option<char> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    (trimmed.chars().take_while(|&c| c == ch).count() >= 3).then_some(ch)
}

fn fence_len(line: &str, ch: char) -> usize {
    line.trim_start().chars().take_while(|&c| c == ch).count()
}

fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// 将文本切成不可分割的块（标题 / 段落 / 代码 / 表格）。
fn parse_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
        } else if let Some(ch) = fence_char(line) {
            let open = fence_len(line, ch);
            let mut j = i + 1;
            while j < lines.len() {
                if fence_char(lines[j]) == Some(ch) && fence_len(lines[j], ch) >= open {
                    j += 1;
                    break;
                }
                j += 1;
            }
            blocks.push(Block::Code(lines[i..j].join("\n")));
            i = j;
        } else if is_heading_line(line) {
            blocks.push(Block::Heading(line.trim_end().to_string()));
            i += 1;
        } else if is_table_line(line) {
            let mut j = i + 1;
            while j < lines.len() && is_table_line(lines[j]) {
                j += 1;
            }
            blocks.push(Block::Table(lines[i..j].join("\n")));
            i = j;
        } else {
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.trim().is_empty()
                    || is_heading_line(l)
                    || fence_char(l).is_some()
                    || is_table_line(l)
                {
                    break;
                }
                j += 1;
            }
            blocks.push(Block::Para(lines[i..j].join("\n")));
            i = j;
        }
    }
    blocks
}

/// `blocks` 拼接为一段文本时的字符数（含块间换行）。
fn blocks_len(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .enumerate()
        .map(|(idx, b)| b.len() + usize::from(idx > 0))
        .sum()
}

/// 逐块贪心成组：绝不拆块；仅含标题的组不在此处被切（标题须黏住其正文）。
fn greedy_group(blocks: Vec<Block>, max_chars: usize) -> Vec<Vec<Block>> {
    let mut out: Vec<Vec<Block>> = Vec::new();
    let mut cur: Vec<Block> = Vec::new();
    let mut cur_len = 0usize;
    for block in blocks {
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty()
            && cur_len + block.len() + sep > max_chars
            && !(cur.len() == 1 && cur[0].is_heading())
        {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        cur_len += block.len() + usize::from(!cur.is_empty());
        cur.push(block);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 按标题把小节聚为 chunk：相邻小节累计到接近 `max_chars` 才切。
fn group_blocks(blocks: Vec<Block>, max_chars: usize) -> Vec<Vec<Block>> {
    let mut sections: Vec<Vec<Block>> = Vec::new();
    for block in blocks {
        if block.is_heading() && sections.last().is_some_and(|s| !s.is_empty()) {
            sections.push(Vec::new());
        }
        match sections.last_mut() {
            Some(sec) => sec.push(block),
            None => sections.push(vec![block]),
        }
    }

    let mut out: Vec<Vec<Block>> = Vec::new();
    let mut cur: Vec<Block> = Vec::new();
    let mut cur_len = 0usize;
    for section in sections {
        let sec_len = blocks_len(&section);
        if cur_len > 0 && cur_len + sec_len + 1 > max_chars {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if sec_len <= max_chars {
            cur_len += sec_len + usize::from(!cur.is_empty());
            cur.extend(section);
        } else {
            out.extend(greedy_group(section, max_chars));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Chunker for MarkdownHeadingChunker {
    fn chunk(&self, doc: &Document) -> Vec<Chunk> {
        if doc.content.trim().is_empty() {
            return vec![];
        }
        let groups = group_blocks(parse_blocks(&doc.content), self.max_chars);
        let mut chunks = Vec::new();
        for (seq, group) in groups.into_iter().enumerate() {
            let text = group.iter().map(Block::text).collect::<Vec<_>>().join("\n");
            if text.trim().is_empty() {
                continue;
            }
            let seq = seq as u32;
            let id = make_chunk_id(&doc.id, seq, &text);
            chunks.push(Chunk {
                id,
                document_id: doc.id.clone(),
                text,
                sequence_index: seq,
                page_number: None,
                embedding: Vec::new(),
                metadata: ChunkMetadata::default(),
            });
        }
        chunks
    }

    fn strategy_name(&self) -> &'static str {
        "markdown_heading"
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
        ChunkingStrategy::MarkdownHeading { max_chars } => {
            Box::new(MarkdownHeadingChunker::new(max_chars))
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

        let c = make_chunker(ChunkingStrategy::MarkdownHeading { max_chars: 100 });
        assert_eq!(c.strategy_name(), "markdown_heading");
    }

    // ── MarkdownHeadingChunker ────────────────────────────────────────

    fn md_chunks(content: &str, max_chars: usize) -> Vec<Chunk> {
        make_chunker(ChunkingStrategy::MarkdownHeading { max_chars }).chunk(&test_doc(content))
    }

    #[test]
    fn markdown_keeps_heading_with_its_body() {
        let body_a = "a".repeat(60);
        let body_b = "b".repeat(60);
        let chunks = md_chunks(&format!("# A\n{body_a}\n# B\n{body_b}\n"), 80);

        assert!(chunks.len() >= 2, "两节应分开：{chunks:?}");
        let a = chunks.iter().find(|c| c.text.contains("# A")).unwrap();
        assert!(a.text.contains(&body_a), "标题须与正文同块：{:?}", a.text);
        let b = chunks.iter().find(|c| c.text.contains("# B")).unwrap();
        assert!(b.text.contains(&body_b), "标题须与正文同块：{:?}", b.text);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn markdown_never_splits_table_block() {
        let table = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let intro = "x".repeat(60);
        let chunks = md_chunks(&format!("{intro}\n\n{table}\n"), 60);

        let holders: Vec<&Chunk> = chunks
            .iter()
            .filter(|c| c.text.contains("| a | b |"))
            .collect();
        assert_eq!(holders.len(), 1, "{chunks:?}");
        assert!(
            holders[0].text.contains("| 3 | 4 |"),
            "表格被切断：{:?}",
            holders[0].text
        );
        for c in &chunks {
            if !std::ptr::eq(c, holders[0]) {
                assert!(!c.text.contains('|'), "表格行泄漏到其他块：{:?}", c.text);
            }
        }
    }

    #[test]
    fn markdown_never_splits_code_block() {
        let code = "```rust\nfn main() {\n    let x = 1;\n}\n```";
        let intro = "y".repeat(50);
        let chunks = md_chunks(&format!("{intro}\n\n{code}\n\noutro tail\n"), 40);

        let holders: Vec<&Chunk> = chunks
            .iter()
            .filter(|c| c.text.contains("fn main()"))
            .collect();
        assert_eq!(holders.len(), 1, "{chunks:?}");
        assert!(
            holders[0].text.contains("let x = 1;") && holders[0].text.contains("```rust"),
            "代码块被切断：{:?}",
            holders[0].text
        );
        for c in &chunks {
            assert_eq!(
                c.text.matches("```").count() % 2,
                0,
                "围栏不成对：{:?}",
                c.text
            );
        }
    }

    #[test]
    fn markdown_emits_oversized_table_whole() {
        let table = (1..=10)
            .map(|r| format!("| row{r} | value{r} |"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = md_chunks(&table, 30);

        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert!(
            chunks[0].text.contains("| row1 | value1 |")
                && chunks[0].text.contains("| row10 | value10 |"),
            "超长表格应整块输出：{:?}",
            chunks[0].text
        );
        assert!(chunks[0].text.chars().count() > 30, "整块应超长");
    }

    /// `max_chars` 应真实生效：两个上限得到不同分块数。
    #[test]
    fn markdown_max_chars_takes_effect() {
        let text = (0..20)
            .map(|i| format!("段落{i}内容"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let small = md_chunks(&text, 30);
        let large = md_chunks(&text, 5000);
        assert!(
            small.len() > large.len(),
            "small={} large={}",
            small.len(),
            large.len()
        );
        assert_eq!(large.len(), 1, "大上限应聚成一块");
    }
}
