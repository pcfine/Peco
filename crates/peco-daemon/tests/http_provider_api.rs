//! 团队级 provider 配置的 HTTP 语义：只列用户文件、凭据不回读、字段级部分更新、
//! 写后重载已打开实例、非法类型 400。

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::http::Method;
use peco_core::config::{LlmApiParams, McpConfig, ProviderEntry, ProvidersConfig, SystemConfig};
use serde_json::{Value, json};
use support::upstream::{FakeUpstream, Reply};
use support::{HttpReply, TestEnv, call, new_env};

fn team_root(env: &TestEnv, team: &str) -> PathBuf {
    env.home.join("teams").join(team)
}

fn write_providers(env: &TestEnv, team: &str, body: &str) {
    let root = team_root(env, team);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("providers.toml"), body).unwrap();
}

fn read_providers(env: &TestEnv, team: &str) -> String {
    std::fs::read_to_string(team_root(env, team).join("providers.toml")).unwrap_or_default()
}

/// 系统层只留一条兜底 provider，让「合并」与「只用用户文件」两种判据可分辨。
fn system_config(name: &str, key: Option<&str>) -> SystemConfig {
    let mut providers = HashMap::new();
    providers.insert(
        name.to_string(),
        ProviderEntry {
            provider_type: "deepseek".to_string(),
            api_key: key.map(str::to_string),
            base_url: Some("https://api.deepseek.com".to_string()),
            api: None,
            default: Some(LlmApiParams {
                model: "deepseek-v4-flash".to_string(),
                temperature: None,
                max_tokens: None,
                stream: None,
                reasoning_effort: None,
            }),
        },
    );
    SystemConfig {
        providers: ProvidersConfig {
            default_provider: name.to_string(),
            providers,
            web_search: None,
        },
        mcp: McpConfig::empty(),
    }
}

async fn put_provider(env: &TestEnv, team: &str, body: Value) -> HttpReply {
    call(
        &env.router(),
        Method::PUT,
        &format!("/api/teams/{team}/providers"),
        Some(body),
    )
    .await
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

/// 用户文件里没有 `api_key` 的条目，且系统层也没配 ⇒ 未配置。
const GATEWAY_WITHOUT_KEY: &str = r#"
default_provider = "gateway"

[providers.gateway]
type = "openai"
base_url = "https://proxy.example.com/v1"

[providers.gateway.default]
model = "gpt-5.2"
temperature = 0.2
"#;

/// 列表只含用户 providers.toml 里的条目；系统层条目由合并兜底，不进列表。
///
/// 变异方法：把列表数据源换成合并后的生效配置 → `deepseek` 会一起出现，本测试变红。
#[tokio::test]
async fn list_returns_only_user_file_entries() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research", GATEWAY_WITHOUT_KEY);

    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("deepseek", Some("sk-system"))),
    );
    let reply = call(&router, Method::GET, "/api/teams/research/providers", None).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(names(&reply.body), vec!["gateway".to_string()]);
    let gateway = find(&reply.body, "gateway");
    assert_eq!(gateway["provider_type"], json!("openai"));
    assert_eq!(
        gateway["base_url"],
        json!("https://proxy.example.com/v1"),
        "呈现的是用户文件里的值"
    );
    assert_eq!(gateway["default_model"], json!("gpt-5.2"));
    assert_eq!(gateway["is_default"], json!(true));
}

/// `api_key` 永不回读；`has_api_key` 认合并后的生效配置。
///
/// 变异方法：`has_api_key` 改成只看用户原始文件 → 系统层补上的 key 不再算数，
/// 第一条断言变红。
#[tokio::test]
async fn api_key_is_never_read_back_but_system_key_counts() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research", GATEWAY_WITHOUT_KEY);

    // ① 系统层给同名条目补了 key ⇒ 实际可用。
    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("gateway", Some("sk-system"))),
    );
    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "gateway")["has_api_key"],
        json!(true),
        "系统层补上的 key 必须算已配置：{}",
        listed.body
    );

    let detail = call(
        &router,
        Method::GET,
        "/api/teams/research/providers/gateway",
        None,
    )
    .await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["has_api_key"], json!(true));
    assert!(
        detail.body.get("api_key").is_none(),
        "凭据不得出现在响应里：{}",
        detail.body
    );
    assert!(
        !detail.body.to_string().contains("sk-system"),
        "响应体不得夹带明文 key：{}",
        detail.body
    );

    // ② 系统层也没配 ⇒ 未配置。
    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("deepseek", None)),
    );
    let unset = call(
        &router,
        Method::GET,
        "/api/teams/research/providers/gateway",
        None,
    )
    .await;
    assert_eq!(unset.status, 200, "body={}", unset.body);
    assert_eq!(unset.body["has_api_key"], json!(false));
}

/// 字段级部分更新：只给 `base_url` 时不丢 api_key / 模型，也不动别的条目。
#[tokio::test]
async fn partial_update_keeps_untouched_fields() {
    let env = new_env(&["research"]).await;
    write_providers(
        &env,
        "research",
        r#"
default_provider = "gateway"

[providers.gateway]
type = "openai"
api = "chat"
api_key = "sk-keep"
base_url = "https://old.example.com/v1"

[providers.gateway.default]
model = "gpt-5.2"
temperature = 0.2

[providers.other]
type = "qwen"
api_key = "sk-other"
"#,
    );
    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("deepseek", None)),
    );

    let saved = call(
        &router,
        Method::PUT,
        "/api/teams/research/providers",
        Some(json!({
            "name": "gateway",
            "type": "openai",
            "base_url": "https://new.example.com/v1",
        })),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(saved.body["success"], json!(true));

    let detail = call(
        &router,
        Method::GET,
        "/api/teams/research/providers/gateway",
        None,
    )
    .await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["base_url"], json!("https://new.example.com/v1"));
    assert_eq!(
        detail.body["has_api_key"],
        json!(true),
        "省略 api_key 不该清空已存凭据：{}",
        detail.body
    );
    assert_eq!(
        detail.body["default_model"],
        json!("gpt-5.2"),
        "省略 default_model 不该丢已存模型：{}",
        detail.body
    );
    assert_eq!(detail.body["api"], json!("chat"));

    // 另一条 provider 原样还在（写入只替换命中的条目）。
    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    let mut got = names(&listed.body);
    got.sort();
    assert_eq!(got, vec!["gateway".to_string(), "other".to_string()]);
    assert_eq!(
        find(&listed.body, "other")["has_api_key"],
        json!(true),
        "别条目的凭据不该被这次写入影响"
    );

    // 显式空串才是清空。
    let cleared = call(
        &router,
        Method::PUT,
        "/api/teams/research/providers",
        Some(json!({"name": "gateway", "type": "openai", "api_key": ""})),
    )
    .await;
    assert_eq!(cleared.status, 200, "body={}", cleared.body);
    let detail = call(
        &router,
        Method::GET,
        "/api/teams/research/providers/gateway",
        None,
    )
    .await;
    assert_eq!(detail.body["has_api_key"], json!(false));
}

/// 写成功后对同团队**已打开**的每个空间重载 provider，让已缓存 Agent 重建。
///
/// 变异方法：删掉 `reload_open_spaces` 的调用 → 实例的 `config()` 仍是旧 base_url，
/// 两个空间的断言同时变红。
#[tokio::test]
async fn save_reloads_every_open_space_of_the_team() {
    let upstream = FakeUpstream::start(|_| async { Reply::Text("pong".into()) }).await;
    let env = new_env(&["research"]).await;
    write_providers(
        &env,
        "research",
        &format!(
            r#"
default_provider = "fake"

[providers.fake]
type = "openai"
api = "chat"
api_key = "test"
base_url = "{}"

[providers.fake.default]
model = "unused"
"#,
            upstream.base_url
        ),
    );
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "research").await;

    let saved = put_provider(
        &env,
        "research",
        json!({
            "name": "fake",
            "type": "openai",
            "base_url": "http://127.0.0.1:9/v1",
        }),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert!(
        saved.body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("reloaded 2"),
        "message 里要带重载结果：{}",
        saved.body
    );

    for ws_id in [&alpha, &beta] {
        let dir = env.work_dir_of(ws_id).await;
        let handle = env
            .hub
            .get(Path::new(&dir))
            .unwrap_or_else(|| panic!("{ws_id} 必须仍在实例表里"));
        let config = handle.workspace().config();
        assert_eq!(
            config.providers.providers["fake"].base_url.as_deref(),
            Some("http://127.0.0.1:9/v1"),
            "{ws_id} 的 provider 快照必须已刷新"
        );
    }
}

/// 请求体里的 `type` 必须是适配器能分派的类型，否则 400 且不落盘。
#[tokio::test]
async fn unsupported_type_is_400_and_writes_nothing() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research", GATEWAY_WITHOUT_KEY);
    let before = read_providers(&env, "research");

    let reply = put_provider(
        &env,
        "research",
        json!({"name": "gateway", "type": "anthropic", "api_key": "sk"}),
    )
    .await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    assert!(
        reply.body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("deepseek"),
        "错误文案要列出合法取值：{}",
        reply.body
    );
    assert_eq!(read_providers(&env, "research"), before, "校验失败不得写盘");

    let draft = env
        .post(
            "/api/teams/research/providers/test",
            json!({"type": "anthropic", "api_key": "sk", "default_model": "x"}),
        )
        .await;
    assert_eq!(draft.status, 400, "body={}", draft.body);
    assert_eq!(draft.code(), "invalid_argument");
}

/// 两个探针端点都走 core 的同一份实现：表单值测（不落盘）与生效配置测。
#[tokio::test]
async fn probes_use_the_shared_core_implementation() {
    let upstream = FakeUpstream::start(|_| async { Reply::Text("pong".into()) }).await;
    let env = new_env(&["research"]).await;
    write_providers(
        &env,
        "research",
        &format!(
            r#"
default_provider = "fake"

[providers.fake]
type = "openai"
api = "chat"
api_key = "test"
base_url = "{}"

[providers.fake.default]
model = "unused"
"#,
            upstream.base_url
        ),
    );

    let draft = env
        .post(
            "/api/teams/research/providers/test",
            json!({
                "type": "openai",
                "api": "chat",
                "api_key": "test",
                "base_url": upstream.base_url,
                "default_model": "probe-model",
            }),
        )
        .await;
    assert_eq!(draft.status, 200, "body={}", draft.body);
    assert_eq!(draft.body["success"], json!(true), "body={}", draft.body);
    assert_eq!(draft.body["model"], json!("probe-model"));
    assert_eq!(draft.body["provider_type"], json!("openai"));
    assert!(
        !read_providers(&env, "research").contains("probe-model"),
        "草稿探针不得落盘"
    );

    let saved = env
        .post("/api/teams/research/providers/fake/test", Value::Null)
        .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(saved.body["success"], json!(true), "body={}", saved.body);
    assert_eq!(saved.body["provider_type"], json!("fake"));
    assert_eq!(saved.body["model"], json!("unused"));
}

/// 未配置的名字是 404，不是 500 也不是静默成功。
#[tokio::test]
async fn unknown_provider_is_404() {
    let env = new_env(&["research"]).await;
    write_providers(&env, "research", GATEWAY_WITHOUT_KEY);

    let detail = env.get("/api/teams/research/providers/ghost").await;
    assert_eq!(detail.status, 404, "body={}", detail.body);
    assert_eq!(detail.code(), "provider_not_found");

    let deleted = env.delete("/api/teams/research/providers/ghost").await;
    assert_eq!(deleted.status, 404, "body={}", deleted.body);
    assert_eq!(deleted.code(), "provider_not_found");
    assert!(
        read_providers(&env, "research").contains("gateway"),
        "删除失败不得动用户文件"
    );
}

/// 团队不存在 ⇒ 404 `team_not_found`，且不凭空造出团队目录。
#[tokio::test]
async fn unknown_team_is_404() {
    let env = new_env(&["research"]).await;

    let listed = env.get("/api/teams/ghost/providers").await;
    assert_eq!(listed.status, 404, "body={}", listed.body);
    assert_eq!(listed.code(), "team_not_found");

    let saved = put_provider(
        &env,
        "ghost",
        json!({"name": "gateway", "type": "openai", "api_key": "sk"}),
    )
    .await;
    assert_eq!(saved.status, 404, "body={}", saved.body);
    assert!(!team_root(&env, "ghost").exists(), "写接口不得创建团队目录");
}

/// 删除默认 provider：文件里余下条目接管默认位，文件结构仍然可解析。
#[tokio::test]
async fn delete_reassigns_default_provider() {
    let env = new_env(&["research"]).await;
    write_providers(
        &env,
        "research",
        r#"
default_provider = "gateway"

[providers.gateway]
type = "openai"

[providers.alpha]
type = "qwen"
"#,
    );

    let deleted = env.delete("/api/teams/research/providers/gateway").await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert_eq!(deleted.body["success"], json!(true));

    let listed = env.get("/api/teams/research/providers").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(names(&listed.body), vec!["alpha".to_string()]);
    assert_eq!(
        find(&listed.body, "alpha")["is_default"],
        json!(true),
        "默认位要落到余下条目上：{}",
        listed.body
    );
}

/// 类型目录是 UI 预填值的来源，路径在团队下但内容与团队无关。
#[tokio::test]
async fn types_catalog_is_served_under_the_team() {
    let env = new_env(&["research"]).await;
    let reply = env.get("/api/teams/research/providers/types").await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    let types: Vec<&str> = reply
        .body
        .as_array()
        .expect("列表必须是数组")
        .iter()
        .filter_map(|item| item["provider_type"].as_str())
        .collect();
    assert!(types.contains(&"deepseek"), "body={}", reply.body);
    assert!(!types.contains(&"anthropic"), "body={}", reply.body);
}
