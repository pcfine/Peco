//! `/api/spaces/{ws_id}/memory/documents` —— 按空间浏览它自己的记忆库。
//!
//! 重点在**读的是哪个库**：每个空间一份物理记忆库（`$PECO_HOME/memory/<ws_id>/`），
//! 端点必须读本空间那一份，而不是团队库。夹具刻意把同一条内容同时写进两个空间的
//! 库与团队库，让「读错了库」必然表现为串记忆。

mod support;

use std::sync::Arc;

use knowledge_base::{
    BackendType, ChunkingStrategySerde, DocumentMetadata, FastembedModelTypeSerde, KbConfig,
};
use peco_core::knowledge::KnowledgeManager;
use peco_core::tools::KnowledgeAccess;
use peco_daemon::memory::WS_ID_TAG;
use serde_json::json;
use support::{TestEnv, new_env};

const MEMORY_KB: &str = "@private_memory";

/// 一条待写入的记忆：`ws_id` 为 `None` = 没有归属标签（迁移过来的老记忆就是这样）。
struct Doc<'a> {
    ws_id: Option<&'a str>,
    source: &'a str,
    title: &'a str,
    content: &'a str,
}

fn doc<'a>(ws_id: Option<&'a str>, source: &'a str, title: &'a str, content: &'a str) -> Doc<'a> {
    Doc {
        ws_id,
        source,
        title,
        content,
    }
}

/// 空间自己的记忆管理器（打开空间时注入的那一份）。
async fn memory_km(env: &TestEnv, ws_id: &str) -> Arc<KnowledgeManager> {
    let handle = env.open(ws_id).await.expect("空间应已打开");
    handle
        .workspace()
        .memory_manager()
        .cloned()
        .expect("打开的空间必须带记忆管理器")
}

/// 团队库（升级前的共享记忆库）；端点不该读它。
///
/// 模板已不再携带团队级 `@private_memory`，这里手工建一份「存量旧库」。
async fn team_km(env: &TestEnv, ws_id: &str) -> Arc<KnowledgeManager> {
    let handle = env.open(ws_id).await.expect("空间应已打开");
    let km = Arc::clone(handle.workspace().knowledge_manager());
    let existing = km.list_kbs().await.expect("列出团队库");
    if existing.iter().any(|kb| kb.name == MEMORY_KB) {
        return km;
    }
    km.create_kb(KbConfig {
        name: MEMORY_KB.into(),
        description: "团队级旧记忆库".into(),
        embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
        chunking: ChunkingStrategySerde::FixedSize { size: 100 },
        backend: BackendType::InMemory,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    })
    .await
    .expect("建出团队级旧记忆库");
    km
}

/// 把文档写进给定管理器的 `@private_memory`，返回与 `docs` 同序的 id。
async fn write(km: &Arc<KnowledgeManager>, docs: &[Doc<'_>]) -> Vec<String> {
    let mut ids = Vec::new();
    for doc in docs {
        let mut metadata = DocumentMetadata::default();
        if let Some(tag) = doc.ws_id {
            metadata
                .extra
                .insert(WS_ID_TAG.to_string(), tag.to_string());
        }
        let written = km
            .add_text_to_kb_with_metadata(MEMORY_KB, doc.title, doc.content, doc.source, metadata)
            .await
            .expect("写入记忆库");
        ids.push(written.id);
    }
    ids
}

async fn raw_titles(km: &Arc<KnowledgeManager>) -> Vec<String> {
    km.list_memory_documents_with_content(MEMORY_KB, 100)
        .await
        .expect("列出记忆库")
        .into_iter()
        .map(|doc| doc.title)
        .collect()
}

fn item_titles(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap().to_string())
        .collect()
}

/// 顺序不是契约的一部分（写入时刻相同的两条只由 id 兜底定序），断言只比集合。
fn sorted_item_titles(body: &serde_json::Value) -> Vec<String> {
    let mut titles = item_titles(body);
    titles.sort();
    titles
}

/// 两个空间各写两条记忆 ⇒ 各自的列表只含自己的（双向）。
///
/// 变异：把 handler 里的记忆管理器换回 `ws.knowledge_manager()`（团队库）→
/// ws_a 的列表会多出团队库那条，两条 `total` 断言与内容断言同时变红。
#[tokio::test]
async fn list_stays_inside_its_own_space_in_both_directions() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;

    write(
        &memory_km(&env, &ws_a).await,
        &[
            doc(None, "ppa_semantic", "a_rust", "用户的主开发语言是 Rust"),
            doc(Some(&ws_a), "ppa_episodic", "a_release", "周五发布 v2"),
        ],
    )
    .await;
    write(
        &memory_km(&env, &ws_b).await,
        &[
            doc(
                None,
                "ppa_semantic",
                "b_python",
                "用户的主开发语言是 Python",
            ),
            doc(None, "ppa_episodic", "b_standup", "周一开例会"),
        ],
    )
    .await;
    // 团队库里也放一条同名库的文档：端点读团队库就会把它列出来。
    write(
        &team_km(&env, &ws_a).await,
        &[doc(
            Some(&ws_a),
            "ppa_semantic",
            "team_leftover",
            "团队级旧记忆",
        )],
    )
    .await;

    // 前提：两份实例里都躺着别人的 / 无标签的文档 —— 否则本用例测不到「读哪个库」。
    assert_eq!(raw_titles(&memory_km(&env, &ws_a).await).await.len(), 2);

    let from_a = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents"))
        .await;
    assert_eq!(from_a.status, 200, "body={}", from_a.body);
    assert_eq!(from_a.body["total"], json!(2), "body={}", from_a.body);
    assert_eq!(
        sorted_item_titles(&from_a.body),
        vec!["a_release", "a_rust"],
        "无标签的记忆同样是本空间的（迁移过来的老记忆没有标签）"
    );
    let a_text = from_a.body.to_string();
    assert!(!a_text.contains("Python"), "不得列出 ws_b 的记忆: {a_text}");
    assert!(
        !a_text.contains("团队级旧记忆"),
        "不得列出团队库的文档: {a_text}"
    );

    let from_b = env
        .get(&format!("/api/spaces/{ws_b}/memory/documents"))
        .await;
    assert_eq!(from_b.status, 200, "body={}", from_b.body);
    assert_eq!(from_b.body["total"], json!(2), "body={}", from_b.body);
    assert_eq!(
        sorted_item_titles(&from_b.body),
        vec!["b_python", "b_standup"]
    );
    let b_text = from_b.body.to_string();
    assert!(!b_text.contains("Rust"), "不得列出 ws_a 的记忆: {b_text}");
    assert!(
        !b_text.contains("团队级旧记忆"),
        "不得列出团队库的文档: {b_text}"
    );
}

/// 空空间：别人的库里写满了，本空间的库还没内容 ⇒ 列表为空，`total` 也是 0。
#[tokio::test]
async fn empty_space_lists_nothing_even_when_other_libraries_have_documents() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;
    write(
        &memory_km(&env, &ws_a).await,
        &[doc(
            None,
            "ppa_semantic",
            "a_rust",
            "用户的主开发语言是 Rust",
        )],
    )
    .await;
    write(
        &team_km(&env, &ws_a).await,
        &[doc(None, "ppa_semantic", "team_leftover", "团队级旧记忆")],
    )
    .await;

    let reply = env
        .get(&format!("/api/spaces/{ws_b}/memory/documents"))
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["total"], json!(0));
    assert_eq!(reply.body["items"], json!([]));
}

/// `category` 按 source 过滤，`query` 对标题 + 正文做大小写不敏感子串匹配。
#[tokio::test]
async fn category_and_query_narrow_within_the_space() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    write(
        &memory_km(&env, &ws_a).await,
        &[
            doc(None, "ppa_profile", "memory_1", "用户偏好简洁的回答"),
            doc(None, "ppa_semantic", "memory_2", "用户在用 Rust 写 daemon"),
            doc(None, "ppa_episodic", "memory_3", "周五上线"),
        ],
    )
    .await;

    let url = |tail: &str| format!("/api/spaces/{ws_a}/memory/documents{tail}");

    let semantic = env.get(&url("?category=semantic")).await;
    assert_eq!(semantic.status, 200, "body={}", semantic.body);
    assert_eq!(item_titles(&semantic.body), vec!["memory_2"]);
    assert_eq!(semantic.body["items"][0]["category"], json!("semantic"));
    assert_eq!(semantic.body["items"][0]["source"], json!("ppa_semantic"));

    let profile = env.get(&url("?category=profile")).await;
    assert_eq!(item_titles(&profile.body), vec!["memory_1"]);

    let episodic = env.get(&url("?category=episodic")).await;
    assert_eq!(item_titles(&episodic.body), vec!["memory_3"]);

    // 正文里是小写 rust，查询用大写：大小写不敏感才算匹配到位。
    let rust = env.get(&url("?query=RUST")).await;
    assert_eq!(item_titles(&rust.body), vec!["memory_2"]);
    assert_eq!(rust.body["total"], json!(1));

    // 标题命中也算：`memory_3` 只在标题里出现。
    let by_title = env.get(&url("?query=memory_3")).await;
    assert_eq!(item_titles(&by_title.body), vec!["memory_3"]);

    let both = env.get(&url("?query=rust&category=episodic")).await;
    assert_eq!(
        both.body["total"],
        json!(0),
        "两个条件是交集: {}",
        both.body
    );

    // 空查询串是「没填」而不是「找空字符串」：前端清空搜索框后必须回到全量。
    let blank = env.get(&url("?query=")).await;
    assert_eq!(blank.body["total"], json!(3), "body={}", blank.body);

    // 取值写错要给明确的 400，而不是悄悄返回整份列表。
    let bogus = env.get(&url("?category=unknown")).await;
    assert_eq!(bogus.status, 400, "body={}", bogus.body);
    assert_eq!(bogus.code(), "invalid_argument");
}

/// 分页边界：`limit` 默认 20、上限 100，`offset` 默认 0；越界页返回空。
#[tokio::test]
async fn pagination_covers_the_offset_boundaries() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    write(
        &memory_km(&env, &ws_a).await,
        &[
            doc(None, "ppa_semantic", "p1", "记忆一"),
            doc(None, "ppa_semantic", "p2", "记忆二"),
            doc(None, "ppa_semantic", "p3", "记忆三"),
            doc(None, "ppa_semantic", "p4", "记忆四"),
            doc(None, "ppa_semantic", "p5", "记忆五"),
        ],
    )
    .await;

    let url = |tail: &str| format!("/api/spaces/{ws_a}/memory/documents{tail}");

    let first = env.get(&url("?limit=2")).await;
    assert_eq!(first.status, 200, "body={}", first.body);
    assert_eq!(first.body["total"], json!(5));
    assert_eq!(first.body["items"].as_array().unwrap().len(), 2);

    let second = env.get(&url("?limit=2&offset=2")).await;
    assert_eq!(second.body["total"], json!(5));
    assert_eq!(second.body["items"].as_array().unwrap().len(), 2);

    let last = env.get(&url("?limit=2&offset=4")).await;
    assert_eq!(last.body["items"].as_array().unwrap().len(), 1);

    let beyond = env.get(&url("?limit=2&offset=5")).await;
    assert_eq!(beyond.body["total"], json!(5), "越界页仍要知道总数");
    assert_eq!(beyond.body["items"], json!([]));

    // 默认 `limit` 是 20：5 条一次给完，不需要调用方传参。
    let default_page = env.get(&url("")).await;
    assert_eq!(default_page.body["items"].as_array().unwrap().len(), 5);

    // 超过上限按 100 处理：本条只断言「不是报错、且给全」。
    let capped = env.get(&url("?limit=1000")).await;
    assert_eq!(capped.status, 200, "body={}", capped.body);
    assert_eq!(capped.body["items"].as_array().unwrap().len(), 5);

    // 三页拼起来正好是全集：分页不能漏条也不能重条。
    let mut seen: Vec<String> = Vec::new();
    for offset in [0, 2, 4] {
        let page = env.get(&url(&format!("?limit=2&offset={offset}"))).await;
        seen.extend(item_titles(&page.body));
    }
    seen.sort();
    assert_eq!(seen, vec!["p1", "p2", "p3", "p4", "p5"]);

    let negative = env.get(&url("?offset=-1")).await;
    assert_eq!(negative.status, 400, "body={}", negative.body);
    assert_eq!(negative.code(), "invalid_argument");
}

/// 详情：本空间的文档给完整正文（不是截图式截断），并带上 metadata。
#[tokio::test]
async fn detail_returns_full_content_for_own_documents() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let long_body = format!("{}尾巴", "长".repeat(300));
    let ids = write(
        &memory_km(&env, &ws_a).await,
        &[doc(Some(&ws_a), "ppa_semantic", "memory_long", &long_body)],
    )
    .await;

    let reply = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents/{}", ids[0]))
        .await;
    assert_eq!(reply.status, 200, "body={}", reply.body);
    assert_eq!(reply.body["title"], json!("memory_long"));
    assert_eq!(reply.body["category"], json!("semantic"));
    assert_eq!(reply.body["source"], json!("ppa_semantic"));
    assert_eq!(
        reply.body["content"].as_str().unwrap(),
        long_body,
        "详情给全文，不是列表里的 200 字摘要"
    );
    assert!(
        reply.body["metadata"][WS_ID_TAG].as_str() == Some(ws_a.as_str()),
        "metadata 原样返回: {}",
        reply.body
    );
    assert!(
        reply.body["created_at"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "写入时填充的 created_at 应一并返回: {}",
        reply.body
    );
}

/// 跨空间与不存在回同一个 404：分开报就等于确认「这个 id 真实存在」。
///
/// 变异：详情改成读团队库 → 下面第二条（别的空间的文档）会变成 200。
#[tokio::test]
async fn detail_hides_foreign_and_missing_documents_behind_404() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;
    let own = write(
        &memory_km(&env, &ws_a).await,
        &[doc(
            None,
            "ppa_semantic",
            "a_rust",
            "用户的主开发语言是 Rust",
        )],
    )
    .await;
    let foreign = write(
        &memory_km(&env, &ws_b).await,
        &[doc(
            None,
            "ppa_semantic",
            "b_python",
            "用户的主开发语言是 Python",
        )],
    )
    .await;

    // 前提：这条路在本空间里是通的，否则「跨空间 404」可能只是路由不通。
    let ok = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents/{}", own[0]))
        .await;
    assert_eq!(ok.status, 200, "body={}", ok.body);

    for (doc_id, label) in [
        (foreign[0].clone(), "别的空间的文档"),
        ("doc_does_not_exist".to_string(), "不存在的 id"),
    ] {
        let reply = env
            .get(&format!("/api/spaces/{ws_a}/memory/documents/{doc_id}"))
            .await;
        assert_eq!(reply.status, 404, "{label} 必须 404: {}", reply.body);
        assert_eq!(reply.code(), "memory_document_not_found", "{label}");
        assert!(
            !reply.body.to_string().contains("Python"),
            "{label} 不得泄漏正文: {}",
            reply.body
        );
    }
}

/// 空间未打开 ⇒ 409，空间不存在 ⇒ 404：读实例的门槛与其它空间作用域接口一致。
#[tokio::test]
async fn memory_endpoints_require_an_open_space() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ids = write(
        &memory_km(&env, &ws_a).await,
        &[doc(
            None,
            "ppa_semantic",
            "a_rust",
            "用户的主开发语言是 Rust",
        )],
    )
    .await;

    let closed = env
        .post(
            &format!("/api/spaces/{ws_a}/close"),
            json!({"force": false}),
        )
        .await;
    assert_eq!(closed.status, 200, "body={}", closed.body);

    let list = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents"))
        .await;
    assert_eq!(list.status, 409, "body={}", list.body);
    assert_eq!(list.code(), "space_not_open");

    let detail = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents/{}", ids[0]))
        .await;
    assert_eq!(detail.status, 409, "body={}", detail.body);
    assert_eq!(detail.code(), "space_not_open");

    // 未索引的 ws_id 是「没有这个空间」，与「有这个空间但没打开」分开报。
    let ghost = env.get("/api/spaces/ws_ghost/memory/documents").await;
    assert_eq!(ghost.status, 404, "body={}", ghost.body);
    assert_eq!(ghost.code(), "space_not_found");
}

/// 记忆库配置被删掉（库不存在）⇒ 空列表，不是 500；详情仍然是 404。
#[tokio::test]
async fn missing_memory_kb_lists_nothing() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    // 删掉配置 = 这个空间从未建过库；打开路径只保证「首次打开时写默认配置」。
    std::fs::remove_file(
        env.home
            .join("memory")
            .join(&ws_a)
            .join("@private_memory")
            .join("kb_config.json"),
    )
    .unwrap();

    let list = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents"))
        .await;
    assert_eq!(list.status, 200, "body={}", list.body);
    assert_eq!(list.body["total"], json!(0));
    assert_eq!(list.body["items"], json!([]));

    let detail = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents/doc_missing"))
        .await;
    assert_eq!(detail.status, 404, "body={}", detail.body);
    assert_eq!(detail.code(), "memory_document_not_found");
}
