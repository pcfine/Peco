//! 知识库管理器 — AI Agent 的统一知识管理入口。
//!
//! 封装 `knowledge_base::KnowledgeBaseManager`，
//! 提供面向用户的人性化知识库操作：
//! - 创建/删除/列表知识库
//! - 自动增量同步（扫描 docs/ 目录 → 对比哈希 → 更新数据库）
//! - 多维度搜索（BM25 + 向量 + 图谱）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Mutex;
use tracing::info;

use super::config::KnowledgeConfig;
use super::error::KnowledgeModuleError;
use super::hash_manifest::{self, FileEntry, FileHashManifest};
use super::sync::SyncReport;

// ---------------------------------------------------------------------------
// KnowledgeManager
// ---------------------------------------------------------------------------

/// 知识库模块 — AI Agent 的统一知识管理入口。
///
/// # 延迟初始化
///
/// 构造是同步的（适配 `GlobalHandler` 的 `LazyLock` 初始化），
/// 实际的知识库管理器加载通过 `ensure_loaded()` 延迟完成。
/// 所有公共方法在内部首先调用 `ensure_loaded()`。
pub struct KnowledgeManager {
    /// 数据根目录
    base_dir: PathBuf,
    /// 模块配置
    config: KnowledgeConfig,
    /// 延迟加载的底层 knowledge-base 管理器
    underlying: Mutex<Option<knowledge_base::KnowledgeBaseManager>>,
    /// 确保 auto_sync_on_start 只执行一次
    auto_sync_done: AtomicBool,
}

impl KnowledgeManager {
    // ── 构造 ────────────────────────────────────────────────────────────────

    /// 同步构造（不加载任何知识库实例，仅初始化结构）。
    ///
    /// 实际的知识库加载由 [`ensure_loaded`](Self::ensure_loaded) 延迟完成。
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            config: KnowledgeConfig::default(),
            underlying: Mutex::new(None),
            auto_sync_done: AtomicBool::new(false),
        }
    }

    /// 带配置的同步构造。
    pub fn with_config(base_dir: PathBuf, config: KnowledgeConfig) -> Self {
        Self {
            base_dir,
            config,
            underlying: Mutex::new(None),
            auto_sync_done: AtomicBool::new(false),
        }
    }

    // ── 初始化 ──────────────────────────────────────────────────────────────

    /// 确保底层 `KnowledgeBaseManager` 已加载（可重复调用，幂等）。
    pub async fn ensure_loaded(&self) -> Result<(), KnowledgeModuleError> {
        let mut guard = self.underlying.lock().await;
        if guard.is_none() {
            let mgr = knowledge_base::KnowledgeBaseManager::load(&self.base_dir).await?;
            *guard = Some(mgr);
        }
        Ok(())
    }

    /// 如配置了 `auto_sync_on_start`，执行一次自动同步（仅首次调用生效）。
    ///
    /// 应在 `ensure_loaded()` 之后调用。幂等 — 第二次调用不会有任何效果。
    pub async fn maybe_auto_sync(&self) -> Result<(), KnowledgeModuleError> {
        if !self.config.auto_sync_on_start || self.auto_sync_done.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let names: Vec<String> = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.list_kbs().await?.into_iter().map(|i| i.name).collect()
        };

        let mut total_changes = 0usize;
        for name in &names {
            match self.sync_kb_inner(name).await {
                Ok(report) => total_changes += report.total_changes(),
                Err(e) => tracing::warn!(kb = %name, error = %e, "auto_sync 失败"),
            }
        }

        if total_changes > 0 {
            info!(total_changes, "auto_sync_on_start 完成");
        }
        Ok(())
    }

    // ── 知识库生命周期 ──────────────────────────────────────────────────────

    /// 创建新知识库。
    ///
    /// 自动在 `<base_dir>/<kb_sanitized>/docs/` 下创建原始文档目录，
    /// 并初始化空的 `file_hashes.json`。
    pub async fn create_kb(
        &self,
        config: knowledge_base::KbConfig,
    ) -> Result<knowledge_base::KbInfo, KnowledgeModuleError> {
        self.ensure_loaded().await?;
        let name = config.name.clone();

        // 第一步：创建知识库（在锁内完成）
        {
            let mut guard = self.underlying.lock().await;
            let mgr = guard.as_mut().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.create_kb(config).await?;
        }

        // 第二步：创建 docs/ 目录和哈希清单（不需要锁）
        let kb_dir = self.base_dir.join(knowledge_base::sanitize_kb_name(&name));
        let docs_dir = kb_dir.join("docs");
        tokio::fs::create_dir_all(&docs_dir)
            .await
            .map_err(KnowledgeModuleError::Io)?;

        let manifest = FileHashManifest::default();
        manifest.save(&kb_dir).await?;

        info!(%name, "知识库已创建（含 docs/ 目录和哈希清单）");

        // 第三步：获取摘要信息
        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
        let list = mgr.list_kbs().await?;
        list.into_iter()
            .find(|i| i.name == name)
            .ok_or(KnowledgeModuleError::NotFound(name))
    }

    /// 删除知识库及其所有数据（数据库 + 原始文档目录 + 哈希清单）。
    pub async fn delete_kb(&self, name: &str) -> Result<(), KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let mut guard = self.underlying.lock().await;
        let mgr = guard.as_mut().ok_or(KnowledgeModuleError::NotInitialized)?;
        mgr.delete_kb(name).await?;

        info!(%name, "知识库已删除");
        Ok(())
    }

    /// 列出所有知识库的摘要信息。
    pub async fn list_kbs(&self) -> Result<Vec<knowledge_base::KbInfo>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
        Ok(mgr.list_kbs().await?)
    }

    // ── 搜索 ────────────────────────────────────────────────────────────────

    /// 在指定知识库中搜索。
    ///
    /// 持有底层管理器锁的时间尽可能短 — 仅在打开知识库时加锁，
    /// 搜索本身不需要锁。
    pub async fn search_kb(
        &self,
        kb_name: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<knowledge_base::SearchResult>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let kb = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.open_kb(kb_name)
                .await
                .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?
        };

        // 锁已释放 — search 在 KnowledgeBase 内部有独立的并发控制
        Ok(kb.search(query, top_k).await?)
    }

    /// 跨所有知识库并发生成嵌入 → 并行搜索 → 合并排序。
    ///
    /// 首先获取所有知识库的 `Arc<KnowledgeBase>` 引用（释放管理器锁），
    /// 然后真正并发执行搜索，实现并行查询。
    pub async fn search_all(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(String, Vec<knowledge_base::SearchResult>)>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        // 第一步：获取所有 KB 的 Arc 引用（持锁时间短）
        let kb_entries: Vec<(String, Arc<knowledge_base::KnowledgeBase>)> = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            let infos = mgr.list_kbs().await?;
            let mut entries = Vec::new();
            for info in infos {
                if let Ok(kb) = mgr.open_kb(&info.name).await {
                    entries.push((info.name, kb));
                }
            }
            entries
        };
        // 锁已释放

        // 第二步：并发搜索所有 KB
        let query_str = query.to_string();
        let handles: Vec<_> = kb_entries
            .into_iter()
            .map(|(name, kb)| {
                let q = query_str.clone();
                tokio::spawn(async move {
                    match kb.search(&q, top_k).await {
                        Ok(hits) if !hits.is_empty() => Some((name, hits)),
                        Ok(_) => None,
                        Err(e) => {
                            tracing::warn!(kb = %name, error = %e, "搜索失败，跳过");
                            None
                        }
                    }
                })
            })
            .collect();

        let results = futures::future::join_all(handles).await;
        let mut merged = Vec::new();
        for result in results {
            if let Ok(Some(entry)) = result {
                merged.push(entry);
            }
        }

        Ok(merged)
    }

    // ── 文档列表 ────────────────────────────────────────────────────────────

    /// 查看指定知识库中的文档列表。
    pub async fn list_documents(
        &self,
        kb_name: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<knowledge_base::DocumentSummary>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        Ok(kb.list_documents(offset, limit).await?)
    }

    /// 直接添加文本内容到知识库（不需要文件）。
    ///
    /// 返回新创建的文档。
    pub async fn add_text_to_kb(
        &self,
        kb_name: &str,
        title: &str,
        content: &str,
        source: &str,
    ) -> Result<knowledge_base::Document, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        Ok(kb.add_text(title, content, source).await?)
    }

    // ── 同步 ────────────────────────────────────────────────────────────────

    /// 同步指定知识库：扫描 docs/ 目录，对比文件哈希，执行增量更新。
    ///
    /// # 同步逻辑
    ///
    /// 1. 遍历 `docs/` 目录下所有支持的文件（递归）
    /// 2. 计算每个文件的 SHA-256 哈希
    /// 3. 与 `file_hashes.json` 对比：
    ///    - **新文件**（哈希清单中无记录）→ 摄入数据库
    ///    - **已变更**（哈希不同）→ 删除旧数据 + 重新摄入
    ///    - **未变更**（哈希相同）→ 跳过
    /// 4. **删除检测**：哈希清单中存在但磁盘上已消失的文件 → 从数据库删除
    /// 5. 更新哈希清单
    pub async fn sync_kb(&self, name: &str) -> Result<SyncReport, KnowledgeModuleError> {
        self.ensure_loaded().await?;
        self.sync_kb_inner(name).await
    }

    /// 内部同步逻辑（不调用 ensure_loaded，供 maybe_auto_sync 使用）。
    async fn sync_kb_inner(&self, name: &str) -> Result<SyncReport, KnowledgeModuleError> {
        let start = std::time::Instant::now();

        let kb_dir = self.base_dir.join(knowledge_base::sanitize_kb_name(name));
        let docs_dir = kb_dir.join("docs");

        // 确保 docs 目录存在
        if !docs_dir.exists() {
            tokio::fs::create_dir_all(&docs_dir)
                .await
                .map_err(KnowledgeModuleError::Io)?;
        }

        // 加载已有哈希清单
        let mut manifest = FileHashManifest::load(&kb_dir).await?;

        // 扫描当前文件
        let current_files =
            hash_manifest::scan_supported_files(&docs_dir, self.config.recursive_scan).await?;

        let mut new_manifest = FileHashManifest {
            updated_at: hash_manifest::now_iso8601(),
            ..Default::default()
        };
        let mut report = SyncReport::new(name);

        // 打开知识库实例
        let kb = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.open_kb(name)
                .await
                .map_err(|_| KnowledgeModuleError::NotFound(name.to_string()))?
        };

        for file_path in &current_files {
            let relative = file_path.strip_prefix(&docs_dir).unwrap_or(file_path);
            let relative_str = relative.to_string_lossy().to_string();

            match self
                .process_one_file(
                    &kb,
                    file_path,
                    &relative_str,
                    &mut manifest,
                    &report.kb_name,
                )
                .await
            {
                Ok(action) => match action {
                    FileAction::Added(entry) => {
                        report.added += 1;
                        report.changed_files.push(relative_str.clone());
                        new_manifest.files.insert(relative_str, entry);
                    }
                    FileAction::Updated(entry) => {
                        report.updated += 1;
                        report.changed_files.push(relative_str.clone());
                        new_manifest.files.insert(relative_str, entry);
                    }
                    FileAction::Skipped(entry) => {
                        report.skipped += 1;
                        new_manifest.files.insert(relative_str, entry.clone());
                    }
                },
                Err(e) => {
                    report.errors.push((relative_str, e.to_string()));
                }
            }
        }

        // 检测已删除的文件
        for (path, entry) in &manifest.files {
            if !new_manifest.files.contains_key(path) {
                match kb.remove_document(&entry.doc_id).await {
                    Ok(()) => {
                        report.removed += 1;
                        info!(kb = %name, path = %path, doc_id = %entry.doc_id, "已删除文件");
                    }
                    Err(e) => {
                        report.errors.push((path.clone(), e.to_string()));
                    }
                }
            }
        }

        // 保存新清单
        new_manifest.save(&kb_dir).await?;

        report.duration_ms = start.elapsed().as_millis() as u64;
        info!(%report, "知识库同步完成");

        Ok(report)
    }

    /// 同步所有知识库。
    ///
    /// 当前按顺序同步每个知识库，避免并发访问 LanceDB 的锁竞争。
    /// 后续可优化为并发同步（不同知识库使用不同的 LanceDB 表）。
    pub async fn sync_all(&self) -> Result<Vec<(String, SyncReport)>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let names: Vec<String> = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.list_kbs().await?.into_iter().map(|i| i.name).collect()
        };

        let mut results = Vec::new();
        for name in names {
            match self.sync_kb(&name).await {
                Ok(report) => results.push((name, report)),
                Err(e) => tracing::warn!(kb = %name, error = %e, "同步失败，跳过"),
            }
        }

        Ok(results)
    }

    // ── 内部辅助方法 ────────────────────────────────────────────────────────

    /// 处理单个文件的同步逻辑。
    async fn process_one_file(
        &self,
        kb: &Arc<knowledge_base::KnowledgeBase>,
        file_path: &Path,
        relative_str: &str,
        manifest: &mut FileHashManifest,
        kb_name: &str,
    ) -> Result<FileAction, KnowledgeModuleError> {
        let (hash, size) = hash_manifest::compute_file_hash(file_path).await?;

        match manifest.files.get(relative_str) {
            Some(entry) if entry.hash == hash => {
                // 未变更 → 跳过
                Ok(FileAction::Skipped(entry.clone()))
            }
            Some(entry) => {
                // 已变更 → 删除旧数据 + 重新摄入
                info!(
                    kb = %kb_name,
                    path = %relative_str,
                    old_hash = %entry.hash,
                    new_hash = %hash,
                    "文件已变更，重新摄入"
                );
                // 删除旧数据（失败不阻塞，记录警告）
                if let Err(e) = kb.remove_document(&entry.doc_id).await {
                    tracing::warn!(
                        kb = %kb_name,
                        doc_id = %entry.doc_id,
                        error = %e,
                        "删除旧文档失败，继续摄入新版本"
                    );
                }

                let doc = kb.add_file(file_path).await?;
                Ok(FileAction::Updated(FileEntry {
                    hash,
                    size,
                    doc_id: doc.id,
                    ingested_at: hash_manifest::now_iso8601(),
                }))
            }
            None => {
                // 新文件 → 摄入
                info!(kb = %kb_name, path = %relative_str, "新文件，摄入中");
                let doc = kb.add_file(file_path).await?;
                Ok(FileAction::Added(FileEntry {
                    hash,
                    size,
                    doc_id: doc.id,
                    ingested_at: hash_manifest::now_iso8601(),
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 内部类型
// ---------------------------------------------------------------------------

/// 文件处理结果。
enum FileAction {
    Added(FileEntry),
    Updated(FileEntry),
    Skipped(FileEntry),
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use knowledge_base::{ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};

    fn make_test_config(name: &str) -> KbConfig {
        KbConfig {
            name: name.to_string(),
            description: "测试知识库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: knowledge_base::BackendType::InMemory,
            storage_path: None,
        }
    }

    #[tokio::test]
    async fn create_and_delete_kb() {
        let tmp = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(tmp.path().to_path_buf());
        km.ensure_loaded().await.unwrap();

        let info = km.create_kb(make_test_config("test-create")).await.unwrap();
        assert_eq!(info.name, "test-create");

        // 验证 docs/ 目录和 file_hashes.json 已创建
        let kb_dir = tmp.path().join("test-create");
        assert!(kb_dir.join("docs").exists());
        assert!(kb_dir.join("file_hashes.json").exists());

        km.delete_kb("test-create").await.unwrap();
    }

    #[tokio::test]
    async fn search_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(tmp.path().to_path_buf());
        km.ensure_loaded().await.unwrap();

        let info = km.create_kb(make_test_config("test-search")).await.unwrap();
        assert_eq!(info.name, "test-search");

        // 通过 open_kb 直接添加文本
        {
            let guard = km.underlying.lock().await;
            let mgr = guard.as_ref().unwrap();
            let kb = mgr.open_kb("test-search").await.unwrap();
            kb.add_text("Hello", "Rust is a systems programming language.", "test")
                .await
                .unwrap();
        }

        let results = km
            .search_kb("test-search", "Rust programming", 3)
            .await
            .unwrap();
        assert!(!results.is_empty());

        let docs = km.list_documents("test-search", 0, 10).await.unwrap();
        assert!(!docs.is_empty());
    }

    #[tokio::test]
    #[ignore = "需要 fastembed 模型下载 (~100MB)"]
    async fn sync_new_and_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(tmp.path().to_path_buf());
        km.ensure_loaded().await.unwrap();

        km.create_kb(make_test_config("test-sync")).await.unwrap();

        let docs_dir = tmp.path().join("test-sync").join("docs");
        tokio::fs::create_dir_all(&docs_dir).await.unwrap();

        // 创建新文件
        tokio::fs::write(docs_dir.join("readme.md"), b"# Test KB\n\nHello world.")
            .await
            .unwrap();

        let report = km.sync_kb("test-sync").await.unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.skipped, 0);

        // 再次同步 → 应跳过
        let report2 = km.sync_kb("test-sync").await.unwrap();
        assert_eq!(report2.skipped, 1);
        assert_eq!(report2.added, 0);

        // 修改文件 → 应更新
        tokio::fs::write(docs_dir.join("readme.md"), b"# Test KB\n\nUpdated content.")
            .await
            .unwrap();

        let report3 = km.sync_kb("test-sync").await.unwrap();
        assert_eq!(report3.updated, 1);

        // 删除文件 → 同步检测并移除
        tokio::fs::remove_file(docs_dir.join("readme.md"))
            .await
            .unwrap();

        let report4 = km.sync_kb("test-sync").await.unwrap();
        assert_eq!(report4.removed, 1);
    }
}
