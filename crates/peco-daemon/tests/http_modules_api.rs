//! agent / skill / knowledge / mcp 四条子树的 HTTP 语义：空间选择器、稳定性 code、
//! 与「单用户 + 按空间隔离」的身份约定。

mod support;

use serde_json::json;
use support::new_env;

/// agent CRUD 往返：建 → 列表可见 → 读正文 → 删 → 列表不可见。
///
/// 变异方法：① 删掉 `create` 里的 `agents::insert` → 列表为空；
/// ② 删掉 `agent_manager().save` → 读正文 500；③ `delete` 不删 DB 行 → 删后仍在列表。
#[tokio::test]
async fn agent_crud_roundtrip_on_a_space() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let created = env
        .post_in(
            &ws_id,
            "/api/agents",
            json!({
                "name": "reviewer",
                "description": "code review",
                "system_prompt": "You review code.",
            }),
        )
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);
    let agent_id = created.body["id"]
        .as_str()
        .expect("响应必须带 id")
        .to_string();

    let listed = env.get_in(&ws_id, "/api/agents").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let items = listed.body.as_array().expect("列表必须是数组");
    assert!(
        items.iter().any(|item| item["id"] == json!(agent_id)),
        "新建的 agent 必须出现在列表里: {}",
        listed.body
    );

    let detail = env.get_in(&ws_id, &format!("/api/agents/{agent_id}")).await;
    assert_eq!(detail.status, 200, "body={}", detail.body);
    assert_eq!(detail.body["system_prompt"], json!("You review code."));
    assert_eq!(detail.body["name"], json!("reviewer"));

    let deleted = env
        .delete_in(&ws_id, &format!("/api/agents/{agent_id}"))
        .await;
    assert_eq!(deleted.status, 200, "body={}", deleted.body);
    assert_eq!(deleted.body["success"], json!(true));

    let after = env.get_in(&ws_id, "/api/agents").await;
    let items = after.body.as_array().expect("列表必须是数组");
    assert!(
        !items.iter().any(|item| item["id"] == json!(agent_id)),
        "删除后不得再出现在列表里: {}",
        after.body
    );
    assert!(
        env.work_dir_of(&ws_id).await.is_dir(),
        "删除 agent 不得连带删掉空间目录"
    );
    // agent 文件树挂在团队能力根下（同团队的空间共用），不在空间目录里。
    let agent_dir = env
        .home
        .join("teams")
        .join("research")
        .join("agents")
        .join("reviewer");
    assert!(
        !agent_dir.exists(),
        "删除应连带移除 {}",
        agent_dir.display()
    );
}

/// 缺 `X-Peco-Space` 且缺 `?ws=` ⇒ 400 `space_required`。
///
/// 变异方法：让提取器在两者皆缺时回退到任意已索引空间 → 变成 200，本测试变红。
#[tokio::test]
async fn missing_space_selector_is_400_space_required() {
    let env = new_env(&["research"]).await;

    for uri in ["/api/agents", "/api/skills", "/api/mcp", "/api/knowledge"] {
        let reply = env.get(uri).await;
        assert_eq!(reply.status, 400, "{uri} body={}", reply.body);
        assert_eq!(reply.code(), "space_required", "{uri} body={}", reply.body);
    }
}

/// 查询参数是 SSE/EventSource 的兜底通道：只带 `?ws=` 也必须被接受。
///
/// 变异方法：删掉 `query_space` 回退 → 本测试 400，而事件流永远拿不到数据。
#[tokio::test]
async fn space_can_come_from_the_query_parameter() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let reply = env.get(&format!("/api/skills?ws={ws_id}")).await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert!(reply.body.is_array(), "body={}", reply.body);
}

/// 未知空间 ⇒ 404 `space_not_found`（DB 无行，不是「没打开」）。
///
/// 变异方法：把提取器的存在性检查删掉 → 落到 handler 的 409，状态码与 code 都变。
#[tokio::test]
async fn unknown_space_is_404_space_not_found() {
    let env = new_env(&["research"]).await;

    let reply = env.get_in("ws_does_not_exist", "/api/agents").await;
    assert_eq!(reply.status, 404, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_found", "body={}", reply.body);
}

/// 有索引行但实例不在表里（已关闭）⇒ 409 `space_not_open`。
///
/// 变异方法：`SpaceRegistry::handle` 改为 `ensure_open`（顺手打开）→ 变成 200，
/// 「先打开再用」的调用顺序约束就消失了。
#[tokio::test]
async fn indexed_but_closed_space_is_409_space_not_open() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let closed = env
        .post(&format!("/api/spaces/{ws_id}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let reply = env.get_in(&ws_id, "/api/skills").await;
    assert_eq!(reply.status, 409, "body={}", reply.body);
    assert_eq!(reply.code(), "space_not_open", "body={}", reply.body);
}

/// `@master` 与裸写的 `master` 都不能被新建 ⇒ 400 `reserved_agent`，且不留任何痕迹。
///
/// 变异方法：删掉 `create` 里的 `ensure_writable_name` 调用 → 变成 201，
/// 空间的 `get_master_agent` 从此指向新建的那个 Agent。
#[tokio::test]
async fn creating_a_master_named_agent_is_400_reserved_agent() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    for name in ["@master", "master"] {
        let reply = env
            .post_in(&ws_id, "/api/agents", json!({"name": name}))
            .await;
        assert_eq!(reply.status, 400, "{name} body={}", reply.body);
        assert_eq!(reply.code(), "reserved_agent", "{name} body={}", reply.body);
    }

    let listed = env.get_in(&ws_id, "/api/agents").await;
    let items = listed.body.as_array().expect("列表必须是数组");
    assert!(
        !items
            .iter()
            .any(|item| item["name"] == json!("@master") || item["name"] == json!("master")),
        "保留名不得落库: {}",
        listed.body
    );
}

/// 删除总管 Agent ⇒ 400 `reserved_agent`，且 DB 索引行原样保留。
///
/// 保留名在这里写成字面量：它是 API 契约的一部分（前端与文档都用 `@master` 指代
/// 总管），实现侧则经 core 的 `is_master_agent` / `ensure_deletable` 判定。
///
/// 变异方法：把 `ensure_deletable` 前置检查挪到 DB 删除之后 → 行被删、文件还在，
/// 「索引行仍在」这条断言变红。
#[tokio::test]
async fn deleting_the_master_agent_is_400_reserved_agent() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    // 空空间没有模板 Agent，补一行索引即足以走到保留名判定。
    sqlx::query(
        "INSERT INTO agents (id, user_id, team_id, name, description) \
         VALUES ('a_master', 'local', 'research', '@master', '')",
    )
    .execute(&env.pool)
    .await
    .expect("补一行总管索引");

    let reply = env.delete_in(&ws_id, "/api/agents/a_master").await;
    assert_eq!(reply.status, 400, "body={}", reply.body);
    assert_eq!(reply.code(), "reserved_agent", "body={}", reply.body);

    let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agents WHERE id = 'a_master'")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "保留名被拒时不得删掉索引行");
}

/// skill 列表在已打开空间上返回数组（200 本身不足以说明读到了实例）。
///
/// 变异方法：handler 改成返回 `Vec::new()` 的空实现 → 数组仍在但保存后不出现，
/// 故这里额外断言保存过的 skill 在列表里。
#[tokio::test]
async fn skill_list_returns_the_saved_skill_as_an_array() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let saved = env
        .put_in(
            &ws_id,
            "/api/skills/alpha-skill",
            json!({"content": "---\nname: alpha-skill\ndescription: demo\n---\n\nbody\n"}),
        )
        .await;
    assert_eq!(saved.status, 200, "body={}", saved.body);

    let listed = env.get_in(&ws_id, "/api/skills").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let skills = listed.body.as_array().expect("body 必须是数组");
    assert!(
        skills
            .iter()
            .any(|skill| skill["name"] == json!("alpha-skill")),
        "保存过的 skill 必须出现在列表里: {}",
        listed.body
    );
}

/// 知识库列表：索引行在 DB，实例缺失（未打开）时仍返回数组，计数按 0。
///
/// 变异方法：把列表 handler 改成硬依赖 `spaces.workspace(...)?` → 未打开空间变 409，
/// 「列表类接口允许操作未打开空间」这条约束被破坏。
#[tokio::test]
async fn knowledge_list_works_without_an_open_instance() {
    let env = new_env(&["research"]).await;
    let ws_id = env.open_space("alpha", "research").await;

    let created = env
        .post_in(&ws_id, "/api/knowledge", json!({"name": "kb-one"}))
        .await;
    assert_eq!(created.status, 201, "body={}", created.body);

    let closed = env
        .post(&format!("/api/spaces/{ws_id}/close"), json!({}))
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let listed = env.get_in(&ws_id, "/api/knowledge").await;
    assert_eq!(listed.status, 200, "body={}", listed.body);
    let kbs = listed.body.as_array().expect("body 必须是数组");
    assert_eq!(kbs.len(), 1, "索引行不需要实例: {}", listed.body);
    assert_eq!(kbs[0]["name"], json!("kb-one"));
    assert_eq!(kbs[0]["document_count"], json!(0));
}
