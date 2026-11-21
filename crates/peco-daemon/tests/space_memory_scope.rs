//! 空间私有记忆：A2 互不可见、A3 工具面只看到本空间的库、A5 存量迁移。
//!
//! 三个用例都从「一个空间一份物理记忆库」出发：读的是哪一份，决定谁能看见什么。

mod support;

use std::sync::Arc;
use std::time::Duration;

use knowledge_base::{
    BackendType, ChunkingStrategySerde, DocumentMetadata, FastembedModelTypeSerde, KbConfig,
};
use peco_core::agent::DynamicContext;
use peco_core::knowledge::KnowledgeManager;
use peco_core::tools::KnowledgeAccess;
use peco_daemon::memory::WS_ID_TAG;
use peco_daemon::memory::config::MemoryConfig;
use peco_daemon::memory::recall::MemoryRecallContext;
use serde_json::json;
use support::{TestEnv, new_env};

const MEMORY_KB: &str = "@private_memory";

fn in_memory_kb(name: &str) -> KbConfig {
    KbConfig {
        name: name.to_string(),
        description: "测试记忆库".into(),
        embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
        chunking: ChunkingStrategySerde::FixedSize { size: 100 },
        backend: BackendType::InMemory,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    }
}

/// 空间自己的记忆管理器。
async fn memory_km(env: &TestEnv, ws_id: &str) -> Arc<KnowledgeManager> {
    let handle = env.open(ws_id).await.expect("空间应已打开");
    handle
        .workspace()
        .memory_manager()
        .cloned()
        .expect("打开的空间必须带记忆管理器")
}

/// 往空间记忆库写一条记忆（带空间标签，与写路径同形）。
async fn write_memory(km: &Arc<KnowledgeManager>, ws_id: &str, text: &str) {
    let mut metadata = DocumentMetadata::default();
    metadata
        .extra
        .insert(WS_ID_TAG.to_string(), ws_id.to_string());
    km.add_text_to_kb_with_metadata(MEMORY_KB, "memory", text, "ppa_semantic", metadata)
        .await
        .expect("写入空间记忆库");
}

// ============================================================================
// A2：两个空间互不可见
// ============================================================================

/// A 写过记忆，B 用同一句查询召回 ⇒ B 什么都召回不到，A 召回得到。
///
/// 变异：把 `MemoryRecallContext` 换回团队管理器 → B 会召回 A 的记忆，本用例变红。
#[tokio::test]
async fn a_memory_written_in_one_space_is_invisible_to_another() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("scope-a", "research").await;
    let ws_b = env.open_space("scope-b", "research").await;

    let a_km = memory_km(&env, &ws_a).await;
    write_memory(&a_km, &ws_a, "用户的主开发语言是 Rust，长期维护 daemon").await;

    let config = MemoryConfig::default();
    let from_a = MemoryRecallContext::new(a_km, config.clone(), ws_a.clone());
    let recalled = from_a.query("Rust").await;
    assert!(
        recalled
            .as_deref()
            .is_some_and(|text| text.contains("Rust")),
        "前提：本空间必须召回得到自己的记忆，实际 {recalled:?}"
    );

    let from_b = MemoryRecallContext::new(memory_km(&env, &ws_b).await, config, ws_b.clone());
    let foreign = from_b.query("Rust").await;
    assert!(
        foreign.is_none(),
        "别的空间不得召回我的记忆，实际 {foreign:?}"
    );
}

// ============================================================================
// A3：工具面只看到本空间的库
// ============================================================================

/// 写一个带指定工具与知识库白名单的 `@master`（模型不会被调用，provider 只是占位）。
fn install_master(env: &TestEnv, team: &str, tools: &[&str], kbs: &[&str]) {
    let root = env.home.join("teams").join(team);
    std::fs::create_dir_all(root.join("agents/@master")).unwrap();
    std::fs::write(
        root.join("providers.toml"),
        "default_provider = \"fake\"\n\n\
         [providers.fake]\n\
         type = \"openai\"\n\
         api = \"chat\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         api_key = \"test\"\n\n\
         [providers.fake.default]\n\
         model = \"unused\"\n",
    )
    .unwrap();
    let tool_lines: String = tools.iter().map(|tool| format!("  - {tool}\n")).collect();
    // `@private_memory` 以 `@` 开头，YAML 的保留指示符，必须加引号。
    let kb_lines: String = kbs.iter().map(|kb| format!("  - \"{kb}\"\n")).collect();
    std::fs::write(
        root.join("agents/@master/agent.md"),
        format!(
            "---\nagent:\n  name: \"@master\"\n  description: \"记忆路由夹具\"\n\
             llm:\n  provider: \"fake\"\n  model: \"unused\"\n  temperature: 0.0\n\
             tools:\n{tool_lines}knowledge_bases:\n{kb_lines}---\n"
        ),
    )
    .unwrap();
}

/// 空间内 agent 的 `list_knowledge_bases` 只出现本空间的 `@private_memory`。
///
/// 团队目录里没有同名库（模板已不再铺），因此列表里出现它只可能来自记忆管理器的
/// 合并；变异：去掉 `memory_manager` 的注入/合并 → 列表里没有 `@private_memory`。
#[tokio::test]
async fn the_agent_sees_exactly_its_own_memory_kb() {
    let env = new_env(&["research"]).await;
    install_master(&env, "research", &["list_knowledge_bases"], &[MEMORY_KB]);
    let ws_a = env.open_space("tool-a", "research").await;
    let ws_b = env.open_space("tool-b", "research").await;

    // 前提：团队目录里确实没有团队级记忆库。
    assert!(
        !env.home
            .join("teams/research/knowledge/@private_memory/kb_config.json")
            .exists(),
        "模板不再铺团队级记忆库"
    );
    // A 的记忆库有内容，B 的是空的：两边都该出现在各自的列表里。
    write_memory(&memory_km(&env, &ws_a).await, &ws_a, "用户偏好简洁的回答").await;

    for ws_id in [&ws_a, &ws_b] {
        let handle = env.open(ws_id).await.expect("空间应已打开");
        let master = handle.master_agent().expect("加载 @master");
        let output = master
            .tool_executor()
            .execute("list_knowledge_bases", "{}")
            .await
            .expect("执行 list_knowledge_bases");
        let text = format!("{output:?}");
        assert!(
            text.contains(MEMORY_KB),
            "空间 {ws_id} 的列表里必须有本空间的记忆库，实际 {text}"
        );
        let parsed: Vec<serde_json::Value> = match output {
            peco_core::tools::Content::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("列表工具应回文本，实际 {other:?}"),
        };
        let names: Vec<&str> = parsed
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![MEMORY_KB],
            "空间 {ws_id} 只该看到自己的记忆库（团队目录里没有同名库）"
        );
    }
}

// ============================================================================
// A5：存量迁移
// ============================================================================

/// 团队旧库里 `ws_id` 名下的三条文档 ⇒ 搬进空间新库、写凭据、团队库摘牌。
///
/// 迁移函数直接跑在那份「已经打开的团队管理器/空间记忆管理器」上：InMemory 后端
/// 的记忆库是进程实例级的，另建实例读到的是空库，那样只会验证到「读到 0 条」。
/// 「打开空间会触发它」由 `migrated.json` 在打开后出现来钉住。
///
/// 变异：把迁移写进团队库而不是空间库 → 新库仍是 0 条；不写 `migrated.json` →
/// 凭据断言变红；不摘牌 → `.disabled` 断言变红。
#[tokio::test]
async fn legacy_team_memories_move_into_the_space_library_once() {
    let env = new_env(&["research"]).await;
    let team_root = env.home.join("teams/research");
    let work_dir = env.work_dir("legacy");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    let handle = env.open(&ws_id).await.expect("空间应已打开");

    // 打开时就跑过一次迁移（此刻团队里还没有旧库）：凭据写下，说明触发点接对了。
    let marker = env.home.join("memory").join(&ws_id).join("migrated.json");
    wait_for(&marker, "迁移凭据").await;
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(recorded["from"], json!("team"));
    assert_eq!(recorded["count"], json!(0), "当时旧库还不存在");

    // 构造存量：团队级旧库 + 三条属于本空间（一条属于别人）的记忆。
    let memory = handle
        .workspace()
        .memory_manager()
        .cloned()
        .expect("必须注入记忆管理器");
    let team_km = Arc::clone(handle.workspace().knowledge_manager());
    team_km
        .create_kb(in_memory_kb(MEMORY_KB))
        .await
        .expect("建出团队级旧记忆库");
    for (index, text) in [
        "用户的主开发语言是 Rust",
        "周五发布 v2",
        "用户偏好简洁的回答",
    ]
    .iter()
    .enumerate()
    {
        let mut metadata = DocumentMetadata::default();
        metadata.extra.insert(WS_ID_TAG.to_string(), ws_id.clone());
        team_km
            .add_text_to_kb_with_metadata(
                MEMORY_KB,
                &format!("legacy_{index}"),
                text,
                "ppa_semantic",
                metadata,
            )
            .await
            .expect("写入团队旧库");
    }
    let mut foreign = DocumentMetadata::default();
    foreign
        .extra
        .insert(WS_ID_TAG.to_string(), "ws_other".to_string());
    team_km
        .add_text_to_kb_with_metadata(
            MEMORY_KB,
            "legacy_other",
            "别人的记忆",
            "ppa_semantic",
            foreign,
        )
        .await
        .unwrap();

    // 凭据存在时迁移会跳过；删掉它重跑，正是「上次没搬完，下次继续」的路径。
    std::fs::remove_file(&marker).unwrap();
    let report = peco_daemon::space::open::migrate_legacy_memory(
        &env.env,
        &team_km,
        &memory,
        &env.home.join("memory").join(&ws_id),
        &ws_id,
        "research",
    )
    .await;
    assert_eq!(report.found, 3, "只搬本空间的文档");
    assert_eq!(report.written, 3);
    assert_eq!(report.failed, 0);

    let docs = memory
        .list_memory_documents_with_content(MEMORY_KB, 100)
        .await
        .expect("列出新库");
    assert_eq!(docs.len(), 3, "三条存量记忆必须搬进新库: {docs:?}");
    assert!(
        !docs.iter().any(|doc| doc.content.contains("别人的")),
        "别的空间的记忆不得搬进来"
    );
    assert!(
        docs.iter()
            .all(|doc| !doc.metadata.extra.contains_key(WS_ID_TAG)),
        "新库天然只含本空间，迁移不再保留归属标签"
    );

    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(recorded["count"], json!(3));
    assert!(
        recorded["at"].as_str().is_some_and(|at| !at.is_empty()),
        "凭据要带迁移时刻: {recorded}"
    );

    // 团队级记忆库摘牌：数据不动，改回名字即回滚。
    let disabled = find_disabled_kb_configs(&team_root.join("knowledge"));
    assert_eq!(
        disabled.len(),
        1,
        "迁移完成后团队级记忆库必须摘牌: {disabled:?}"
    );
    assert!(
        team_root
            .join("knowledge")
            .read_dir()
            .unwrap()
            .flatten()
            .all(|entry| !entry.path().join("kb_config.json").is_file()),
        "摘牌后团队管理器再也扫不到这个库"
    );
}

/// 团队知识根下被摘牌的库配置（`kb_config.json.disabled`）。
fn find_disabled_kb_configs(knowledge_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(knowledge_root) else {
        return found;
    };
    for entry in entries.flatten() {
        let config = entry.path().join("kb_config.json.disabled");
        if config.is_file() {
            found.push(config);
        }
    }
    found
}

/// 轮询等一个文件出现（迁移是异步的）。
async fn wait_for(path: &std::path::Path, label: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "10s 内必须出现{label}: {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
