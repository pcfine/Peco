// ============================================================================
// Peco 会话端点集成测试（归档式清空 + 可观测性 + 任务注册表）
// ============================================================================
//
// 覆盖：
//   - DELETE /api/peco/session 默认归档式清空（先归档后删除，快照消失）
//   - DELETE /api/peco/session?archive=false 硬删除（无归档）
//   - GET /api/peco/session 的 context_metrics（估算 token 两口径 + 压缩时间线）
//   - GET /api/peco/archives 列表 + /archives/:id 下载 + 用户隔离
//   - 任务注册表生命周期：is_running 上报 / cancel 端点 / 附着入队 / 用户隔离
//     （经 try_register 注入伪 run，不依赖 LLM；完整流测试见 #[ignore] 用例）

mod common;

use common::TestApp;
use model_provider::{InputItem, Role, Usage};
use peco_core::persistence::SessionPersister;
use peco_core::session::{AnnotatedMessage, MessageId, MessageSource, SessionSnapshot};
use peco_server::session_store::SqliteSessionPersister;
use std::time::Duration;

fn user_msg(text: &str) -> InputItem {
    InputItem::Message {
        role: Role::User,
        content: text.into(),
    }
}

fn assistant_msg(text: &str) -> InputItem {
    InputItem::Message {
        role: Role::Assistant,
        content: text.into(),
    }
}

fn pinned_summary(text: &str) -> AnnotatedMessage {
    AnnotatedMessage::new(
        MessageId(0),
        0,
        InputItem::Message {
            role: Role::System,
            content: text.into(),
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
    assert_eq!(metrics["compaction_trigger_tokens"], 384_000);
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

/// 为测试用户预置一个含 `n` 轮（每轮 user + assistant）的会话快照，无 pinned 摘要。
async fn seed_turns(app: &TestApp, session_id: &str, n: usize) {
    let committed_turns: Vec<Vec<AnnotatedMessage>> = (0..n)
        .map(|i| {
            vec![
                annotated(i, user_msg(&format!("第 {i} 轮提问"))),
                annotated(i, assistant_msg(&format!("第 {i} 轮回答"))),
            ]
        })
        .collect();
    let snapshot = SessionSnapshot {
        committed_turns,
        turn_index: n,
        total_usage: Usage {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
        },
        next_message_id: 10,
        pending_inputs: Vec::new(),
        pinned_summary: None,
    };
    let persister = SqliteSessionPersister::new(app.state.db.clone());
    persister
        .save(&snapshot, session_id, "个人助理", 1_700_000_000)
        .await
        .unwrap();
}

// ── 分 turn 分页 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_session_pagination_returns_tail_window_with_global_turn_index() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    let body: serde_json::Value = app
        .get("/api/peco/session?turns=2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    // turn_index 保持全局位置号（尾部两轮 = 3、4），非窗口内相对位置
    assert_eq!(turns[0]["turn_index"], 3);
    assert_eq!(turns[1]["turn_index"], 4);
    assert_eq!(body["total_turns"], 5);
    assert_eq!(body["has_more"], true);
}

#[tokio::test]
async fn test_session_pagination_before_window() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    let body: serde_json::Value = app
        .get("/api/peco/session?before=3&turns=2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // before=3（排他上界）→ 窗口 [1, 3)
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["turn_index"], 1);
    assert_eq!(turns[1]["turn_index"], 2);
    assert_eq!(body["total_turns"], 5);
    assert_eq!(body["has_more"], true);
}

#[tokio::test]
async fn test_session_pagination_clamps_turns_and_full_default() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    // turns=0 → clamp 到 1，返回尾部 1 轮
    let body: serde_json::Value = app
        .get("/api/peco/session?turns=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turns"].as_array().unwrap().len(), 1);
    assert_eq!(body["turns"][0]["turn_index"], 4);

    // turns 缺省 → 全量（向后兼容），无 has_more
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turns"].as_array().unwrap().len(), 5);
    assert_eq!(body["total_turns"], 5);
    assert_eq!(body["has_more"], false);
}

#[tokio::test]
async fn test_session_pagination_has_more_false_at_head() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    // 窗口起点 0（before 覆盖到最早）→ 无更早轮
    let body: serde_json::Value = app
        .get("/api/peco/session?turns=10")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turns"].as_array().unwrap().len(), 5);
    assert_eq!(body["has_more"], false);
}

#[tokio::test]
async fn test_session_pagination_before_zero_is_empty_window() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    // before=0（排他上界为 0）→ 空窗口；前面已无可加载轮
    let body: serde_json::Value = app
        .get("/api/peco/session?before=0&turns=2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turns"].as_array().unwrap().len(), 0);
    assert_eq!(body["total_turns"], 5);
    assert_eq!(body["has_more"], false);
}

#[tokio::test]
async fn test_session_pagination_before_out_of_range_clamps_to_total() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 5).await;

    // before 越界 → clamp 到 total，等价于尾部窗口
    let body: serde_json::Value = app
        .get("/api/peco/session?before=999&turns=2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["turn_index"], 3);
    assert_eq!(turns[1]["turn_index"], 4);
    assert_eq!(body["has_more"], true);
}

#[tokio::test]
async fn test_session_pagination_turns_clamped_to_max_page() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    // 超过 MAX_PAGE_TURNS(200) 一档
    seed_turns(&app, &session_id, 201).await;

    let body: serde_json::Value = app
        .get("/api/peco/session?turns=999")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turns = body["turns"].as_array().unwrap();
    // 显式 turns 收 clamp 到 1..=200 → 只回最近 200 轮，全局位置号从 1 起
    assert_eq!(turns.len(), 200);
    assert_eq!(turns[0]["turn_index"], 1);
    assert_eq!(body["total_turns"], 201);
    assert_eq!(body["has_more"], true);
}

// ── /session/metrics 轻量端点 ──────────────────────────────────────────────

#[tokio::test]
async fn test_session_metrics_endpoint_returns_metrics_without_turns() {
    let app = TestApp::new().await;
    let session_id = format!("{}-private-session", app.user_id);
    seed_turns(&app, &session_id, 3).await;

    let body: serde_json::Value = app
        .get("/api/peco/session/metrics")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // 轻量端点只回 context_metrics，不携带 turn 历史
    assert!(body.get("turns").is_none());
    let metrics = &body["context_metrics"];
    assert!(!metrics.is_null());
    assert_eq!(metrics["compaction_count"], 0);
    assert!(metrics["estimated_total_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_session_metrics_endpoint_null_when_no_session() {
    let app = TestApp::new().await;

    let body: serde_json::Value = app
        .get("/api/peco/session/metrics")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["context_metrics"].is_null());
}

// ── chat 会话快照无回归（turns_to_dto 共享 helper 后行为不变）──────────────

#[tokio::test]
async fn test_chat_session_snapshot_still_returns_full_turns() {
    let app = TestApp::new().await;

    // 建对话
    let conv_resp = app
        .post("/api/conversations")
        .json(&serde_json::json!({ "title": "回归对话" }))
        .send()
        .await
        .unwrap();
    assert_eq!(conv_resp.status(), 201);
    let conv: serde_json::Value = conv_resp.json().await.unwrap();
    let conv_id = conv["id"].as_str().unwrap();

    // 为该对话预置 3 轮快照
    seed_turns(&app, conv_id, 3).await;

    // 旧路由（DEPRECATED /api/conversations/:id/session）仍走同一 handler
    let body: serde_json::Value = app
        .get(&format!("/api/conversations/{conv_id}/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // 全量轮、全局位置号、不含 peco 专属分页字段
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0]["turn_index"], 0);
    assert_eq!(turns[2]["turn_index"], 2);
    assert!(body.get("total_turns").is_none());
    assert!(body.get("has_more").is_none());
    assert!(body.get("context_metrics").is_none());
    // 正文可读（首轮 user 提问在 messages 中）
    assert_eq!(turns[0]["messages"][0]["role"], "user");
    assert_eq!(turns[0]["messages"][0]["content"], "第 0 轮提问");
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

// ── 任务注册表生命周期（runner 与 SSE 连接解耦） ─────────────────────────────

#[tokio::test]
async fn test_session_reports_not_running_without_run() {
    let app = TestApp::new().await;
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], false);
}

#[tokio::test]
async fn test_stream_attach_without_run_rejected() {
    let app = TestApp::new().await;

    // 无 message 且无活跃 run — 纯附着无对象，400
    let resp = app.get("/api/peco/stream").send().await.unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_cancel_without_run_is_404() {
    let app = TestApp::new().await;

    let resp = app.post("/api/peco/stream/cancel").send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_registered_run_lifecycle_via_endpoints() {
    let app = TestApp::new().await;

    // 注入伪 run（不启动 runner — 只测注册表与端点的接缝）
    let registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], true);

    let resp = app.post("/api/peco/stream/cancel").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);

    // runner 退出 → 注册产物 drop → 条目消失 → is_running 翻回 false
    drop(registration);
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], false);
}

#[tokio::test]
async fn test_run_registry_is_user_scoped() {
    let app = TestApp::new().await;
    let (uid2, _token2) = app.register_user2().await;

    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], true);

    // 其他用户不可见也不可取消
    let resp = app
        .get_as("/api/peco/session", &_token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["is_running"], false);
    assert_ne!(uid2, app.user_id);

    let resp = app
        .post_as("/api/peco/stream/cancel", &_token2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_stream_attaches_and_enqueues_into_active_run() {
    let app = TestApp::new().await;
    let mut registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    // 携 message 附着到活跃 run → 200 SSE，消息入控制通道（经 runner 转发给 looper）
    let resp = app
        .get("/api/peco/stream?message=hello")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/event-stream")
    );

    let cmd = tokio::time::timeout(Duration::from_secs(2), registration.control_rx.recv())
        .await
        .expect("消息应在超时前排入控制通道")
        .expect("控制通道不应关闭");
    let peco_server::peco::active::ControlCommand::Query(text) = cmd else {
        panic!("应为 Query 命令，实际 {cmd:?}");
    };
    assert_eq!(text, "hello");
}

// ── POST /api/peco/stream/query（常驻连接复用，不新开 SSE） ────────────────

#[tokio::test]
async fn test_query_endpoint_enqueues_into_active_run() {
    let app = TestApp::new().await;
    let mut registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    let resp = app
        .post("/api/peco/stream/query")
        .json(&serde_json::json!({ "message": "  第二条消息  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);

    let cmd = tokio::time::timeout(Duration::from_secs(2), registration.control_rx.recv())
        .await
        .expect("消息应在超时前排入控制通道")
        .expect("控制通道不应关闭");
    let peco_server::peco::active::ControlCommand::Query(text) = cmd else {
        panic!("应为 Query 命令，实际 {cmd:?}");
    };
    assert_eq!(text, "第二条消息", "入队前应 trim");
}

#[tokio::test]
async fn test_query_without_run_is_404() {
    let app = TestApp::new().await;

    let resp = app
        .post("/api/peco/stream/query")
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_query_rejects_blank_message() {
    let app = TestApp::new().await;
    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    for payload in ["", "   ", "\n\t "] {
        let resp = app
            .post("/api/peco/stream/query")
            .json(&serde_json::json!({ "message": payload }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "空/纯空白消息应拒绝：{payload:?}");
    }

    // 缺字段 / 非法 JSON 同样 400（JsonRejection 归一化）
    let resp = app
        .post("/api/peco/stream/query")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// 控制通道积压必须报 409（背压）而不是 404（无 run）—— 后者会让调用方
/// 白建一个 run，且掩盖真实的积压状态。
#[tokio::test]
async fn test_query_reports_backpressure_not_missing_run() {
    let app = TestApp::new().await;
    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    // 不排空 control_rx：灌满容量 32 的通道
    let mut last_status = 0;
    for _ in 0..33 {
        let resp = app
            .post("/api/peco/stream/query")
            .json(&serde_json::json!({ "message": "x" }))
            .send()
            .await
            .unwrap();
        last_status = resp.status().as_u16();
    }
    assert_eq!(last_status, 409, "第 33 条应为背压 409");
}

#[tokio::test]
async fn test_query_is_user_scoped() {
    let app = TestApp::new().await;
    let (_uid2, token2) = app.register_user2().await;
    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    let resp = app
        .post_as("/api/peco/stream/query", &token2)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "不得投递到他人的 run");
}

/// 常驻连接下「附着 + POST 投递」的组合：消息只能到一次。
#[tokio::test]
async fn test_attach_then_query_delivers_exactly_once() {
    let app = TestApp::new().await;
    let mut registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    // 纯附着：不携带 message，只订阅事件流
    let resp = app.get("/api/peco/stream").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let resp = app
        .post("/api/peco/stream/query")
        .json(&serde_json::json!({ "message": "second" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let cmd = tokio::time::timeout(Duration::from_secs(2), registration.control_rx.recv())
        .await
        .expect("消息应在超时前送达")
        .expect("控制通道不应关闭");
    assert!(matches!(
        cmd,
        peco_server::peco::active::ControlCommand::Query(ref t) if t == "second"
    ));

    // 不得重复投递
    assert!(
        tokio::time::timeout(Duration::from_millis(200), registration.control_rx.recv())
            .await
            .is_err(),
        "同一条消息不应投递两次"
    );
}

#[tokio::test]
async fn test_snapshot_reports_turn_in_flight() {
    let app = TestApp::new().await;

    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], false);
    assert_eq!(body["turn_in_flight"], false);

    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    // 抢注即为即将开跑
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], true);
    assert_eq!(body["turn_in_flight"], true);

    // runner 上报停靠（Idle）：run 仍在，但无轮次在途
    app.state.peco_runs.set_turn_in_flight(&app.user_id, false);
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], true);
    assert_eq!(body["turn_in_flight"], false);
}

#[tokio::test]
async fn test_snapshot_reports_inflight_user_input() {
    let app = TestApp::new().await;

    let _registration = app.state.peco_runs.try_register(&app.user_id).unwrap();

    // 尚未上报在途输入：字段缺省（serde skip_serializing_if）
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("inflight_user_input").is_none());

    // runner 在 TurnStart 上报本轮 query → 快照带回，供整页刷新后恢复
    app.state
        .peco_runs
        .set_inflight_input(&app.user_id, Some("我刚发的问题".to_string()));
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turn_in_flight"], true);
    assert_eq!(body["inflight_user_input"], "我刚发的问题");

    // 轮次收尾（Idle）：字段随 turn_in_flight 一并清空
    app.state.peco_runs.set_turn_in_flight(&app.user_id, false);
    app.state.peco_runs.set_inflight_input(&app.user_id, None);
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turn_in_flight"], false);
    assert!(body.get("inflight_user_input").is_none());
}

/// 完整流测试：需要 DEEPSEEK_API_KEY（与 chat 端的 SSE 测试同门槛）。
/// 覆盖 发起 → 断开 → is_running → 重新附着 → cancel 的端到端链路。
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY"]
async fn test_stream_full_lifecycle_with_disconnect_and_reattach() {
    let app = TestApp::new().await;

    // 1. 发起任务，读到首段事件后断开连接
    let resp = app
        .get("/api/peco/stream?message=用一句话介绍你自己")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut resp = resp;
    let mut buf = Vec::new();
    while buf.len() < 64 {
        let chunk = resp.chunk().await.unwrap().expect("流未结束应产出数据块");
        buf.extend_from_slice(&chunk);
    }
    drop(resp); // 模拟用户关闭页面

    // 2. 断开后任务仍在执行
    tokio::time::sleep(Duration::from_millis(500)).await;
    let body: serde_json::Value = app
        .get("/api/peco/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["is_running"], true);

    // 3. 重新附着（纯附着模式）
    let resp = app.get("/api/peco/stream").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);

    // 4. 取消任务
    let resp = app.post("/api/peco/stream/cancel").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 5. 等 runner 退出后 is_running 翻回 false
    let mut gone = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let body: serde_json::Value = app
            .get("/api/peco/session")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["is_running"] == false {
            gone = true;
            break;
        }
    }
    assert!(gone, "取消后 run 应退出并清理注册表");
}
