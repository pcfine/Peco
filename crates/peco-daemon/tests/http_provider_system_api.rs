//! 系统层 provider 配置的 HTTP 语义：落在 daemon 自己的数据根、团队从中继承、
//! 写后连 console 一起刷、删条目即时生效、坏文件的两条错误面各不相同。

mod support;

use axum::http::Method;
use peco_daemon::provider::{PROVIDERS_FILE, ProviderSource, layer_roots_for, layers_for};
use peco_daemon::provider::{load_user_providers, system_layer};
use serde_json::{Value, json};
use support::upstream::{FakeUpstream, Reply};
use support::{TestEnv, call, new_env};

fn system_file(env: &TestEnv) -> std::path::PathBuf {
    env.home.join(PROVIDERS_FILE)
}

fn write_system_providers(env: &TestEnv, body: &str) {
    std::fs::write(system_file(env), body).unwrap();
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .expect("列表必须是数组")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn find<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .expect("列表必须是数组")
        .iter()
        .find(|item| item["name"] == json!(name))
        .unwrap_or_else(|| panic!("列表里没有 '{name}'：{list}"))
}

/// 空 home：系统层没有任何条目，Peco 团队的列表也必须是空的。
///
/// 旧实现的幽灵 `deepseek` 来自 `SystemConfig::load()` 的兜底分支；基座改空以后
/// 它不该再出现 —— 否则用户会以为「系统层已经配好了」。
#[tokio::test]
async fn empty_home_has_no_ghost_provider() {
    let env = new_env(&["peco"]).await;
    assert!(!system_file(&env).exists(), "新装 home 不该有系统层文件");

    let system = env.get("/api/providers").await;
    assert_eq!(system.status, 200, "body={}", system.body);
    assert!(names(&system.body).is_empty(), "body={}", system.body);

    let peco = env.get("/api/teams/peco/providers").await;
    assert_eq!(peco.status, 200, "body={}", peco.body);
    assert!(names(&peco.body).is_empty(), "body={}", peco.body);
}

/// 系统层写入落在 `$PECO_HOME/providers.toml`：不是 core 的 XDG 默认位置。
#[tokio::test]
async fn system_layer_writes_into_the_daemon_data_root() {
    let env = new_env(&["peco"]).await;

    let saved = call(
        &env.router(),
        Method::PUT,
        "/api/providers",
        Some(json!({"name": "gw", "type": "openai", "api_key": "sk-x", "set_default": true})),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(saved.body["success"], json!(true));

    let path = system_file(&env);
    assert!(
        path.exists(),
        "系统层文件必须落在数据根：{}",
        path.display()
    );
    let written = load_user_providers(&path).unwrap();
    assert_eq!(written.providers["gw"].api_key.as_deref(), Some("sk-x"));
    assert_eq!(
        written.default_provider, "gw",
        "首条新增没有可继承的默认，必须接管默认位"
    );

    // 文件内容里不该出现第二份副本：写入只改这一个文件。
    let root_files: Vec<String> = std::fs::read_dir(&env.home)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        root_files.contains(&PROVIDERS_FILE.to_string()),
        "{root_files:?}"
    );
}

/// 系统层条目被团队继承，列表里标注 `source: system`。
#[tokio::test]
async fn teams_inherit_system_entries() {
    let env = new_env(&["peco", "research"]).await;
    write_system_providers(
        &env,
        "default_provider = \"gw\"\n\n[providers.gw]\ntype = \"openai\"\napi_key = \"sk-sys\"\n",
    );

    let listed = env.get("/api/teams/research/providers").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let gw = find(&listed.body, "gw");
    assert_eq!(gw["source"], json!("system"));
    assert_eq!(gw["has_api_key"], json!(true));
    assert_eq!(gw["api_key_status"], json!("literal"));
    assert_eq!(gw["is_default"], json!(true), "默认位由系统层声明");
}

/// 系统层列表只反映系统文件自己：`is_default` 不随团队文件的改动而变。
#[tokio::test]
async fn system_list_judges_default_from_its_own_file() {
    let env = new_env(&["peco", "research"]).await;
    write_system_providers(
        &env,
        "default_provider = \"gw\"\n\n[providers.gw]\ntype = \"openai\"\n\
         \n[providers.alpha]\ntype = \"qwen\"\n",
    );
    // 团队文件把默认改到自己的条目上：系统层列表仍是 gw。
    let team_root = env.home.join("teams").join("research");
    std::fs::write(
        team_root.join(PROVIDERS_FILE),
        "default_provider = \"mine\"\n\n[providers.mine]\ntype = \"openai\"\n",
    )
    .unwrap();

    let system = env.get("/api/providers").await;
    assert_eq!(system.status, 200, "body={}", system.body);
    assert_eq!(
        names(&system.body),
        vec!["alpha".to_string(), "gw".to_string()]
    );
    assert_eq!(find(&system.body, "gw")["is_default"], json!(true));
    assert_eq!(find(&system.body, "alpha")["is_default"], json!(false));
    for item in system.body.as_array().unwrap() {
        assert_eq!(item["source"], json!("system"));
    }

    // 合并视角里同一份配置的默认是团队的条目 —— 两个视角刻意不同。
    let team = env.get("/api/teams/research/providers").await;
    assert_eq!(find(&team.body, "mine")["is_default"], json!(true));
    assert_eq!(find(&team.body, "gw")["is_default"], json!(false));
}

/// 删掉系统层默认条目：成功文案点明默认改成了谁，且影响所有团队。
#[tokio::test]
async fn deleting_the_system_default_says_where_the_default_moved() {
    let env = new_env(&["peco"]).await;
    write_system_providers(
        &env,
        "default_provider = \"gw\"\n\n[providers.gw]\ntype = \"openai\"\n\
         \n[providers.qwen]\ntype = \"qwen\"\n",
    );

    let deleted = env.delete("/api/providers/gw").await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    let message = deleted.body["message"].as_str().unwrap_or_default();
    assert!(message.contains("qwen"), "要点明默认改成了哪条：{message}");
    assert!(
        message.contains("every team"),
        "系统层是公共兜底，必须点明影响面：{message}"
    );

    let nothing_left = env.delete("/api/providers/qwen").await;
    assert_eq!(nothing_left.status, 200, "body={}", nothing_left.body);
    let message = nothing_left.body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no provider left"),
        "删空后要说清后果：{message}"
    );

    // 删不存在的名字仍是 404，不静默成功。
    let ghost = env.delete("/api/providers/ghost").await;
    assert_eq!(ghost.status, 404, "body={}", ghost.body);
    assert_eq!(ghost.code(), "provider_not_found");
}

/// 系统层写入刷到**所有团队**的已打开空间：不只是 Peco 自己的。
#[tokio::test]
async fn open_spaces_of_every_team_see_system_layer_writes() {
    let env = new_env(&["peco", "research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let dir = env.work_dir_of(&ws_id).await;
    let ws = env
        .hub
        .get(std::path::Path::new(&dir))
        .expect("空间必须已打开")
        .workspace();
    assert!(!ws.config().providers.providers.contains_key("gw"));

    let saved = call(
        &env.router(),
        Method::PUT,
        "/api/providers",
        Some(json!({"name": "gw", "type": "openai", "api_key": "sk-x", "set_default": true})),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert!(
        ws.config().providers.providers.contains_key("gw"),
        "别的团队的已打开空间也必须刷到"
    );
    assert_eq!(ws.config().default_provider_name(), "gw");

    let deleted = env.delete("/api/providers/gw").await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert!(
        !ws.config().providers.providers.contains_key("gw"),
        "删除后打开的空间里也必须消失"
    );
    assert_eq!(ws.config().default_provider_name(), "");
}

/// 系统层改动能刷到**只有 console、没有打开空间**的团队。
///
/// console 不在 Hub 里，`reload_open_spaces` 遍历的已打开空间列表看不到它：
/// 修复前这条用例的后两个断言必红。
#[tokio::test]
async fn console_only_team_sees_system_layer_changes() {
    let env = new_env(&["peco", "research"]).await;
    let state = env.app_state();
    let router = peco_daemon::http::router(state.clone());

    // 只有 console：团队没有任何已打开空间。
    let console = state.spaces.team_workspace("research").await.unwrap();
    assert!(
        !console.config().providers.providers.contains_key("gw"),
        "系统层还没写这条"
    );

    let saved = call(
        &router,
        Method::PUT,
        "/api/providers",
        Some(json!({"name": "gw", "type": "openai", "api_key": "sk-x", "set_default": true})),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert!(
        console.config().providers.providers.contains_key("gw"),
        "系统层写入后 console 必须已刷新"
    );

    let deleted = call(&router, Method::DELETE, "/api/providers/gw", None).await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert!(
        !console.config().providers.providers.contains_key("gw"),
        "删除后 console 里也必须消失（启动快照不得复活它）"
    );
}

/// `layers_for` 的新形状：系统层恒在最低位。
#[tokio::test]
async fn layers_put_the_system_layer_on_the_bottom() {
    let env = new_env(&["peco", "research"]).await;

    let peco = layers_for(&env.env, "peco");
    assert_eq!(peco.len(), 2, "Peco 不自我继承");
    assert_eq!(
        peco.iter().map(|l| l.source).collect::<Vec<_>>(),
        vec![ProviderSource::System, ProviderSource::Team]
    );
    assert_eq!(peco[0].root, env.env.system_providers_root());

    let team = layers_for(&env.env, "research");
    assert_eq!(team.len(), 3);
    assert_eq!(
        team.iter().map(|l| l.source).collect::<Vec<_>>(),
        vec![
            ProviderSource::System,
            ProviderSource::Peco,
            ProviderSource::Team
        ]
    );
    assert_eq!(team[0].root, env.env.system_providers_root());
    assert_eq!(
        layer_roots_for(&env.env, "research"),
        team.iter().map(|l| l.root.clone()).collect::<Vec<_>>()
    );
    assert_eq!(system_layer(&env.env).root, env.env.system_providers_root());
}

/// 系统层文件损坏：列表是 400（文案含路径），打开空间是 500 且文案不带路径。
///
/// 两条路径读文件的方式不同（路由级走 `load_user_providers`、打开期走 core 的
/// `load_layered`），这里如实锁住这个已知缺陷的可见后果。
#[tokio::test]
async fn broken_system_file_fails_the_list_and_the_open_differently() {
    let upstream = FakeUpstream::start(|_| async { Reply::Text("pong".into()) }).await;
    let env = new_env(&["peco", "research"]).await;
    support::install_team(&env, "research", &upstream, &[]);
    let ws_id = env.open_space("alpha", "research").await;

    // 让实例退场，再用坏文件拦住下一次打开。
    let dir = env.work_dir_of(&ws_id).await;
    env.hub.forget(std::path::Path::new(&dir));
    write_system_providers(&env, "providers = [not toml\n");

    let opened = env
        .post(&format!("/api/spaces/{ws_id}/open"), Value::Null)
        .await;
    assert_eq!(opened.status, 500, "body={}", opened.body);
    assert_eq!(opened.code(), "workspace_open_failed");

    let listed = env.get("/api/providers").await;
    assert_eq!(listed.status, 400, "body={}", listed.body);
    assert_eq!(listed.code(), "invalid_argument");
    let detail = listed.body["message"].as_str().unwrap_or_default();
    assert!(
        detail.contains(&system_file(&env).display().to_string()),
        "路由级文案必须带路径，用户才能自己修：{detail}"
    );
}
