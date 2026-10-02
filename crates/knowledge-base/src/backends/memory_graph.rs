// ============================================================================
// MemoryGraphStore — 轻量级内存图存储
// ============================================================================
//
// 为缺乏原生图支持的后端（如 LanceDB）提供内存中的 GraphStore 实现。
// 核心逻辑移植自 InMemoryBackend 的 GraphStore 实现。

use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{edge_matches_between, remove_node_from_memory};
use crate::error::KnowledgeError;
use crate::traits::graph_store::*;

/// 轻量级内存图存储，可独立使用或与其他后端组合。
///
/// 使用 `RwLock` 保护内部状态，支持并发读写。
pub struct MemoryGraphStore {
    edges: RwLock<Vec<KnowledgeEdge>>,
    nodes: RwLock<HashMap<String, GraphNode>>,
}

impl MemoryGraphStore {
    /// 创建空的图存储。
    pub fn new() -> Self {
        Self {
            edges: RwLock::new(Vec::new()),
            nodes: RwLock::new(HashMap::new()),
        }
    }

    /// 返回已存储的边数量。
    pub async fn edge_count(&self) -> usize {
        self.edges.read().await.len()
    }

    /// 返回已存储的节点数量。
    pub async fn node_count(&self) -> usize {
        self.nodes.read().await.len()
    }
}

impl Default for MemoryGraphStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GraphStore for MemoryGraphStore {
    async fn add_edge(&self, edge: KnowledgeEdge) -> Result<(), KnowledgeError> {
        self.edges.write().await.push(edge);
        Ok(())
    }

    async fn add_edges(&self, edges: &[KnowledgeEdge]) -> Result<(), KnowledgeError> {
        self.edges.write().await.extend(edges.iter().cloned());
        Ok(())
    }

    async fn remove_node_edges(&self, node_id: &str) -> Result<(), KnowledgeError> {
        let nid = node_id.to_string();
        let mut e = self.edges.write().await;
        e.retain(|edge| edge.source_id != nid && edge.target_id != nid);
        Ok(())
    }

    async fn edges_between(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<KnowledgeEdge>, KnowledgeError> {
        let edges = self.edges.read().await;
        Ok(edges
            .iter()
            .filter(|e| edge_matches_between(e, source_id, target_id, edge_type))
            .cloned()
            .collect())
    }

    async fn remove_edges_between(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: Option<&EdgeType>,
    ) -> Result<usize, KnowledgeError> {
        let mut e = self.edges.write().await;
        let before = e.len();
        e.retain(|edge| !edge_matches_between(edge, source_id, target_id, edge_type));
        Ok(before - e.len())
    }

    async fn remove_node(&self, node_id: &str, cascade: bool) -> Result<usize, KnowledgeError> {
        let removed_edges = {
            let mut e = self.edges.write().await;
            remove_node_from_memory(&mut e, node_id, cascade)?
        };
        self.nodes.write().await.remove(node_id);
        Ok(removed_edges)
    }

    async fn traverse(
        &self,
        start_node: &str,
        edge_types: &[EdgeType],
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError> {
        let edges = self.edges.read().await;
        let mut visited: HashMap<String, u32> = HashMap::new();
        let mut results: Vec<TraversalStep> = Vec::new();

        // 插入起始节点。
        visited.insert(start_node.to_string(), 0);
        results.push(TraversalStep {
            node: GraphNode {
                id: start_node.to_string(),
                labels: Vec::new(),
                properties: HashMap::new(),
                distance: 0,
            },
            via_edge: None,
        });

        // BFS
        let mut frontier: VecDeque<(String, u32)> = VecDeque::from([(start_node.to_string(), 0)]);

        while let Some((current, depth)) = frontier.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let next_depth = depth + 1;

            for edge in edges.iter() {
                if !edge_types.is_empty() && !edge_types.contains(&edge.edge_type) {
                    continue;
                }

                let neighbor = match direction {
                    TraversalDirection::Outgoing if edge.source_id == current => &edge.target_id,
                    TraversalDirection::Incoming if edge.target_id == current => &edge.source_id,
                    TraversalDirection::Both if edge.source_id == current => &edge.target_id,
                    TraversalDirection::Both if edge.target_id == current => &edge.source_id,
                    _ => continue,
                };

                if visited.contains_key(neighbor) {
                    continue;
                }

                visited.insert(neighbor.clone(), next_depth);
                results.push(TraversalStep {
                    node: GraphNode {
                        id: neighbor.clone(),
                        labels: Vec::new(),
                        properties: edge.properties.clone(),
                        distance: next_depth,
                    },
                    via_edge: Some(edge.edge_type.clone()),
                });
                frontier.push_back((neighbor.clone(), next_depth));
            }
        }

        Ok(results)
    }

    async fn shortest_path(
        &self,
        from: &str,
        to: &str,
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError> {
        let edges = self.edges.read().await;
        let mut visited: HashMap<String, (u32, Option<String>, Option<EdgeType>)> = HashMap::new();
        // (node, depth, parent, via_edge)

        let mut frontier: VecDeque<(String, u32)> = VecDeque::from([(from.to_string(), 0)]);
        visited.insert(from.to_string(), (0, None, None));

        let mut found = false;

        while let Some((current, depth)) = frontier.pop_front() {
            if current == to {
                found = true;
                break;
            }
            if depth >= max_depth {
                continue;
            }

            let next_depth = depth + 1;
            for edge in edges.iter() {
                if !edge_types.is_empty() && !edge_types.contains(&edge.edge_type) {
                    continue;
                }

                // 最短路径使用无向遍历。
                let neighbor = if edge.source_id == current {
                    &edge.target_id
                } else if edge.target_id == current {
                    &edge.source_id
                } else {
                    continue;
                };

                if visited.contains_key(neighbor) {
                    continue;
                }

                visited.insert(
                    neighbor.clone(),
                    (
                        next_depth,
                        Some(current.clone()),
                        Some(edge.edge_type.clone()),
                    ),
                );
                frontier.push_back((neighbor.clone(), next_depth));
            }
        }

        if !found {
            return Ok(None);
        }

        // 重建路径。
        let mut path: Vec<TraversalStep> = Vec::new();
        let mut cur = to.to_string();
        loop {
            let (dist, parent, via) = visited
                .get(&cur)
                .cloned()
                .expect("target node must be in visited");
            path.push(TraversalStep {
                node: GraphNode {
                    id: cur.clone(),
                    labels: Vec::new(),
                    properties: HashMap::new(),
                    distance: dist,
                },
                via_edge: via,
            });
            match parent {
                Some(p) => cur = p,
                None => break,
            }
        }
        path.reverse();
        Ok(Some(path))
    }

    async fn expand(
        &self,
        start_chunk_ids: &[String],
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Vec<GraphNode>, KnowledgeError> {
        let mut all_nodes: Vec<GraphNode> = Vec::new();
        for cid in start_chunk_ids {
            let steps = self
                .traverse(cid, edge_types, TraversalDirection::Both, max_depth)
                .await?;
            all_nodes.extend(steps.into_iter().map(|s| s.node));
        }
        // 按 ID 去重。
        let mut seen = HashMap::new();
        all_nodes.retain(|n| seen.insert(n.id.clone(), ()).is_none());
        Ok(all_nodes)
    }

    async fn upsert_node(&self, node: GraphNode) -> Result<(), KnowledgeError> {
        self.nodes.write().await.insert(node.id.clone(), node);
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<GraphNode>, KnowledgeError> {
        Ok(self.nodes.read().await.get(node_id).cloned())
    }

    /// 遍历内存 map 取子图：先按 label 收节点集，再按节点集过滤边（子图闭合）。
    ///
    /// 与 HelixDB 实现的语义一致 —— 节点集**由节点侧**决定（不靠边反推），
    /// 因此孤立点仍会出现在返回节点集内；两端有一端不在节点集内的边一律丢弃。
    async fn list_subgraph(
        &self,
        label: &str,
    ) -> Result<(Vec<GraphNode>, Vec<KnowledgeEdge>), KnowledgeError> {
        let nodes_map = self.nodes.read().await;
        let nodes: Vec<GraphNode> = nodes_map
            .values()
            .filter(|n| n.labels.iter().any(|l| l.as_str() == label))
            .cloned()
            .collect();
        let kept: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

        let edges = self.edges.read().await;
        let closed = edges
            .iter()
            .filter(|e| kept.contains(e.source_id.as_str()) && kept.contains(e.target_id.as_str()))
            .cloned()
            .collect();

        Ok((nodes, closed))
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_traverse() {
        let gs = MemoryGraphStore::new();

        gs.add_edge(KnowledgeEdge {
            source_id: "A".into(),
            target_id: "B".into(),
            edge_type: EdgeType::Custom("knows".into()),
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        gs.add_edge(KnowledgeEdge {
            source_id: "B".into(),
            target_id: "C".into(),
            edge_type: EdgeType::Custom("knows".into()),
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        let steps = gs
            .traverse("A", &[], TraversalDirection::Outgoing, 2)
            .await
            .unwrap();

        assert_eq!(steps.len(), 3); // A, B, C
        assert_eq!(steps[0].node.id, "A");
        assert_eq!(steps[1].node.id, "B");
        assert_eq!(steps[2].node.id, "C");
    }

    #[tokio::test]
    async fn test_shortest_path() {
        let gs = MemoryGraphStore::new();

        gs.add_edge(KnowledgeEdge {
            source_id: "A".into(),
            target_id: "B".into(),
            edge_type: EdgeType::Custom("knows".into()),
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        gs.add_edge(KnowledgeEdge {
            source_id: "B".into(),
            target_id: "C".into(),
            edge_type: EdgeType::Custom("knows".into()),
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        let path = gs
            .shortest_path("A", "C", &[], 10)
            .await
            .unwrap()
            .expect("path should exist");

        assert_eq!(path.len(), 3);
        assert_eq!(path[0].node.id, "A");
        assert_eq!(path[1].node.id, "B");
        assert_eq!(path[2].node.id, "C");
    }

    #[tokio::test]
    async fn test_shortest_path_not_found() {
        let gs = MemoryGraphStore::new();

        let path = gs.shortest_path("A", "Z", &[], 10).await.unwrap();

        assert!(path.is_none());
    }

    #[tokio::test]
    async fn test_edge_count() {
        let gs = MemoryGraphStore::new();
        assert_eq!(gs.edge_count().await, 0);

        gs.add_edge(KnowledgeEdge {
            source_id: "A".into(),
            target_id: "B".into(),
            edge_type: EdgeType::Custom("test".into()),
            weight: 1.0,
            properties: HashMap::new(),
        })
        .await
        .unwrap();

        assert_eq!(gs.edge_count().await, 1);
    }

    // ── 边/节点删除原语 ──────────────────────────────────────────────────

    fn edge(source: &str, target: &str, label: &str, weight: f32) -> KnowledgeEdge {
        KnowledgeEdge {
            source_id: source.into(),
            target_id: target.into(),
            edge_type: EdgeType::Custom(label.into()),
            weight,
            properties: HashMap::new(),
        }
    }

    async fn seeded() -> MemoryGraphStore {
        let gs = MemoryGraphStore::new();
        gs.add_edges(&[
            edge("A", "B", "朋友", 0.9),
            edge("B", "A", "反向朋友", 0.5),
            edge("A", "C", "朋友", 0.4),
        ])
        .await
        .unwrap();
        gs
    }

    /// 有向命中：只删 `source → target`，反向边完好。
    #[tokio::test]
    async fn remove_edges_between_is_directed() {
        let gs = seeded().await;
        let t = EdgeType::Custom("朋友".into());

        let matched = gs.edges_between("A", "B", Some(&t)).await.unwrap();
        assert_eq!(matched.len(), 1, "只有 A→B 这一条");
        assert!(
            (matched[0].weight - 0.9).abs() < f32::EPSILON,
            "带真实 weight"
        );

        assert_eq!(
            gs.remove_edges_between("A", "B", Some(&t)).await.unwrap(),
            1
        );
        assert_eq!(gs.edge_count().await, 2, "B→A 与 A→C 都应存活");
        assert!(
            gs.edges_between("A", "B", Some(&t))
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// 谓词不匹配时不删（谓词是全匹配，无规范化）。
    #[tokio::test]
    async fn remove_edges_between_matches_predicate_exactly() {
        let gs = seeded().await;
        let wrong = EdgeType::Custom("同事".into());

        assert_eq!(
            gs.remove_edges_between("A", "B", Some(&wrong))
                .await
                .unwrap(),
            0
        );
        assert_eq!(gs.edge_count().await, 3);
    }

    /// `edge_type: None` 双向匹配：正反两条一起删。
    #[tokio::test]
    async fn remove_edges_between_wildcard_covers_both_directions() {
        let gs = seeded().await;

        assert_eq!(gs.remove_edges_between("A", "B", None).await.unwrap(), 2);
        assert_eq!(gs.edge_count().await, 1, "只剩 A→C");
        assert!(gs.edges_between("A", "B", None).await.unwrap().is_empty());
    }

    /// 并行边全删，计数如实（内存后端 `add_edges` 只做 extend，不去重）。
    #[tokio::test]
    async fn remove_edges_between_counts_parallel_edges() {
        let gs = MemoryGraphStore::new();
        gs.add_edges(&[
            edge("A", "B", "朋友", 0.9),
            edge("A", "B", "朋友", 0.8),
            edge("A", "B", "朋友", 0.7),
        ])
        .await
        .unwrap();

        let t = EdgeType::Custom("朋友".into());
        assert_eq!(
            gs.remove_edges_between("A", "B", Some(&t)).await.unwrap(),
            3
        );
        assert_eq!(gs.edge_count().await, 0);
    }

    /// 无匹配返回 0（不是错误）。
    #[tokio::test]
    async fn remove_edges_between_no_match_returns_zero() {
        let gs = seeded().await;
        let t = EdgeType::Custom("朋友".into());

        assert_eq!(
            gs.remove_edges_between("C", "A", Some(&t)).await.unwrap(),
            0
        );
        assert_eq!(gs.edge_count().await, 3);
    }

    /// `cascade: false` 且有残留边 → `InvalidInput`，且**什么都没删**。
    #[tokio::test]
    async fn remove_node_without_cascade_is_rejected_when_edges_remain() {
        let gs = seeded().await;
        gs.upsert_node(GraphNode {
            id: "A".into(),
            labels: vec!["Entity".into()],
            properties: HashMap::new(),
            distance: 0,
        })
        .await
        .unwrap();

        let err = gs.remove_node("A", false).await.unwrap_err();
        assert!(matches!(err, KnowledgeError::InvalidInput(_)), "{err:?}");
        assert_eq!(gs.edge_count().await, 3, "拒绝时不得动边");
        assert!(
            gs.get_node("A").await.unwrap().is_some(),
            "拒绝时不得动节点"
        );
    }

    /// `cascade: true` → 边与节点俱删，返回删掉的边数。
    #[tokio::test]
    async fn remove_node_with_cascade_deletes_edges_and_node() {
        let gs = seeded().await;
        gs.upsert_node(GraphNode {
            id: "A".into(),
            labels: vec!["Entity".into()],
            properties: HashMap::new(),
            distance: 0,
        })
        .await
        .unwrap();

        // A 关联三条：A→B、B→A（反向）、A→C
        assert_eq!(gs.remove_node("A", true).await.unwrap(), 3);
        assert_eq!(gs.edge_count().await, 0);
        assert!(gs.get_node("A").await.unwrap().is_none());
    }

    /// 无边节点 `cascade: false` 直接删掉，返回 0。
    #[tokio::test]
    async fn remove_node_without_edges_succeeds_without_cascade() {
        let gs = MemoryGraphStore::new();
        gs.upsert_node(GraphNode {
            id: "Lonely".into(),
            labels: vec!["Entity".into()],
            properties: HashMap::new(),
            distance: 0,
        })
        .await
        .unwrap();

        assert_eq!(gs.remove_node("Lonely", false).await.unwrap(), 0);
        assert!(gs.get_node("Lonely").await.unwrap().is_none());
    }

    /// 孤儿行为回归：删边后对端实体节点仍然存活（删一条事实不该带走实体）。
    #[tokio::test]
    async fn removing_an_edge_leaves_the_peer_node_alive() {
        let gs = seeded().await;
        for id in ["A", "B", "C"] {
            gs.upsert_node(GraphNode {
                id: id.into(),
                labels: vec!["Entity".into()],
                properties: HashMap::new(),
                distance: 0,
            })
            .await
            .unwrap();
        }

        let t = EdgeType::Custom("朋友".into());
        gs.remove_edges_between("A", "C", Some(&t)).await.unwrap();

        assert!(gs.get_node("C").await.unwrap().is_some(), "C 应存活");
        // 且 C 不再作为 A 的邻居出现（遍历结果里没有它）
        let steps = gs
            .traverse("A", &[], TraversalDirection::Outgoing, 1)
            .await
            .unwrap();
        assert!(
            !steps.iter().any(|s| s.node.id == "C"),
            "C 不应再是 A 的邻居"
        );
    }

    // ── 子图快照（list_subgraph）──────────────────────────────────────────

    /// 子图闭合 + 孤立点：节点集**由节点侧**决定，不能靠 edges 反推。
    #[tokio::test]
    async fn list_subgraph_keeps_isolated_entity_and_drops_cross_label_edges() {
        let gs = MemoryGraphStore::new();
        for (id, label) in [
            ("entity:Entity:e1", "Entity"),
            ("entity:Entity:e2", "Entity"),
            ("entity:Entity:e3", "Entity"), // 孤立点：无边
            ("chunk:c1", "Chunk"),
        ] {
            gs.upsert_node(GraphNode {
                id: id.into(),
                labels: vec![label.into()],
                properties: HashMap::new(),
                distance: 0,
            })
            .await
            .unwrap();
        }
        gs.add_edges(&[
            edge("entity:Entity:e1", "entity:Entity:e2", "朋友", 0.95),
            // 跨 label：有一端不是 Entity，必须丢
            edge("entity:Entity:e1", "chunk:c1", "MENTIONS", 1.0),
        ])
        .await
        .unwrap();

        let (nodes, edges) = gs.list_subgraph("Entity").await.unwrap();

        // (c) 孤立实体仍在返回节点集内（证明不能靠 edges 反推 nodes）
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "3 个 Entity 节点（含 1 个孤立点）：{ids:?}");
        assert!(ids.contains(&"entity:Entity:e3"), "孤立实体不得丢失");

        // (a) 只保留 Entity→Entity 边
        assert_eq!(edges.len(), 1, "Entity→Chunk 的边必须丢：{edges:?}");
        assert_eq!(edges[0].edge_type, EdgeType::Custom("朋友".into()));
        // (b) 端点是稳定 id
        assert_eq!(edges[0].source_id, "entity:Entity:e1");
        assert_eq!(edges[0].target_id, "entity:Entity:e2");
    }

    /// 空图 / 无匹配 label：返回空，不报错。
    #[tokio::test]
    async fn list_subgraph_empty_when_no_matching_label() {
        let gs = MemoryGraphStore::new();
        let (nodes, edges) = gs.list_subgraph("Entity").await.unwrap();
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }
}
