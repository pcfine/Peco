//! `POST /api/knowledge/{id}/upload`：体积超限必须是可诊断的 413。
//!
//! axum 的 multipart 提取器默认只放行 2 MB 请求体，超限错误若不翻译，
//! 用户只会看到「Error parsing multipart/form-data request」——看不出是体积问题。

mod support;

use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use peco_daemon::knowledge::{MAX_DOC_BYTES, MAX_UPLOAD_BODY_BYTES};
use serde_json::{Value, json};
use support::new_env;
use tower::ServiceExt;

const BOUNDARY: &str = "peco-test-boundary";

/// `file` 字段占 `file_len` 字节的 multipart 请求体。
///
/// 按长度直接铺字节而不是先造文件内容再拼接：50 MB 量级的用例只应有一份缓冲。
fn multipart_body_with_file_of_len(file_len: usize) -> Vec<u8> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.md\"\r\n\
         Content-Type: text/markdown\r\n\r\n"
    )
    .into_bytes();
    body.resize(body.len() + file_len, b'a');
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn upload(
    env: &support::TestEnv,
    ws_id: &str,
    kb_id: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/knowledge/{kb_id}/upload"))
        .header("authorization", support::bearer_header())
        .header("x-peco-space", ws_id)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();

    let response = env.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// `env.home` 下是否存在名为 `name` 的文件（超限时不得落盘）。
fn contains_file(dir: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if contains_file(&path, name) {
                return true;
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return true;
        }
    }
    false
}

/// 文件体超过文件上限（也顶掉请求体上限）⇒ 413 `file_too_large`，
/// 文案带上限字节数，且不落盘、不写 `documents` 行。
///
/// 变异方法（实测过）：把 `multipart_read_error` 换回原样透出 —— 状态变 400、
/// `code` 缺失，两条断言都红。去掉路由上的 `DefaultBodyLimit` 不会让本用例变红
/// （2 MB 默认值同样被这个函数翻译成 413），那条回归由
/// `upload_above_the_axum_default_limit_is_accepted` 覆盖。
#[tokio::test]
async fn oversized_upload_is_413_with_the_limit_in_the_message() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let created = env
        .post_in(&ws_id, "/api/knowledge", json!({"name": "kb-big"}))
        .await;
    assert_eq!(created.status, 201, "建库失败: {}", created.body);
    let kb_id = created.body["id"].as_str().expect("id").to_string();

    let (status, body) = upload(
        &env,
        &ws_id,
        &kb_id,
        multipart_body_with_file_of_len(MAX_UPLOAD_BODY_BYTES + 1),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body={body}");
    assert_eq!(body["code"], json!("file_too_large"), "body={body}");
    let details = body["details"].as_str().unwrap_or_default();
    assert!(
        details.contains(&MAX_DOC_BYTES.to_string()),
        "文案必须给出上限: {body}"
    );
    assert!(
        !details.contains("Error parsing"),
        "不得原样透出 multipart 解析错误: {body}"
    );

    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "超限不得写 documents 行");
    assert!(
        !contains_file(&env.home, "big.md"),
        "超限不得落盘到 {}",
        env.home.display()
    );
}

/// 超过 axum 默认 2 MB 请求体上限、但在 50 MB 以内的文件必须被接受：
/// 路由没有抬高 `DefaultBodyLimit` 时这里会是 413，正是原来的缺陷。
///
/// 变异方法（实测过）：去掉 `/{id}/upload` 上的 `DefaultBodyLimit` 层 ——
/// 5 MB 请求体在提取阶段就被 2 MB 默认值截断，本用例红。
#[tokio::test]
async fn upload_above_the_axum_default_limit_is_accepted() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let created = env
        .post_in(&ws_id, "/api/knowledge", json!({"name": "kb-medium"}))
        .await;
    assert_eq!(created.status, 201, "建库失败: {}", created.body);
    let kb_id = created.body["id"].as_str().expect("id").to_string();

    // 5 MB：远大于 axum multipart 的 2 MB 默认值，仍在上限之内
    let file_len = 5 * 1024 * 1024;
    let (status, body) = upload(
        &env,
        &ws_id,
        &kb_id,
        multipart_body_with_file_of_len(file_len),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(body["file_size"], json!(file_len), "body={body}");
    assert_eq!(body["status"], json!("pending"), "body={body}");

    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "被接受的上传必须落一行 documents");
}

/// 文件本身超过 50 MB、请求体仍在 51 MB 以内的档位：由 `check_doc_size` 拒绝。
///
/// body 层截断时 handler 走不到写盘，「不落盘、不写行」永远是恒真的；只有
/// 载荷卡在两个上限之间，这两条断言才可能失败。
#[tokio::test]
async fn file_over_the_doc_limit_leaves_no_file_and_no_row() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let created = env
        .post_in(&ws_id, "/api/knowledge", json!({"name": "kb-over-file"}))
        .await;
    assert_eq!(created.status, 201, "建库失败: {}", created.body);
    let kb_id = created.body["id"].as_str().expect("id").to_string();

    let file_len = MAX_DOC_BYTES + 1;
    // 用例前提：请求体必须仍在上限之内，否则先被 body 层截断。
    assert!(file_len < MAX_UPLOAD_BODY_BYTES);

    let (status, body) = upload(
        &env,
        &ws_id,
        &kb_id,
        multipart_body_with_file_of_len(file_len),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body={body}");
    assert_eq!(body["code"], json!("file_too_large"), "body={body}");
    let details = body["details"].as_str().unwrap_or_default();
    assert!(
        details.starts_with("file too large: ") && details.contains(&file_len.to_string()),
        "文案须带实际体积与上限: {body}"
    );

    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "超限不得写 documents 行");
    assert!(
        !contains_file(&env.home, "big.md"),
        "超限不得落盘到 {}",
        env.home.display()
    );
}

/// 非体积类的 multipart 错误要说明是解析失败，而不是吞成「file field is required」。
#[tokio::test]
async fn malformed_multipart_reports_read_failure() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let created = env
        .post_in(&ws_id, "/api/knowledge", json!({"name": "kb-malformed"}))
        .await;
    assert_eq!(created.status, 201, "建库失败: {}", created.body);
    let kb_id = created.body["id"].as_str().expect("id").to_string();

    let (status, body) = upload(&env, &ws_id, &kb_id, b"not a multipart body".to_vec()).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let details = body["details"].as_str().unwrap_or_default();
    assert!(
        details.starts_with("failed to read uploaded file: "),
        "应报解析失败而非字段缺失: {body}"
    );
}
