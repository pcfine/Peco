//! 记忆图谱 / 文档列表的响应组装 —— 纯逻辑，无 axum 依赖。
//!
//! 单测直接构造 `GraphNode` / `KnowledgeEdge` / `DocumentSummary` 断言纯函数行为，
//! 不需要 Router / State / 网络。HTTP 薄壳留在 `peco/handler.rs`。
//!
//! 隔离缺口提醒：`build_graph` / `build_document_page` 只做响应整形，**不做**任何
//! per-user / per-KB 过滤 —— 数据层没有可用的过滤键（design §3.4）。

use std::collections::HashSet;

use knowledge_base::{Document, DocumentSummary, GraphNode, KnowledgeEdge};
use serde::Serialize;

/// E4 全量扫描的条数上限（design §3.2 E4 / §3.3-M13）。
///
/// 取 `SCAN_LIMIT + 1` 条探测是否超限（同 E2 的 `limit+1` 范式）：返回行数 >
/// `SCAN_LIMIT` ⇒ handler 返回 `500` + `warn!`，**不静默截断** —— 静默只扫前 N 条
/// 会重演「明明有正文却报无匹配」。当前 N≈211 ≪ 2000，正常路径不触发。
pub const SCAN_LIMIT: usize = 2000;

/// snippet 命中点**单侧**字符数（design §3.3-M13）。
pub const SNIPPET_PAD: usize = 40;

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

// ── E3 文档详情 / E4 内容检索（v2） ─────────────────────────────────────────

/// `GET /api/peco/memory/documents/{id}` 响应（E3）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryDocumentDetail {
    pub id: String,
    pub title: String,
    /// = `Document.source_path`。
    pub source: String,
    /// `Document.metadata.file_type`，缺失 → `null`。
    pub file_type: Option<String>,
    /// `Document.metadata.created_at`（ISO 8601），缺失 → `null`。
    pub created_at: Option<String>,
    /// `Document.content` 全文（可能为空串）。
    pub content: String,
}

/// `GET /api/peco/memory/search` 的一条命中（E4）。**无 `score`** ——
/// 子串扫描无相关性信号，不承诺任何恒定量（design §1.2-#15）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemorySearchHit {
    pub id: String,
    pub title: String,
    /// = `Document.source_path`。
    pub source: String,
    pub file_type: Option<String>,
    /// 首个命中位置 ± [`SNIPPET_PAD`] 字符窗口。
    pub snippet: String,
}

/// `GET /api/peco/memory/search` 响应（E4）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemorySearchResponse {
    pub hits: Vec<MemorySearchHit>,
}

/// 组装文档详情响应：`source_path` → `source`，`metadata` 两字段缺失时为 `None`。
pub fn build_document_detail(doc: Document) -> MemoryDocumentDetail {
    MemoryDocumentDetail {
        id: doc.id,
        title: doc.title,
        source: doc.source_path,
        file_type: doc.metadata.file_type,
        created_at: doc.metadata.created_at,
        content: doc.content,
    }
}

/// 组装检索响应：仅包一层 `{ hits }`。
pub fn build_search_response(hits: Vec<MemorySearchHit>) -> MemorySearchResponse {
    MemorySearchResponse { hits }
}

/// 大小写不敏感子串查找：返回 `content` 中**首个**匹配的**字符**下标。
///
/// **按字符**比较（Unicode 安全）：对每个字符起点 `i`，逐字符将
/// `content[i+k]` 与 `query[k]` 各自 `char::to_lowercase()` 后比较。中文场景
/// 大小写不敏感等价于精确；对 ASCII（如 `小C` vs `小c`）生效。
/// 复杂度 O(len(content) × len(query))，N ≤ 1450 字 → 极轻。
/// 空 `query` → `None`（handler 已确保 `q` 非空）。
pub fn find_ci(content: &str, query: &str) -> Option<usize> {
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<char> = content.chars().collect();
    if needle.len() > hay.len() {
        return None;
    }
    for i in 0..=(hay.len() - needle.len()) {
        let matched =
            (0..needle.len()).all(|k| hay[i + k].to_lowercase().eq(needle[k].to_lowercase()));
        if matched {
            return Some(i);
        }
    }
    None
}

/// 生成命中片段：`content` 的**字符**窗口 `[i-PAD, i+PAD+qlen)`，`i = find_ci(..)`。
///
/// 左侧被截 → 前缀 `…`；右侧被截 → 后缀 `…`；未命中 → `""`。
/// **必须按字符切**（`chars()` 收集后切片），禁止按字节 —— 否则中文 ⋯ 边界 panic。
/// 长度上界 ≈ `2*SNIPPET_PAD + qlen + 2` 字符。
pub fn make_snippet(content: &str, query: &str) -> String {
    let Some(i) = find_ci(content, query) else {
        return String::new();
    };
    let chars: Vec<char> = content.chars().collect();
    let qlen = query.chars().count();
    let start = i.saturating_sub(SNIPPET_PAD);
    let end = (i + qlen + SNIPPET_PAD).min(chars.len());

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(chars[start..end].iter());
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// 纯检索：`source` 本地过滤 → `find_ci` 命中判定 → `make_snippet` →
/// 按 `id` 升序 → 截断 `limit`。
///
/// 每文档至多一条命中（以文档为单位扫描）⇒ **天然去重**，无 `dedup` 步骤。
/// 排序用 `id`（唯一内容哈希）⇒ 确定全序、跨调用稳定（design §3.2 E4）。
pub fn search_documents(
    docs: Vec<Document>,
    query: &str,
    source: Option<&str>,
    limit: usize,
) -> Vec<MemorySearchHit> {
    let mut hits: Vec<MemorySearchHit> = docs
        .into_iter()
        .filter(|d| source.is_none_or(|want| d.source_path == want))
        .filter_map(|d| {
            find_ci(&d.content, query)?;
            Some(MemorySearchHit {
                snippet: make_snippet(&d.content, query),
                id: d.id,
                title: d.title,
                source: d.source_path,
                file_type: d.metadata.file_type,
            })
        })
        .collect();

    hits.sort_by(|a, b| a.id.cmp(&b.id));
    hits.truncate(limit);
    hits
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
    ///   断言 `len == 1` 因此能真正区分两种实现（旧 fixture 里被砍边排在保留边之后，
    ///   `truncate(1)` 恰好留下合法边，错误实现同样过 —— 恒真，已替换）。
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

    // ── v2：E3 详情 / E4 检索 ──────────────────────────────────────────────

    fn full_doc(
        id: &str,
        title: &str,
        source: &str,
        content: &str,
        file_type: Option<&str>,
        created_at: Option<&str>,
    ) -> Document {
        Document {
            id: id.into(),
            kb_id: None,
            title: title.into(),
            source_path: source.into(),
            content: content.into(),
            metadata: knowledge_base::DocumentMetadata {
                file_type: file_type.map(str::to_string),
                created_at: created_at.map(str::to_string),
                ..Default::default()
            },
        }
    }

    /// E3：`build_document_detail` 6 字段逐字映射（`source_path` → `source`，
    /// `metadata` 两字段如实透传）。
    #[test]
    fn build_document_detail_maps_all_fields() {
        let d = full_doc(
            "ef5c3ede5ae751ce",
            "chen 的朋友小C及其作品",
            "ppa_semantic",
            "……全文……",
            Some("txt"),
            Some("2026-09-16T09:57:58.915195367+00:00"),
        );

        let detail = build_document_detail(d);

        assert_eq!(
            detail,
            MemoryDocumentDetail {
                id: "ef5c3ede5ae751ce".into(),
                title: "chen 的朋友小C及其作品".into(),
                source: "ppa_semantic".into(),
                file_type: Some("txt".into()),
                created_at: Some("2026-09-16T09:57:58.915195367+00:00".into()),
                content: "……全文……".into(),
            }
        );
    }

    /// E3：`metadata` 两字段缺失 ⇒ `file_type`/`created_at` 均为 `None`（非空串）。
    #[test]
    fn build_document_detail_missing_metadata_is_none() {
        let d = full_doc("id1", "t", "s", "", None, None);
        let detail = build_document_detail(d);
        assert_eq!(detail.file_type, None);
        assert_eq!(detail.created_at, None);
        assert_eq!(detail.content, "");
    }

    /// `find_ci`：**字符**下标（中文偏移）；大小写不敏感；无命中 → None。
    #[test]
    fn find_ci_is_char_indexed_and_case_insensitive() {
        assert_eq!(find_ci("春天的爱情故事", "爱情"), Some(3));
        assert_eq!(find_ci("小C", "小c"), Some(0));
        assert_eq!(find_ci("小c", "小C"), Some(0));
        assert_eq!(find_ci("abc", "bc"), Some(1));
        assert_eq!(find_ci("没有匹配", "爱情"), None);
        assert_eq!(find_ci("anything", ""), None);
    }

    /// `make_snippet`：命中在开头不留前缀；在结尾不留后缀；超长中间命中两侧 `…`；
    /// 中文多字节切片不 panic；长度有界。
    #[test]
    fn make_snippet_pads_and_bounds() {
        // 命中在开头：无前缀省略号
        let s = make_snippet("爱情故事在这里", "爱情");
        assert!(!s.starts_with('…'), "命中在开头不应有前缀 …：{s}");
        assert!(s.contains("爱情"));

        // 命中在结尾：无后缀省略号
        let content = format!("{}爱情", "填".repeat(200));
        let s = make_snippet(&content, "爱情");
        assert!(!s.ends_with('…'), "命中在结尾不应有后缀 …：{s}");
        assert!(s.ends_with("爱情"));

        // 超长中间命中：两侧皆有 …
        let content = format!("{}爱情{}", "前".repeat(200), "后".repeat(200));
        let s = make_snippet(&content, "爱情");
        assert!(
            s.starts_with('…') && s.ends_with('…'),
            "中间命中应两侧 …：{s}"
        );
        assert!(s.contains("爱情"));

        // 长度上界 ≈ 2*SNIPPET_PAD + qlen + 2
        assert!(
            s.chars().count() <= 2 * SNIPPET_PAD + 2 + 2,
            "snippet 长度应有界，实际 {}",
            s.chars().count()
        );

        // 未命中 → 空串
        assert_eq!(make_snippet("abc", "zzz"), "");
    }

    /// E4 核心回归：整词匹配会漏的中文子串必须命中（推翻 M8 的失败场景）。
    #[test]
    fn search_documents_matches_substring_unlike_fulltext() {
        let docs = vec![full_doc(
            "d1",
            "标题不参与检索",
            "ppa_semantic",
            "…书名叫《春天的爱情故事》…",
            Some("txt"),
            None,
        )];

        // ① 整词匹配时「爱情」返回 0；子串扫描必须命中 1 条
        let hits = search_documents(docs.clone(), "爱情", None, 20);
        assert_eq!(hits.len(), 1, "「爱情」应作为子串命中（M8 失败场景）");
        assert_eq!(hits[0].snippet, "…书名叫《春天的爱情故事》…");
        assert_eq!(hits[0].id, "d1");
        assert_eq!(hits[0].source, "ppa_semantic");
        assert_eq!(hits[0].file_type, Some("txt".into()));

        // ② 「春天」同样命中
        assert_eq!(search_documents(docs.clone(), "春天", None, 20).len(), 1);
    }

    /// E4：大小写不敏感（ASCII）；`title` 不参与匹配。
    #[test]
    fn search_documents_is_case_insensitive_and_ignores_title() {
        let docs = vec![full_doc(
            "d1",
            "小C 在标题里",
            "s",
            "正文只有 abc",
            None,
            None,
        )];
        // 小写 q 命中大写正文? 本例正文无 C → 标题有 C 但 title 不参与匹配
        assert!(search_documents(docs.clone(), "小c", None, 20).is_empty());

        let docs2 = vec![full_doc("d1", "无关标题", "s", "正文含小C字样", None, None)];
        assert_eq!(search_documents(docs2, "小c", None, 20).len(), 1);
    }

    /// E4：`source` 精确本地过滤。
    #[test]
    fn search_documents_filters_by_source() {
        let docs = vec![
            full_doc("d1", "t", "ppa_episodic", "爱情", None, None),
            full_doc("d2", "t", "ppa_semantic", "爱情", None, None),
        ];
        let hits = search_documents(docs.clone(), "爱情", Some("ppa_episodic"), 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "d1");

        let none = search_documents(docs, "爱情", Some("ppa_nope"), 20);
        assert!(none.is_empty());
    }

    /// E4：按 `id` 升序；同一文档多命中只 1 条；截断到 `limit`。
    #[test]
    fn search_documents_sorts_dedups_and_truncates() {
        let docs = vec![
            full_doc("zzz", "t", "s", "爱情爱情爱情", None, None),
            full_doc("aaa", "t", "s", "爱情", None, None),
            full_doc("mmm", "t", "s", "爱情", None, None),
        ];

        let hits = search_documents(docs.clone(), "爱情", None, 20);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"], "按 id 升序且每文档仅一条");

        let limited = search_documents(docs, "爱情", None, 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, "aaa");
    }

    /// E4：`build_search_response` 恰 5 键、无 `score`；空 → `{"hits":[]}`。
    #[test]
    fn build_search_response_shape() {
        let hit = MemorySearchHit {
            id: "d1".into(),
            title: "t".into(),
            source: "s".into(),
            file_type: None,
            snippet: "sn".into(),
        };
        let resp = build_search_response(vec![hit.clone()]);
        assert_eq!(resp.hits, vec![hit]);

        let json = serde_json::to_value(&resp).unwrap();
        let obj = json["hits"][0].as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["file_type", "id", "snippet", "source", "title"]);
        assert!(!obj.contains_key("score"), "命中不得包含 score");

        let empty = serde_json::to_value(build_search_response(vec![])).unwrap();
        assert_eq!(empty, serde_json::json!({ "hits": [] }));
    }
}
