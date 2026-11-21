//! 工作空间回收站：A6 移入、A7 不可见/不可打开、A8 恢复、A9 同目录重建、
//! A10 永久销毁、A11 上限 100、A12 保留空间。
//!
//! 所有断言都围绕两组不变量：回收站只挂起（会话、记忆、目录文件都留着），销毁才
//! 删会话与记忆 —— 而工作目录里的用户文件在任何一条路径上都不删。

mod support;

use std::path::{Path, PathBuf};

use peco_core::agent::DynamicContext;
use peco_core::tools::KnowledgeAccess;
use peco_daemon::db::workspaces;
use peco_daemon::dynamic::SpaceCatalogContext;
use peco_daemon::space::manifest;
use peco_daemon::space::peco::{PECO_TEAM_ID, resolve as resolve_peco};
use serde_json::json;
use support::{TestEnv, new_env};

/// 空间的记忆根。
fn memory_root(env: &TestEnv, ws_id: &str) -> PathBuf {
    env.home.join("memory").join(ws_id)
}

/// 该空间记忆库的配置文件。
fn memory_config(env: &TestEnv, ws_id: &str) -> PathBuf {
    memory_root(env, ws_id).join("@private_memory/kb_config.json")
}

/// 走 `POST .../conversation/ensure` 建出空间会话（唤醒投递用的同一条）。
async fn ensure_conversation(env: &TestEnv, ws_id: &str) -> String {
    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/ensure"),
            json!({}),
        )
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    reply.body["conversation_id"].as_str().unwrap().to_string()
}

/// 八张子表各播一行：销毁用例要能逐张断言清零。
async fn seed_history(env: &TestEnv, conv_id: &str) {
    let statements = [
        "INSERT INTO messages (id, conversation_id, role, content) \
         VALUES ('m1', ?, 'user', 'hello')",
        "INSERT INTO session_snapshots (conversation_id, session_id, description, created_at, \
         snapshot_json) VALUES (?, 's1', 'd', 0, '{}')",
        "INSERT INTO conversation_inflight_turns (conversation_id, session_id, turn_index, \
         reason, payload_json) VALUES (?, 's1', 0, 'tool', '{}')",
        "INSERT INTO usage_logs (id, user_id, agent_name, conversation_id) \
         VALUES ('u1', 'local', '@master', ?)",
        "INSERT INTO peco_compaction_log (id, user_id, conversation_id, evicted_turns, \
         tokens_before, tokens_after, summary_chars) VALUES ('c1', 'local', ?, 1, 10, 5, 3)",
        "INSERT INTO peco_session_archives (id, user_id, conversation_id, turn_count, \
         content_md) VALUES ('a1', 'local', ?, 1, 'archive')",
        "INSERT INTO peco_context_shadow (id, user_id, conversation_id, turn_index, \
         trigger_caliber, pinned_tokens, committed_full_tokens, committed_view_tokens, \
         view_window_tokens, fold_triggered, guard_triggered, strip_evaluated, strip_enforced, \
         stripped_items, released_tokens, tool_output_items, tool_output_over_cap, \
         image_output_items, image_base64_bytes, snapshot_json_bytes) \
         VALUES ('p1', 'local', ?, 0, 'full', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
        "INSERT INTO artifacts (id, conversation_id, user_id, kind, mime, title, rel_path) \
         VALUES ('ar1', ?, 'local', 'image', 'image/png', 't', 'x.png')",
    ];
    for sql in statements {
        sqlx::query(sql)
            .bind(conv_id)
            .execute(&env.pool)
            .await
            .unwrap_or_else(|e| panic!("播种失败: {sql}: {e}"));
    }
}

/// 八张子表在某个会话上的行数（`artifacts` / `usage_logs` 也按 conversation_id 计）。
async fn history_rows(env: &TestEnv, conv_id: &str) -> i64 {
    let mut total = 0;
    for table in [
        "messages",
        "session_snapshots",
        "conversation_inflight_turns",
        "usage_logs",
        "peco_compaction_log",
        "peco_session_archives",
        "peco_context_shadow",
        "artifacts",
    ] {
        let sql = format!("SELECT count(*) FROM {table} WHERE conversation_id = ?");
        total += sqlx::query_scalar::<_, i64>(&sql)
            .bind(conv_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    }
    total
}

async fn conversations_of(env: &TestEnv, ws_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversations WHERE workspace_id = ?")
        .bind(ws_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn row_of(env: &TestEnv, ws_id: &str) -> Option<workspaces::WorkspaceRow> {
    workspaces::by_id(&env.pool, ws_id).await.unwrap()
}

async fn trash_ids(env: &TestEnv) -> Vec<String> {
    let reply = env.get("/api/spaces/trash").await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    reply
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ws_id"].as_str().unwrap().to_string())
        .collect()
}

// ============================================================================
// A6：移入回收站 = 只挂起
// ============================================================================

/// 删除 ⇒ `deleted_at` 非空、marker 消失、会话与记忆原样留着。
///
/// 变异：把删除写成硬删（`workspaces::delete`）→ 行不见了，本用例第一组断言变红。
#[tokio::test]
async fn trashing_keeps_the_row_the_conversations_and_the_memories() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("trash-a", "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    let marker = work_dir.join(manifest::MANIFEST_FILE);
    assert!(marker.is_file(), "前提：删除前 marker 在");
    let conv_id = ensure_conversation(&env, &ws_id).await;
    seed_history(&env, &conv_id).await;
    // 记忆用落盘后端：删完之后要用另一个实例把同一批文档读回来，InMemory 做不到。
    env.write_memory_backend(&ws_id, "LanceDb", "all-mini-l-m-l6-v2-q");
    let memory = env
        .open(&ws_id)
        .await
        .unwrap()
        .workspace()
        .memory_manager()
        .cloned()
        .expect("必须注入记忆管理器");
    memory
        .add_text_to_kb(
            "@private_memory",
            "fact",
            "用户的主开发语言是 Rust",
            "ppa_semantic",
        )
        .await
        .expect("写入记忆库");
    drop(memory);
    let config = memory_config(&env, &ws_id);
    assert!(config.is_file(), "前提：打开空间已写下记忆配置");

    let reply = env.delete(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["trashed"], json!(true));

    let row = row_of(&env, &ws_id).await.expect("回收站里的行必须留着");
    assert!(row.deleted_at.is_some(), "deleted_at 必须非空");
    assert!(
        !marker.exists(),
        "marker 必须被摘掉（否则 rescan 会复活它）"
    );
    assert_eq!(conversations_of(&env, &ws_id).await, 1, "会话必须原样留着");
    assert_eq!(history_rows(&env, &conv_id).await, 8, "八张子表都不得被动");
    assert!(config.is_file(), "记忆配置必须原样留着");
    assert!(memory_root(&env, &ws_id).is_dir(), "记忆目录必须原样留着");
    assert_eq!(trash_ids(&env).await, vec![ws_id.clone()]);

    // 回收站里的空间只读可浏览：记忆全在，只是不可写、不可打开。
    let browsed = env
        .get(&format!("/api/spaces/{ws_id}/memory/documents"))
        .await;
    assert_eq!(
        browsed.status, 200,
        "回收站里的记忆必须可浏览: {}",
        browsed.body
    );
    assert_eq!(browsed.body["total"], json!(1), "body={}", browsed.body);
    assert_eq!(browsed.body["items"][0]["title"], json!("fact"));
}

// ============================================================================
// A7：回收站里的空间不可见、不可打开
// ============================================================================

/// 删掉之后：清单里没有、动态清单里没有、rescan 不复活、打开与派发都拒绝。
///
/// 变异：去掉 `workspaces::list` 的 `deleted_at IS NULL` → 第一条与动态清单断言变红。
#[tokio::test]
async fn a_trashed_space_is_invisible_and_cannot_be_opened() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("trash-b", "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    let trashed = env.delete(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(trashed.status, 200, "body={}", trashed.body);

    // 清单：不在活动空间里；详情仍能读到状态（前端恢复流程要用）。
    let listed = env.get("/api/spaces").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    assert!(
        listed
            .body
            .as_array()
            .unwrap()
            .iter()
            .all(|view| view["ws_id"] != json!(ws_id)),
        "回收站里的空间不得出现在清单里: {}",
        listed.body
    );
    let detail = env.get(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["state"], json!("trashed"));

    // 派发目标清单：Peco 的 system prompt 里不该出现这个目录。
    let peco = std::sync::Arc::new(resolve_peco(&env.home));
    let catalog = SpaceCatalogContext::new(env.pool.clone(), peco, env.home.clone());
    let text = catalog.query("").await.expect("清单应可渲染");
    assert!(
        !text.contains(&ws_id) && !text.contains(&work_dir.display().to_string()),
        "动态清单不得包含回收站里的空间: {text}"
    );

    // 磁盘普查：marker 已被摘掉，不会把这一行复活成「可用」。
    let report = workspaces::rescan(&env.pool, &env.home).await.unwrap();
    let _ = report;
    let row = row_of(&env, &ws_id).await.expect("行仍在");
    assert!(row.is_trashed(), "普查不得清掉 deleted_at");
    assert!(
        workspaces::list(&env.pool)
            .await
            .unwrap()
            .iter()
            .all(|row| row.id != ws_id),
        "普查之后仍不在活动清单里"
    );

    // 打开：两条入口都拒绝。
    let opened = env
        .post(&format!("/api/spaces/{ws_id}/open"), json!({}))
        .await;
    assert_eq!(opened.status, 409, "body={}", opened.body);
    assert_eq!(opened.code(), "space_trashed");
    let direct = match env.open(&ws_id).await {
        Ok(_) => panic!("回收站里的空间不得被直接打开"),
        Err(err) => err,
    };
    assert_eq!(direct.code(), "space_trashed");

    // 排期目标同样拒绝：指向回收站的排期没有可执行的对象。
    let schedule = env
        .post(
            "/api/schedules",
            json!({
                "name": "trashed-target",
                "target_ws_id": ws_id,
                "prompt": "做个总结",
                "cron_expr": "0 9 * * *",
            }),
        )
        .await;
    assert_eq!(schedule.status, 409, "body={}", schedule.body);
    assert_eq!(schedule.code(), "space_trashed");
}

// ============================================================================
// A8：恢复
// ============================================================================

/// 恢复 ⇒ marker 重建（ws_id 不变）、`deleted_at` 清空、会话与记忆可读。
///
/// 记忆用 LanceDb（落盘后端）：本用例要跨实例读到同一批文档，InMemory 不行。
#[tokio::test]
async fn restoring_rebuilds_the_marker_and_keeps_the_history_readable() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("restore-a");
    let ws_id = env.resolve(&work_dir, "research").await.unwrap();
    env.write_memory_backend(&ws_id, "LanceDb", "all-mini-l-m-l6-v2-q");
    env.open(&ws_id).await.expect("空间应已打开");

    let conv_id = ensure_conversation(&env, &ws_id).await;
    let memory = env
        .open(&ws_id)
        .await
        .unwrap()
        .workspace()
        .memory_manager()
        .cloned()
        .expect("必须注入记忆管理器");
    memory
        .add_text_to_kb(
            "@private_memory",
            "fact",
            "用户的主开发语言是 Rust",
            "ppa_semantic",
        )
        .await
        .expect("写入记忆库");
    // 放开实例：daemon 之后会用另一个实例打开同一个 LanceDb 目录。
    drop(memory);

    let closed_at_before = row_of(&env, &ws_id).await.unwrap().closed_at;
    let trashed = env.delete(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(trashed.status, 200, "body={}", trashed.body);

    let restored = env
        .post(&format!("/api/spaces/{ws_id}/restore"), json!({}))
        .await;
    assert_eq!(restored.status, 200, "body={}", restored.body);
    assert_eq!(restored.body["restored"], json!(true));
    assert_eq!(restored.body["ws_id"], json!(ws_id));

    let row = row_of(&env, &ws_id).await.expect("恢复后行仍在");
    assert!(row.deleted_at.is_none(), "deleted_at 必须被清空");
    assert_eq!(row.closed_at, closed_at_before, "恢复不改变关闭时刻");
    let marker = manifest::read(&work_dir)
        .unwrap()
        .expect("marker 必须被重建");
    assert_eq!(
        manifest::ws_id_of(&marker),
        ws_id,
        "重建的 marker 必须派生回同一个身份"
    );

    // 幂等：再恢复一次返回 200，什么都不变。
    let again = env
        .post(&format!("/api/spaces/{ws_id}/restore"), json!({}))
        .await;
    assert_eq!(again.status, 200, "body={}", again.body);
    assert_eq!(again.body["restored"], json!(false));

    // 会话与记忆都可读：会话还是同一条（历史没丢），记忆库仍有那一条文档。
    let handle = env.open(&ws_id).await.expect("恢复后必须能重新打开");
    let same = ensure_conversation(&env, &ws_id).await;
    assert_eq!(same, conv_id, "恢复后必须还是同一条会话");
    let docs = handle
        .workspace()
        .memory_manager()
        .cloned()
        .expect("必须注入记忆管理器")
        .list_memory_documents_with_content("@private_memory", 100)
        .await
        .expect("列出记忆库");
    assert_eq!(docs.len(), 1, "恢复后记忆必须还在: {docs:?}");
    assert!(docs[0].content.contains("Rust"));
}

/// 恢复的失败面：目录没了 ⇒ `work_dir_missing`；目录被别的空间占用 ⇒ `work_dir_reused`。
#[tokio::test]
async fn restoring_reports_the_two_unrecoverable_states() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("restore-b", "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    env.delete(&format!("/api/spaces/{ws_id}")).await;

    // 目录被别的空间占用：marker 存在但身份不同（重建同名目录即可造出这个状态）。
    std::fs::create_dir_all(&work_dir).unwrap();
    let other = peco_core::workspace::manifest::new_manifest(
        "someone-else",
        "",
        &env.home.join("teams/research"),
    );
    manifest::write(&work_dir, &other).unwrap();
    let reused = env
        .post(&format!("/api/spaces/{ws_id}/restore"), json!({}))
        .await;
    assert_eq!(reused.status, 409, "body={}", reused.body);
    assert_eq!(reused.code(), "work_dir_reused");

    // 目录没了：恢复只能报「目录不见了」，救不回来。
    let other_ws = env.open_space("restore-c", "research").await;
    let other_dir = env.work_dir_of(&other_ws).await;
    env.delete(&format!("/api/spaces/{other_ws}")).await;
    std::fs::remove_dir_all(&other_dir).unwrap();
    let missing = env
        .post(&format!("/api/spaces/{other_ws}/restore"), json!({}))
        .await;
    assert_eq!(missing.status, 409, "body={}", missing.body);
    assert_eq!(missing.code(), "work_dir_missing");
}

// ============================================================================
// A9：同目录重建
// ============================================================================

/// 回收站行占着 `work_dir` ⇒ 同一目录再建空间时先销毁旧行（会话没了），新 ws_id 生效。
///
/// 变异：`POST /api/spaces` 不处理 trashed 行 → 会命中 UNIQUE 或返回旧 ws_id，
/// 本用例的「新 ws_id 不同 / 旧会话清零」断言变红。
#[tokio::test]
async fn rebuilding_in_the_same_directory_destroys_the_trashed_row() {
    let env = new_env(&["research"]).await;
    let work_dir = env.work_dir("rebuild");
    let old_ws = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
        )
        .await;
    let old_ws = old_ws.body["ws_id"].as_str().unwrap().to_string();
    let old_conv = ensure_conversation(&env, &old_ws).await;
    seed_history(&env, &old_conv).await;
    env.delete(&format!("/api/spaces/{old_ws}")).await;

    let remade = env
        .post(
            "/api/spaces",
            json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research", "name": "重建"}),
        )
        .await;
    assert_eq!(remade.status, 201, "body={}", remade.body);
    let new_ws = remade.body["ws_id"].as_str().unwrap().to_string();
    assert_ne!(new_ws, old_ws, "新空间必须是新身份");
    assert!(
        row_of(&env, &old_ws).await.is_none(),
        "旧行必须被销毁（否则 UNIQUE 会把目录一直锁给一个已删除的空间）"
    );
    assert_eq!(conversations_of(&env, &old_ws).await, 0, "旧会话必须清零");
    assert_eq!(history_rows(&env, &old_conv).await, 0, "旧会话历史必须清零");
    assert!(!memory_root(&env, &old_ws).exists(), "旧记忆目录必须消失");
    assert_eq!(
        manifest::read(&work_dir)
            .unwrap()
            .map(|m| manifest::ws_id_of(&m)),
        Some(new_ws.clone()),
        "marker 必须换成新身份"
    );
    assert_eq!(trash_ids(&env).await, Vec::<String>::new(), "回收站已清空");
}

// ============================================================================
// A10：永久销毁
// ============================================================================

/// 销毁 ⇒ 会话八张子表清零、记忆目录消失、空间行消失；工作目录文件与台账仍在。
///
/// 变异：销毁时连 `work_dir` 里的文件一起删 → 「用户文件仍在」变红；跳过记忆清理 →
/// 「记忆目录消失」变红。
#[tokio::test]
async fn purging_clears_sessions_and_memories_but_never_the_work_dir() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("purge-a", "research").await;
    let work_dir = env.work_dir_of(&ws_id).await;
    let user_file = work_dir.join("notes.md");
    std::fs::write(&user_file, "用户自己的文件").unwrap();
    let conv_id = ensure_conversation(&env, &ws_id).await;
    seed_history(&env, &conv_id).await;
    assert!(memory_config(&env, &ws_id).is_file());
    // 台账与审计：销毁不得动它们。
    env.insert_dispatch(&ws_id, "done", "dispatch_to_workspace", "peco", "completed")
        .await;
    sqlx::query(
        "INSERT INTO memory_audit (user_id, kb_name, doc_id, title, content, source, reason, \
         deleted_by, deleted_at) VALUES ('local', '@private_memory', 'doc-1', 't', 'c', \
         'ppa_semantic', 'manual_organize', 'agent:@memory', '2026-01-01T00:00:00+00:00')",
    )
    .execute(&env.pool)
    .await
    .unwrap();

    let purged = env.delete(&format!("/api/spaces/{ws_id}?purge=true")).await;
    assert_eq!(purged.status, 200, "body={}", purged.body);
    assert_eq!(purged.body["purged"], json!(true));

    assert_eq!(history_rows(&env, &conv_id).await, 0, "八张子表必须清零");
    assert_eq!(conversations_of(&env, &ws_id).await, 0, "会话行必须清零");
    assert!(
        !memory_root(&env, &ws_id).exists(),
        "记忆目录必须消失: {}",
        memory_root(&env, &ws_id).display()
    );
    assert!(row_of(&env, &ws_id).await.is_none(), "空间行必须消失");
    assert!(user_file.is_file(), "工作目录里的用户文件永不被删");
    assert_eq!(env.count_dispatches(&ws_id).await, 1, "台账必须长于空间行");
    let audits = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM memory_audit")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(audits, 1, "记忆审计必须留着（历史审计不动）");
    assert!(
        !work_dir.join(manifest::MANIFEST_FILE).exists(),
        "marker 一并摘掉"
    );
}

// ============================================================================
// A11：上限 100
// ============================================================================

/// 回收站超过 100 条 ⇒ 同一请求内销毁最旧的一条（会话与记忆一起没了）。
///
/// 变异：去掉上限清扫 → 计数变 101；清扫只删行不删会话 → 「最旧的会话清零」变红。
#[tokio::test]
async fn the_trash_never_exceeds_one_hundred_entries() {
    let env = new_env(&["research"]).await;
    // `workspaces.team_id` 有外键：团队目录是 `new_env` 造的，索引行要自己补。
    peco_daemon::db::teams::upsert(
        &env.pool,
        &peco_daemon::db::teams::NewTeam {
            id: "research",
            display_name: "research",
            template_version: 1,
        },
    )
    .await
    .unwrap();

    // 先铺 100 条回收站行（只写库：它们的目录与记忆都不存在，清理会走空路径）。
    for index in 0..100 {
        let ws_id = format!("ws_old{index:03}");
        let work_dir = env
            .home
            .join("spaces")
            .join(format!("old{index:03}"))
            .join("work");
        sqlx::query(
            "INSERT INTO workspaces (id, work_dir, team_id, label, deleted_at) \
             VALUES (?, ?, 'research', 'old', ?)",
        )
        .bind(&ws_id)
        .bind(work_dir.to_string_lossy().into_owned())
        // 越靠前的越旧：上限按 `deleted_at` 升序淘汰。
        .bind(format!("2026-01-01 00:00:{:02}", index % 60))
        .execute(&env.pool)
        .await
        .unwrap();
    }
    // 最旧那条要有会话与记忆，才能验证「销毁」不只是删行。
    let oldest_conv = "conv_oldest";
    sqlx::query(
        "INSERT INTO conversations (id, user_id, agent_name, workspace_id) \
         VALUES (?, 'local', '@master', 'ws_old000')",
    )
    .bind(oldest_conv)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (id, conversation_id, role, content) \
         VALUES ('m_old', ?, 'user', 'hi')",
    )
    .bind(oldest_conv)
    .execute(&env.pool)
    .await
    .unwrap();
    let oldest_memory = memory_root(&env, "ws_old000");
    std::fs::create_dir_all(oldest_memory.join("@private_memory")).unwrap();
    std::fs::write(
        oldest_memory.join("@private_memory/kb_config.json"),
        "{\"name\":\"@private_memory\"}",
    )
    .unwrap();
    assert_eq!(
        workspaces::count_trashed(&env.pool).await.unwrap(),
        100,
        "前提：回收站正好 100 条"
    );

    // 第 101 条：删除触发清扫。
    let ws_id = env.open_space("trash-cap", "research").await;
    let reply = env.delete(&format!("/api/spaces/{ws_id}")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);

    assert_eq!(
        workspaces::count_trashed(&env.pool).await.unwrap(),
        100,
        "上限是 100：多出来的那条必须在同一请求内销毁"
    );
    assert!(
        row_of(&env, "ws_old000").await.is_none(),
        "最旧的一条必须被销毁"
    );
    assert_eq!(
        history_rows(&env, oldest_conv).await,
        0,
        "被销毁的空间连会话历史一起清"
    );
    assert!(!oldest_memory.exists(), "被销毁的空间连记忆目录一起清");
    assert!(
        row_of(&env, "ws_old001").await.is_some(),
        "只销毁超出上限的那几条"
    );
    assert!(row_of(&env, &ws_id).await.is_some(), "本次移入的这条留着");
}

// ============================================================================
// A12：保留空间
// ============================================================================

/// `ws_peco` 两种删除强度都拒绝：400 `reserved_space`，行保持活跃。
#[tokio::test]
async fn the_peco_space_cannot_be_trashed_or_purged() {
    let env = new_env(&[]).await;
    peco_daemon::space::bootstrap::bootstrap_peco(&env.env).await;

    let trashed = env.delete("/api/spaces/ws_peco").await;
    assert_eq!(trashed.status, 400, "body={}", trashed.body);
    assert_eq!(trashed.code(), "reserved_space");
    let purged = env.delete("/api/spaces/ws_peco?purge=true").await;
    assert_eq!(purged.status, 400, "body={}", purged.body);
    assert_eq!(purged.code(), "reserved_space");

    let row = row_of(&env, "ws_peco").await.expect("保留空间的行必须还在");
    assert!(!row.is_trashed(), "拒绝删除后不得置 deleted_at");
    assert_eq!(row.team_id, PECO_TEAM_ID);
    assert!(trash_ids(&env).await.is_empty());
    assert!(Path::new(&row.work_dir).is_dir(), "目录也不得动");
}
