//! `/api/spaces/{ws_id}/files` —— 空间工作目录的只读浏览。
//!
//! 重点不在「能不能列目录」，而在**路径越界的唯一判据**：规范化后是否仍在
//! `work_dir` 之下。三条越界路径（`..` / 绝对路径 / 外部符号链接）都归到
//! `400 invalid_path`，前端不必为每种逃逸各写一个分支。

mod support;

use std::path::Path;

use serde_json::json;
use support::new_env;

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
}

async fn opened_space(name: &str) -> (support::TestEnv, String, std::path::PathBuf) {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space(name, "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    (env, ws_id, work_dir)
}

/// 根目录列表：目录在前、同类按名称升序；文件的 `size_bytes` 是真实字节数。
///
/// 变异：去掉 `kind != "dir"` 这一层比较（改成纯名称排序）→ 目录 `zzz` 落到
/// 末尾，首条断言变红。
#[tokio::test]
async fn root_listing_puts_dirs_first_then_names_ascending() {
    let (env, ws_id, work_dir) = opened_space("files-root").await;
    write(&work_dir, "README.md", b"# hi\n");
    write(&work_dir, "a.txt", b"a");
    std::fs::create_dir_all(work_dir.join("zzz")).unwrap();

    let reply = env.get(&format!("/api/spaces/{ws_id}/files")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["path"], json!(""));
    assert_eq!(reply.body["truncated"], json!(false));

    // `.workspace.json` 是空间自己的记录文件，就在目录里 —— 浏览不特判它，
    // 否则「目录里有什么」与 `ls` 的回答不一致。
    let names: Vec<&str> = reply.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["zzz", ".workspace.json", "README.md", "a.txt"],
        "目录在前，名称升序"
    );

    let entries = reply.body["entries"].as_array().unwrap();
    let readme = entries
        .iter()
        .find(|entry| entry["name"] == json!("README.md"))
        .unwrap();
    assert_eq!(readme["kind"], json!("file"));
    assert_eq!(readme["size_bytes"], json!(5));
    // 与库里的 `datetime('now')` 同形：前端的相对时间解析只认这一种。
    let modified = readme["modified_at"].as_str().unwrap();
    assert_eq!(modified.len(), 19, "YYYY-MM-DD HH:MM:SS: {modified}");
    assert_eq!(entries[0]["kind"], json!("dir"));
    assert!(entries[0].get("size_bytes").is_none(), "目录不给字节数");
}

/// 子目录按 `path` 列出，`path` 原样回显 —— 面包屑直接用它，不必前端再拼。
#[tokio::test]
async fn subdirectory_listing_echoes_the_relative_path() {
    let (env, ws_id, work_dir) = opened_space("files-sub").await;
    write(&work_dir, "src/main.rs", b"fn main() {}");
    write(&work_dir, "src/lib.rs", b"pub fn f() {}");

    let reply = env
        .get(&format!("/api/spaces/{ws_id}/files?path=src"))
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["path"], json!("src"));
    let names: Vec<&str> = reply.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["lib.rs", "main.rs"]);
}

/// 指向文件 ⇒ 回正文；`max_bytes` 截断只影响 `text`，`size_bytes` 仍是全量。
#[tokio::test]
async fn file_body_is_text_and_max_bytes_truncates_it() {
    let (env, ws_id, work_dir) = opened_space("files-text").await;
    write(&work_dir, "notes.txt", b"hello world");

    let full = env
        .get(&format!("/api/spaces/{ws_id}/files?path=notes.txt"))
        .await;
    assert_eq!(full.status, 200, "body={}", full.body);
    assert_eq!(full.body["kind"], json!("file"));
    assert_eq!(full.body["text"], json!("hello world"));
    assert_eq!(full.body["size_bytes"], json!(11));
    assert_eq!(full.body["truncated"], json!(false));
    assert_eq!(full.body["binary"], json!(false));

    let cut = env
        .get(&format!(
            "/api/spaces/{ws_id}/files?path=notes.txt&max_bytes=4"
        ))
        .await;
    assert_eq!(cut.body["text"], json!("hell"));
    assert_eq!(cut.body["truncated"], json!(true));
    assert_eq!(cut.body["size_bytes"], json!(11), "截断不改变全量大小");
}

/// 含 NUL 的文件是二进制：只给 `binary:true`，不给 `text`。
#[tokio::test]
async fn binary_file_has_no_text() {
    let (env, ws_id, work_dir) = opened_space("files-binary").await;
    write(&work_dir, "blob.bin", &[0u8, 1, 2, 3]);

    let reply = env
        .get(&format!("/api/spaces/{ws_id}/files?path=blob.bin"))
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["binary"], json!(true));
    assert!(reply.body.get("text").is_none(), "二进制不给正文");
    assert_eq!(reply.body["size_bytes"], json!(4));
}

/// `..` 越界 ⇒ `400 invalid_path`，且响应体不含目标文件内容。
///
/// 变异：去掉 `resolve_within` 里 `target.starts_with(&root)` 这一句 →
/// `canonicalize` 直接把 `../../secret.txt` 解析成外部文件，200 且正文泄露，
/// 本测试红。
#[tokio::test]
async fn parent_dir_escape_is_invalid_path() {
    let (env, ws_id, work_dir) = opened_space("files-escape").await;
    // work_dir = <home>/spaces/<name>/work ⇒ ../../../ = <home>/.. 之外。
    let secret = work_dir.join("../../../secret.txt");
    std::fs::write(&secret, b"TOP-SECRET").unwrap();

    let reply = env
        .get(&format!(
            "/api/spaces/{ws_id}/files?path=../../../secret.txt"
        ))
        .await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_path");
    assert!(
        !reply.body.to_string().contains("TOP-SECRET"),
        "错误体不得回显外部文件内容"
    );
}

/// 绝对路径即使落在 work_dir 内也是非法参数：请求体只接受相对路径，
/// 否则前端拿到的 `path` 就有两种写法。
#[tokio::test]
async fn absolute_path_is_invalid_path() {
    let (env, ws_id, work_dir) = opened_space("files-abs").await;
    write(&work_dir, "README.md", b"# hi\n");

    let uri = format!(
        "/api/spaces/{ws_id}/files?path={}",
        work_dir.join("README.md").to_string_lossy()
    );
    let reply = env.get(&uri).await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_path");
}

/// 指向外部的符号链接与 `..` 同罪：路径字符串本身是干净的，逃逸在链接目标上。
#[cfg(unix)]
#[tokio::test]
async fn symlink_pointing_outside_is_invalid_path() {
    let (env, ws_id, work_dir) = opened_space("files-symlink").await;
    let secret = env.home.join("outside.txt");
    std::fs::write(&secret, b"OUTSIDE").unwrap();
    std::os::unix::fs::symlink(&secret, work_dir.join("link.txt")).unwrap();

    let reply = env
        .get(&format!("/api/spaces/{ws_id}/files?path=link.txt"))
        .await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_path");
}

/// 不存在的路径与越界同归一类：都是「这个空间里没有这个东西」，
/// 不给前端多一个需要分支的错误码。
#[tokio::test]
async fn missing_path_is_invalid_path() {
    let (env, ws_id, _work_dir) = opened_space("files-missing").await;

    let reply = env
        .get(&format!("/api/spaces/{ws_id}/files?path=nope.txt"))
        .await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "invalid_path");
}

/// 空间未打开 ⇒ `409 space_not_open`，与其它空间作用域接口一致。
#[tokio::test]
async fn closed_space_is_409_space_not_open() {
    let (env, ws_id, _work_dir) = opened_space("files-closed").await;
    let closed = env
        .post(
            &format!("/api/spaces/{ws_id}/close"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let reply = env.get(&format!("/api/spaces/{ws_id}/files")).await;
    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_open");
}

/// 单层超过 1000 项 ⇒ `truncated:true` 且只回 1000 条。
///
/// 变异：去掉 `entries.truncate(MAX_ENTRIES)` → 条数变 1005，本测试红。
#[tokio::test]
async fn listing_truncates_beyond_one_thousand_entries() {
    let (env, ws_id, work_dir) = opened_space("files-many").await;
    for index in 0..1005 {
        std::fs::write(work_dir.join(format!("f{index:04}.txt")), b"").unwrap();
    }

    let reply = env.get(&format!("/api/spaces/{ws_id}/files")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["truncated"], json!(true));
    assert_eq!(reply.body["entries"].as_array().unwrap().len(), 1000);
}
