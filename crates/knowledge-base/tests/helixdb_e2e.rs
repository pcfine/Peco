//! HelixDB 后端端到端回归测试（需真实实例，默认 `#[ignore]`）。
//!
//! 覆盖 `add_facts → query_entity_facts / query_relation_path` 的完整链路，
//! 回归 ⑤⑥⑦ 三个缺陷：
//!
//! * ⑤ 遍历返回的 `node.id` 必须是稳定身份（`entity:Entity:<hash>`），
//!   而不是内部自增整数 —— 否则 `query_relation_path` 恒为 `None`；
//! * ⑥ 遍历结果携带 `properties["name"]`，调用方能读回实体名；
//! * ⑦ 通配遍历不把起点自身计入结果。
//!
//! # 运行方式
//!
//! 需要一个**一次性** HelixDB 实例（HelixDB 无 per-KB 隔离，全部 KB 共享同一
//! schema/实例，因此绝不可指向 `@private_memory` 所在的 6970 生产实例）：
//!
//! ```bash
//! # 在 /tmp 起一个 in-memory 一次性实例（端口 6971）
//! cd /tmp && rm -rf helix-e2e && mkdir helix-e2e && cd helix-e2e
//! helix init local --no-skills --quiet
//! printf '[project]\nname = "helix-e2e"\nqueries = "db"\ncontainer_runtime = "docker"\n\n[local.e2e-test]\nport = 6971\nimage = "ghcr.io/helixdb/enterprise-dev"\ntag = "latest"\n\n[enterprise]\n' > helix.toml
//! helix start e2e-test --quiet
//!
//! # 运行测试
//! PECO_KB_HELIX_URL=http://127.0.0.1:6971 \
//!   cargo test -p knowledge-base --features helixdb --test helixdb_e2e -- --ignored --nocapture
//! ```
//!
//! 若未设置 `PECO_KB_HELIX_URL`，回退到 `http://127.0.0.1:6971`。

#![cfg(feature = "helixdb")]

use std::collections::HashSet;
use std::sync::Arc;

use knowledge_base::compute_entity_id;
use knowledge_base::manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig,
};
use knowledge_base::manager::{KnowledgeBase, KnowledgeBaseManager};
use knowledge_base::traits::EdgeType;
use knowledge_base::types::{Fact, StorageMode};

/// 与 `resolve_helix_url` 相同的回退链，但测试默认指向一次性实例 6971
/// 而非生产 6970，避免误写入用户数据。
const DEFAULT_E2E_HELIX_URL: &str = "http://127.0.0.1:6971";

fn e2e_helix_url() -> String {
    std::env::var("PECO_KB_HELIX_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_E2E_HELIX_URL.to_string())
}

/// 测试用 KB 配置 —— HelixDB 后端 + `bge-base-zh-v1.5`（缓存模型，离线可用）。
fn e2e_config(url: &str) -> KbConfig {
    KbConfig {
        name: "e2e-helixdb".to_string(),
        description: "HelixDB 图后端端到端回归".to_string(),
        embedding_model: FastembedModelTypeSerde::BGEBaseZHV15,
        chunking: ChunkingStrategySerde::OverlappingWindow {
            size: 800,
            overlap: 200,
        },
        backend: BackendType::HelixDb,
        storage_path: None,
        default_storage_mode: StorageMode::Full,
        helix_url: Some(url.to_string()),
    }
}

/// ⑤⑥⑦：`add_facts` 写入的 5 条事实，经 `query_entity_facts` 必须读回
/// 稳定 id、实体名与谓词；`query_relation_path` 必须能找到路径。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_add_facts_query_entity_facts_and_relation_path() {
    let url = e2e_helix_url();

    // 独立临时目录，避免在仓库目录留下任何 kb_config.json / 数据。
    let base_dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load(base_dir.path())
        .await
        .expect("加载 KB manager 失败");
    let kb = manager
        .create_kb(e2e_config(&url))
        .await
        .expect("创建 HelixDB KB 失败");

    let facts = vec![
        Fact::new("小C", "朋友", "chen", 1.0),
        Fact::new("小C", "性别", "男", 1.0),
        Fact::new("小C", "年龄", "30", 1.0),
        Fact::new("小C", "喜欢", "美女", 1.0),
        Fact::new("小C", "作者", "春天的爱情故事", 1.0),
    ];
    let stored = kb.add_facts(&facts, false).await.expect("add_facts 失败");
    assert_eq!(stored.len(), 5, "5 条事实应全部写入");

    // ── query_entity_facts ──
    let steps = kb
        .query_entity_facts("小C", 2)
        .await
        .expect("query_entity_facts 失败");
    assert!(
        !steps.is_empty(),
        "query_entity_facts 应返回邻接事实，实际为空"
    );

    // ⑤ 每个 node.id 都是稳定身份，不是内部自增整数。
    for step in &steps {
        assert!(
            step.node.id.starts_with("entity:Entity:"),
            "遍历节点 id 应是稳定哈希形态，实际为 {:?}",
            step.node.id
        );
    }

    // ⑥ 实体名读回。
    let names: HashSet<String> = steps
        .iter()
        .filter_map(|s| s.node.properties.get("name").cloned())
        .collect();
    for expected in ["chen", "男", "30", "美女", "春天的爱情故事"] {
        assert!(
            names.contains(expected),
            "读回的实体名缺少 {expected:?}，实际 {names:?}"
        );
    }

    // 谓词（via_edge）读回 —— 直连边应带真实标签。
    let predicates: HashSet<String> = steps
        .iter()
        .filter_map(|s| s.via_edge.as_ref().map(EdgeType::as_label))
        .map(str::to_string)
        .collect();
    for expected in ["朋友", "性别", "年龄", "喜欢", "作者"] {
        assert!(
            predicates.contains(expected),
            "读回的谓词缺少 {expected:?}，实际 {predicates:?}"
        );
    }

    // ⑦ 起点自身不计入结果。
    let start_id = compute_entity_id("小C", "Entity");
    assert!(
        !steps.iter().any(|s| s.node.id == start_id),
        "通配遍历不应把起点自身计入结果"
    );

    // ⑦ 直连邻居的跳数必须恰为 1（而不是降级值 0）。只对 5 个已知直连邻居断言：
    // HelixDB 无 per-KB 隔离且实例跨运行保留，全称断言会被上一轮留下的两跳节点打挂。
    for expected in ["chen", "男", "30", "美女", "春天的爱情故事"] {
        let step = steps
            .iter()
            .find(|s| s.node.properties.get("name").map(String::as_str) == Some(expected))
            .unwrap_or_else(|| panic!("直连邻居 {expected:?} 应出现在结果里"));
        assert_eq!(
            step.node.distance, 1,
            "直连邻居 {expected:?} 距离应为 1，实际 {}",
            step.node.distance
        );
    }

    // ⑦ 两跳节点的距离应为 2 —— 由「节点首次出现在第几层」推出，不是降级值。
    // HelixDB 的 `$distance` 只在搜索命中上可用，遍历路径拿不到，因此这里验证
    // 分层遍历（d1..dN）的层号确实换成了精确跳数。
    kb.add_facts(&[Fact::new("chen", "朋友", "老王", 1.0)], false)
        .await
        .expect("写入两跳事实失败");
    let steps = kb
        .query_entity_facts("小C", 2)
        .await
        .expect("query_entity_facts 失败");
    let two_hop = steps
        .iter()
        .find(|s| s.node.properties.get("name").map(String::as_str) == Some("老王"))
        .expect("两跳实体「老王」应出现在结果里");
    assert_eq!(two_hop.node.distance, 2, "两跳节点距离应为 2");

    // ── query_relation_path（⑤ 的关键回归：恒 None 才是 bug）──
    let path = kb
        .query_relation_path("小C", "chen")
        .await
        .expect("query_relation_path 失败");
    assert!(
        path.is_some(),
        "query_relation_path(\"小C\", \"chen\") 应返回 Some(path)"
    );
    let path = path.unwrap();
    assert!(!path.is_empty());
    assert_eq!(path[0].node.id, start_id, "路径起点应是 from 实体");
    assert_eq!(
        path.last().unwrap().node.id,
        compute_entity_id("chen", "Entity"),
        "路径终点应是 to 实体"
    );
}

// ============================================================================
// 图删除回归（delete_fact / delete_entity / edges_between）
// ============================================================================

/// 每个用例独占的实体名命名空间。
///
/// HelixDB 实例跨运行保留数据、且无 per-KB 隔离，用例之间会互相污染 ——
/// 现有用例的注释已经踩过这个坑（全称断言被上一轮留下的节点打挂）。因此
/// 所有实体名都带「用例标签 + 进程号 + 纳秒」后缀：即便某次运行在断言处
/// panic、清理没跑完，残留数据也不会被后续运行读到。
struct E2eScope {
    prefix: String,
}

impl E2eScope {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时钟早于 UNIX 纪元")
            .as_nanos();
        Self {
            prefix: format!("e2e_del_{tag}_{}_{nanos}", std::process::id()),
        }
    }

    /// 生成一个本用例独占的实体名。
    fn name(&self, local: &str) -> String {
        format!("{}_{local}", self.prefix)
    }
}

/// 用独立 KB 跑一段用例主体的公共脚手架。
async fn with_e2e_kb<F, Fut>(f: F)
where
    F: FnOnce(Arc<KnowledgeBase>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let url = e2e_helix_url();
    let base_dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load(base_dir.path())
        .await
        .expect("加载 KB manager 失败");
    let kb = manager
        .create_kb(e2e_config(&url))
        .await
        .expect("创建 HelixDB KB 失败");
    f(kb).await;
}

/// 清理本用例创建的实体（级联带走它们的边）。
///
/// 失败只忽略 —— 清理不是断言目标，实体名已带纳秒后缀，残留不会被后续运行读到。
async fn cleanup_entities(kb: &Arc<KnowledgeBase>, names: &[&str]) {
    for name in names {
        let _ = kb.delete_entity(name, true).await;
    }
}

/// `delete_fact` 只删指定方向的那条边：其余事实完好，对端实体节点存活。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_delete_fact_removes_only_that_edge() {
    with_e2e_kb(|kb| async move {
        let scope = E2eScope::new("fact");
        let subj = scope.name("subj");
        let peer = scope.name("peer");
        let other = scope.name("other");

        kb.add_facts(
            &[
                Fact::new(&subj, "朋友", &peer, 0.95),
                Fact::new(&subj, "年龄", &other, 0.9),
            ],
            false,
        )
        .await
        .expect("add_facts 失败");

        // 先读：命中且带真实 weight（HelixDB 的写响应不报告删除数，存在性只能靠读）
        let before = kb
            .read_fact(&subj, "朋友", &peer)
            .await
            .expect("read_fact 失败");
        assert_eq!(before.len(), 1, "删除前应恰好读到一条边");
        assert!(
            (before[0].weight - 0.95).abs() < 1e-6,
            "读回的边应携带真实 weight，实际 {}",
            before[0].weight
        );
        assert_eq!(before[0].source_id, compute_entity_id(&subj, "Entity"));
        assert_eq!(before[0].target_id, compute_entity_id(&peer, "Entity"));

        assert_eq!(
            kb.delete_fact(&subj, "朋友", &peer)
                .await
                .expect("delete_fact 失败"),
            1
        );

        // 删掉的那条没了，另一条完好
        assert!(
            kb.read_fact(&subj, "朋友", &peer)
                .await
                .expect("read_fact 失败")
                .is_empty(),
            "被删的边不应再读到"
        );
        assert_eq!(
            kb.read_fact(&subj, "年龄", &other)
                .await
                .expect("read_fact 失败")
                .len(),
            1,
            "未涉及的边不应受影响"
        );

        // 对端实体节点存活（孤儿语义：删边不删端点）
        let (peer_id, peer_node, _) = kb.read_entity(&peer).await.expect("read_entity 失败");
        assert_eq!(peer_id, compute_entity_id(&peer, "Entity"));
        assert!(peer_node.is_some(), "对端实体节点应存活");

        // 遍历结果里 peer 不再是 subj 的邻居，other 仍是
        let steps = kb
            .query_entity_facts(&subj, 1)
            .await
            .expect("query_entity_facts 失败");
        let neighbours: HashSet<String> = steps
            .iter()
            .filter_map(|s| s.node.properties.get("name").cloned())
            .collect();
        assert!(
            !neighbours.contains(&peer),
            "peer 不应再是邻居: {neighbours:?}"
        );
        assert!(
            neighbours.contains(&other),
            "other 应仍是邻居: {neighbours:?}"
        );

        cleanup_entities(&kb, &[&subj, &peer, &other]).await;
    })
    .await;
}

/// 删除是**有向**的：`a -[p]-> b` 删掉后，`b -[q]-> a` 必须完好。
///
/// HelixDB 的 `DropEdgeLabeled` 只匹配出边，这条断言把「反向边被误删」钉死。
/// 同时验证「不存在的事实」在本层是可观测的：`read_fact` 为空、`delete_fact`
/// 返回 0（工具层据此报 `Fact not found`，而不是静默成功 —— 见 peco-core 的
/// `delete_entity_fact_missing_fact_rejected_without_audit_row`）。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_delete_fact_is_directed_and_not_idempotent() {
    with_e2e_kb(|kb| async move {
        let scope = E2eScope::new("directed");
        let a = scope.name("a");
        let b = scope.name("b");

        kb.add_facts(
            &[
                Fact::new(&a, "朋友", &b, 0.9),
                Fact::new(&b, "同事", &a, 0.4),
                // 同一对端点、不同谓词 —— 实测确认 `OutE: "<label>"` 不按标签过滤
                // 边流，这条用来钉死「按谓词删除」不会误伤同对端点的其他谓词。
                Fact::new(&a, "同事", &b, 0.7),
            ],
            false,
        )
        .await
        .expect("add_facts 失败");

        assert_eq!(
            kb.delete_fact(&a, "朋友", &b)
                .await
                .expect("delete_fact 失败"),
            1,
            "只有「朋友」这一条应计入删除数"
        );

        // 同对端点、不同谓词的那条必须完好
        let sibling = kb.read_fact(&a, "同事", &b).await.expect("read_fact 失败");
        assert_eq!(sibling.len(), 1, "同对端点的其他谓词不得被连带删除");
        assert!(
            (sibling[0].weight - 0.7).abs() < 1e-6,
            "同对端点边的 weight 应原样，实际 {}",
            sibling[0].weight
        );

        // 反向边完好
        let reverse = kb.read_fact(&b, "同事", &a).await.expect("read_fact 失败");
        assert_eq!(reverse.len(), 1, "反向边不得被连带删除");
        assert!(
            (reverse[0].weight - 0.4).abs() < 1e-6,
            "反向边 weight 应原样，实际 {}",
            reverse[0].weight
        );

        // 重复删除 → 0（非幂等：本层能观测到「没删到东西」）
        assert_eq!(
            kb.delete_fact(&a, "朋友", &b)
                .await
                .expect("delete_fact 失败"),
            0,
            "第二次删除应返回 0"
        );

        // 从未存在的事实：读为空、删为 0 —— 工具层的「先读」正是建立在这个信号上
        let ghost = scope.name("ghost");
        assert!(
            kb.read_fact(&a, "朋友", &ghost)
                .await
                .expect("read_fact 失败")
                .is_empty()
        );
        assert_eq!(
            kb.delete_fact(&a, "朋友", &ghost)
                .await
                .expect("delete_fact 失败"),
            0
        );

        cleanup_entities(&kb, &[&a, &b, &ghost]).await;
    })
    .await;
}

/// `delete_entity(cascade: false)` 在实体仍有边时被拒；`cascade: true` 才删，
/// 且级联**双向**带走关联边、对端实体存活。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_delete_entity_requires_cascade() {
    with_e2e_kb(|kb| async move {
        let scope = E2eScope::new("entity");
        let ent = scope.name("ent");
        let peer = scope.name("peer");

        kb.add_facts(
            &[
                Fact::new(&ent, "朋友", &peer, 0.9),
                Fact::new(&ent, "年龄", &peer, 0.8),
            ],
            false,
        )
        .await
        .expect("add_facts 失败");

        // 先从对端建一条指向 ent 的反向边，验证级联的双向性
        kb.add_facts(&[Fact::new(&peer, "同事", &ent, 0.5)], false)
            .await
            .expect("add_facts 失败");

        // cascade: false + 有残留边 → 拒绝，且什么都没动
        let err = kb
            .delete_entity(&ent, false)
            .await
            .expect_err("有残留边时必须拒绝");
        assert!(
            matches!(err, knowledge_base::KnowledgeError::InvalidInput(_)),
            "应为 InvalidInput，实际 {err:?}"
        );
        assert_eq!(
            kb.read_fact(&ent, "朋友", &peer)
                .await
                .expect("read_fact 失败")
                .len(),
            1,
            "拒绝时不得动边"
        );

        // 删除前的双向快照（工具层把它写进审计，回滚据此重建）
        let (entity_id, node, edges) = kb.read_entity(&ent).await.expect("read_entity 失败");
        assert_eq!(entity_id, compute_entity_id(&ent, "Entity"));
        assert!(node.is_some(), "实体节点应存在");
        assert_eq!(edges.len(), 3, "两条出边 + 一条入边，实际 {edges:?}");

        // cascade: true → 边与节点俱删
        let (removed_edges, removed_nodes) = kb
            .delete_entity(&ent, true)
            .await
            .expect("delete_entity 失败");
        assert_eq!(removed_edges, 3, "两条出边 + 一条入边");
        assert_eq!(removed_nodes, 1);

        assert!(
            kb.read_fact(&ent, "朋友", &peer)
                .await
                .expect("read_fact 失败")
                .is_empty()
        );
        assert!(
            kb.read_fact(&peer, "同事", &ent)
                .await
                .expect("read_fact 失败")
                .is_empty(),
            "反向边应被级联带走"
        );

        let (_, ent_node, _) = kb.read_entity(&ent).await.expect("read_entity 失败");
        assert!(ent_node.is_none(), "节点应已删除");
        let (_, peer_node, _) = kb.read_entity(&peer).await.expect("read_entity 失败");
        assert!(peer_node.is_some(), "对端实体应存活");

        cleanup_entities(&kb, &[&ent, &peer]).await;
    })
    .await;
}
