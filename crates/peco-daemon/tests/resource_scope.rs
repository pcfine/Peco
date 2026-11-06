//! 投影表的资源边界：agents / knowledge_bases 按团队、conversations 按空间。
//!
//! 单用户库里 `user_id` 恒为 'local'，用它当唯一维度会让两个团队的同名资源
//! 只能存成一行，改一个就改到另一个。

mod support;

use peco_daemon::LOCAL_USER_ID;
use peco_daemon::db::conversations::{self, CreateConversationParams};
use serde_json::json;
use support::new_env;

/// 最小可用 `providers.toml`：打开空间时构造 Agent 需要一条可解析的 provider。
const PROVIDERS_TOML: &str = r#"
default_provider = "test"

[providers.test]
type = "deepseek"
api_key = "sk-test-local"
base_url = "http://127.0.0.1:9"

[providers.test.default]
model = "test-model"
"#;

/// 往团队能力根里写 provider 配置与一个 agent 目录（绕开 HTTP，等同模板安装）。
fn write_agent(env: &support::TestEnv, team: &str, name: &str, body: &str) {
    let root = env.home.join("teams").join(team);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("providers.toml"), PROVIDERS_TOML).unwrap();
    let dir = root.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.md"),
        format!("---\nagent:\n  name: \"{name}\"\n  description: \"{body}\"\n---\n\n{body}\n"),
    )
    .unwrap();
}

/// 列表里名为 `name` 的项，按名字取一条（每个团队各有一份）。
fn named<'a>(body: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    body.as_array()
        .unwrap_or_else(|| panic!("列表必须是数组: {body}"))
        .iter()
        .find(|item| item["name"] == json!(name))
        .unwrap_or_else(|| panic!("列表里应有 {name}: {body}"))
}

/// 两个团队各自有 `@master`：各自的列表只看到自己那份，改一份不影响另一份。
///
/// 变异方法（实测过）：① 把 `agents` 的 UNIQUE 改回 `(user_id, name)` —— 第二个
/// 团队的 `@master` 补不进索引，beta 的列表变空，本测试在 `named(...)` 上 panic；
/// ② 把 `list_index_by_team` 的 `team_id` 条件去掉 —— 两边都看到两行，
/// `count_of` 断言与 id 不等的断言同时变红。
#[tokio::test]
async fn two_teams_keep_their_own_master_agent() {
    let env = new_env(&["research", "design"]).await;
    write_agent(&env, "research", "@master", "research master");
    write_agent(&env, "design", "@master", "design master");
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "design").await;

    let listed_alpha = env.get_in(&alpha, "/api/agents").await;
    assert_eq!(listed_alpha.status, 200, "body={}", listed_alpha.body);
    assert_eq!(
        listed_alpha.body.as_array().unwrap().len(),
        1,
        "research 只应看到自己的 @master: {}",
        listed_alpha.body
    );
    let alpha_id = named(&listed_alpha.body, "@master")["id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed_beta = env.get_in(&beta, "/api/agents").await;
    assert_eq!(listed_beta.status, 200, "body={}", listed_beta.body);
    assert_eq!(
        listed_beta.body.as_array().unwrap().len(),
        1,
        "design 只应看到自己的 @master: {}",
        listed_beta.body
    );
    let beta_id = named(&listed_beta.body, "@master")["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(alpha_id, beta_id, "两个团队的同名 agent 必须是两行索引");

    // 改 research 的那份，design 的那份不受影响。
    let patched = env
        .patch_in(
            &alpha,
            &format!("/api/agents/{alpha_id}"),
            json!({"description": "changed"}),
        )
        .await;
    assert_eq!(patched.status, 200, "body={}", patched.body);

    let alpha_after = env.get_in(&alpha, "/api/agents").await;
    assert_eq!(
        named(&alpha_after.body, "@master")["description"],
        json!("changed")
    );
    let beta_after = env.get_in(&beta, "/api/agents").await;
    assert_eq!(
        named(&beta_after.body, "@master")["description"],
        json!("design master"),
        "另一个团队的 @master 不得被改到: {}",
        beta_after.body
    );

    // 跨团队指同一个 id：既不能改也不能删，必须是 404 而不是命中别人的行。
    let cross_patch = env
        .patch_in(
            &beta,
            &format!("/api/agents/{alpha_id}"),
            json!({"description": "hijack"}),
        )
        .await;
    assert_eq!(cross_patch.status, 404, "body={}", cross_patch.body);
    let cross_delete = env
        .delete_in(&beta, &format!("/api/agents/{alpha_id}"))
        .await;
    assert_eq!(cross_delete.status, 404, "body={}", cross_delete.body);

    let rows = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agents WHERE team_id = 'research' AND name = '@master'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(rows, 1, "跨团队的删除/改名不得动到 research 的行");
}

/// 两个团队各有同名知识库：列表互不可见，同团队内同名才冲突。
///
/// 变异方法（实测过）：① 把 `find_by_name_and_team` 的 `team_id` 条件去掉 ——
/// design 建同名 KB 变 409；② 去掉 `list_by_team` 的 `team_id` 条件 —— 两边列表
/// 都变成 2 条，`len() == 1` 变红。
#[tokio::test]
async fn two_teams_keep_their_own_knowledge_base() {
    let env = new_env(&["research", "design"]).await;
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "design").await;

    let created_alpha = env
        .post_in(&alpha, "/api/knowledge", json!({"name": "kb-one"}))
        .await;
    assert_eq!(created_alpha.status, 201, "body={}", created_alpha.body);
    let alpha_kb = created_alpha.body["id"].as_str().unwrap().to_string();

    let created_beta = env
        .post_in(&beta, "/api/knowledge", json!({"name": "kb-one"}))
        .await;
    assert_eq!(
        created_beta.status, 201,
        "另一团队的同名 KB 是另一份资源: {}",
        created_beta.body
    );
    let beta_kb = created_beta.body["id"].as_str().unwrap().to_string();
    assert_ne!(alpha_kb, beta_kb, "同名 KB 必须是两份资源");

    for (ws_id, own) in [(&alpha, &alpha_kb), (&beta, &beta_kb)] {
        let listed = env.get_in(ws_id, "/api/knowledge").await;
        assert_eq!(listed.status, 200, "body={}", listed.body);
        let kbs = listed.body.as_array().unwrap();
        assert_eq!(kbs.len(), 1, "只能看到本团队的 KB: {}", listed.body);
        assert_eq!(kbs[0]["id"].as_str().unwrap(), own.as_str());
    }

    // 同团队内同名仍然冲突。
    let dup = env
        .post_in(&alpha, "/api/knowledge", json!({"name": "kb-one"}))
        .await;
    assert_eq!(dup.status, 409, "body={}", dup.body);

    // 跨团队指向别人的 KB id：读与删都必须 404。
    let cross_get = env
        .get_in(&beta, &format!("/api/knowledge/{alpha_kb}"))
        .await;
    assert_eq!(cross_get.status, 404, "body={}", cross_get.body);
    let cross_delete = env
        .delete_in(&beta, &format!("/api/knowledge/{alpha_kb}"))
        .await;
    assert_eq!(cross_delete.status, 404, "body={}", cross_delete.body);

    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM knowledge_bases")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2, "跨团队的删除不得抹掉另一团队的 KB 行");
}

/// 同团队两空间聊同一个 agent：会话历史各自一份，归档/计数也按空间算。
///
/// chat 模块尚未拷入，这里直接测 DB 层契约（handler 接线留给会话层）。
///
/// 变异方法（实测过）：① 去掉 `list_by_workspace_and_agent` 的 `workspace_id`
/// 条件 —— B 空间读到 A 的会话；② 去掉 `scope = 'agent'` —— 私有会话混进列表。
#[tokio::test]
async fn conversations_are_scoped_by_workspace() {
    let env = new_env(&["research"]).await;
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "research").await;

    conversations::insert(
        &env.pool,
        &CreateConversationParams {
            id: "c_alpha".into(),
            workspace_id: alpha.clone(),
            agent_id: None,
            agent_name: "@master".into(),
            title: "alpha history".into(),
        },
    )
    .await
    .unwrap();

    let in_alpha = conversations::list_by_workspace_and_agent(&env.pool, &alpha, "@master", false)
        .await
        .unwrap();
    assert_eq!(in_alpha.len(), 1, "A 空间应看到自己写入的会话");
    let in_beta = conversations::list_by_workspace_and_agent(&env.pool, &beta, "@master", false)
        .await
        .unwrap();
    assert!(in_beta.is_empty(), "B 空间不得看到 A 空间的会话");

    assert_eq!(
        conversations::count_active(&env.pool, &alpha, "@master")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        conversations::count_active(&env.pool, &beta, "@master")
            .await
            .unwrap(),
        0,
        "活跃计数必须按空间算"
    );

    // B 空间的自动归档不得碰到 A 空间的会话。
    let archived = conversations::archive_oldest(&env.pool, &beta, "@master", 10)
        .await
        .unwrap();
    assert_eq!(archived, 0, "B 空间没有可归档的会话");
    assert!(
        conversations::find_by_id_and_workspace(&env.pool, "c_alpha", &alpha)
            .await
            .unwrap()
            .unwrap()
            .archived_at
            .is_none(),
        "A 空间的会话不得被 B 空间的归档动到"
    );
    assert!(
        conversations::find_by_id_and_workspace(&env.pool, "c_alpha", &beta)
            .await
            .unwrap()
            .is_none(),
        "跨空间按 id 查必须查不到"
    );

    // 私有会话（scope='peco'）仍不进 agent 会话列表。
    conversations::ensure_private_session(
        &env.pool,
        LOCAL_USER_ID,
        &alpha,
        "c_peco",
        "@master",
        "peco",
    )
    .await
    .unwrap();
    let listed = conversations::list_by_workspace_and_agent(&env.pool, &alpha, "@master", true)
        .await
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "私有会话不得混进 agent 会话列表: {listed:?}"
    );
}
