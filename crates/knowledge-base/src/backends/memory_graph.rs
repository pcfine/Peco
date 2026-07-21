// ============================================================================
// MemoryGraphStore — 轻量级内存图存储
// ============================================================================
//
// 为缺乏原生图支持的后端（如 LanceDB）提供内存中的 GraphStore 实现。
// 核心逻辑移植自 InMemoryBackend 的 GraphStore 实现。

use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use tokio::sync::RwLock;

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
}
