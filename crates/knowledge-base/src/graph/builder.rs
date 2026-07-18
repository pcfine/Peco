use crate::traits::graph_store::{EdgeType, KnowledgeEdge};
use crate::types::{Chunk, Document};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// KnowledgeGraphBuilder
// ---------------------------------------------------------------------------

/// 从摄入的文档构建结构化知识图谱。
///
/// 第一阶段（自动，零 LLM 成本）：创建 CONTAINS 和 NEXT_CHUNK 边。
/// 第二阶段（未来，基于 LLM）：实体提取和 RELATION 边。
/// 第三阶段（未来）：社区检测和摘要。
pub struct KnowledgeGraphBuilder;

impl KnowledgeGraphBuilder {
    pub fn new() -> Self {
        Self
    }

    /// 构建第一阶段结构化边。
    ///
    /// * `CONTAINS` — 每个分块一条边：`Document → Chunk`。
    /// * `NEXT_CHUNK` — 顺序边：`Chunk[n] → Chunk[n+1]`。
    pub fn build_structural_edges(&self, doc: &Document, chunks: &[Chunk]) -> Vec<KnowledgeEdge> {
        if chunks.is_empty() {
            return vec![];
        }

        let mut edges = Vec::with_capacity(chunks.len() * 2);

        // CONTAINS：Document → Chunk
        for chunk in chunks {
            edges.push(KnowledgeEdge {
                source_id: doc.id.clone(),
                target_id: chunk.id.clone(),
                edge_type: EdgeType::Contains,
                weight: 1.0,
                properties: HashMap::new(),
            });
        }

        // NEXT_CHUNK：Chunk[i] → Chunk[i+1]
        for window in chunks.windows(2) {
            edges.push(KnowledgeEdge {
                source_id: window[0].id.clone(),
                target_id: window[1].id.clone(),
                edge_type: EdgeType::NextChunk,
                weight: 0.8,
                properties: HashMap::new(),
            });
        }

        edges
    }
}

impl Default for KnowledgeGraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkMetadata;

    fn make_chunks(doc_id: &str, count: usize) -> Vec<Chunk> {
        (0..count)
            .map(|i| Chunk {
                id: format!("{doc_id}-{i:04}-aaaaaaaa"),
                document_id: doc_id.into(),
                text: format!("chunk {i}"),
                sequence_index: i as u32,
                page_number: None,
                embedding: Vec::new(),
                metadata: ChunkMetadata::default(),
            })
            .collect()
    }

    #[test]
    fn build_structural_edges_empty() {
        let builder = KnowledgeGraphBuilder::new();
        let doc = Document {
            id: "d".into(),
            kb_id: None,
            title: "T".into(),
            source_path: "p".into(),
            content: "c".into(),
            metadata: Default::default(),
        };
        let edges = builder.build_structural_edges(&doc, &[]);
        assert!(edges.is_empty());
    }

    #[test]
    fn build_structural_edges_single_chunk() {
        let builder = KnowledgeGraphBuilder::new();
        let doc = Document {
            id: "d".into(),
            kb_id: None,
            title: "T".into(),
            source_path: "p".into(),
            content: "c".into(),
            metadata: Default::default(),
        };
        let chunks = make_chunks("d", 1);
        let edges = builder.build_structural_edges(&doc, &chunks);

        // 只有 CONTAINS，单个分块没有 NEXT_CHUNK。
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::Contains);
        assert_eq!(edges[0].source_id, "d");
        assert_eq!(edges[0].target_id, "d-0000-aaaaaaaa");
    }

    #[test]
    fn build_structural_edges_multi_chunk() {
        let builder = KnowledgeGraphBuilder::new();
        let doc = Document {
            id: "d".into(),
            kb_id: None,
            title: "T".into(),
            source_path: "p".into(),
            content: "c".into(),
            metadata: Default::default(),
        };
        let chunks = make_chunks("d", 3);
        let edges = builder.build_structural_edges(&doc, &chunks);

        // 3 CONTAINS + 2 NEXT_CHUNK = 5
        assert_eq!(edges.len(), 5);

        let contains_count = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Contains)
            .count();
        assert_eq!(contains_count, 3);

        let next_count = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::NextChunk)
            .count();
        assert_eq!(next_count, 2);

        // 验证 NEXT_CHUNK 顺序。
        let next_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::NextChunk)
            .collect();
        assert_eq!(next_edges[0].source_id, "d-0000-aaaaaaaa");
        assert_eq!(next_edges[0].target_id, "d-0001-aaaaaaaa");
        assert_eq!(next_edges[1].source_id, "d-0001-aaaaaaaa");
        assert_eq!(next_edges[1].target_id, "d-0002-aaaaaaaa");
    }
}
