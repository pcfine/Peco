//! chat / artifact 两条子树的 HTTP 语义：会话按空间隔离、CRUD 往返、
//! 产物落盘根随空间走、SSE 路由在无可用 Agent 时不挂起。
//!
//! 与四个资源模块共用同一套前置：所有 handler 都要求 `LocalUser`，空间选择器
//! 缺失即 400，未打开即 409。

mod support;

use std::path::Path;
use std::time::Duration;

use peco_core::artifacts::ArtifactDecl;
use serde_json::json;
use support::{call_in_space, new_env};

use axum::http::Method;

/// 建会话的完整路径；`{agent}` 用 `@master` —— 建会话不校验 Agent 是否在盘上
/// （历史可以先于 Agent 存在），这样夹具不必先造一个 agent.md。
fn conversations_uri(agent: &str) -> String {
    format!("/api/chat/{agent}/conversations")
}

/// 建会话的响应必须自报归属空间：前端据此断言列表没有跨空间串数据。
///
/// 变异方法：`create_conversation` 不再写 `workspace_id`（回到按 `user_id` 落行）
/// → 行的 `workspace_id` 为空串，本测试的相等断言变红。
#[tokio::test]
async fn create_conversation_reports_its_workspace() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let created = env
        .post_in(
            &ws_id,
            &conversations_uri("@master"),
            json!({"title": "第一个"}),
        )
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    assert_eq!(
        created.body["workspace_id"],
        json!(ws_id),
        "body={}",
        created.body
    );
    assert_eq!(created.body["title"], json!("第一个"));
    assert_eq!(created.body["archived"], json!(false));

    let conv_id = created.body["id"].as_str().expect("响应必须带 id");

    let listed = env.get_in(&ws_id, &conversations_uri("@master")).await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let items = listed.body.as_array().expect("列表必须是数组");
    assert_eq!(items.len(), 1, "body={}", listed.body);
    assert_eq!(items[0]["id"], json!(conv_id));
    assert_eq!(items[0]["workspace_id"], json!(ws_id));

    // 改名 + 归档 + 恢复：PATCH 的三个分支都要能命中本空间的会话。
    let renamed = env
        .patch_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}"),
            json!({"title": "改名后"}),
        )
        .await;
    assert_eq!(renamed.status, 200, "body={}", renamed.body);
    assert_eq!(renamed.body["title"], json!("改名后"));

    let archived = env
        .patch_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}"),
            json!({"archive": true}),
        )
        .await;
    assert_eq!(
        archived.body["archived"],
        json!(true),
        "body={}",
        archived.body
    );

    let active_only = env.get_in(&ws_id, &conversations_uri("@master")).await;
    assert!(
        active_only
            .body
            .as_array()
            .expect("列表必须是数组")
            .is_empty(),
        "默认只列活跃会话: {}",
        active_only.body
    );
    let with_archived = env
        .get_in(
            &ws_id,
            &format!("{}?status=all", conversations_uri("@master")),
        )
        .await;
    assert_eq!(
        with_archived.body.as_array().expect("列表必须是数组").len(),
        1,
        "status=all 必须带出已归档会话: {}",
        with_archived.body
    );

    // 删除后彻底消失，消息端点也随之 404。
    let deleted = env
        .delete_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}"),
        )
        .await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert_eq!(deleted.body["success"], json!(true));

    let gone = env
        .get_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}/messages"),
        )
        .await;
    assert_eq!(gone.status, 404, "body={}", gone.body);
}

/// 同团队两个空间各建一条会话，彼此的列表与单条读取互不可见。
///
/// 变异方法：`list_conversations` 换回「按 user_id 列」的查询（或去掉
/// `workspace_id` 条件）→ 每个空间都能看到两条，断言变红。
#[tokio::test]
async fn conversations_are_isolated_between_spaces_of_one_team() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("alpha", "research").await;
    let ws_b = env.open_space("beta", "research").await;

    let conv_a = env
        .post_in(&ws_a, &conversations_uri("@master"), json!({"title": "A"}))
        .await;
    let conv_b = env
        .post_in(&ws_b, &conversations_uri("@master"), json!({"title": "B"}))
        .await;
    assert_eq!(conv_a.status, 201, "body={}", conv_a.body);
    assert_eq!(conv_b.status, 201, "body={}", conv_b.body);
    let id_a = conv_a.body["id"].as_str().unwrap().to_string();
    let id_b = conv_b.body["id"].as_str().unwrap().to_string();
    assert_ne!(id_a, id_b);

    let list_a = env.get_in(&ws_a, &conversations_uri("@master")).await;
    let ids_a: Vec<&str> = list_a
        .body
        .as_array()
        .expect("列表必须是数组")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(ids_a, vec![id_a.as_str()], "A 只应看到自己的会话");

    let list_b = env.get_in(&ws_b, &conversations_uri("@master")).await;
    let ids_b: Vec<&str> = list_b
        .body
        .as_array()
        .expect("列表必须是数组")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(ids_b, vec![id_b.as_str()], "B 只应看到自己的会话");

    // 单条读取同理：拿别的空间的会话 id 过来一律 404（不是 403 —— 不泄露存在性）。
    for suffix in ["messages", "session", "artifacts"] {
        let crossed = env
            .get_in(
                &ws_b,
                &format!("/api/chat/@master/conversations/{id_a}/{suffix}"),
            )
            .await;
        assert_eq!(crossed.status, 404, "{suffix} body={}", crossed.body);
    }
}

/// 缺空间选择器 ⇒ 400 `space_required`（chat 与 artifact 两条子树都是强制项）。
///
/// 变异方法：让 `LocalUser` 在两者皆缺时回退到任意已索引空间 → 变成 2xx/404，
/// 本测试的红线就没了。
#[tokio::test]
async fn chat_and_artifact_routes_require_a_space_selector() {
    let env = new_env(&["research"]).await;

    for (method, uri) in [
        (Method::GET, "/api/chat/@master/conversations".to_string()),
        (Method::POST, "/api/chat/@master/conversations".to_string()),
        (Method::GET, "/api/artifacts/art_1/content".to_string()),
        (Method::GET, "/api/artifacts/art_1/download".to_string()),
    ] {
        let reply = call_in_space(&env.router(), method.clone(), &uri, None, None).await;
        assert_eq!(reply.status, 400, "{method} {uri} body={}", reply.body);
        assert_eq!(reply.code(), "space_required", "{method} {uri}");
    }
}

/// 空间已索引但未打开 ⇒ 409 `space_not_open`：需要实例的端点必须先让空间打开。
///
/// 变异方法：把 `SpaceRegistry::handle` 改成 `ensure_open`（顺手打开）→ 变 200。
#[tokio::test]
async fn chat_routes_need_an_open_space() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let created = env
        .post_in(&ws_id, &conversations_uri("@master"), json!({}))
        .await;
    let conv_id = created.body["id"].as_str().unwrap().to_string();

    let closed = env
        .post(&format!("/api/spaces/{ws_id}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    // 摘要与产物列表都要拿实例（前者只读库，后者要 stat 文件）：后者 409。
    let artifacts = env
        .get_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}/artifacts"),
        )
        .await;
    assert_eq!(artifacts.status, 409, "body={}", artifacts.body);
    assert_eq!(
        artifacts.code(),
        "space_not_open",
        "body={}",
        artifacts.body
    );

    let stream = env
        .get_in(
            &ws_id,
            &format!("/api/chat/@master/conversations/{conv_id}/stream?message=hi"),
        )
        .await;
    assert_eq!(stream.status, 409, "body={}", stream.body);
    assert_eq!(stream.code(), "space_not_open", "body={}", stream.body);
}

/// 产物的 `rel_path` 相对各自空间的 work 根：同团队两个空间不共享落盘根。
///
/// 变异方法：落盘根换回全局 `data_dir/work/<user_id>` → 两行的 `rel_path`
/// 相同、且拼出的绝对路径落不进各自的 work 根，前缀断言变红。
#[tokio::test]
async fn artifact_rel_path_is_relative_to_each_space_work_dir() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("alpha", "research").await;
    let ws_b = env.open_space("beta", "research").await;

    let conv_a = env
        .post_in(&ws_a, &conversations_uri("@master"), json!({}))
        .await;
    let conv_b = env
        .post_in(&ws_b, &conversations_uri("@master"), json!({}))
        .await;
    let conv_a_id = conv_a.body["id"].as_str().unwrap().to_string();
    let conv_b_id = conv_b.body["id"].as_str().unwrap().to_string();

    let mut persisted = Vec::new();
    for ws_id in [&ws_a, &ws_b] {
        let handle = env.open(ws_id).await.expect("空间必须已打开");
        let ws = handle.workspace();
        let output = ws.work_dir().output().to_path_buf();
        std::fs::create_dir_all(&output).unwrap();
        let file = output.join("report.txt");
        std::fs::write(&file, b"hello").unwrap();
        persisted.push((ws, file));
    }

    let mut rel_paths = Vec::new();
    for (index, (ws, file)) in persisted.iter().enumerate() {
        let conv_id = if index == 0 { &conv_a_id } else { &conv_b_id };
        let decl = ArtifactDecl {
            path: file.to_string_lossy().into_owned(),
            title: Some("report.txt".into()),
            mime: None,
        };
        let item = peco_daemon::artifact::persist_declaration(
            &env.pool,
            ws.work_dir(),
            conv_id,
            peco_daemon::LOCAL_USER_ID,
            &decl,
        )
        .await
        .expect("落库不得失败")
        .expect("首次声明必须登记成功");

        assert_eq!(item.kind, "document");
        assert_eq!(item.title, "report.txt");
        let row = peco_daemon::db::artifacts::find_by_id_and_user(
            &env.pool,
            &item.id,
            peco_daemon::LOCAL_USER_ID,
        )
        .await
        .unwrap()
        .expect("登记后必须能读回行");
        assert_eq!(row.rel_path, "output/report.txt");
        assert!(
            row.rel_path.starts_with("output/"),
            "产物必须相对 work 根: {}",
            row.rel_path
        );
        let resolved = ws.work_dir().root().join(&row.rel_path);
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(file).unwrap(),
            "rel_path 必须在自己的 work 根下解析回原文件"
        );
        rel_paths.push(resolved);
    }

    let (a, b) = (rel_paths[0].clone(), rel_paths[1].clone());
    assert_ne!(a, b, "两个空间的产物绝对路径不得相同");
    assert!(
        !a.starts_with(Path::new(b.parent().unwrap())),
        "A 的产物的落点是 {}，不该在 B 的目录里",
        a.display()
    );

    // 取流端点必须能把 `rel_path` 解析回本空间的文件：落盘根一旦换成全局目录，
    // 这里就从 200 变成 404。
    let artifact_id = peco_daemon::db::artifacts::list_by_conversation(&env.pool, &conv_a_id)
        .await
        .unwrap()[0]
        .id
        .clone();
    let served = env
        .get_in(&ws_a, &format!("/api/artifacts/{artifact_id}/content"))
        .await;
    assert_eq!(
        served.status, 200,
        "内容端点必须在本空间的 work 根下找到文件: {}",
        served.body
    );
    let crossed_content = env
        .get_in(&ws_b, &format!("/api/artifacts/{artifact_id}/content"))
        .await;
    assert_eq!(
        crossed_content.status, 404,
        "别的空间不得取到本空间的产物: {}",
        crossed_content.body
    );

    // HTTP 面同样按空间过滤：拿 A 的会话 id 走 B 的空间选择器 ⇒ 404。
    let crossed = env
        .get_in(
            &ws_b,
            &format!("/api/chat/@master/conversations/{conv_a_id}/artifacts"),
        )
        .await;
    assert_eq!(crossed.status, 404, "body={}", crossed.body);

    let own = env
        .get_in(
            &ws_a,
            &format!("/api/chat/@master/conversations/{conv_a_id}/artifacts"),
        )
        .await;
    assert_eq!(own.status, 200, "body={}", own.body);
    let items = own.body.as_array().expect("列表必须是数组");
    assert_eq!(items.len(), 1, "body={}", own.body);
    assert_eq!(items[0]["title"], json!("report.txt"));
    assert_eq!(items[0]["available"], json!(true));
    assert!(
        items[0]["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("/api/artifacts/")),
        "取流地址必须指回本模块: {}",
        own.body
    );
}

/// 无可用 provider 时 SSE 路由必须给出确定的失败，而不是挂着不返回。
///
/// 「不挂住」是本测试的主判据：夹具里的 Agent 指向一个 providers.toml 里不存在
/// 的 provider，实例加载即失败，流式请求应在握手前变成 5xx。若将来改成先建流再
/// 报错，响应体里必须出现 `event: error` —— 两种形态都算「明确」，`timeout` 是
/// 唯一的红/绿分界。
///
/// 变异方法：把 handler 里的 Agent 加载挪进 `tokio::spawn`（响应先 200、错误只
/// 打到日志）→ 本测试拿到 200 且体里没有 error 事件，两条断言同时变红。
#[tokio::test]
async fn stream_without_a_usable_provider_fails_fast() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let agent = env
        .post_in(
            &ws_id,
            "/api/agents",
            json!({"name": "reviewer", "provider": "no-such-provider"}),
        )
        .await;
    assert_eq!(agent.status, 201, "body={}", agent.body);

    let conv = env
        .post_in(&ws_id, &conversations_uri("reviewer"), json!({}))
        .await;
    assert_eq!(conv.status, 201, "body={}", conv.body);
    let conv_id = conv.body["id"].as_str().unwrap();

    let uri = format!("/api/chat/reviewer/conversations/{conv_id}/stream?message=hi");
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        call_in_space(&env.router(), Method::GET, &uri, None, Some(&ws_id)),
    )
    .await
    .expect("流式请求不得挂住");

    if reply.status.is_success() {
        let body = reply.body.to_string();
        assert!(
            body.contains("event: error"),
            "2xx 必须带明确的 error 事件，实际: {body}"
        );
    } else {
        assert!(
            reply.status.is_server_error(),
            "失败必须是 5xx，实际 {} body={}",
            reply.status,
            reply.body
        );
        // 非 `Coded` 变体不带 `code` 字段：类别走 `error`，细分文案在 `details`。
        assert_eq!(
            reply.body["error"],
            json!("internal"),
            "body={}",
            reply.body
        );
        assert!(
            reply.body["details"]
                .as_str()
                .is_some_and(|details| details.contains("no-such-provider")),
            "错误文案要点明是哪个 provider 不可用: {}",
            reply.body
        );
    }
}
