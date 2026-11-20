//! 前端产物托管：SPA fallback 的路由契约。
//!
//! 唯一入口 `/` 必须给出前端，`/api` 与 `/uploads` 的 404 语义必须保持空体，
//! 带 hash 的资源未命中必须 404 —— 三者任一被 fallback 吃掉，前端拿到的就是
//! 一段 HTML，报错位置离真相很远。

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use peco_daemon::http::static_files::WEBUI_DIST_ENV;
use tower::ServiceExt;

/// `PECO_WEBUI_DIST` 是进程级变量：本文件里改它的用例必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 静态层在 `router()` 构造时就定下了 dist，故只有「设变量 + 建路由」这一段需要
/// 串行；请求阶段读不到这个变量，不必让锁跨 await。
fn router_with_dist(env: &support::TestEnv, dist: Option<&Path>) -> Router {
    let _serial = serial();
    match dist {
        Some(dir) => {
            let _dist_env = DistEnv::set(dir);
            env.router()
        }
        None => {
            let _dist_env = DistEnv::unset();
            env.router()
        }
    }
}

struct DistEnv(Option<OsString>);

impl DistEnv {
    fn set(dir: &Path) -> Self {
        let previous = std::env::var_os(WEBUI_DIST_ENV);
        unsafe { std::env::set_var(WEBUI_DIST_ENV, dir) };
        Self(previous)
    }

    fn unset() -> Self {
        let previous = std::env::var_os(WEBUI_DIST_ENV);
        unsafe { std::env::remove_var(WEBUI_DIST_ENV) };
        Self(previous)
    }
}

impl Drop for DistEnv {
    fn drop(&mut self) {
        match self.0.take() {
            Some(raw) => unsafe { std::env::set_var(WEBUI_DIST_ENV, raw) },
            None => unsafe { std::env::remove_var(WEBUI_DIST_ENV) },
        }
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
    content_type: String,
    cors: Option<String>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// 匿名请求：静态层必须能这样访问（登录页自己要先打得开）。
async fn get(router: &Router, uri: &str) -> Reply {
    get_with(router, uri, Request::builder()).await
}

/// 带 Bearer 令牌：`/api` 与 `/uploads` 的路由契约要在过闸门之后才看得到。
async fn get_api(router: &Router, uri: &str) -> Reply {
    get_with(router, uri, support::bearer(Request::builder())).await
}

async fn get_with(router: &Router, uri: &str, builder: support::RequestBuilder) -> Reply {
    let request = builder
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.expect("路由不会失败");
    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let cors = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    Reply {
        status,
        body,
        content_type,
        cors,
    }
}

const INDEX_BODY: &str = "<div id=\"root\"></div>";

fn dist_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), INDEX_BODY).unwrap();
    std::fs::create_dir_all(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("assets/app.js"), "console.log(1)").unwrap();
    // 无扩展名的真实文件与前端路由同形，只有先试 ServeDir 才分得开。
    std::fs::write(dir.path().join("robots"), "User-agent: *").unwrap();
    dir
}

async fn router_with_site() -> (support::TestEnv, tempfile::TempDir) {
    let env = support::new_env(&[]).await;
    let dist = dist_fixture();
    (env, dist)
}

#[tokio::test]
async fn index_and_extensionless_deep_links_serve_the_spa() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    for uri in ["/", "/spaces/ws_peco"] {
        let reply = get(&router, uri).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri} 应回前端入口");
        assert_eq!(reply.text(), INDEX_BODY, "{uri} 应回 index.html");
    }
}

/// 带扩展名的未命中回 404：回 HTML 会让浏览器把 HTML 当 JS 解析。
#[tokio::test]
async fn a_missing_asset_is_not_replaced_by_the_spa() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    let hit = get(&router, "/assets/app.js").await;
    assert_eq!(hit.status, StatusCode::OK);
    assert!(
        hit.content_type.contains("javascript"),
        "资源要带正确 Content-Type，实际 {}",
        hit.content_type
    );

    let miss = get(&router, "/assets/nope.js").await;
    assert_eq!(miss.status, StatusCode::NOT_FOUND, "命中率不该回 HTML");
    assert!(miss.body.is_empty(), "未命中的资源应是空体");
}

/// 末段带点且文件不存在就是 404，末尾带不带 `/` 都一样：`/a.b` 既不能 307 到
/// `/a.b/`，也不能被 SPA 回退成 index.html。
#[tokio::test]
async fn a_dotted_path_that_does_not_exist_is_404_with_or_without_a_trailing_slash() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    for uri in ["/a.b", "/a.b/", "/assets/nope.js"] {
        let reply = get(&router, uri).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{uri} 应回 404，实际 {}（重定向会让前端把 HTML 当资源）",
            reply.status
        );
        assert!(reply.body.is_empty(), "{uri} 的响应体必须为空");
    }
}

/// 无扩展名的真实文件优先于 SPA 回退：先试 ServeDir，404 才回 index.html。
#[tokio::test]
async fn an_extensionless_file_in_dist_beats_the_spa_fallback() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    let reply = get(&router, "/robots").await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "dist 里有的文件不该回前端入口"
    );
    assert_eq!(reply.text(), "User-agent: *");
}

/// 前端产物公开、接口不公开，且两者都不再带跨域头（同源部署 + vite 代理）。
///
/// 变异方法：把闸门改成罩住整棵路由（含 fallback）⇒ 匿名 `/` 变成 401，
/// 「登录页自己要先打得开」这条断言变红。
#[tokio::test]
async fn static_responses_are_public_and_nobody_ships_cors_headers() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    let index = get(&router, "/").await;
    assert_eq!(index.status, StatusCode::OK, "前端入口不得要求认证");
    assert_eq!(index.text(), INDEX_BODY);
    assert_eq!(index.cors, None, "不再下发跨域头");

    let api = get_api(&router, "/api/peco").await;
    assert_eq!(api.status, StatusCode::OK);
    assert_eq!(api.cors, None, "接口同样不下发跨域头");

    let anonymous = get(&router, "/api/peco").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "接口要认证");
}

/// `/api` 与 `/uploads` 的 404 必须保持「空体」，不能被 fallback 换成 HTML。
#[tokio::test]
async fn api_and_uploads_misses_stay_empty_404() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    for uri in ["/api/nope", "/uploads/nope"] {
        let reply = get_api(&router, uri).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(reply.body.len(), 0, "{uri} 的响应体必须为空");
    }

    let alive = get_api(&router, "/api/peco").await;
    assert_eq!(alive.status, StatusCode::OK, "真实接口不受静态层影响");
}

/// 前缀守卫要按规范化后的首段判定：多斜杠、percent-encode、大小写都是同一含义。
#[tokio::test]
async fn api_prefix_guards_survive_normalization_tricks() {
    let (env, dist) = router_with_site().await;
    let router = router_with_dist(&env, Some(dist.path()));

    for uri in ["//api/nope", "/%61pi/nope", "/api%2fnope", "/API/nope"] {
        let reply = get_api(&router, uri).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri} 属于接口前缀");
        assert!(reply.body.is_empty(), "{uri} 的响应体必须为空");
    }

    // 真实接口在守卫之外：判定放宽不能把它一起挡掉。
    assert_eq!(get_api(&router, "/api/peco").await.status, StatusCode::OK);
    assert_eq!(get(&router, "/spaces/ws_peco").await.status, StatusCode::OK);
}

/// 未设 `PECO_WEBUI_DIST` 时行为与没有静态层时逐条一致。
#[tokio::test]
async fn without_the_env_var_every_path_is_404() {
    let env = support::new_env(&[]).await;
    let router = router_with_dist(&env, None);

    let reply = get(&router, "/").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get_api(&router, "/api/peco").await.status,
        StatusCode::OK,
        "接口与静态层是否挂载无关"
    );
}

/// dist 半残（有 assets 但缺 index.html）时整层不挂：只做一个「目录不存在」的
/// 否定断言区分不出「层没挂」与「层挂了但文件没有」，故造一个引用得上的资源。
#[tokio::test]
async fn an_unusable_dist_leaves_the_static_layer_off() {
    let env = support::new_env(&[]).await;
    let broken = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(broken.path().join("assets")).unwrap();
    std::fs::write(broken.path().join("assets/app.js"), "console.log(1)").unwrap();
    let router = router_with_dist(&env, Some(broken.path()));

    let asset = get(&router, "/assets/app.js").await;
    assert_eq!(
        asset.status,
        StatusCode::NOT_FOUND,
        "缺 index.html 时静态层必须整层缺席，不能只把入口关掉"
    );
    assert_eq!(
        get(&router, "/").await.status,
        StatusCode::NOT_FOUND,
        "dist 不可用时 `/` 仍是 404"
    );
    assert_eq!(get_api(&router, "/api/peco").await.status, StatusCode::OK);
}
