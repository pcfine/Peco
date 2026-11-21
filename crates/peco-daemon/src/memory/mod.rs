//! 按空间隔离的记忆：写路径（轮末提取）、读路径（查询前召回）、旧记忆清理。
//!
//! 物理上团队共用一套 `@private_memory` KB，逻辑隔离只靠文档 metadata 里的
//! [`WS_ID_TAG`]：写路径必须打标，读路径必须按标过滤 —— 两侧各漏一处就会
//! 跨空间串记忆，因此读写共用同一个键名常量。
//!
//! 精简面（相比 `peco-server` 的同名模块）：只做文档记忆，不做图事实、取代
//! 事务、审计行、巩固流水线与 embedding 去重重排；daemon 的浏览端点与面板
//! 留给后续阶段。

pub mod analyzer;
pub mod cleanup;
pub mod compose;
pub mod config;
pub mod hook;
pub mod layout;
pub mod migration;
pub mod recall;

/// 记忆库的单次扫描上限。
///
/// 浏览端点、存量迁移与销毁都要「把库里的文档取出来」，三处取同一个数：
/// 各自写一份上界，改一处就会漏掉另一处（比如上限调大后迁移仍只搬一万条）。
/// 取一个远大于单用户实际规模的具体上界，而不是把 `usize::MAX` 交给底层按不可能
/// 达到的规模预分配缓冲。
pub const MEMORY_SCAN_LIMIT: usize = 10_000;

/// 文档记忆里的空间标签键。
///
/// 值取 `ws_id`（空间实例的身份），不是团队 id：同团队多个空间共用一套 KB，
/// 团队粒度会把它们重新混成一体。
pub const WS_ID_TAG: &str = "ws_id";

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, OnceLock};

    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
    use peco_core::knowledge::KnowledgeManager;
    use tokio::sync::Mutex;

    /// 并发建库会抢同一份 fastembed 模型缓存，抢输的那次直接初始化失败 ——
    /// 夹具因此串行建库；库建成之后的检索与写入互不影响。
    static KB_BUILD: OnceLock<Mutex<()>> = OnceLock::new();

    /// 建一个 InMemory 后端的 `@private_memory` KB。
    ///
    /// 返回临时目录本身：`KnowledgeManager` 只记路径，目录被回收后元数据落盘
    /// 会失败，所以用例必须持有它到结束。
    pub(crate) async fn memory_kb() -> (Arc<KnowledgeManager>, tempfile::TempDir) {
        let _guard = KB_BUILD.get_or_init(|| Mutex::new(())).lock().await;
        let dir = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(dir.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(KbConfig {
            name: "@private_memory".into(),
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
        (km, dir)
    }
}
