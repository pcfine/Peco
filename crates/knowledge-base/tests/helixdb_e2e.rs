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
//! 需要一个**一次性** HelixDB 实例（全部 KB 共享同一实例、写入跨运行保留，因此
//! 绝不可指向 `@private_memory` 所在的 6970 生产实例）：
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
    let start_id = kb.entity_id("小C");
    assert!(
        !steps.iter().any(|s| s.node.id == start_id),
        "通配遍历不应把起点自身计入结果"
    );

    // ⑦ 直连邻居的跳数必须恰为 1（而不是降级值 0）。只对 5 个已知直连邻居断言：
    // 实例跨运行保留数据、用例复用同一 KB 命名空间，全称断言会被上一轮留下的两跳节点打挂。
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
        kb.entity_id("chen"),
        "路径终点应是 to 实体"
    );
}

// ============================================================================
// 图删除回归（delete_fact / delete_entity / edges_between）
// ============================================================================

/// 每个用例独占的实体名命名空间。
///
/// HelixDB 实例跨运行保留数据，复用同一 KB 命名空间的用例会互相污染（现有
/// 用例的注释已踩过这个坑：全称断言被上一轮留下的节点打挂）。因此
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
        assert_eq!(before[0].source_id, kb.entity_id(&subj));
        assert_eq!(before[0].target_id, kb.entity_id(&peer));

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
        assert_eq!(peer_id, kb.entity_id(&peer));
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
        assert_eq!(entity_id, kb.entity_id(&ent));
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

// ============================================================================
// 知识库隔离（T3 / T4 / T5 / T5b）
// ============================================================================

/// 具名 KB 配置 —— 名字由调用方给定，好让用例在共享实例上拿到独占命名空间。
fn e2e_config_named(url: &str, name: &str) -> KbConfig {
    KbConfig {
        name: name.to_string(),
        ..e2e_config(url)
    }
}

/// 在既有 manager 上按名建库；名字唯一性由调用方（用例前缀）保证。
async fn create_named_kb(
    manager: &KnowledgeBaseManager,
    url: &str,
    name: &str,
) -> Arc<KnowledgeBase> {
    manager
        .create_kb(e2e_config_named(url, name))
        .await
        .expect("创建 HelixDB KB 失败")
}

/// T3：同 scope 两库写入**相同内容**（内容寻址 ⇒ 相同文档 id）互不删除。
///
/// HelixDB 实例跨运行保留数据，因此 scope 与库名都带用例前缀，
/// 免得历史残留落进本用例的命名空间把断言打偏。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_same_content_in_two_kbs_is_isolated() {
    let url = e2e_helix_url();
    let scope = E2eScope::new("t3");
    let base_dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load_with_scope(base_dir.path(), &scope.prefix)
        .await
        .expect("加载 KB manager 失败");
    let kb_a = create_named_kb(&manager, &url, &scope.name("kba")).await;
    let kb_b = create_named_kb(&manager, &url, &scope.name("kbb")).await;

    // 相同正文 ⇒ 相同内容哈希 ⇒ 相同文档 id —— 隔离若不生效，两库会互相顶掉。
    let content = format!("{} 同内容双库互不删除", scope.prefix);
    let doc_a = kb_a
        .add_text(&scope.name("title_a"), &content, &scope.name("src_a"))
        .await
        .expect("kb_a 写入失败");
    let doc_b = kb_b
        .add_text(&scope.name("title_b"), &content, &scope.name("src_b"))
        .await
        .expect("kb_b 写入失败");
    assert_eq!(doc_a.id, doc_b.id, "相同内容应得到相同文档 id");

    assert!(
        kb_a.get_document(&doc_a.id)
            .await
            .expect("读 kb_a 文档失败")
            .is_some(),
        "kb_b 写入同内容不得顶掉 kb_a 的文档"
    );
    assert!(
        kb_b.get_document(&doc_b.id)
            .await
            .expect("读 kb_b 文档失败")
            .is_some(),
        "kb_b 自己的文档应存在"
    );

    kb_b.remove_document(&doc_b.id)
        .await
        .expect("删 kb_b 文档失败");
    assert!(
        kb_a.get_document(&doc_a.id)
            .await
            .expect("读 kb_a 文档失败")
            .is_some(),
        "删 kb_b 的文档不得波及 kb_a"
    );
    assert!(
        kb_b.get_document(&doc_b.id)
            .await
            .expect("读 kb_b 文档失败")
            .is_none(),
        "kb_b 的文档应已删除"
    );
}

/// T4：跨库同名实体不撞 id、各自新建、事实不跨库泄漏。
///
/// 两库的**主语同名**（对象不同），才能让「id 是否并入 ns」「事实是否只回本库」
/// 都可观测 —— 若 id 撞车，`node_exists` 会让后写的库静默跳过建点。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_same_entity_name_across_kbs_does_not_collide() {
    let url = e2e_helix_url();
    let scope = E2eScope::new("t4");
    let base_dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load_with_scope(base_dir.path(), &scope.prefix)
        .await
        .expect("加载 KB manager 失败");
    let kb_a = create_named_kb(&manager, &url, &scope.name("kba")).await;
    let kb_b = create_named_kb(&manager, &url, &scope.name("kbb")).await;

    let subject = scope.name("用户");
    let obj_a = scope.name("obj_a");
    let obj_b = scope.name("obj_b");

    kb_a.add_facts(&[Fact::new(&subject, "has", &obj_a, 1.0)], false)
        .await
        .expect("kb_a 写入失败");
    kb_b.add_facts(&[Fact::new(&subject, "has", &obj_b, 1.0)], false)
        .await
        .expect("kb_b 写入失败");

    assert_ne!(
        kb_a.entity_id(&subject),
        kb_b.entity_id(&subject),
        "同名实体在不同库必须得到不同 id（ns 入哈希）"
    );

    let (id_a, node_a, _) = kb_a.read_entity(&subject).await.expect("读 kb_a 实体失败");
    let (id_b, node_b, _) = kb_b.read_entity(&subject).await.expect("读 kb_b 实体失败");
    assert_eq!(id_a, kb_a.entity_id(&subject));
    assert_eq!(id_b, kb_b.entity_id(&subject));
    assert!(node_a.is_some(), "kb_a 的实体节点应已新建");
    assert!(
        node_b.is_some(),
        "kb_b 的实体节点应已新建（不得因同名同 id 被 node_exists 静默跳过）"
    );

    let names_a: HashSet<String> = kb_a
        .query_entity_facts(&subject, 1)
        .await
        .expect("kb_a 遍历失败")
        .iter()
        .filter_map(|s| s.node.properties.get("name").cloned())
        .collect();
    assert!(
        names_a.contains(&obj_a),
        "kb_a 应能读到自己的事实: {names_a:?}"
    );
    assert!(
        !names_a.contains(&obj_b),
        "kb_a 不得读到 kb_b 的事实: {names_a:?}"
    );

    let names_b: HashSet<String> = kb_b
        .query_entity_facts(&subject, 1)
        .await
        .expect("kb_b 遍历失败")
        .iter()
        .filter_map(|s| s.node.properties.get("name").cloned())
        .collect();
    assert!(
        names_b.contains(&obj_b),
        "kb_b 应能读到自己的事实: {names_b:?}"
    );
    assert!(
        !names_b.contains(&obj_a),
        "kb_b 不得读到 kb_a 的事实: {names_b:?}"
    );

    cleanup_entities(&kb_a, &[&subject, &obj_a]).await;
    cleanup_entities(&kb_b, &[&subject, &obj_b]).await;
}

/// T5：同 scope 两库、以及跨 scope 的同名库，搜索各自只回本库命中。
///
/// 两库正文不同（内容寻址 ⇒ 不同文档 id），命中归属才可判定；查询词取两库共有短语。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_search_stays_within_its_namespace() {
    let url = e2e_helix_url();
    let scope = E2eScope::new("t5");
    let shared = format!("{} 公共检索短语", scope.prefix);

    // ── 同 scope，两库 ──
    let dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load_with_scope(dir.path(), &scope.prefix)
        .await
        .expect("加载 KB manager 失败");
    let kb_a = create_named_kb(&manager, &url, &scope.name("kba")).await;
    let kb_b = create_named_kb(&manager, &url, &scope.name("kbb")).await;

    let doc_a = kb_a
        .add_text(
            &scope.name("title_a"),
            &format!("alpha {} {shared}", scope.prefix),
            &scope.name("src_a"),
        )
        .await
        .expect("写 kb_a 失败");
    let doc_b = kb_b
        .add_text(
            &scope.name("title_b"),
            &format!("beta {} {shared}", scope.prefix),
            &scope.name("src_b"),
        )
        .await
        .expect("写 kb_b 失败");
    assert_ne!(doc_a.id, doc_b.id);

    let hits_a = kb_a.search(&shared, 10).await.expect("kb_a 搜索失败");
    assert!(!hits_a.is_empty(), "kb_a 应检索到自己的命中");
    for h in &hits_a {
        assert_ne!(
            h.document_id, doc_b.id,
            "kb_a 不得回 kb_b 的文档: {hits_a:?}"
        );
    }
    let hits_b = kb_b.search(&shared, 10).await.expect("kb_b 搜索失败");
    assert!(!hits_b.is_empty(), "kb_b 应检索到自己的命中");
    for h in &hits_b {
        assert_ne!(
            h.document_id, doc_a.id,
            "kb_b 不得回 kb_a 的文档: {hits_b:?}"
        );
    }

    // ── 跨 scope：同名 KB、不同工作区 —— 各自只回本库 ──
    let dir1 = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let dir2 = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let mgr1 = KnowledgeBaseManager::load_with_scope(dir1.path(), &format!("{}_u1", scope.prefix))
        .await
        .expect("加载 u1 manager 失败");
    let mgr2 = KnowledgeBaseManager::load_with_scope(dir2.path(), &format!("{}_u2", scope.prefix))
        .await
        .expect("加载 u2 manager 失败");
    let ka = create_named_kb(&mgr1, &url, "@private_memory").await;
    let kb2 = create_named_kb(&mgr2, &url, "@private_memory").await;

    let shared2 = format!("{} 私有记忆短语", scope.prefix);
    let da = ka
        .add_text("u1", &format!("u1 专属 {shared2}"), "u1")
        .await
        .expect("u1 写失败");
    let db = kb2
        .add_text("u2", &format!("u2 专属 {shared2}"), "u2")
        .await
        .expect("u2 写失败");
    assert_ne!(da.id, db.id);

    let ha = ka.search(&shared2, 10).await.expect("u1 搜索失败");
    assert!(!ha.is_empty(), "u1 应检索到自己的命中");
    for h in &ha {
        assert_ne!(h.document_id, db.id, "u1 不得回 u2 的文档: {ha:?}");
    }
    let hb = kb2.search(&shared2, 10).await.expect("u2 搜索失败");
    assert!(!hb.is_empty(), "u2 应检索到自己的命中");
    for h in &hb {
        assert_ne!(h.document_id, da.id, "u2 不得回 u1 的文档: {hb:?}");
    }
}

/// T5b：同一实例里大语料与小 KB 共存时，小 KB 仍能召回自己的文档。
///
/// 大语料清一色是与查询无关的内容，因此这条用例真正能红的是「后置 `ns` 过滤
/// 缺失或选点错」—— 小库命中被判到别的命名空间或被过滤掉时结果为空；全局过采样
/// 倍率给不给都过，不构成覆盖目标。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_small_kb_recall_against_large_corpus() {
    let url = e2e_helix_url();
    let scope = E2eScope::new("t5b");
    let dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load_with_scope(dir.path(), &scope.prefix)
        .await
        .expect("加载 KB manager 失败");
    let kb_big = create_named_kb(&manager, &url, &scope.name("big")).await;
    let kb_small = create_named_kb(&manager, &url, &scope.name("small")).await;

    // 大语料：50 条短文档，内容刻意不含 needle，用来占满全局检索名次。
    for i in 0..50 {
        let content = format!(
            "{} 大语料第 {i} 条：这是一段与目标查询无关的普通文本，仅用于占据检索名次。",
            scope.prefix
        );
        kb_big
            .add_text(&scope.name("big"), &content, &scope.name("big_src"))
            .await
            .expect("写大语料失败");
    }

    let needle = format!("{}needle", scope.prefix);
    let small_doc = kb_small
        .add_text(
            &scope.name("small"),
            &format!("{needle} 小库独有的检索目标"),
            &scope.name("small_src"),
        )
        .await
        .expect("写小库失败");

    let hits = kb_small.search(&needle, 5).await.expect("小库搜索失败");
    assert!(
        !hits.is_empty(),
        "小库应能召回自己的文档（后置 ns 过滤缺失或选点错会让它变空）"
    );
    for h in &hits {
        assert_eq!(
            h.document_id, small_doc.id,
            "小库检索不得回大语料的文档: {hits:?}"
        );
    }
}

/// 多分块文档：一段长文本切成多个分块后仍能检索命中。
///
/// 其余 e2e 文档都只有 1 个分块，`create_next_chunk_edge` 的 `chunks.windows(2)`
/// 循环从不执行 —— 这条用例补上多分块（`NEXT_CHUNK`）路径的运行时覆盖。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_multi_chunk_document_is_searchable() {
    let url = e2e_helix_url();
    let scope = E2eScope::new("chunks");
    let dir = tempfile::tempdir().expect("创建临时 KB 目录失败");
    let manager = KnowledgeBaseManager::load_with_scope(dir.path(), &scope.prefix)
        .await
        .expect("加载 KB manager 失败");
    let kb = create_named_kb(&manager, &url, &scope.name("kb")).await;

    let needle = format!("{}needle", scope.prefix);
    // 80 行、每行 40+ 字符 ⇒ 远超 1700 字符，按 800/200 的窗口必然切出多个分块。
    let mut content = String::new();
    for i in 0..80 {
        content.push_str(&format!("第{i}行：{needle} 多分块填充文本。"));
    }
    assert!(
        content.chars().count() > 1700,
        "构造的长文本应超过 1700 字符"
    );

    let before = kb.stats().await.expect("读取统计失败").chunk_count;
    let doc = kb
        .add_text(&scope.name("title"), &content, &scope.name("src"))
        .await
        .expect("写入多分块文档失败");
    let after = kb.stats().await.expect("读取统计失败").chunk_count;
    assert!(
        after - before >= 3,
        "长文本应切成 ≥3 个分块（写入前 {before}，写入后 {after}）"
    );

    let hits = kb.search(&needle, 5).await.expect("搜索失败");
    assert!(!hits.is_empty(), "多分块文档应能检索命中");
    for h in &hits {
        assert_eq!(h.document_id, doc.id, "命中应归属本文档: {hits:?}");
    }
}

/// 事实谓词恰与 `schema.related_edge`（默认 `RELATED_TO`）同名时，边仍应建成。
///
/// `add_edges` 曾按标签字符串分派：谓词等于 `related_edge` 就走 RELATED_TO 构造器，
/// 而该构造器把两端钉成 `$label=Document` ⇒ 实体端点选空 ⇒ 建边失败。谓词是模型
/// 自由文本，撞上保留标签并不罕见，因此端点选点必须与 label 无关。
#[tokio::test]
#[ignore = "需要运行中的 HelixDB 实例（见文件头注释的起实例命令）"]
async fn e2e_fact_predicate_equal_to_related_edge_label() {
    with_e2e_kb(|kb| async move {
        let scope = E2eScope::new("relpred");
        let subj = scope.name("subj");
        let peer = scope.name("peer");

        kb.add_facts(&[Fact::new(&subj, "RELATED_TO", &peer, 1.0)], false)
            .await
            .expect("谓词为 RELATED_TO 的事实应能写入");

        let edges = kb
            .read_fact(&subj, "RELATED_TO", &peer)
            .await
            .expect("read_fact 失败");
        assert_eq!(edges.len(), 1, "谓词 RELATED_TO 的边应存在");
        assert_eq!(edges[0].source_id, kb.entity_id(&subj));
        assert_eq!(edges[0].target_id, kb.entity_id(&peer));

        cleanup_entities(&kb, &[&subj, &peer]).await;
    })
    .await;
}
