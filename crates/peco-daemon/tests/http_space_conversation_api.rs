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
