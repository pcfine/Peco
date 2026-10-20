// ============================================================================
// Skill 集成测试 — REST 校验闸门 + Tier-3 资源文件写入
//
// 覆盖两处修复：
//   1. PUT /skills/{name} 与 POST /skills/import 不再裸写文件，统一走
//      SkillRegister 的「校验 → 原子写 → 刷缓存」路径；
//   2. SKILL.md 可携带 files[] 一并写入 scripts / references / assets。
// ============================================================================

mod common;

use common::TestApp;
use serde_json::json;

const DEMO_SKILL: &str = "---\nname: demo\ndescription: A demo skill for integration tests\n---\n\n# Demo\n\nDo things.\n";

fn skills_dir(app: &TestApp) -> std::path::PathBuf {
    app.state
        .workspace_manager
        .workspace_dir(&app.user_id)
        .join("skills")
}

// ── 校验闸门 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_upsert_rejects_invalid_name() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/skills/Bad Name")
        .json(&json!({ "content": DEMO_SKILL }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 目录名 / frontmatter name 不一致
    let resp = app
        .put("/api/skills/other")
        .json(&json!({ "content": DEMO_SKILL }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 无 frontmatter 的裸 Markdown
    let resp = app
        .put("/api/skills/raw")
        .json(&json!({ "content": "# just markdown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_upsert_rejects_resource_traversal_without_partial_write() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/skills/demo")
        .json(&json!({
            "content": DEMO_SKILL,
            "files": [{ "path": "../evil.txt", "content": "pwned" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 全部校验先于任何写入 —— SKILL.md 也不应落盘
    let resp = app.get("/api/skills/demo").send().await.unwrap();
    assert_eq!(resp.status(), 404);

    let dir = skills_dir(&app);
    assert!(!dir.join("evil.txt").exists());
    assert!(!dir.join("demo/SKILL.md").exists());
}

// ── 资源文件写入 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_upsert_writes_skill_and_resource_files() {
    let app = TestApp::new().await;

    let resp = app
        .put("/api/skills/demo")
        .json(&json!({
            "content": DEMO_SKILL,
            "files": [
                { "path": "scripts/run.sh", "content": "#!/bin/sh\necho hi\n" },
                { "path": "references/notes.md", "content": "# Notes\n" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let dir = skills_dir(&app);
    assert!(dir.join("demo/SKILL.md").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.join("demo/references/notes.md")).unwrap(),
        "# Notes\n"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("demo/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "scripts must be executable");
    }

    // 缓存已在写入后刷新 —— 列表立即可见
    let resp = app.get("/api/skills").send().await.unwrap();
    let list: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|s| s["name"] == "demo"));
}

#[tokio::test]
async fn test_import_requires_name_and_content() {
    let app = TestApp::new().await;

    // 缺少必填的 name
    let resp = app
        .post("/api/skills/import")
        .json(&json!({ "content": DEMO_SKILL }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);

    // 内容为空
    let resp = app
        .post("/api/skills/import")
        .json(&json!({ "name": "demo", "content": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 合法导入
    let resp = app
        .post("/api/skills/import")
        .json(&json!({
            "name": "demo",
            "content": DEMO_SKILL,
            "files": [{ "path": "assets/logo.txt", "content": "logo" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(skills_dir(&app).join("demo/assets/logo.txt").is_file());
}

// ── 目录文件读取 ────────────────────────────────────────────────────────────

/// 准备一个带嵌套资源与二进制的 skill 目录（直接落盘，绕开写入 API 的白名单）。
fn seed_skill_tree(app: &TestApp) -> std::path::PathBuf {
    let dir = skills_dir(app).join("demo");
    std::fs::create_dir_all(dir.join("scripts/__pycache__")).unwrap();
    std::fs::create_dir_all(dir.join("references")).unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::fs::write(dir.join("SKILL.md"), DEMO_SKILL).unwrap();
    std::fs::write(dir.join("scripts/run.py"), "print('hi')\n").unwrap();
    std::fs::write(dir.join("scripts/__pycache__/run.pyc"), [0u8, 1, 2, 3]).unwrap();
    std::fs::write(dir.join("references/notes.md"), "# Notes\n").unwrap();
    std::fs::write(dir.join(".env"), "TOKEN=secret\n").unwrap();
    std::fs::write(dir.join("state/cache.json"), "{\"a\":1}").unwrap();
    dir
}

#[tokio::test]
async fn test_list_files_reports_tree_and_write_boundary() {
    let app = TestApp::new().await;
    seed_skill_tree(&app);

    let resp = app.get("/api/skills/demo/files").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let files = body["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();

    // 根级 SKILL.md 不重复列出
    assert!(!paths.contains(&"SKILL.md"));
    assert!(paths.contains(&"scripts/run.py"));
    assert!(paths.contains(&"scripts/__pycache__/run.pyc"));
    assert!(paths.contains(&"references/notes.md"));
    assert!(paths.contains(&"state/cache.json"));
    assert!(paths.contains(&".env"));
    // 升序
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);

    let find = |p: &str| files.iter().find(|f| f["path"] == p).unwrap();
    assert_eq!(find("scripts/run.py")["writable"], true);
    assert_eq!(find("references/notes.md")["writable"], true);
    assert_eq!(find("scripts/__pycache__/run.pyc")["writable"], false);
    assert_eq!(find("state/cache.json")["writable"], false);
    assert_eq!(find("scripts/run.py")["kind"], "code");
    assert_eq!(find("references/notes.md")["kind"], "markdown");
    assert_eq!(find("scripts/__pycache__/run.pyc")["kind"], "binary");
    assert_eq!(find(".env")["kind"], "env");
    assert_eq!(find("state/cache.json")["kind"], "data");
    assert!(
        find("state/cache.json")["mtime"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
}

#[tokio::test]
async fn test_read_file_returns_content_and_metadata() {
    let app = TestApp::new().await;
    seed_skill_tree(&app);

    let resp = app
        .get("/api/skills/demo/files?path=scripts/run.py")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["path"], "scripts/run.py");
    assert_eq!(body["content"], "print('hi')\n");
    assert_eq!(body["truncated"], false);
    assert_eq!(body["size"], 12);
    assert_eq!(body["kind"], "code");
}

#[tokio::test]
async fn test_read_file_rejects_traversal_and_binary() {
    let app = TestApp::new().await;
    let dir = seed_skill_tree(&app);

    for bad in [
        "../demo/SKILL.md",
        "/etc/passwd",
        "scripts/../SKILL.md",
        "./scripts/run.py",
        "scripts\\run.py",
    ] {
        let resp = app
            .get(&format!("/api/skills/demo/files?path={bad}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "path {bad} should be rejected");
    }

    // 二进制 → 415
    let resp = app
        .get("/api/skills/demo/files?path=scripts/__pycache__/run.pyc")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 415);

    // 不存在 → 404
    let resp = app
        .get("/api/skills/demo/files?path=scripts/nope.py")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 目录 → 400
    let resp = app
        .get("/api/skills/demo/files?path=scripts")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 未知 skill → 404
    let resp = app.get("/api/skills/none/files").send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert!(dir.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn test_read_file_rejects_symlink_escape() {
    let app = TestApp::new().await;
    let dir = seed_skill_tree(&app);
    std::os::unix::fs::symlink("/etc/passwd", dir.join("scripts/escape.py")).unwrap();

    // 符号链接不出现在清单里
    let resp = app.get("/api/skills/demo/files").send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let paths: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(!paths.contains(&"scripts/escape.py"));

    // 直接请求也被拦截
    let resp = app
        .get("/api/skills/demo/files?path=scripts/escape.py")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_read_file_truncates_over_limit() {
    let app = TestApp::new().await;
    let dir = seed_skill_tree(&app);
    let big = "a".repeat(300 * 1024);
    std::fs::write(dir.join("references/big.md"), &big).unwrap();

    let resp = app
        .get("/api/skills/demo/files?path=references/big.md")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["truncated"], true);
    assert_eq!(body["size"], 300 * 1024);
    assert_eq!(body["content"].as_str().unwrap().len(), 256 * 1024);
}

#[tokio::test]
async fn test_files_rejects_traversal_in_skill_name() {
    let app = TestApp::new().await;
    seed_skill_tree(&app);

    // axum 会把 %2F 解码进 {name}，`..%2F..%2Fetc` 即 `../../etc`。
    let resp = app
        .get("/api/skills/..%2F..%2Fetc/files")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let resp = app
        .get("/api/skills/%2e%2e%2fdemo/files?path=SKILL.md")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ── 兄弟端点名称闸门 ────────────────────────────────────────────────────────

/// `get` / `export` / `delete` 与 `files` 一样暴露 `{name}`，必须同样校验，
/// 否则 `..%2F..%2F<dir>` 会逃出 skills 根目录——读他人内容、删任意目录。
#[tokio::test]
async fn test_get_export_delete_reject_traversal_in_skill_name() {
    let app = TestApp::new().await;

    // skills_dir 的爷爷目录：`../../<name>` 正好落在这里。
    let escape_target = skills_dir(&app)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("escape-{}", &app.user_id[..8]));
    std::fs::create_dir_all(&escape_target).unwrap();
    std::fs::write(escape_target.join("SKILL.md"), DEMO_SKILL).unwrap();
    std::fs::write(escape_target.join("keep.txt"), "keep").unwrap();

    let encoded = format!("..%2F..%2Fescape-{}", &app.user_id[..8]);

    let resp = app
        .get(&format!("/api/skills/{encoded}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "GET 应拒绝越界名称");

    let resp = app
        .get(&format!("/api/skills/{encoded}/export"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "export 应拒绝越界名称");

    let resp = app
        .delete(&format!("/api/skills/{encoded}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "DELETE 应拒绝越界名称");
    assert!(
        escape_target.join("keep.txt").is_file(),
        "越界名称不得删除 skills 根之外的目录"
    );
}
