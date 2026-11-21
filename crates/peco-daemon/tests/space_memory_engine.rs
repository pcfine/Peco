//! A1 / A4：打开空间不加载嵌入模型，两个空间的记忆库只初始化一次引擎。
//!
//! 判据是进程内的「引擎初始化」日志条数：`FastembedEngine::new` 只在真正加载模型
//! 时打一条 info，缓存命中时一条都不打。用例独占一个测试二进制（计数器是全局的），
//! 因此计数不会被别的用例污染。

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use peco_core::tools::KnowledgeAccess;
use serde_json::json;
use support::new_env;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

const ENGINE_INIT_MESSAGE: &str = "Fastembed embedding engine initialized";

/// 数「引擎初始化」事件；其余事件一概忽略。
struct EngineInitCounter(Arc<AtomicUsize>);

impl<S: Subscriber> Layer<S> for EngineInitCounter {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        struct Message<'a>(&'a AtomicUsize);
        impl Visit for Message<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" && format!("{value:?}").contains(ENGINE_INIT_MESSAGE) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        event.record(&mut Message(&self.0));
    }
}

/// 打开两个空间：先按下写入默认记忆配置、且一条引擎日志都不打；再把两个库各自
/// 建出来（InMemory + 同一个模型）—— 引擎只初始化一次。
///
/// 变异：把 `layout::ensure` 换成 `create_kb`（或打开路径上 `open_kb`）→ 打开阶段
/// 就出现引擎日志，第一条断言变红；去掉 `FastembedEngine::new` 的进程级缓存 →
/// 两个库各初始化一次，计数变 2。
#[tokio::test]
async fn opening_a_space_writes_the_memory_config_without_loading_the_model() {
    let init_count = Arc::new(AtomicUsize::new(0));
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(EngineInitCounter(Arc::clone(&init_count))),
    )
    .expect("本用例独占测试二进制，订阅者只装一次");

    let env = new_env(&["research"]).await;
    // 这里刻意不用夹具的 `open_space`：它会把记忆后端换成 InMemory，
    // 而本用例要看的就是「默认配置被写下」这件事。
    let mut ws_ids = Vec::new();
    for name in ["engine-a", "engine-b"] {
        let work_dir = env.work_dir(name);
        let reply = env
            .post(
                "/api/spaces",
                json!({"work_dir": work_dir.to_string_lossy(), "team_id": "research"}),
            )
            .await;
        assert_eq!(reply.status, 201, "建空间失败: {}", reply.body);
        let ws_id = reply.body["ws_id"].as_str().unwrap().to_string();

        let config = env
            .home
            .join("memory")
            .join(&ws_id)
            .join("@private_memory/kb_config.json");
        assert!(
            config.is_file(),
            "打开空间必须写下该空间自己的记忆库配置: {}",
            config.display()
        );
        let raw = std::fs::read_to_string(&config).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["name"], json!("@private_memory"));
        assert_eq!(value["backend"], json!("HelixDb"), "默认后端沿用 HelixDb");
        assert_eq!(
            value["embedding_model"],
            json!("b-g-e-base-z-h-v15"),
            "默认模型与设计一致"
        );
        assert_eq!(value["chunking"]["size"], json!(800));
        assert_eq!(value["chunking"]["overlap"], json!(200));
        assert_eq!(value["default_storage_mode"], json!("full"));
        ws_ids.push(ws_id);
    }

    assert_eq!(
        init_count.load(Ordering::SeqCst),
        0,
        "打开空间不得加载嵌入模型（那会让每次开空间多几秒）"
    );

    // 两个空间各建一次记忆库：同一个模型类型只该初始化一次引擎。
    for (index, ws_id) in ws_ids.iter().enumerate() {
        env.write_memory_backend(ws_id, "InMemory", "all-mini-l-m-l6-v2-q");
        let handle = env.open(ws_id).await.expect("空间应已打开");
        let memory = handle
            .workspace()
            .memory_manager()
            .cloned()
            .expect("打开的空间必须带记忆管理器");
        memory
            .add_text_to_kb(
                "@private_memory",
                &format!("memory_{index}"),
                &format!("空间 {index} 的记忆"),
                "ppa_semantic",
            )
            .await
            .expect("写入空间记忆库");
    }

    assert_eq!(
        init_count.load(Ordering::SeqCst),
        1,
        "两个空间的记忆库必须共用同一份已加载模型"
    );
}
