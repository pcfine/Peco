//! 团队级资源寻址：没有空间的团队也能读写自己的能力根。
//!
//! 能力根是团队级（`$PECO_HOME/teams/<team>/agents|skills|knowledge|mcpconfig.json`），
//! 空间只贡献 work_dir。这些用例只带 `X-Peco-Team`，不打开任何空间。

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::{Value, json};
use support::{HttpReply, TestEnv, new_env};

/// 团队能力根里的一个 agent：目录名即寻址名。
fn write_agent(env: &TestEnv, team: &str, name: &str, description: &str) {
    let dir = env.home.join("teams").join(team).join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.md"),
        format!(
            "---\nagent:\n  name: \"{name}\"\n  description: \"{description}\"\n---\n\n# prompt\n"
        ),
    )
    .unwrap();
}

fn write_skill(env: &TestEnv, team: &str, name: &str) {
    let dir = env.home.join("teams").join(team).join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: \"{name}\"\ndescription: \"d\"\n---\n\n# skill\n"),
    )
    .unwrap();
}

fn write_mcp_config(env: &TestEnv, team: &str, server: &str) -> Value {
    let root = env.home.join("teams").join(team);
    std::fs::create_dir_all(&root).unwrap();
    let config = json!({"mcpServers": {server: {
        "transport": "stdio",
        "command": "npx",
        "args": ["-y", "x"],
    }}});
    std::fs::write(root.join("mcpconfig.json"), config.to_string()).unwrap();
    config
}

/// 走真实路由并附带任意请求头：两个选择器的优先级只能在这一层看出来。
async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> HttpReply {
    use tower::ServiceExt;

    let mut builder = support::bearer(Request::builder().method(method).uri(uri));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request_body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(request_body).unwrap())
        .await
        .expect("router 不会失败");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("响应体可读");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    HttpReply {
        status,
        headers,
        body,
    }
}

/// 单请求夹具：每个请求一份 `AppState`（不关心 console 复用的用例用它）。
async fn team_call(env: &TestEnv, method: Method, uri: &str, body: Option<Value>) -> HttpReply {
    call(
        &env.router(),
        method,
        uri,
        body,
        &[("X-Peco-Team", "machine")],
    )
    .await
}

async fn count_spaces(env: &TestEnv) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces")
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

fn names(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap_or_else(|| panic!("列表必须是数组: {body}"))
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// 只有磁盘上的 `@master`、`workspaces` 表一行没有 ⇒ 带 `X-Peco-Team` 也要列出来。
///
/// 变异方法：去掉 `ResourceScope` 的团队分支 → 400 `space_required`，本测试在
/// 状态码断言处变红。
#[tokio::test]
async fn team_selector_lists_agents_without_any_space() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");
    assert_eq!(count_spaces(&env).await, 0, "用例前提：没有任何空间行");

    let reply = team_call(&env, Method::GET, "/api/agents", None).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    let items = reply.body.as_array().expect("列表必须是数组");
    assert_eq!(items.len(), 1, "body={}", reply.body);
    assert_eq!(items[0]["name"], json!("@master"));
    assert_eq!(items[0]["is_master"], json!(true));
    assert_eq!(items[0]["description"], json!("机器人总管"));
    assert_eq!(count_spaces(&env).await, 0, "读写能力根不得顺手建空间");
    assert!(
        !env.home
            .join("work/team-console/machine/.workspace.json")
            .exists(),
        "console 落的是临时目录，不是可重启恢复的空间（不建 marker）"
    );
}

/// 同一路径可写：POST / PATCH / DELETE 改的是 `teams/<t>/agents/`。
///
/// `@master` 的删除拒绝与原行为一致（400 `reserved_agent`），文件必须还在。
#[tokio::test]
async fn team_selector_writes_into_the_team_root() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");

    let created = team_call(
        &env,
        Method::POST,
        "/api/agents",
        Some(json!({"name": "helper", "description": "帮手 — 干活"})),
    )
    .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    let agent_id = created.body["id"].as_str().unwrap().to_string();

    let path = env.home.join("teams/machine/agents/helper/agent.md");
    assert!(
        path.is_file(),
        "POST 必须写进团队能力根: {}",
        path.display()
    );

    let updated = team_call(
        &env,
        Method::PATCH,
        &format!("/api/agents/{agent_id}"),
        Some(json!({"description": "改过的描述"})),
    )
    .await;
    assert_eq!(updated.status, 200, "body={}", updated.body);
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        on_disk.contains("改过的描述"),
        "PATCH 必须落回团队能力根: {on_disk}"
    );

    let deleted = team_call(
        &env,
        Method::DELETE,
        &format!("/api/agents/{agent_id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert!(!path.exists(), "DELETE 必须删掉团队能力根里的目录");

    // `@master` 要先进索引行才可删（列表是唯一的补行入口）。
    let listed = team_call(&env, Method::GET, "/api/agents", None).await;
    let master_id = listed.body.as_array().expect("数组")[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = team_call(
        &env,
        Method::DELETE,
        &format!("/api/agents/{master_id}"),
        None,
    )
    .await;
    assert_eq!(refused.status, 400, "body={}", refused.body);
    assert_eq!(refused.code(), "reserved_agent", "body={}", refused.body);
    assert!(
        env.home
            .join("teams/machine/agents/@master/agent.md")
            .is_file(),
        "@master 被拒后文件必须原样保留"
    );
    assert_eq!(count_spaces(&env).await, 0);
}

/// skills / MCP / 知识库同样读团队根。
#[tokio::test]
async fn team_selector_reads_skills_mcp_and_knowledge() {
    let env = new_env(&["machine"]).await;
    write_skill(&env, "machine", "review");
    let mcp_config = write_mcp_config(&env, "machine", "filesystem");

    let skills = team_call(&env, Method::GET, "/api/skills", None).await;
    assert_eq!(skills.status, 200, "body={}", skills.body);
    assert_eq!(names(&skills.body), vec!["review".to_string()]);

    let mcp = team_call(&env, Method::GET, "/api/mcp", None).await;
    assert_eq!(mcp.status, 200, "body={}", mcp.body);
    assert_eq!(
        mcp.body["mcpServers"]["filesystem"], mcp_config["mcpServers"]["filesystem"],
        "读到的必须是团队根里那份配置"
    );

    let created = team_call(
        &env,
        Method::POST,
        "/api/knowledge",
        Some(json!({"name": "handbook"})),
    )
    .await;
    assert_eq!(created.status, 201, "body={}", created.body);

    let knowledge = team_call(&env, Method::GET, "/api/knowledge", None).await;
    assert_eq!(knowledge.status, 200, "body={}", knowledge.body);
    assert_eq!(names(&knowledge.body), vec!["handbook".to_string()]);
    assert!(
        env.home
            .join("teams/machine/knowledge/handbook/kb_config.json")
            .is_file(),
        "知识库目录必须落在团队能力根里"
    );
}

/// 未知团队 ⇒ 404 `team_not_found`（不是 400，也不是 500）。
#[tokio::test]
async fn unknown_team_is_404_team_not_found() {
    let env = new_env(&["machine"]).await;

    let reply = call(
        &env.router(),
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "ghost")],
    )
    .await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "team_not_found", "body={}", reply.body);
}

/// `$PECO_HOME` 根下不该出现能力根目录：出现即代表能力根被抬到了 home。
fn assert_no_capability_dirs_at_home(env: &TestEnv, team: &str) {
    for dir in ["skills", "knowledge", "agents", "workflows"] {
        let path = env.home.join(dir);
        assert!(
            !path.exists(),
            "X-Peco-Team: {team} 把能力根抬出了 teams/：$PECO_HOME/{dir} 已存在"
        );
    }
}

/// 越界的 `X-Peco-Team` 一律 404，且不得在任何地方建出能力根目录。
///
/// `..` 会让 `team_root` 变成 `$PECO_HOME`，随后 `open_layered` 就在那里建
/// `skills/` `knowledge/` `agents/` `workflows/` 并接受读写 —— 只断言状态码
/// 看不见这件事，目录断言才是这条闸门的证据。
///
/// 变异方法：去掉 `team_workspace` 的路径归属校验 → 三个取值全部变成 200/201，
/// `$PECO_HOME` 下被建出能力根目录，本测试在状态码与目录断言上同时变红。
#[tokio::test]
async fn team_selector_rejects_path_escapes() {
    let env = new_env(&["machine"]).await;

    for team in ["..", "../..", "foo/.."] {
        // GET 与 POST 各一条：写路径才是真正在能力根里建目录、写文件的那条。
        for (method, uri, body) in [
            (Method::GET, "/api/agents", None),
            (
                Method::POST,
                "/api/knowledge",
                Some(json!({"name": "escaped"})),
            ),
        ] {
            let reply = call(&env.router(), method, uri, body, &[("X-Peco-Team", team)]).await;
            assert_eq!(reply.status, 404, "team={team} {uri} body={}", reply.body);
            assert_eq!(
                reply.code(),
                "team_not_found",
                "team={team} {uri} body={}",
                reply.body
            );
        }
        assert_no_capability_dirs_at_home(&env, team);
    }
}

/// 历史团队目录名不合 `^[a-z][a-z0-9-]{1,31}$` 也要能打开。
///
/// 变异方法：路径归属校验改成直接复用 `team::validate_team_id` → 本用例 404，
/// 既有团队页整页打不开。
#[tokio::test]
async fn directory_name_outside_the_id_syntax_still_opens() {
    let env = new_env(&["peco-dev.bak-v1"]).await;
    write_agent(&env, "peco-dev.bak-v1", "@master", "备份团队总管");

    let reply = call(
        &env.router(),
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "peco-dev.bak-v1")],
    )
    .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(names(&reply.body), vec!["@master".to_string()]);
}

/// 保留团队 `peco` 的团队根是合法目标：路径归属校验不能把保留团队一并挡掉。
#[tokio::test]
async fn reserved_team_root_is_still_reachable() {
    let env = new_env(&["peco"]).await;
    write_agent(&env, "peco", "@master", "Peco 总管");

    let reply = call(
        &env.router(),
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "peco")],
    )
    .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(names(&reply.body), vec!["@master".to_string()]);
}

/// 两个选择器同时在场时空间优先：团队说的是 A，团队头说 B 也读 A。
///
/// 变异方法：把团队分支挪到空间分支之前 → 读到 B 的 `@master`，描述断言变红。
#[tokio::test]
async fn space_selector_wins_over_team_selector() {
    let env = new_env(&["research", "writing"]).await;
    write_agent(&env, "research", "@master", "研究总管");
    write_agent(&env, "writing", "@master", "写作总管");
    let ws_id = env.open_space("alpha", "research").await;

    let reply = call(
        &env.router(),
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Space", &ws_id), ("X-Peco-Team", "writing")],
    )
    .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    let items = reply.body.as_array().expect("数组");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["description"], json!("研究总管"));
}

/// 两个选择器都没有 ⇒ 400 `space_required`（错误码是既有契约）。
#[tokio::test]
async fn no_selector_is_400_space_required() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");

    for uri in ["/api/agents", "/api/skills", "/api/mcp", "/api/knowledge"] {
        let reply = call(&env.router(), Method::GET, uri, None, &[]).await;
        assert_eq!(reply.status, 400, "{uri} body={}", reply.body);
        assert_eq!(reply.code(), "space_required", "{uri} body={}", reply.body);
    }
}

/// console 连续两次列表请求只构建一次：第二次拿到的还是同一个实例。
///
/// 变异方法：`team_workspace` 去掉缓存（每次都 open）→ `Arc::ptr_eq` 变红；
/// 去掉「优先复用已打开空间」则本用例不变红，由 [`space_selector_wins_over_team_selector`]
/// 与 `console_yields_to_an_opened_space` 覆盖。
#[tokio::test]
async fn console_is_built_once_per_team() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");
    let state = env.app_state();
    let router = peco_daemon::http::router(state.clone());

    let first = call(
        &router,
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "machine")],
    )
    .await;
    assert_eq!(first.status, 200, "body={}", first.body);
    let built = state
        .spaces
        .console("machine")
        .expect("第一次请求必须建出 console");
    assert_eq!(
        built.root(),
        env.home.join("teams/machine"),
        "console 的能力根必须是团队根"
    );

    let second = call(
        &router,
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "machine")],
    )
    .await;
    assert_eq!(second.status, 200, "body={}", second.body);
    let reused = state.spaces.console("machine").expect("console 仍在");
    assert!(
        std::sync::Arc::ptr_eq(&built, &reused),
        "第二次请求必须复用同一个实例"
    );
}

/// 团队出现已打开空间后 console 退场：同一份 `knowledge/` 不该被两棵管理器打开。
#[tokio::test]
async fn console_yields_to_an_opened_space() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");
    let state = env.app_state();
    let router = peco_daemon::http::router(state.clone());

    let built = call(
        &router,
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "machine")],
    )
    .await;
    assert_eq!(built.status, 200, "body={}", built.body);
    assert!(state.spaces.console("machine").is_some());

    let ws_id = env.open_space("alpha", "machine").await;
    let reused = call(
        &router,
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "machine")],
    )
    .await;
    assert_eq!(reused.status, 200, "body={}", reused.body);
    assert_eq!(names(&reused.body), vec!["@master".to_string()]);
    assert!(
        state.spaces.console("machine").is_none(),
        "有已打开空间时 console 必须被摘掉"
    );
    assert!(
        env.hub.get(&env.work_dir_of(&ws_id).await).is_some(),
        "复用的必须是那个已打开空间"
    );
}

/// 删除团队后 console 缓存不得泄漏。
#[tokio::test]
async fn deleting_the_team_drops_the_console() {
    let env = new_env(&["machine"]).await;
    write_agent(&env, "machine", "@master", "机器人总管");
    let state = env.app_state();
    let router = peco_daemon::http::router(state.clone());

    let listed = call(
        &router,
        Method::GET,
        "/api/agents",
        None,
        &[("X-Peco-Team", "machine")],
    )
    .await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert!(state.spaces.console("machine").is_some());

    let deleted = call(&router, Method::DELETE, "/api/teams/machine", None, &[]).await;
    assert_eq!(deleted.status, 204, "body={}", deleted.body);

    assert!(
        state.spaces.console("machine").is_none(),
        "团队已删，console 缓存必须清掉"
    );
    assert!(!env.home.join("teams/machine").exists());
}
