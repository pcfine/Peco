//! `PECO_WEBUI_DIST` 指向前端产物时的静态层。
//!
//! 生产只有一个入口：`/` 是前端，`/api` 与 `/uploads` 是接口。因此这里挂在
//! fallback 上，并且先判前缀再判扩展名 —— 接口的 404 语义不能被 SPA 回退吃掉，
//! 带 hash 的资源未命中也不能回 HTML（浏览器会把 HTML 当 JS 解析并报出误导
//! 到前端代码的语法错误）。

use std::convert::Infallible;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use tracing::warn;

/// dist 目录的环境变量名。部署脚本写进 unit 的 `Environment=`。
pub const WEBUI_DIST_ENV: &str = "PECO_WEBUI_DIST";

const INDEX_FILE: &str = "index.html";
const INDEX_URI: &str = "/index.html";

/// 首段命中即空 404 的前缀：这两棵子树的 404 属于接口，不属于前端路由。
///
/// 首段比较走「按 `/` 切段 + percent-decode + 忽略大小写」，故 `//api/x`、
/// `/%61pi/x`、`/API/x` 与 `/api/x` 等价；字符串 `starts_with` 会把它们漏给前端。
const API_PREFIXES: [&str; 2] = ["api", "uploads"];

/// 按 `PECO_WEBUI_DIST` 建静态层；没设置或 dist 不可用时返回 `None`，即不挂。
pub(crate) fn service_from_env() -> Option<Router> {
    let dist = PathBuf::from(std::env::var_os(WEBUI_DIST_ENV)?);
    if !dist.join(INDEX_FILE).is_file() {
        // 仓库搬走或 dist 没构建，是部署被破坏而不是启动前置条件不满足：
        // 记一条 warn、静态层缺席，由部署脚本的 status 判红，别让 daemon 起不来。
        warn!(dist = %dist.display(), "webui dist unusable; serving api only");
        return None;
    }
    Some(service(dist))
}

fn service(dist: PathBuf) -> Router {
    let files = ServeDir::new(&dist);
    Router::new().fallback_service(tower::service_fn(move |req: Request<Body>| {
        let files = files.clone();
        async move { Ok::<_, Infallible>(spa_response(files, req).await) }
    }))
}

async fn spa_response(files: ServeDir, req: Request<Body>) -> Response<Body> {
    let path = req.uri().path().to_owned();
    if is_api_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if has_extension(&path) {
        return serve(files, req).await;
    }

    // dist 里可能有 `/favicon` 这类无扩展名文件，所以先让 ServeDir 试一次，
    // 只有它回 404 才当成前端路由回 index.html。
    let (parts, body) = req.into_parts();
    let attempt = serve(files.clone(), Request::from_parts(parts.clone(), body)).await;
    if attempt.status() != StatusCode::NOT_FOUND {
        return attempt;
    }
    let mut retry = Request::from_parts(parts, Body::empty());
    *retry.uri_mut() = Uri::from_static(INDEX_URI);
    serve(files, retry).await
}

async fn serve(files: ServeDir, req: Request<Body>) -> Response<Body> {
    match files.oneshot(req).await {
        Ok(res) => res.map(Body::new),
        // `ServeDir` 的失败都已在内部折成响应，错误类型不可构造。
        Err(never) => match never {},
    }
}

fn is_api_path(path: &str) -> bool {
    let decoded = percent_decode(path);
    let first = decoded.split('/').find(|segment| !segment.is_empty());
    first.is_some_and(|segment| {
        API_PREFIXES
            .iter()
            .any(|prefix| segment.eq_ignore_ascii_case(prefix))
    })
}

/// 末段带点即有扩展名：那是资源请求，未命中回 404 而不是 index.html。
///
/// 末尾的 `/` 不算末段（`/a.b/` 与 `/a.b` 一样是资源请求），否则 `/a.b/`
/// 会先 307 到 `/a.b/`、再把无扩展名的路径回成 index.html。
fn has_extension(path: &str) -> bool {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// 只用于前缀判定的宽松解码：非法转义原样保留，路径解析的权威仍是 `ServeDir`。
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(high << 4 | low);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_prefixes_are_matched_on_the_first_decoded_segment() {
        for path in [
            "/api",
            "/api/",
            "/api/nope",
            "/uploads",
            "/uploads/nope",
            // 同一含义的其它写法：漏掉任何一个，接口的 404 就会变成 200 HTML。
            "//api/nope",
            "/%61pi/nope",
            "/api%2fnope",
            "/API/nope",
            "/Uploads/nope",
        ] {
            assert!(is_api_path(path), "{path} 属于接口前缀");
        }
        for path in ["/apidocs", "/uploadsx", "/spaces/api", "/", "/%2fspaces"] {
            assert!(!is_api_path(path), "{path} 不属于接口前缀");
        }
    }

    #[test]
    fn only_the_last_segment_decides_the_extension() {
        for path in ["/assets/index.js", "/favicon.svg", "/a.b", "/a.b/"] {
            assert!(has_extension(path), "{path} 是资源");
        }
        for path in ["/", "/spaces/ws_peco", "/a.b/c", "/api/peco", "//"] {
            assert!(!has_extension(path), "{path} 是前端路由");
        }
    }

    #[test]
    fn percent_decoding_keeps_invalid_escapes_verbatim() {
        assert_eq!(percent_decode("/%61pi/%2f"), "/api//");
        assert_eq!(percent_decode("/100%"), "/100%");
        assert_eq!(percent_decode("/%zz"), "/%zz");
        assert_eq!(percent_decode("/a%4"), "/a%4");
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }
}
