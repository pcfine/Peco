//! 团队级 provider 配置的 HTTP 语义：生效并集与来源标注、多层继承与字段级合并、
//! 默认 provider 的显式性、继承条目不可删、凭据不回读、写后重载已打开实例、
//! 非法类型 400。

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::http::Method;
use peco_core::config::{
    LlmApiParams, McpConfig, ProviderEntry, ProvidersConfig, SystemConfig, UserConfig,
};
use peco_daemon::space::peco::PECO_TEAM_ID;
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

/// 团队文件里**显式写出**的 `default_provider` —— 没写就是 `None`。
///
/// 按 TOML 键判而不是按字符串判：团队没接管默认位时，文件里连这个键都不该出现，
/// 否则合并时会被当成团队自己声明的默认值。
fn written_default(env: &TestEnv, team: &str) -> Option<String> {
    let raw = read_providers(env, team);
    let document: toml::Value =
        toml::from_str(&raw).unwrap_or_else(|e| panic!("团队文件不是合法 TOML（{e}）：\n{raw}"));
    document
        .get("default_provider")
        .map(|value| value.as_str().unwrap_or_default().to_string())
}

/// 系统层没有任何 provider：低层「无可继承的默认」的场景用它。
///
/// `default_provider` 保留 serde 的默认值，但 providers 是空的 —— 名字指不到
/// 端点，因此不构成可继承的默认。
fn no_provider_system_config() -> SystemConfig {
    SystemConfig {
        providers: ProvidersConfig {
            default_provider: "deepseek".to_string(),
            providers: HashMap::new(),
            web_search: None,
        },
        mcp: McpConfig::empty(),
    }
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

/// 列表里由本团队文件定义的条目 —— 继承来的不可编辑，不该混进本文件的断言。
fn team_owned_names(list: &Value) -> Vec<String> {
    list.as_array()
        .expect("列表必须是数组")
        .iter()
        .filter(|item| item["source"] == json!("team"))
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// 已打开实例的生效配置快照。
async fn open_config(env: &TestEnv, ws_id: &str) -> UserConfig {
    let dir = env.work_dir_of(ws_id).await;
    let handle = env
        .hub
        .get(Path::new(&dir))
        .unwrap_or_else(|| panic!("{ws_id} 必须仍在实例表里"));
    handle.workspace().config()
}

/// Peco 层（团队全局默认）：一条带 key 的 provider，默认也是它。
///
/// 默认名刻意与 serde 的 `"deepseek"` 默认值不同：两者相同的话，
/// 「没写 default_provider 的层沿用低层」与「采纳 serde 默认值」会给出同样结果，
/// 断言分辨不出来。
const PECO_PROVIDERS: &str = r#"
default_provider = "peco-shared"

[providers.peco-shared]
type = "deepseek"
api_key = "sk-peco"
base_url = "https://peco.example.com/v1"

[providers.peco-shared.default]
model = "peco-model"
temperature = 0.1
"#;

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

/// 列表返回生效并集：团队层没写的条目从 Peco / 系统层继承，并标注来源。
///
/// 变异方法：把 `layers_for` 里的 Peco 层去掉 → 继承条目整条消失，本测试变红。
#[tokio::test]
async fn list_returns_the_effective_union_with_source() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);

    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-fallback", Some("sk-sys"))),
    );
    let reply = call(&router, Method::GET, "/api/teams/research/providers", None).await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(
        names(&reply.body),
        vec!["peco-shared".to_string(), "sys-fallback".to_string()],
        "团队层没写条目：Peco 在前，系统层在后"
    );

    let inherited = find(&reply.body, "peco-shared");
    assert_eq!(inherited["source"], json!("peco"));
    assert_eq!(inherited["provider_type"], json!("deepseek"));
    assert_eq!(inherited["base_url"], json!("https://peco.example.com/v1"));
    assert_eq!(inherited["default_model"], json!("peco-model"));
    assert_eq!(inherited["has_api_key"], json!(true));
    assert_eq!(
        inherited["is_default"],
        json!(true),
        "默认位按生效配置判定：{}",
        reply.body
    );
    assert!(
        !reply.body.to_string().contains("sk-peco"),
        "凭据不得回读：{}",
        reply.body
    );

    let system = find(&reply.body, "sys-fallback");
    assert_eq!(system["source"], json!("system"));
    assert_eq!(system["has_api_key"], json!(true));
    assert_eq!(system["is_default"], json!(false));

    // 详情与列表同源：继承条目也能读单条。
    let detail = call(
        &router,
        Method::GET,
        "/api/teams/research/providers/peco-shared",
        None,
    )
    .await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["source"], json!("peco"));
    assert_eq!(
        detail.body["base_url"],
        json!("https://peco.example.com/v1")
    );
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
    let mut got = team_owned_names(&listed.body);
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
    // 系统层显式注入：清单是生效并集，让它随开发机的 providers.toml 漂移
    // 会让「余下条目」的断言失去意义。
    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-fallback", Some("sk-sys"))),
    );

    let deleted = call(
        &router,
        Method::DELETE,
        "/api/teams/research/providers/gateway",
        None,
    )
    .await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert_eq!(deleted.body["success"], json!(true));

    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(team_owned_names(&listed.body), vec!["alpha".to_string()]);
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

/// 打开团队空间时，生效配置里的条目来自 Peco 层（key / 地址 / 模型三个字段）。
///
/// 变异方法：把 `layers_for` 里的 Peco 层去掉（opener 因此只传团队根）→
/// 生效配置里没有 `peco-shared`，本测试变红。
#[tokio::test]
async fn opened_space_inherits_the_peco_layer() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);

    let ws_id = env.open_space("alpha", "research").await;
    let config = open_config(&env, &ws_id).await;

    let entry = config
        .provider_entry(Some("peco-shared"))
        .expect("团队没写条目，生效配置必须来自 Peco 层");
    assert_eq!(entry.api_key.as_deref(), Some("sk-peco"));
    assert_eq!(
        entry.base_url.as_deref(),
        Some("https://peco.example.com/v1")
    );
    assert_eq!(
        entry.default.as_ref().map(|d| d.model.as_str()),
        Some("peco-model")
    );
    assert_eq!(config.default_provider_name(), "peco-shared");
}

/// 团队层只写 `api_key` ⇒ 地址与模型仍来自 Peco（字段级合并，不是整条替换）。
#[tokio::test]
async fn team_layer_overrides_only_the_fields_it_writes() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);
    write_providers(
        &env,
        "research",
        r#"
[providers.peco-shared]
type = "deepseek"
api_key = "sk-team"
"#,
    );

    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-fallback", Some("sk-sys"))),
    );
    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);

    let entry = find(&listed.body, "peco-shared");
    assert_eq!(entry["source"], json!("team"), "本层写了它，来源就是本层");
    assert_eq!(entry["has_api_key"], json!(true));
    assert_eq!(
        entry["base_url"],
        json!("https://peco.example.com/v1"),
        "本层没写地址，保留 Peco 的：{}",
        listed.body
    );
    assert_eq!(
        entry["default_model"],
        json!("peco-model"),
        "本层没写模型，保留 Peco 的：{}",
        listed.body
    );
    assert!(
        !listed.body.to_string().contains("sk-team"),
        "凭据不得回读：{}",
        listed.body
    );
}

/// 默认 provider 只在本层显式写出时生效：只加 provider 不写默认 ⇒ 仍是 Peco 的默认。
#[tokio::test]
async fn team_default_provider_applies_only_when_written() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;
    // 低层里还要有一条名叫 `deepseek` 的 provider：serde 的默认值恰好是
    // `"deepseek"`，有同名条目时「采纳默认值」才会真的把默认位改掉 —— 少了它，
    // 这类实现也能靠「名字不存在，回落到低层默认」侥幸通过。
    write_providers(
        &env,
        PECO_TEAM_ID,
        &format!(
            "{PECO_PROVIDERS}\n[providers.deepseek]\ntype = \"deepseek\"\napi_key = \"sk-plain\"\n"
        ),
    );
    write_providers(
        &env,
        "research",
        r#"
[providers.team-qwen]
type = "qwen"
api_key = "sk-qwen"
"#,
    );

    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-fallback", Some("sk-sys"))),
    );
    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "peco-shared")["is_default"],
        json!(true),
        "没写 default_provider 的团队层不得顶掉 Peco 的默认：{}",
        listed.body
    );
    assert_eq!(
        find(&listed.body, "team-qwen")["is_default"],
        json!(false),
        "body={}",
        listed.body
    );

    write_providers(
        &env,
        "research",
        r#"
default_provider = "team-qwen"

[providers.team-qwen]
type = "qwen"
api_key = "sk-qwen"
"#,
    );
    let listed = call(&router, Method::GET, "/api/teams/research/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "team-qwen")["is_default"],
        json!(true),
        "显式写出默认位才接管：{}",
        listed.body
    );
    assert_eq!(
        find(&listed.body, "peco-shared")["is_default"],
        json!(false),
        "body={}",
        listed.body
    );
}

/// 团队没文件、低层已有默认 ⇒ 首条 provider 不接管默认位（请求里 set_default 关着）。
///
/// 变异方法：把 `apply_upsert` 的接管条件改回「新条目且是文件里第一条」→
/// 团队文件被写入 `default_provider`，`is_default` 从 Peco 的候选上掉下来，
/// 三条断言全红。
#[tokio::test]
async fn first_provider_keeps_the_inherited_default() {
    let env = new_env(&[PECO_TEAM_ID, "shop"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);
    let router =
        peco_daemon::http::router(env.app_state_with_system_config(no_provider_system_config()));

    let saved = call(
        &router,
        Method::PUT,
        "/api/teams/shop/providers",
        Some(json!({
            "name": "qwen-side",
            "type": "qwen",
            "api_key": "sk-qwen",
            "set_default": false,
        })),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);

    assert_eq!(
        written_default(&env, "shop"),
        None,
        "没接管默认位就不该把 default_provider 写进团队文件：{}",
        read_providers(&env, "shop")
    );
    assert!(
        read_providers(&env, "shop").contains("qwen-side"),
        "条目本身必须落盘：{}",
        read_providers(&env, "shop")
    );

    let listed = call(&router, Method::GET, "/api/teams/shop/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "peco-shared")["is_default"],
        json!(true),
        "默认位仍属于继承来的 provider：{}",
        listed.body
    );
    assert_eq!(
        find(&listed.body, "qwen-side")["is_default"],
        json!(false),
        "新增的条目没被要求接管默认位：{}",
        listed.body
    );
}

/// 同一个场景下显式 `set_default: true` ⇒ 一定接管（低层有默认也照旧）。
#[tokio::test]
async fn explicit_set_default_takes_over_the_inherited_default() {
    let env = new_env(&[PECO_TEAM_ID, "shop"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);
    let router =
        peco_daemon::http::router(env.app_state_with_system_config(no_provider_system_config()));

    let saved = call(
        &router,
        Method::PUT,
        "/api/teams/shop/providers",
        Some(json!({
            "name": "qwen-side",
            "type": "qwen",
            "api_key": "sk-qwen",
            "set_default": true,
        })),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(
        written_default(&env, "shop").as_deref(),
        Some("qwen-side"),
        "显式接管必须写进团队文件：{}",
        read_providers(&env, "shop")
    );

    let listed = call(&router, Method::GET, "/api/teams/shop/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "qwen-side")["is_default"],
        json!(true),
        "body={}",
        listed.body
    );
    assert_eq!(
        find(&listed.body, "peco-shared")["is_default"],
        json!(false),
        "body={}",
        listed.body
    );
}

/// 低层没有任何 provider ⇒ 首条仍按旧规则接管（否则新增的 provider 永远用不到）。
///
/// 变异方法：把接管条件收成「只在 `set_default` 为真时接管」→ 文件里没有
/// `default_provider`，两条断言变红。
#[tokio::test]
async fn first_provider_takes_over_when_nothing_to_inherit() {
    let env = new_env(&[PECO_TEAM_ID, "shop"]).await;
    let router =
        peco_daemon::http::router(env.app_state_with_system_config(no_provider_system_config()));

    let saved = call(
        &router,
        Method::PUT,
        "/api/teams/shop/providers",
        Some(json!({
            "name": "qwen-side",
            "type": "qwen",
            "api_key": "sk-qwen",
            "set_default": false,
        })),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(
        written_default(&env, "shop").as_deref(),
        Some("qwen-side"),
        "无可继承的默认时首条要接管：{}",
        read_providers(&env, "shop")
    );

    let listed = call(&router, Method::GET, "/api/teams/shop/providers", None).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "qwen-side")["is_default"],
        json!(true),
        "body={}",
        listed.body
    );
}

/// Peco 团队自己保存时，「低层」是系统层：系统已有可用的默认 ⇒ 首条不接管。
#[tokio::test]
async fn peco_team_keeps_the_system_default() {
    let env = new_env(&[PECO_TEAM_ID]).await;
    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-main", Some("sk-sys"))),
    );

    let saved = call(
        &router,
        Method::PUT,
        &format!("/api/teams/{PECO_TEAM_ID}/providers"),
        Some(json!({
            "name": "peco-qwen",
            "type": "qwen",
            "api_key": "sk-qwen",
            "set_default": false,
        })),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert_eq!(
        written_default(&env, PECO_TEAM_ID),
        None,
        "系统层已给出默认，Peco 层不该抢：{}",
        read_providers(&env, PECO_TEAM_ID)
    );

    let listed = call(
        &router,
        Method::GET,
        &format!("/api/teams/{PECO_TEAM_ID}/providers"),
        None,
    )
    .await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert_eq!(
        find(&listed.body, "sys-main")["is_default"],
        json!(true),
        "body={}",
        listed.body
    );
    assert_eq!(
        find(&listed.body, "peco-qwen")["is_default"],
        json!(false),
        "body={}",
        listed.body
    );
}

/// 改 Peco 层 ⇒ 所有团队的已打开空间都跟着刷新；改团队层 ⇒ 只刷自己。
#[tokio::test]
async fn peco_save_reloads_spaces_of_every_team() {
    let env = new_env(&[PECO_TEAM_ID, "research", "writing"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "writing").await;

    let saved = put_provider(
        &env,
        PECO_TEAM_ID,
        json!({
            "name": "peco-shared",
            "type": "deepseek",
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
        "改 Peco 要重载所有已打开空间：{}",
        saved.body
    );
    for ws_id in [&alpha, &beta] {
        let config = open_config(&env, ws_id).await;
        assert_eq!(
            config.providers.providers["peco-shared"]
                .base_url
                .as_deref(),
            Some("http://127.0.0.1:9/v1"),
            "{ws_id} 继承 Peco 的条目必须已刷新"
        );
    }

    let saved = put_provider(
        &env,
        "research",
        json!({"name": "team-only", "type": "qwen", "api_key": "sk-qwen"}),
    )
    .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);
    assert!(
        saved.body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("reloaded 1"),
        "改团队只该重载本团队：{}",
        saved.body
    );
    assert!(
        open_config(&env, &alpha)
            .await
            .providers
            .providers
            .contains_key("team-only"),
        "本团队的空间必须拿到新条目"
    );
    assert!(
        !open_config(&env, &beta)
            .await
            .providers
            .providers
            .contains_key("team-only"),
        "别的团队不该被重载"
    );
}

/// 只在继承层存在的条目删不掉：400 `inherited_provider`，文案说明怎么办，且不写盘。
#[tokio::test]
async fn deleting_an_inherited_provider_is_400() {
    let env = new_env(&[PECO_TEAM_ID, "research"]).await;
    write_providers(&env, PECO_TEAM_ID, PECO_PROVIDERS);
    write_providers(&env, "research", GATEWAY_WITHOUT_KEY);
    let before = read_providers(&env, "research");

    let router = peco_daemon::http::router(
        env.app_state_with_system_config(system_config("sys-fallback", Some("sk-sys"))),
    );

    let inherited = call(
        &router,
        Method::DELETE,
        "/api/teams/research/providers/peco-shared",
        None,
    )
    .await;
    assert_eq!(inherited.status, 400, "body={}", inherited.body);
    assert_eq!(inherited.code(), "inherited_provider");
    let message = inherited.body["message"].as_str().unwrap_or_default();
    assert!(message.contains("继承自 Peco"), "{message}");
    assert!(message.contains("无法删除"), "{message}");
    assert_eq!(
        read_providers(&env, "research"),
        before,
        "被拒的删除不得写盘"
    );

    // 系统层的同名条目同样不能删。
    let system = call(
        &router,
        Method::DELETE,
        "/api/teams/research/providers/sys-fallback",
        None,
    )
    .await;
    assert_eq!(system.status, 400, "body={}", system.body);
    assert_eq!(system.code(), "inherited_provider");
    assert!(
        system.body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("系统配置"),
        "body={}",
        system.body
    );
    assert_eq!(read_providers(&env, "research"), before);
}
