//! 磁盘 → DB 索引、团队级缓存失效扇出、以及「索引行已陈旧」的视图过滤。

mod support;

use peco_core::workspace::WorkSpace;
use serde_json::json;
use support::new_env;

/// 最小可用 `providers.toml`：构造 Agent 需要一条能解析的 provider 条目。
/// 指向不可达地址即可 —— 这些测试只加载配置，不发请求。
const PROVIDERS_TOML: &str = r#"
default_provider = "test"

[providers.test]
type = "deepseek"
api_key = "sk-test-local"
base_url = "http://127.0.0.1:9"

[providers.test.default]
model = "test-model"
"#;

fn agent_md(name: &str, description: &str, body: &str) -> String {
    format!("---\nagent:\n  name: \"{name}\"\n  description: \"{description}\"\n---\n\n{body}\n")
}

/// 往团队能力根里写一个 agent 目录（绕开 HTTP，模拟模板安装/外部编辑器）。
fn write_agent(env: &support::TestEnv, team: &str, name: &str, body: &str) {
    let dir = env.home.join("teams").join(team).join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.md"),
        agent_md(name, "written on disk", body),
    )
    .unwrap();
}

fn agent_dir(env: &support::TestEnv, team: &str, name: &str) -> std::path::PathBuf {
    env.home.join("teams").join(team).join("agents").join(name)
}

/// 团队级 provider 配置：`UserConfig::load` 从能力根读它。
fn write_providers(env: &support::TestEnv, team: &str) {
    let dir = env.home.join("teams").join(team);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("providers.toml"), PROVIDERS_TOML).unwrap();
}

async fn count_agent_rows(env: &support::TestEnv, name: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM agents WHERE name = ?")
        .bind(name)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

fn listed_names(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .expect("列表必须是数组")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

async fn open_workspace(env: &support::TestEnv, ws_id: &str) -> std::sync::Arc<WorkSpace> {
    env.open(ws_id).await.expect("空间必须已打开").workspace()
}

/// 只在磁盘上存在（库里没有索引行）的 agent 必须出现在 `GET /api/agents`。
///
/// 变异方法：删掉 `list` 里的 `index::ensure_agents_indexed` 调用 —— 模板安装/
/// 手工拷贝进来的 agent 从列表里消失，本测试变红。
#[tokio::test]
async fn disk_agent_is_listed_without_a_db_row() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    let ws_id = env.open_space("alpha", "research").await;
    assert_eq!(
        count_agent_rows(&env, "newbie").await,
        0,
        "前置：库里没有行"
    );

    write_agent(&env, "research", "newbie", "you are new");

    let listed = env.get_in(&ws_id, "/api/agents").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert!(
        listed_names(&listed.body).contains(&"newbie".to_string()),
        "磁盘上的 agent 必须可见: {}",
        listed.body
    );
    assert_eq!(
        count_agent_rows(&env, "newbie").await,
        1,
        "列表应补上索引行"
    );
}

/// 索引补齐是幂等的：第二次调用不再新增行，返回值必须能分辨这一点。
///
/// 变异方法：把 `insert_if_absent` 换成无 `ON CONFLICT` 的 `insert` ——
/// 第二次调用要么报错要么重复插入，断言与返回值都不再成立。
#[tokio::test]
async fn ensure_agents_indexed_is_idempotent() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    let ws_id = env.open_space("alpha", "research").await;
    let ws = open_workspace(&env, &ws_id).await;

    write_agent(&env, "research", "fresh", "body");
    let first = peco_daemon::space::index::ensure_agents_indexed(&env.pool, &ws, "research")
        .await
        .unwrap();
    assert_eq!(first, 1, "磁盘上的新 agent 应补 1 行");

    let second = peco_daemon::space::index::ensure_agents_indexed(&env.pool, &ws, "research")
        .await
        .unwrap();
    assert_eq!(second, 0, "第二次必须是 0 行");
    assert_eq!(count_agent_rows(&env, "fresh").await, 1);
}

/// 核心判据：文件改了、没重启，已打开的空间必须读到新正文。
///
/// 变异方法（两种都实测过）：
/// ① 去掉 `invalidate` 只留 `rescan()` → Tier-2 仍是旧实例，读回旧正文，本测试变红；
/// ② 整个扇出改成空操作 → 同上。
#[tokio::test]
async fn refresh_team_agents_replaces_cached_agent_from_disk() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    write_agent(&env, "research", "@master", "old body");
    let ws_id = env.open_space("alpha", "research").await;
    let ws = open_workspace(&env, &ws_id).await;

    // 先进 Tier-2 缓存。
    let before = ws.get_master_agent().unwrap().system_prompt();
    assert!(before.contains("old body"), "前置：先读到旧正文: {before}");

    // 外部编辑器改文件（不经过任何写接口）。
    write_agent(&env, "research", "@master", "new body");

    let refreshed =
        peco_daemon::space::fanout::refresh_team_agents(&env.app_state(), "research").await;
    assert_eq!(refreshed, 1, "一个已打开空间应被刷新");

    let after = ws.get_master_agent().unwrap().system_prompt();
    assert!(
        after.contains("new body"),
        "缓存未失效，读到旧正文: {after}"
    );
    assert!(!after.contains("old body"), "旧正文不得残留: {after}");
}

/// 同 team 的每个已打开空间都要被扇到：它们各有自己的 AgentManager。
///
/// 变异方法：把扇出改成只刷新第一个空间（或只刷发起方）—— 第二个空间读完仍是
/// 旧正文，本测试变红。
#[tokio::test]
async fn fanout_reaches_every_open_space_of_the_team() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    write_agent(&env, "research", "@master", "old body");

    let first_id = env.open_space("alpha", "research").await;
    let second_id = env.open_space("beta", "research").await;
    let first = open_workspace(&env, &first_id).await;
    let second = open_workspace(&env, &second_id).await;

    for ws in [&first, &second] {
        assert!(
            ws.get_master_agent()
                .unwrap()
                .system_prompt()
                .contains("old body")
        );
    }

    write_agent(&env, "research", "@master", "new body");

    let refreshed =
        peco_daemon::space::fanout::refresh_team_agents(&env.app_state(), "research").await;
    assert_eq!(refreshed, 2, "两个已打开空间都应被刷新");

    for (label, ws) in [("alpha", &first), ("beta", &second)] {
        let body = ws.get_master_agent().unwrap().system_prompt();
        assert!(body.contains("new body"), "{label} 读到旧正文: {body}");
    }
}

/// 已关闭的空间既不参与扇出，也不让扇出失败。
///
/// 变异方法：删掉 `open_work_dirs_by_team` 的 `closed_at IS NULL` ——
/// 「库内已关闭但实例仍在」的那一步会算成 2 而不是 1，本测试变红。
#[tokio::test]
async fn closed_space_is_not_fanned_out() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    write_agent(&env, "research", "@master", "old body");

    let first_id = env.open_space("alpha", "research").await;
    let second_id = env.open_space("beta", "research").await;
    let state = env.app_state();
    assert_eq!(
        peco_daemon::space::fanout::refresh_team_agents(&state, "research").await,
        2
    );

    // 库里标记关闭、实例还留在表里（重启对账与关闭之间的真实窗口）。
    peco_daemon::db::workspaces::set_closed_at(&env.pool, &second_id)
        .await
        .unwrap();
    assert_eq!(
        peco_daemon::space::fanout::refresh_team_agents(&state, "research").await,
        1,
        "已关闭的空间不该被算进扇出"
    );

    // 走完整关闭流程：实例被摘表，扇出归零且不 panic。
    let closed = env
        .post(&format!("/api/spaces/{first_id}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);
    assert_eq!(
        peco_daemon::space::fanout::refresh_team_agents(&state, "research").await,
        0
    );
}

/// 索引行存在但目录已消失的 agent 不得出现在列表里（且不是 500）。
///
/// 变异方法：删掉 `list` 里的 `md_path(..).is_file()` 过滤 —— 陈旧行重新出现，
/// 详情页随后 500，本测试变红。
#[tokio::test]
async fn stale_index_row_is_hidden() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    let ws_id = env.open_space("alpha", "research").await;

    write_agent(&env, "research", "gone", "temporary");
    let before = env.get_in(&ws_id, "/api/agents").await;
    assert!(
        listed_names(&before.body).contains(&"gone".to_string()),
        "前置：索引后应可见: {}",
        before.body
    );

    std::fs::remove_dir_all(agent_dir(&env, "research", "gone")).unwrap();

    let after = env.get_in(&ws_id, "/api/agents").await;
    assert_eq!(after.status, 200, "陈旧行不得变成 500: {}", after.body);
    assert!(
        !listed_names(&after.body).contains(&"gone".to_string()),
        "目录已消失的 agent 不得出现在列表里: {}",
        after.body
    );
    assert_eq!(
        count_agent_rows(&env, "gone").await,
        1,
        "本期只补不删：行仍在，由视图过滤"
    );
}

/// 外部编辑器改文件 → 已打开空间读到新正文：文件监听 → 索引 → 扇出的整条链路。
///
/// 变异方法：把 `space/open.rs` 的 `start_file_watcher` 结果改成 `None`（不启动
/// 监听）—— 轮询超时，本测试变红。
///
/// 用轮询而不是固定 sleep：去抖窗口是 500ms，但 CI 上的调度延迟不受本测试控制。
#[tokio::test]
async fn file_watch_pushes_external_edits_into_the_open_space() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    write_agent(&env, "research", "@master", "old body");
    let ws_id = env.open_space("alpha", "research").await;
    let ws = open_workspace(&env, &ws_id).await;
    assert!(
        ws.get_master_agent()
            .unwrap()
            .system_prompt()
            .contains("old body")
    );

    write_agent(&env, "research", "@master", "written by an editor");

    let mut body = String::new();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        body = ws.get_master_agent().unwrap().system_prompt();
        if body.contains("written by an editor") {
            break;
        }
    }
    assert!(
        body.contains("written by an editor"),
        "文件监听未把外部改动送进已打开的空间: {body}"
    );
}

/// 写接口（`POST/PATCH/DELETE /api/agents`）也必须扇出到同团队的其它空间。
///
/// 变异方法：删掉 handler 里对应的 `fanout::refresh_team_agents` 调用 ——
/// 另一个空间的 Tier-1 永远停在旧名字集合、Tier-2 继续交出已删除的实例，
/// 三个阶段（新建可见 / 改名可见 / 删除后加载失败）逐条变红。
#[tokio::test]
async fn agent_write_api_fans_out_to_the_other_spaces() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research");
    let first_id = env.open_space("alpha", "research").await;
    let second_id = env.open_space("beta", "research").await;
    let first = open_workspace(&env, &first_id).await;
    let second = open_workspace(&env, &second_id).await;
    assert!(
        !second
            .agent_manager()
            .list_names()
            .contains(&"coder".into())
    );

    let created = env
        .post_in(
            &first_id,
            "/api/agents",
            json!({"name": "coder", "description": "writes code", "system_prompt": "code please"}),
        )
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    assert!(
        second
            .agent_manager()
            .list_names()
            .contains(&"coder".into()),
        "另一个空间的 Tier-1 没被扇到: {:?}",
        second.agent_manager().list_names()
    );

    // 另一个空间先加载一次，进 Tier-2。
    let agent_id = created.body["id"].as_str().unwrap().to_string();
    assert!(second.agent_manager().load_cached("coder").is_ok());
    assert!(first.agent_manager().load_cached("coder").is_ok());

    let deleted = env
        .delete_in(&first_id, &format!("/api/agents/{agent_id}"))
        .await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert!(
        second.agent_manager().load_cached("coder").is_err(),
        "删除后另一个空间仍在交出缓存实例"
    );
    assert!(
        !second
            .agent_manager()
            .list_names()
            .contains(&"coder".into())
    );
}
