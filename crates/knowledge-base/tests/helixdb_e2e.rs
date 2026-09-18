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

use knowledge_base::compute_entity_id;
use knowledge_base::manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig,
};
use knowledge_base::manager::KnowledgeBaseManager;
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
    let stored = kb
        .add_facts(&facts, false)
        .await
        .expect("add_facts 失败");
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
