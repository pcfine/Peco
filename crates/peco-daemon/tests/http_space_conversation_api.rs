//! `/api/spaces/{ws_id}/conversation/ensure` —— 空间单会话的惰性创建与幂等。

mod support;

use axum::http::Method;
use serde_json::json;
use support::{call, new_env};

/// 该空间的 `scope='space'` 行：`(id, agent_name, title, agent_id)`。
async fn space_sessions(
    env: &support::TestEnv,
    ws_id: &str,
) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT id, agent_name, title, agent_id FROM conversations \
         WHERE scope = 'space' AND workspace_id = ? ORDER BY agent_name",
    )
    .bind(ws_id)
    .fetch_all(&env.pool)
    .await
    .unwrap()
}

/// 连续两次 ensure：第一次 `created=true`，第二次 `false` 且 id 相同。
#[tokio::test]
async fn ensure_is_idempotent() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let first = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/ensure"),
            json!({"agent_name": "@master"}),
        )
        .await;
    assert_eq!(first.status, 200, "body={}", first.body);
    assert_eq!(first.body["created"], json!(true));

    let second = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/ensure"),
            json!({"agent_name": "@master"}),
        )
        .await;
    assert_eq!(second.status, 200, "body={}", second.body);
    assert_eq!(second.body["created"], json!(false), "第二次不得新建");
    assert_eq!(
        second.body["conversation_id"], first.body["conversation_id"],
        "两次必须指向同一条会话"
    );

    let rows = space_sessions(&env, &ws_id).await;
    assert_eq!(rows.len(), 1, "唯一索引之外还要真的只有一行: {rows:?}");
    let (id, agent_name, title, agent_id) = &rows[0];
    assert_eq!(first.body["conversation_id"], json!(id));
    assert_eq!(agent_name, "@master");
    assert_eq!(title, "空间对话");
    assert_eq!(*agent_id, None, "空间会话不属于某个 agent 索引行");
}

/// 请求体可省：缺省即 `@master`，产出的行与显式传它时一致。
#[tokio::test]
async fn ensure_defaults_to_master_without_a_body() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let reply = call(
        &env.router(),
        Method::POST,
        &format!("/api/spaces/{ws_id}/conversation/ensure"),
        None,
    )
    .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["created"], json!(true));
    let rows = space_sessions(&env, &ws_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "@master");
}

/// 唯一性按 `(workspace_id, agent_name)`：换一个 agent 是另一条会话，
/// 换一个空间也是另一条。
#[tokio::test]
async fn ensure_keys_the_session_by_space_and_agent() {
    let env = new_env(&["research"]).await;
    let alpha = env.open_space("alpha", "research").await;
    let beta = env.open_space("beta", "research").await;

    let master = env
        .post(
            &format!("/api/spaces/{alpha}/conversation/ensure"),
            json!({}),
        )
        .await;
    let coder = env
        .post(
            &format!("/api/spaces/{alpha}/conversation/ensure"),
            json!({"agent_name": "@coder"}),
        )
        .await;
    let other_space = env
        .post(
            &format!("/api/spaces/{beta}/conversation/ensure"),
            json!({}),
        )
        .await;

    assert_eq!(master.body["created"], json!(true));
    assert_eq!(coder.body["created"], json!(true), "不同 agent 各一条");
    assert_eq!(other_space.body["created"], json!(true), "不同空间各一条");
    assert_ne!(
        master.body["conversation_id"],
        coder.body["conversation_id"]
    );
    assert_ne!(
        master.body["conversation_id"],
        other_space.body["conversation_id"]
    );
    assert_eq!(space_sessions(&env, &alpha).await.len(), 2);
    assert_eq!(space_sessions(&env, &beta).await.len(), 1);
}

/// 不存在的 `ws_id` 沿用既有错误映射：`404 space_not_found`，不建会话行。
#[tokio::test]
async fn ensure_on_unknown_space_is_404() {
    let env = new_env(&["research"]).await;

    let reply = env
        .post("/api/spaces/ws_ghost/conversation/ensure", json!({}))
        .await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found");
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(total, 0, "404 不得写会话行");
}

/// 并发 ensure 只有一次 `created=true`，全部拿到同一个 id。
///
/// 变异：去掉 `ensure_space_session` 的「撞键回查」→ 输掉竞态的那次把唯一索引
/// 错误抛给调用方，下面的 id 断言拿到 500 变红。
#[tokio::test]
async fn concurrent_ensure_yields_one_session() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let uri = format!("/api/spaces/{ws_id}/conversation/ensure");
    let (a, b) = tokio::join!(env.post(&uri, json!({})), env.post(&uri, json!({})),);

    for reply in [&a, &b] {
        assert_eq!(reply.status, 200, "body={}", reply.body);
    }
    assert_eq!(
        a.body["conversation_id"], b.body["conversation_id"],
        "两个请求必须落到同一条会话"
    );
    let created = [&a, &b]
        .iter()
        .filter(|reply| reply.body["created"] == json!(true))
        .count();
    assert_eq!(created, 1, "只有一个请求能新建");
    assert_eq!(space_sessions(&env, &ws_id).await.len(), 1);
}

/// 该会话的历史条数。
async fn message_count(env: &support::TestEnv, conv_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = ?")
        .bind(conv_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// 造两条消息、一条产物登记与一份会话快照：清空要处理的三张表。
async fn seed_conversation_history(env: &support::TestEnv, conv_id: &str) {
    sqlx::query(
        "INSERT INTO messages (id, conversation_id, role, content) \
         VALUES ('m_1', ?, 'user', '清除前的提问'), ('m_2', ?, 'assistant', '清除前的回答')",
    )
    .bind(conv_id)
    .bind(conv_id)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifacts (id, conversation_id, user_id, kind, mime, title, rel_path, size_bytes) \
         VALUES ('a_1', ?, 'local', 'image', 'image/png', '出的图', 'output/x.png', 12)",
    )
    .bind(conv_id)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO session_snapshots (conversation_id, session_id, description, created_at, snapshot_json) \
         VALUES (?, ?, '空间对话', 1, '{}')",
    )
    .bind(conv_id)
    .bind(conv_id)
    .execute(&env.pool)
    .await
    .unwrap();
}

/// 造一行在途检查点：这一轮还没收尾，或上一轮崩在这里留下的残渣。
///
/// `payload_json` 是已落地工具结果的序列，`'[]'` 即空检查点。
async fn seed_inflight_turn(env: &support::TestEnv, conv_id: &str) {
    sqlx::query(
        "INSERT INTO conversation_inflight_turns \
         (conversation_id, session_id, turn_index, reason, payload_json) \
         VALUES (?, ?, 0, 'executing_tools', '[]')",
    )
    .bind(conv_id)
    .bind(conv_id)
    .execute(&env.pool)
    .await
    .unwrap();
}

/// 清空只删消息与上下文：会话行、产物行都留着。
///
/// 会话行不能删 —— `artifacts.conversation_id` 是 `ON DELETE CASCADE`，删行会
/// 连带抹掉产物登记；快照必须删 —— 留着它，下一轮会把已删的消息水化回模型
/// 上下文。这里不造在途检查点：能清空的前提就是没有一轮在跑。
#[tokio::test]
async fn clear_empties_messages_and_keeps_the_session() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let ensure_uri = format!("/api/spaces/{ws_id}/conversation/ensure");
    let cleared = env.post(&ensure_uri, json!({})).await;
    let conv_id = cleared.body["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    seed_conversation_history(&env, &conv_id).await;

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/clear"),
            json!({}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["success"], json!(true));
    assert_eq!(message_count(&env, &conv_id).await, 0, "消息必须清空");
    assert_eq!(
        space_sessions(&env, &ws_id).await.len(),
        1,
        "会话行本身留着"
    );
    let artifacts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE conversation_id = ?")
            .bind(&conv_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(artifacts, 1, "产物登记行不能随清空消失");
    assert_eq!(
        snapshot_count(&env, &conv_id).await,
        0,
        "快照留着会让下一轮水化出旧上下文"
    );
    assert_eq!(
        inflight_count(&env, &conv_id).await,
        0,
        "清空后不得留下在途检查点"
    );

    // 会话 id 不变，所以前端不必重挂聊天壳：再 ensure 拿回的是同一条空会话。
    let again = env.post(&ensure_uri, json!({})).await;
    assert_eq!(again.status, 200, "body={}", again.body);
    assert_eq!(again.body["conversation_id"], json!(conv_id));
    assert_eq!(again.body["created"], json!(false), "不得新建会话");
    assert_eq!(message_count(&env, &conv_id).await, 0);
}

/// 该会话快照的条数。
async fn snapshot_count(env: &support::TestEnv, conv_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM session_snapshots WHERE conversation_id = ?")
        .bind(conv_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// 在途检查点的条数。
async fn inflight_count(env: &support::TestEnv, conv_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM conversation_inflight_turns WHERE conversation_id = ?")
        .bind(conv_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// 一轮还在跑时拒绝清空：在途轮的落库排在清空之后，放它过去就只清了一半。
///
/// 只注册运行、不写检查点：409 必须由「真的在跑」引起，与库里有没有残渣无关。
/// 断言 messages 一条没少 —— 拒绝必须发生在删消息之前，否则 409 也救不回
/// 已经删掉的那一半。
#[tokio::test]
async fn clear_is_refused_while_a_turn_is_in_flight() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let ensure_uri = format!("/api/spaces/{ws_id}/conversation/ensure");
    let ensured = env.post(&ensure_uri, json!({})).await;
    let conv_id = ensured.body["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    seed_conversation_history(&env, &conv_id).await;

    // 注册表随 `AppState` 持有，`env.post` 每次都新建一份，故这里自己捏住同一个
    // 状态直到请求跑完：接收端与守卫都不能提前 drop，否则条目不算在跑。
    let state = env.app_state();
    let (_control_rx, _guard) = state.chat_runs.register(&conv_id);
    let reply = call(
        &peco_daemon::http::router(state),
        Method::POST,
        &format!("/api/spaces/{ws_id}/conversation/clear"),
        Some(json!({})),
    )
    .await;

    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "conversation_in_flight");
    assert_eq!(
        message_count(&env, &conv_id).await,
        2,
        "拒绝必须先于删消息：一条都不能少"
    );
    assert_eq!(
        snapshot_count(&env, &conv_id).await,
        1,
        "被拒的请求不得动会话快照"
    );
}

/// 崩溃残留的检查点不许挡住清空：那一行只说明上一轮没跑完，不代表现在有运行。
///
/// 变异：门槛改回 `load_inflight().is_some()` → 这条永久 409，清空对崩过一次的
/// 会话彻底失效。
#[tokio::test]
async fn stale_inflight_checkpoint_does_not_block_clear() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;
    let ensure_uri = format!("/api/spaces/{ws_id}/conversation/ensure");
    let ensured = env.post(&ensure_uri, json!({})).await;
    let conv_id = ensured.body["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    seed_conversation_history(&env, &conv_id).await;
    seed_inflight_turn(&env, &conv_id).await;

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/clear"),
            json!({}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["success"], json!(true));
    assert_eq!(message_count(&env, &conv_id).await, 0, "消息必须清空");
    assert_eq!(
        snapshot_count(&env, &conv_id).await,
        0,
        "快照留着会让下一轮水化出旧上下文"
    );
    assert_eq!(
        inflight_count(&env, &conv_id).await,
        0,
        "残渣检查点要随清空一起清掉"
    );
}

/// 没有会话可清时也是成功：前端不必为「本来就干净」分辨一次失败。
#[tokio::test]
async fn clear_without_a_session_is_idempotently_ok() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let reply = env
        .post(
            &format!("/api/spaces/{ws_id}/conversation/clear"),
            json!({}),
        )
        .await;

    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["success"], json!(true));
    assert!(
        space_sessions(&env, &ws_id).await.is_empty(),
        "清空不该顺手建出会话"
    );
}

/// 不存在的 `ws_id` 与 ensure 同一个错误映射。
#[tokio::test]
async fn clear_on_unknown_space_is_404() {
    let env = new_env(&["research"]).await;

    let reply = env
        .post("/api/spaces/ws_ghost/conversation/clear", json!({}))
        .await;

    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found");
}
