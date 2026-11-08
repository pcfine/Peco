//! `/api/spaces/{ws_id}/memory/documents` —— 按空间浏览记忆。
//!
//! 重点不在「能不能列出来」，而在**归属只由 `ws_id` 标签决定**：同团队的空间
//! 共用一套物理记忆库，物理上彼此可见，可见性因此完全由过滤决定。用例的夹具
//! 刻意把同一批文档写进两个空间的库里，让「去掉过滤」必然表现为串记忆。

mod support;

use std::sync::{Arc, OnceLock};

use knowledge_base::{
    BackendType, ChunkingStrategySerde, DocumentMetadata, FastembedModelTypeSerde, KbConfig,
};
use peco_core::knowledge::KnowledgeManager;
use peco_daemon::memory::WS_ID_TAG;
use serde_json::json;
use support::{TestEnv, new_env};

const MEMORY_KB: &str = "@private_memory";

/// 并发建库会抢同一份 fastembed 模型缓存，抢输的那次初始化直接失败 ——
/// 夹具因此串行建库；库建成之后的读写互不影响。
static KB_BUILD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// 一条待写入的记忆：`ws_id` 为 `None` 就是升级前的老记忆（没有归属标签）。
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

async fn memory_km(env: &TestEnv, ws_id: &str) -> Arc<KnowledgeManager> {
    env.open(ws_id)
        .await
        .expect("空间应已打开")
        .workspace()
        .knowledge_manager()
        .clone()
}

/// 保证 `@private_memory` 存在。第二个 `KnowledgeManager` 实例会从磁盘上的
/// `kb_config.json` 打开同一个库，因此只需建一次。
async fn ensure_memory_kb(km: &KnowledgeManager) {
    let _guard = KB_BUILD
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let existing = km.list_kbs().await.unwrap();
    if existing.iter().any(|kb| kb.name == MEMORY_KB) {
        return;
    }
    km.create_kb(KbConfig {
        name: MEMORY_KB.into(),
        description: "测试记忆库".into(),
        embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
        chunking: ChunkingStrategySerde::FixedSize { size: 100 },
        backend: BackendType::InMemory,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    })
    .await
    .unwrap();
}

/// 把同一批文档写进给定空间的库里，返回与 `docs` 同序的文档 id。
///
/// 写两份是夹具的刻意设计：生产上同团队的空间共用一套物理 KB（`kb_scope` =
/// team_id），而测试用的 InMemory 后端是进程实例级的，各 `WorkSpace` 一份。
/// 只写一份的话，「对方空间看不到我的记忆」会因为对方库里根本没有这些文档而
/// 恒成立 —— 去掉 ws_id 过滤也照样通过，隔离用例就成了假绿。
async fn seed(env: &TestEnv, spaces: &[&str], docs: &[Doc<'_>]) -> Vec<String> {
    let mut ids = Vec::new();
    for (index, ws_id) in spaces.iter().enumerate() {
        let km = memory_km(env, ws_id).await;
        ensure_memory_kb(&km).await;
        for doc in docs {
            let mut metadata = DocumentMetadata::default();
            if let Some(tag) = doc.ws_id {
                metadata
                    .extra
                    .insert(WS_ID_TAG.to_string(), tag.to_string());
            }
            let written = km
                .add_text_to_kb_with_metadata(
                    MEMORY_KB,
                    doc.title,
                    doc.content,
                    doc.source,
                    metadata,
                )
                .await
                .unwrap();
            // 文档 id 由正文决定，各实例写入同一份内容得到同一个 id：
            // 收集第一份即可，用例不必记住「哪个 id 来自哪份实例」。
            if index == 0 {
                ids.push(written.id);
            }
        }
    }
    ids
}

async fn raw_titles(env: &TestEnv, ws_id: &str) -> Vec<String> {
    memory_km(env, ws_id)
        .await
        .list_memory_documents_with_content(MEMORY_KB, 100)
        .await
        .unwrap()
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
/// 变异：去掉列表里的 `belongs_to` 过滤 → 两个空间都看到 4 条（外加一份无标签
/// 老记忆共 5 条），两条 `assert_eq!(total, 2)` 与内容断言同时变红。
#[tokio::test]
async fn list_stays_inside_its_own_space_in_both_directions() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;
    seed(
        &env,
        &[&ws_a, &ws_b],
        &[
            doc(
                Some(&ws_a),
                "ppa_semantic",
                "a_rust",
                "用户的主开发语言是 Rust",
            ),
            doc(Some(&ws_a), "ppa_episodic", "a_release", "周五发布 v2"),
            doc(
                Some(&ws_b),
                "ppa_semantic",
                "b_python",
                "用户的主开发语言是 Python",
            ),
            doc(Some(&ws_b), "ppa_episodic", "b_standup", "周一开例会"),
            // 没有 ws_id 的老记忆：同一份库里存在，但谁都无权展示它。
            doc(None, "ppa_semantic", "legacy_indent", "用户偏好 tab 缩进"),
        ],
    )
    .await;

    // 前提：两份实例里都确实躺着别人的文档 —— 否则本用例测不到过滤。
    for ws_id in [&ws_a, &ws_b] {
        let titles = raw_titles(&env, ws_id).await;
        assert_eq!(titles.len(), 5, "库里应能看到全部 5 条: {titles:?}");
        assert!(titles.contains(&"b_python".to_string()), "{titles:?}");
    }

    let from_a = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents"))
        .await;
    assert_eq!(from_a.status, 200, "body={}", from_a.body);
    assert_eq!(from_a.body["total"], json!(2), "body={}", from_a.body);
    assert_eq!(
        sorted_item_titles(&from_a.body),
        vec!["a_release", "a_rust"]
    );
    let a_text = from_a.body.to_string();
    assert!(!a_text.contains("Python"), "不得列出 ws_b 的记忆: {a_text}");
    assert!(
        !a_text.contains("周一开例会"),
        "不得列出 ws_b 的记忆: {a_text}"
    );
    assert!(
        !a_text.contains("tab 缩进"),
        "不得列出无标签的老记忆: {a_text}"
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
        !b_text.contains("周五发布 v2"),
        "不得列出 ws_a 的记忆: {b_text}"
    );
}

/// 空空间：库里只有别人的记忆与无标签老记忆 ⇒ 列表为空，`total` 也是 0。
///
/// 变异：去掉 `belongs_to` 过滤 → 这份列表会把 ws_a 的两条与老记忆一起列出来。
#[tokio::test]
async fn empty_space_lists_nothing_even_when_the_kb_has_documents() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;
    seed(
        &env,
        &[&ws_a, &ws_b],
        &[
            doc(
                Some(&ws_a),
                "ppa_semantic",
                "a_rust",
                "用户的主开发语言是 Rust",
            ),
            doc(None, "ppa_semantic", "legacy_indent", "用户偏好 tab 缩进"),
        ],
    )
    .await;

    assert_eq!(
        raw_titles(&env, &ws_b).await.len(),
        2,
        "前提：库里有两份文档"
    );

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
    seed(
        &env,
        &[&ws_a],
        &[
            doc(Some(&ws_a), "ppa_profile", "memory_1", "用户偏好简洁的回答"),
            doc(
                Some(&ws_a),
                "ppa_semantic",
                "memory_2",
                "用户在用 Rust 写 daemon",
            ),
            doc(Some(&ws_a), "ppa_episodic", "memory_3", "周五上线"),
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
    seed(
        &env,
        &[&ws_a],
        &[
            doc(Some(&ws_a), "ppa_semantic", "p1", "记忆一"),
            doc(Some(&ws_a), "ppa_semantic", "p2", "记忆二"),
            doc(Some(&ws_a), "ppa_semantic", "p3", "记忆三"),
            doc(Some(&ws_a), "ppa_semantic", "p4", "记忆四"),
            doc(Some(&ws_a), "ppa_semantic", "p5", "记忆五"),
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
///
/// 变异：详情改成不做 `belongs_to` 校验 → 下一个用例变红，本用例不变。
#[tokio::test]
async fn detail_returns_full_content_for_own_documents() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let long_body = format!("{}尾巴", "长".repeat(300));
    let ids = seed(
        &env,
        &[&ws_a],
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
/// 变异：详情去掉 `belongs_to` 校验（直接返回文档）→ 跨空间那条断言从 404
/// 变成 200，本用例变红。
#[tokio::test]
async fn detail_hides_foreign_missing_and_legacy_documents_behind_404() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;
    let ws_b = env.open_space("mem-b", "research").await;
    let ids = seed(
        &env,
        &[&ws_a, &ws_b],
        &[
            doc(
                Some(&ws_a),
                "ppa_semantic",
                "a_rust",
                "用户的主开发语言是 Rust",
            ),
            doc(
                Some(&ws_b),
                "ppa_semantic",
                "b_python",
                "用户的主开发语言是 Python",
            ),
            doc(None, "ppa_semantic", "legacy_indent", "用户偏好 tab 缩进"),
        ],
    )
    .await;
    let foreign_id = &ids[1];
    let legacy_id = &ids[2];

    // 前提：这条路在本空间里是通的，否则「跨空间 404」可能只是路由不通。
    let own = env
        .get(&format!("/api/spaces/{ws_a}/memory/documents/{}", ids[0]))
        .await;
    assert_eq!(own.status, 200, "body={}", own.body);

    for (doc_id, label) in [
        (foreign_id.clone(), "别的空间的文档"),
        (legacy_id.clone(), "没有归属标签的老记忆"),
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
    let ids = seed(
        &env,
        &[&ws_a],
        &[doc(
            Some(&ws_a),
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

/// 新空间还没有记忆库 ⇒ 空列表，不是 500；详情仍然是 404。
#[tokio::test]
async fn missing_memory_kb_lists_nothing() {
    let env = new_env(&["research"]).await;
    let ws_a = env.open_space("mem-a", "research").await;

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
