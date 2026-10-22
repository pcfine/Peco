// ============================================================================
// 对话产物（artifact）集成测试 — 登记、列表、内容、Range、下载、归属隔离
// ============================================================================
//
// 覆盖两条边界：
// 1. 归属：他人产物一律 404（不泄露存在性）。
// 2. 路径：只有 `<data_dir>/work/<user_id>/output/` 之下可登记，
//    `projects/`、`tmp/` 与 `..` 逃逸一律拒绝。

mod common;

use common::TestApp;
use peco_core::artifacts::ArtifactDecl;
use peco_core::workdir::WorkDirs;
use serde_json::json;

async fn create_conversation(app: &TestApp) -> String {
    let resp = app
        .post("/api/conversations")
        .json(&json!({ "title": "产物测试" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

fn work_dirs(app: &TestApp) -> WorkDirs {
    WorkDirs::from_workspace_root(&app.state.workspace_manager.workspace_dir(&app.user_id))
        .expect("test workspace must follow <data_dir>/workspaces/<uid> layout")
}

/// 在 work 目录下写文件，返回绝对路径。
fn write_work_file(app: &TestApp, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
    // output/ 必须存在：路径校验对 output 做 canonicalize（前缀比较的前提）
    std::fs::create_dir_all(work_dirs(app).output()).unwrap();
    let path = work_dirs(app).root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

async fn register(
    app: &TestApp,
    conv_id: &str,
    abs: &std::path::Path,
    title: Option<&str>,
) -> Result<Option<peco_server::artifact::ArtifactItem>, String> {
    peco_server::artifact::persist_declaration(
        &app.state.db,
        &work_dirs(app),
        conv_id,
        &app.user_id,
        &ArtifactDecl {
            path: abs.to_string_lossy().into_owned(),
            title: title.map(str::to_string),
            mime: None,
        },
    )
    .await
}

// ── 登记 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn registers_output_file_and_derives_metadata() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(
        &app,
        "output/media-gen/2026-10-20/a.jpeg",
        b"\xff\xd8JPEG\xff\xd9",
    );

    let item = register(&app, &conv, &abs, Some("红狐狸"))
        .await
        .expect("must accept output file")
        .expect("first registration must produce an item");

    assert_eq!(item.kind, "image");
    assert_eq!(item.mime, "image/jpeg");
    assert_eq!(item.title, "红狐狸");
    assert_eq!(item.size_bytes, 8);
    assert!(item.available);
    assert_eq!(item.url, format!("/api/artifacts/{}/content", item.id));
}

#[tokio::test]
async fn title_falls_back_to_file_name() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/gen.mp4", b"mp4");

    let item = register(&app, &conv, &abs, None).await.unwrap().unwrap();
    assert_eq!(item.title, "gen.mp4");
    assert_eq!(item.kind, "video");
    assert_eq!(item.mime, "video/mp4");
}

#[tokio::test]
async fn duplicate_declaration_is_deduped() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");

    let first = register(&app, &conv, &abs, None).await.unwrap();
    assert!(first.is_some());
    let second = register(&app, &conv, &abs, None).await.unwrap();
    assert!(second.is_none(), "same path must not be registered twice");

    let listed = app
        .get(&format!("/api/chat/unknown/conversations/{conv}/artifacts"))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let body: serde_json::Value = listed.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ── 路径边界 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rejects_projects_dir() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "projects/site/index.html", b"<html>");

    let err = register(&app, &conv, &abs, None)
        .await
        .expect_err("projects/ must not be publishable");
    assert!(err.contains("outside output dir"), "unexpected: {err}");
}

#[tokio::test]
async fn rejects_tmp_dir() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "tmp/scratch.txt", b"x");

    let err = register(&app, &conv, &abs, None).await.unwrap_err();
    assert!(err.contains("outside output dir"), "unexpected: {err}");
}

#[tokio::test]
async fn rejects_escape_through_parent_segments() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    // 目录存在但目标在 output 之外：canonicalize 后前缀不匹配
    write_work_file(&app, "tmp/secret.txt", b"secret");
    let abs = work_dirs(&app).output().join("../tmp/secret.txt");

    let err = register(&app, &conv, &abs, None).await.unwrap_err();
    assert!(err.contains("outside output dir"), "unexpected: {err}");
}

/// 前缀比较必须按**路径组件**，不能是字符串前缀
/// （`output-evil/` 与 `output/` 字符串前缀相同，很容易被写错成 `str::starts_with`）。
#[tokio::test]
async fn rejects_sibling_dir_with_shared_string_prefix() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output-evil/a.png", b"PNG");

    let err = register(&app, &conv, &abs, None).await.unwrap_err();
    assert!(err.contains("outside output dir"), "unexpected: {err}");
}

#[tokio::test]
async fn rejects_relative_path() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let err = register(&app, &conv, std::path::Path::new("output/a.png"), None)
        .await
        .unwrap_err();
    assert!(err.contains("must be absolute"), "unexpected: {err}");
}

#[tokio::test]
async fn rejects_missing_file() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = work_dirs(&app).output().join("media-gen/nope.jpg");
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();

    let err = register(&app, &conv, &abs, None).await.unwrap_err();
    assert!(err.contains("not readable"), "unexpected: {err}");
}

// ── 端点 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn content_supports_range_requests() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let payload = b"0123456789abcdef";
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/v.mp4", payload);
    let item = register(&app, &conv, &abs, None).await.unwrap().unwrap();

    let full = app
        .get(&format!("/api/artifacts/{}/content", item.id))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(full.bytes().await.unwrap().as_ref(), payload);

    // 视频拖进度条依赖 206 + Content-Range
    let partial = app
        .get(&format!("/api/artifacts/{}/content", item.id))
        .header("Range", "bytes=2-5")
        .send()
        .await
        .unwrap();
    assert_eq!(partial.status(), 206);
    let range = partial
        .headers()
        .get("content-range")
        .expect("206 must carry Content-Range")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(range, format!("bytes 2-5/{}", payload.len()));
    assert_eq!(partial.bytes().await.unwrap().as_ref(), b"2345");
}

#[tokio::test]
async fn download_sets_attachment_with_encoded_title() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");
    let item = register(&app, &conv, &abs, Some("红狐狸.png"))
        .await
        .unwrap()
        .unwrap();

    let resp = app
        .get(&format!("/api/artifacts/{}/download", item.id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let disposition = resp
        .headers()
        .get("content-disposition")
        .expect("download must set Content-Disposition")
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.starts_with("attachment;"));
    // 中文走 RFC 5987 —— header 必须是 ASCII
    assert!(
        disposition.is_ascii(),
        "header must stay ASCII: {disposition}"
    );
    assert!(disposition.contains("filename*=UTF-8''%E7%BA%A2"));
}

#[tokio::test]
async fn content_is_inline_for_browser_rendering() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");
    let item = register(&app, &conv, &abs, None).await.unwrap().unwrap();

    let resp = app
        .get(&format!("/api/artifacts/{}/content", item.id))
        .send()
        .await
        .unwrap();
    let disposition = resp
        .headers()
        .get("content-disposition")
        .expect("inline disposition")
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.starts_with("inline;"));
}

#[tokio::test]
async fn other_user_gets_404() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");
    let item = register(&app, &conv, &abs, None).await.unwrap().unwrap();
    let (_, token2) = app.register_user2().await;

    let resp = app
        .get_as(&format!("/api/artifacts/{}/content", item.id), &token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn missing_file_is_reported_gone() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");
    let item = register(&app, &conv, &abs, None).await.unwrap().unwrap();

    std::fs::remove_file(&abs).unwrap();

    let resp = app
        .get(&format!("/api/artifacts/{}/content", item.id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 列表仍返回该产物，但标记为不可用 —— 元数据保留，文件可弃
    let listed: serde_json::Value = app
        .get(&format!("/api/chat/unknown/conversations/{conv}/artifacts"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["available"], false);
}

// ── 内容安全 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn declared_html_is_downgraded_inline() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(
        &app,
        "output/report/evil.html",
        b"<script>alert(1)</script>",
    );

    // 脚本可以声明任意 MIME —— 同源内联 text/html 就是存储型 XSS
    let item = peco_server::artifact::persist_declaration(
        &app.state.db,
        &work_dirs(&app),
        &conv,
        &app.user_id,
        &ArtifactDecl {
            path: abs.to_string_lossy().into_owned(),
            title: Some("evil.html".to_string()),
            mime: Some("text/html".to_string()),
        },
    )
    .await
    .unwrap()
    .unwrap();

    let resp = app
        .get(&format!("/api/artifacts/{}/content", item.id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        resp.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
}

// ── 顺序契约：先落库、后发事件 ───────────────────────────────────────────────

#[tokio::test]
async fn tool_result_event_persists_before_emitting() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.jpeg", b"JPEG");
    let decls = vec![ArtifactDecl {
        path: abs.to_string_lossy().into_owned(),
        title: Some("红狐狸".to_string()),
        mime: None,
    }];

    let ev = peco_server::artifact::build_tool_result_event(
        &app.state.db,
        Some(&work_dirs(&app)),
        &conv,
        &app.user_id,
        "call_1".to_string(),
        "shell".to_string(),
        "生成完成".to_string(),
        Vec::new(),
        &decls,
    )
    .await;

    let peco_server::chat::ChatSseEvent::ToolResult { artifacts, .. } = ev else {
        panic!("expected ToolResult event");
    };
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].title, "红狐狸");

    // 事件里的 id 必须已经在库里 —— 否则前端拿到的是不存在的凭据
    let stored = peco_server::db::artifacts::find_by_id_and_user(
        &app.state.db,
        &artifacts[0].id,
        &app.user_id,
    )
    .await
    .unwrap();
    assert!(stored.is_some(), "event id must already be persisted");
}

/// 登记失败（路径越界）时事件仍要发出，只是 artifacts 为空：
/// 产物登记是旁路副作用，不能否决主对话。
#[tokio::test]
async fn tool_result_event_degrades_when_declaration_rejected() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    write_work_file(&app, "tmp/secret.txt", b"secret");
    let decls = vec![ArtifactDecl {
        path: work_dirs(&app)
            .output()
            .join("../tmp/secret.txt")
            .to_string_lossy()
            .into_owned(),
        title: None,
        mime: None,
    }];

    let ev = peco_server::artifact::build_tool_result_event(
        &app.state.db,
        Some(&work_dirs(&app)),
        &conv,
        &app.user_id,
        "call_1".to_string(),
        "shell".to_string(),
        "done".to_string(),
        Vec::new(),
        &decls,
    )
    .await;

    let peco_server::chat::ChatSseEvent::ToolResult {
        artifacts, result, ..
    } = ev
    else {
        panic!("expected ToolResult event");
    };
    assert!(artifacts.is_empty(), "越界声明不得登记");
    assert_eq!(result, "done", "工具结果本身不受影响");
}

// ── 级联删除 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn deleting_conversation_removes_rows_but_keeps_files() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let abs = write_work_file(&app, "output/media-gen/2026-10-20/a.png", b"PNG");
    register(&app, &conv, &abs, None).await.unwrap().unwrap();

    let resp = app
        .delete(&format!("/api/conversations/{conv}"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete failed: {}",
        resp.status()
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE conversation_id = ?")
        .bind(&conv)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0, "行随会话级联删除");

    // 「产物长期保留」是文件层面的承诺；行没了，文件仍在工作层。
    // 这是有意的取舍：删会话等于放弃发现入口，磁盘交给用户自己清。
    assert!(abs.exists(), "文件不随会话删除");
}

#[tokio::test]
async fn list_requires_conversation_ownership() {
    let app = TestApp::new().await;
    let conv = create_conversation(&app).await;
    let (_, token2) = app.register_user2().await;

    let resp = app
        .get_as(
            &format!("/api/chat/unknown/conversations/{conv}/artifacts"),
            &token2,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
