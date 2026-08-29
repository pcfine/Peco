// ============================================================================
// Peco 会话端点集成测试（归档式清空 + 可观测性）
// ============================================================================
//
// 覆盖：
//   - DELETE /api/peco/session 默认归档式清空（先归档后删除，快照消失）
//   - DELETE /api/peco/session?archive=false 硬删除（无归档）
//   - GET /api/peco/session 的 context_metrics（估算 token 两口径 + 压缩时间线）
//   - GET /api/peco/archives 列表 + /archives/:id 下载 + 用户隔离

mod common;

use common::TestApp;
use model_provider::{InputItem, Role, Usage};
use peco_core::persistence::SessionPersister;
use peco_core::session::{AnnotatedMessage, MessageId, MessageSource, SessionSnapshot};
use peco_server::session_store::SqliteSessionPersister;

fn user_msg(text: &str) -> InputItem {
    InputItem::Message {
        role: Role::User,
        content: text.to_string(),
    }
}

fn assistant_msg(text: &str) -> InputItem {
    InputItem::Message {
        role: Role::Assistant,
        content: text.to_string(),
    }
}

fn pinned_summary(text: &str) -> AnnotatedMessage {
    AnnotatedMessage::new(
        MessageId(0),
        0,
        InputItem::Message {
            role: Role::System,
            content: text.to_string(),
        },
        MessageSource::SystemInjection {
            reason: "compaction".to_string(),
        },
    )
}

fn annotated(turn: usize, msg: InputItem) -> AnnotatedMessage {
    AnnotatedMessage::new(MessageId(1), turn, msg, MessageSource::UserInput)
}

/// 为测试用户预置一个含 pinned 摘要 + 两轮历史的会话快照。
async fn seed_session(app: &TestApp, session_id: &str) {
    let snapshot = SessionSnapshot {
        committed_turns: vec![
            vec![
                annotated(
                    0,
                    user_msg("这是一段足够长的中文提问，用于产生 token 估算。"),
                ),
                annotated(0, assistant_msg("这是对应的中文回答内容，长度同样足够。")),
            ],
            vec![annotated(
                1,
                user_msg("第二轮的中文提问内容，长度也足够产生估算。"),
            )],
        ],
        turn_index: 2,
        total_usage: Usage {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
        },
        next_message_id: 10,
        pending_inputs: Vec::new(),
        pinned_summary: Some(pinned_summary(
            "<earlier_context_summary>## 已做决定\n- 采用方案 A</earlier_context_summary>",
        )),
    };
    let persister = SqliteSessionPersister::new(app.state.db.clone());
    persister
        .save(&snapshot, session_id, "个人助理", 1_700_000_000)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_clear_archives_then_deletes_snapshot() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    let resp = app.delete("/api/peco/session").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 快照已删除
    let persister = SqliteSessionPersister::new(app.state.db.clone());
    assert!(persister.load(&session_id).await.unwrap().is_none());

    // 归档已生成（静默 — 无列表 UI，经端点取回）
    let archives: Vec<serde_json::Value> = app
        .get("/api/peco/archives")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0]["turn_count"], 2);
    assert_eq!(archives[0]["conversation_id"], session_id);

    let id = archives[0]["id"].as_str().unwrap().to_string();
    let download = app
        .get(&format!("/api/peco/archives/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), 200);
    let md = download.text().await.unwrap();
    // 归档自包含：pinned 摘要 + 历史 turn + 用量元数据
    assert!(md.contains("采用方案 A"));
    assert!(md.contains("## 用户"));
    assert!(md.contains("1000"));
}

#[tokio::test]
async fn test_clear_with_archive_false_skips_archive() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    let resp = app
        .delete("/api/peco/session?archive=false")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let archives: Vec<serde_json::Value> = app
        .get("/api/peco/archives")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(archives.is_empty());

    let persister = SqliteSessionPersister::new(app.state.db.clone());
    assert!(persister.load(&session_id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_clear_empty_session_is_idempotent_success() {
    let app = TestApp::new().await;

    // 无会话时清空 — 归档无内容可存，直接成功
    let resp = app.delete("/api/peco/session").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);

    let archives: Vec<serde_json::Value> = app
        .get("/api/peco/archives")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(archives.is_empty());
}

#[tokio::test]
async fn test_session_snapshot_includes_context_metrics() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let metrics = &body["context_metrics"];
    assert!(!metrics.is_null());
    // 两口径均 > 0 且全量口径 ≥ viewable 口径（tool/reasoning 只计入全量）
    let total = metrics["estimated_total_tokens"].as_u64().unwrap();
    let view = metrics["estimated_view_tokens"].as_u64().unwrap();
    assert!(total > 0);
    assert!(view > 0);
    assert!(total >= view);
    assert!(metrics["pinned_summary_tokens"].as_u64().unwrap() > 0);
    // 阈值口径：与默认配置一致
    assert_eq!(metrics["history_token_budget"], 128_000);
    assert_eq!(metrics["compaction_trigger_tokens"], 256_000);
    // 尚无压缩发生
    assert_eq!(metrics["compaction_count"], 0);
}

#[tokio::test]
async fn test_session_metrics_absent_when_no_session() {
    let app = TestApp::new().await;
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["context_metrics"].is_null());
}

#[tokio::test]
async fn test_archive_download_is_user_scoped() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    app.delete("/api/peco/session").send().await.unwrap();
    let archives: Vec<serde_json::Value> = app
        .get("/api/peco/archives")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = archives[0]["id"].as_str().unwrap().to_string();

    // 第二个用户不可读取
    let (uid2, token2) = app.register_user2().await;
    assert_ne!(uid2, app.user_id);
    let resp = app
        .get_as(&format!("/api/peco/archives/{id}"), &token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 未认证请求被拒
    let resp = app
        .client
        .get(format!("{}/api/peco/archives/{id}", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_clear_also_removes_compaction_log() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    // 预置该会话的压缩日志 — conversation_id 清空后复用，日志不得残留污染新会话
    for i in 0..3 {
        peco_server::db::compaction_log::insert(
            &app.state.db,
            &format!("log-{i}"),
            &app.user_id,
            &session_id,
            2,
            20_000,
            9_000,
            300,
        )
        .await
        .unwrap();
    }

    let resp = app.delete("/api/peco/session").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let rows = peco_server::db::compaction_log::list_by_conversation(
        &app.state.db,
        &app.user_id,
        &session_id,
    )
    .await
    .unwrap();
    assert!(rows.is_empty());

    // 清空后重取指标 — 会话为空，不再有任何旧会话的压缩数据
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["context_metrics"].is_null());
}

#[tokio::test]
async fn test_clear_survives_corrupted_snapshot() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_session(&app, &session_id).await;

    // 篡改快照为非法 JSON（模拟旧版本写入的旧格式/损坏数据）
    sqlx::query(
        "UPDATE session_snapshots SET snapshot_json = '{not-json' WHERE conversation_id = ?",
    )
    .bind(&session_id)
    .execute(&app.state.db)
    .await
    .unwrap();

    // 损坏快照不得夺走用户的重置能力 — 清空仍须成功（跳过归档、照常删除）
    let resp = app.delete("/api/peco/session").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let persister = SqliteSessionPersister::new(app.state.db.clone());
    assert!(persister.load(&session_id).await.unwrap().is_none());

    // 无法反序列化 → 无法归档，归档表为空
    let archives: Vec<serde_json::Value> = app
        .get("/api/peco/archives")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(archives.is_empty());
}
