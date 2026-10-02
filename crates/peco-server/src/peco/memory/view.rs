//! 记忆图谱 / 文档列表的响应组装 —— 纯逻辑，无 axum 依赖。
//!
//! 单测直接构造 `GraphNode` / `KnowledgeEdge` / `DocumentSummary` 断言纯函数行为，
//! 不需要 Router / State / 网络。HTTP 薄壳留在 `peco/handler.rs`。
//!
//! 隔离缺口提醒：`build_graph` / `build_document_page` 只做响应整形，**不做**任何
//! per-user / per-KB 过滤 —— 数据层没有可用的过滤键（design §3.4）。

use std::collections::HashSet;

use knowledge_base::{DocumentSummary, GraphNode, KnowledgeEdge};
use serde::Serialize;

/// 图谱响应里的一个实体节点。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryNode {
    /// 稳定 id（`entity:Entity:<hash8>`），非 HelixDB 内部 `$id`。
    pub id: String,
    /// 节点 `name` 属性；缺失时 `""`。
    pub name: String,
}

/// 图谱响应里的一条谓词边。**不含边 id** —— HelixDB 边只有不稳定内部 id。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryEdge {
    pub source: String,
    pub target: String,
    /// 边 label 原文（谓词，大小写敏感，无规范化）。
    pub predicate: String,
    pub weight: f32,
}

/// `GET /api/peco/memory/graph` 响应。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryGraphResponse {
    pub nodes: Vec<MemoryNode>,
    pub edges: Vec<MemoryEdge>,
    /// `nodes` 或 `edges` 任一被上限截断。
    pub truncated: bool,
}

/// 文档列表响应里的一项（全部来自 `DocumentSummary`）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryDocumentItem {
    pub id: String,
    pub title: String,
    /// = `DocumentSummary.source_path`。
    pub source: String,
    /// 由 `metadata` JSON 解析；缺失/损坏时为 `None`。
    pub file_type: Option<String>,
}

/// `GET /api/peco/memory/documents` 响应。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryDocumentPage {
    pub documents: Vec<MemoryDocumentItem>,
    pub offset: usize,
    pub limit: usize,
    /// `limit+1` 探测：请求侧多取一条，返回行数 > `limit` 即还有下一页。
    pub has_more: bool,
}

/// 组装图谱响应：稳定排序 → 截断节点 → **先按保留节点集过滤边** → 截断边。
///
/// 顺序是关键（design §5.1 N-4）：
/// 1. 节点按 `id` 稳定排序（同一输入两次调用逐字相同），再截断到 `node_limit`；
/// 2. `edges` 先过滤掉**任一端点不在保留节点集**的边（子图闭合 + 不占名额），
///    再按 `edge_limit` 截断 —— 若先截断后过滤，`edge_limit` 会退化成「候选上限」，
///    被节点截断砍掉的边会白白吃掉名额。
///
/// `truncated = 节点被截断 || 边被截断`。
pub fn build_graph(
    nodes: Vec<GraphNode>,
    edges: Vec<KnowledgeEdge>,
    node_limit: usize,
    edge_limit: usize,
) -> MemoryGraphResponse {
    // 稳定排序键 = id（唯一），保证输出顺序与输入顺序无关。
    let mut nodes = nodes;
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let node_cut = nodes.len() > node_limit;
    nodes.truncate(node_limit);

    let kept: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let mut edges: Vec<KnowledgeEdge> = edges
        .into_iter()
        .filter(|e| kept.contains(e.source_id.as_str()) && kept.contains(e.target_id.as_str()))
        .collect();
    // 无稳定边 id，用复合键做确定性排序；同键保持输入序（稳定排序）。
    edges.sort_by(|a, b| {
        (
            a.source_id.as_str(),
            a.target_id.as_str(),
            a.edge_type.as_label(),
        )
            .cmp(&(
                b.source_id.as_str(),
                b.target_id.as_str(),
                b.edge_type.as_label(),
            ))
    });

    let edge_cut = edges.len() > edge_limit;
    edges.truncate(edge_limit);

    let nodes = nodes
        .into_iter()
        .map(|n| MemoryNode {
            name: n.properties.get("name").cloned().unwrap_or_default(),
            id: n.id,
        })
        .collect();
    let edges = edges
        .into_iter()
        .map(|e| {
            let predicate = e.edge_type.as_label().to_string();
            MemoryEdge {
                source: e.source_id,
                target: e.target_id,
                predicate,
                weight: e.weight,
            }
        })
        .collect();

    MemoryGraphResponse {
        nodes,
        edges,
        truncated: node_cut || edge_cut,
    }
}

/// 组装文档列表响应：`limit+1` 探测 `has_more`。
///
/// `rows` 由 handler 以 `limit + 1` 条请求得到 —— 返回行数 > `limit` 即还有下一页，
/// 截断到 `limit`。无 count 端点也能翻页（design §3.1 E2）。
pub fn build_document_page(
    rows: Vec<DocumentSummary>,
    offset: usize,
    limit: usize,
) -> MemoryDocumentPage {
    let has_more = rows.len() > limit;
    let documents = rows
        .into_iter()
        .take(limit)
        .map(|d| MemoryDocumentItem {
            id: d.id,
            title: d.title,
            source: d.source_path,
            file_type: d.file_type,
        })
        .collect();

    MemoryDocumentPage {
        documents,
        offset,
        limit,
        has_more,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use knowledge_base::EdgeType;

    fn node(id: &str, name: &str) -> GraphNode {
        let properties = if name.is_empty() {
            HashMap::new()
        } else {
            HashMap::from([("name".to_string(), name.to_string())])
        };
        GraphNode {
            id: id.into(),
            labels: vec!["Entity".into()],
            properties,
            distance: 0,
        }
    }

    fn edge(source: &str, target: &str, predicate: &str, weight: f32) -> KnowledgeEdge {
        KnowledgeEdge {
            source_id: source.into(),
            target_id: target.into(),
            edge_type: EdgeType::Custom(predicate.into()),
            weight,
            properties: HashMap::new(),
        }
    }

    fn doc(id: &str, title: &str, source: &str, file_type: Option<&str>) -> DocumentSummary {
        DocumentSummary {
            id: id.into(),
            title: title.into(),
            source_path: source.into(),
            chunk_count: 0,
            file_type: file_type.map(str::to_string),
        }
    }

    /// (a) node_limit 截断节点，且返回边的端点全部落在保留节点集内（子图闭合）。
    #[test]
    fn node_limit_truncates_and_edges_stay_closed() {
        let nodes: Vec<GraphNode> = (0..10)
            .map(|i| node(&format!("n{i:02}"), &format!("N{i}")))
            .collect();
        let edges = vec![
            edge("n00", "n01", "p", 1.0),
            edge("n00", "n09", "q", 1.0), // n09 会被节点截断砍掉
        ];

        let resp = build_graph(nodes, edges, 5, 100);

        assert_eq!(resp.nodes.len(), 5);
        assert!(resp.truncated, "节点被截断 ⇒ truncated");
        let kept: HashSet<&str> = resp.nodes.iter().map(|n| n.id.as_str()).collect();
        for e in &resp.edges {
            assert!(
                kept.contains(e.source.as_str()) && kept.contains(e.target.as_str()),
                "边端点必须在返回节点集内：{e:?}"
            );
        }
        assert!(
            resp.edges.iter().all(|e| e.target != "n09"),
            "端点被砍的边必须一并丢弃"
        );
    }

    /// (b) edge_cut 分支：节点不动、边数 > edge_limit ⇒ 截断且 truncated=true。
    #[test]
    fn edge_limit_truncates_and_flags_truncated() {
        let nodes = vec![node("a", "A"), node("b", "B"), node("c", "C")];
        let edges = vec![
            edge("a", "b", "p", 1.0),
            edge("b", "c", "p", 1.0),
            edge("a", "c", "p", 1.0),
            edge("c", "a", "p", 1.0),
            edge("b", "a", "p", 1.0),
        ];

        let resp = build_graph(nodes, edges, 10, 2);

        assert_eq!(resp.nodes.len(), 3, "节点不被截断");
        assert_eq!(resp.edges.len(), 2);
        assert!(resp.truncated, "边被截断 ⇒ truncated");
    }

    /// (c) 先按保留节点集过滤边、再按 edge_limit 截断 —— 被节点截断砍掉的边
    /// **不占** edge_limit 名额（N-4 断言）。
    ///
    /// 判别力：fixture 里被节点截断砍掉的边 `a→c` 在排序键 `(source, target, label)`
    /// 上**排在**保留边 `b→a` 之前（`"a" < "b"`）。于是：
    /// - 正确实现（先过滤后截断）：过滤掉 `a→c`（端点 c 不在保留节点集）→ 剩 `b→a`
    ///   → 截断到 1 条 ⇒ `len == 1`；
    /// - 错误实现（先截断后过滤）：先截断到 `[a→c]`，再过滤掉 `a→c` ⇒ `len == 0`。
    /// 断言 `len == 1` 因此能真正区分两种实现（旧 fixture 里被砍边排在保留边之后，
    /// `truncate(1)` 恰好留下合法边，错误实现同样过 —— 恒真，已替换）。
    #[test]
    fn dropped_edges_do_not_consume_edge_limit() {
        // node_limit=2 按 id 排序保留 a、b；c 被砍。
        let nodes = vec![node("a", "A"), node("b", "B"), node("c", "C")];
        let edges = vec![
            edge("a", "c", "p", 1.0), // 端点 c 被砍 → 先丢，不占名额；排序键 a→c 在前
            edge("b", "a", "p", 1.0), // 唯一存活边；排序键 b→a 在后
        ];

        let resp = build_graph(nodes, edges, 2, 1);

        assert_eq!(
            resp.edges.len(),
            1,
            "若先截断会得到 0 条（名额被废边 a→c 吃掉）"
        );
        assert_eq!(resp.edges[0].source, "b");
        assert_eq!(resp.edges[0].target, "a");
        assert!(resp.truncated, "节点被截断 ⇒ truncated");
    }

    /// (d) 节点排序稳定：同一输入两次调用逐字相同，且按 id 升序。
    #[test]
    fn node_order_is_stable_across_calls() {
        let make = || vec![node("n3", "C"), node("n1", "A"), node("n2", "B")];

        let first = build_graph(make(), vec![], 10, 10);
        let second = build_graph(make(), vec![], 10, 10);

        assert_eq!(first.nodes, second.nodes, "同一输入两次调用逐字相同");
        let ids: Vec<&str> = first.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["n1", "n2", "n3"], "按 id 升序稳定排序");
    }

    /// (e) 空输入 ⇒ 空响应、truncated=false。
    #[test]
    fn empty_input_yields_empty_response() {
        let resp = build_graph(vec![], vec![], 200, 500);
        assert!(resp.nodes.is_empty());
        assert!(resp.edges.is_empty());
        assert!(!resp.truncated);
    }

    /// 节点 name 缺失 → `""`；边 predicate/weight 如实透传。
    #[test]
    fn node_name_defaults_empty_and_edge_fields_passthrough() {
        let resp = build_graph(
            vec![node("a", ""), node("b", "小C")],
            vec![edge("a", "b", "朋友", 0.95)],
            10,
            10,
        );

        let names: Vec<&str> = resp.nodes.iter().map(|n| n.name.as_str()).collect();
        // 排序按 id：a 在前（name 缺省为 ""）
        assert_eq!(names, vec!["", "小C"]);
        assert_eq!(resp.edges.len(), 1);
        assert_eq!(resp.edges[0].predicate, "朋友");
        assert!((resp.edges[0].weight - 0.95).abs() < f32::EPSILON);
    }

    /// 文档列表：3 行 / limit=2 ⇒ 截断到 2 且 has_more=true。
    #[test]
    fn document_page_truncates_and_detects_more() {
        let rows = vec![
            doc("1", "t1", "ppa_episodic", Some("txt")),
            doc("2", "t2", "ppa_semantic", None),
            doc("3", "t3", "ppa_profile", Some("md")),
        ];

        let page = build_document_page(rows, 0, 2);

        assert_eq!(page.documents.len(), 2);
        assert!(page.has_more);
        assert_eq!(page.offset, 0);
        assert_eq!(page.limit, 2);
        assert_eq!(page.documents[0].source, "ppa_episodic");
        assert_eq!(page.documents[1].file_type, None);
    }

    /// 文档列表：1 行 / limit=2 ⇒ has_more=false。
    #[test]
    fn document_page_reports_no_more_when_rows_within_limit() {
        let rows = vec![doc("1", "t1", "s", Some("txt"))];
        let page = build_document_page(rows, 0, 2);
        assert_eq!(page.documents.len(), 1);
        assert!(!page.has_more);
    }

    /// 文档列表：空行（offset 严格超界）⇒ 空页、has_more=false（非错误）。
    #[test]
    fn document_page_empty_rows_is_empty_page() {
        let page = build_document_page(vec![], 1_000_000_000_000_000_000, 10);
        assert!(page.documents.is_empty());
        assert!(!page.has_more);
        assert_eq!(page.offset, 1_000_000_000_000_000_000);
    }
}
