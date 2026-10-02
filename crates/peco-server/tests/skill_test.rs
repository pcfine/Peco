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
