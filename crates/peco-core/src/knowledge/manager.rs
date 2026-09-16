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
use tracing::{debug, info, warn};

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
/// 构造是同步的，
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

    /// 丢弃底层 `KnowledgeBaseManager` 并重新加载。
    ///
    /// 适用于工作空间下新增或删除了知识库配置后触发全量刷新。
    /// 注意：若配置了 `auto_sync_on_start`，reload 后允许再次自动同步。
    pub async fn reload(&self) -> Result<(), KnowledgeModuleError> {
        let mut guard = self.underlying.lock().await;
        *guard = None;
        drop(guard);
        // 允许重新触发 auto_sync
        self.auto_sync_done.store(false, Ordering::SeqCst);
        self.ensure_loaded().await?;
        info!("KnowledgeManager reloaded");
        Ok(())
    }

    /// 如配置了 `auto_sync_on_start`，执行一次自动同步（仅首次调用生效）。
    ///
    /// 幂等 — 第二次调用不会有任何效果。
    pub async fn maybe_auto_sync(&self) -> Result<(), KnowledgeModuleError> {
        if !self.config.auto_sync_on_start || self.auto_sync_done.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.ensure_loaded().await?;

        let names: Vec<String> = {
            let guard = self.underlying.lock().await;
            let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
            mgr.list_kbs().await?.into_iter().map(|i| i.name).collect()
        };

        let mut total_changes = 0usize;
        for name in &names {
            match self.sync_kb_impl(name).await {
                Ok(report) => total_changes += report.total_changes(),
                Err(e) => warn!(kb = %name, error = %e, "auto_sync failed"),
            }
        }

        if total_changes > 0 {
            info!(total_changes, "auto_sync_on_start completed");
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

        info!(%name, "Knowledge base created (with docs/ directory and hash manifest)");

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

        info!(%name, "Knowledge base deleted");
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

    /// 跨所有知识库并发搜索。
    ///
    /// 委托给 [`KnowledgeBaseManager::search_all()`](knowledge_base::KnowledgeBaseManager::search_all)，
    /// 其内部通过 `futures::future::join_all` 并发轮询实现非阻塞查询。
    /// 单个 KB 搜索失败时自动记录 warning 并跳过，不影响其他 KB。
    pub async fn search_all(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(String, Vec<knowledge_base::SearchResult>)>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;
        Ok(mgr.search_all(query, top_k).await)
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

        let doc = kb.add_text(title, content, source).await?;
        info!(kb = %kb_name, title = %title, doc_id = %doc.id, "Text added to knowledge base");
        Ok(doc)
    }

    /// 按指定存储模式添加文本到知识库。
    pub async fn add_text_to_kb_with_mode(
        &self,
        kb_name: &str,
        title: &str,
        content: &str,
        source: &str,
        mode: knowledge_base::StorageMode,
    ) -> Result<knowledge_base::Document, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        let doc = kb.add_text_with_mode(title, content, source, mode).await?;
        info!(kb = %kb_name, title = %title, doc_id = %doc.id, mode = ?mode, "Text added to knowledge base (with mode)");
        Ok(doc)
    }

    /// 读取指定知识库中的单个文档（含原文内容）。
    ///
    /// 文档不存在时返回 `None`；知识库不存在时返回
    /// [`KnowledgeModuleError::NotFound`]，其余打开失败（后端 IO 等）
    /// 保留原始错误 — 不把基础设施故障伪装成「不存在」。
    pub async fn get_document(
        &self,
        kb_name: &str,
        doc_id: &str,
    ) -> Result<Option<knowledge_base::Document>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr.open_kb(kb_name).await.map_err(|e| match e {
            knowledge_base::KnowledgeError::NotFound(_) => {
                KnowledgeModuleError::NotFound(kb_name.to_string())
            }
            other => other.into(),
        })?;

        Ok(kb.get_document(doc_id).await?)
    }

    /// 删除指定知识库中的单个文档，返回删除计数报告。
    ///
    /// 知识库不存在时返回 [`KnowledgeModuleError::NotFound`]，其余打开失败
    /// （后端 IO 等）保留原始错误 — 不把基础设施故障伪装成「不存在」。
    pub async fn delete_document(
        &self,
        kb_name: &str,
        doc_id: &str,
    ) -> Result<knowledge_base::DeleteReport, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr.open_kb(kb_name).await.map_err(|e| match e {
            knowledge_base::KnowledgeError::NotFound(_) => {
                KnowledgeModuleError::NotFound(kb_name.to_string())
            }
            other => other.into(),
        })?;

        let report = kb.remove_document(doc_id).await?;
        info!(
            kb = %kb_name,
            doc_id = %doc_id,
            removed_chunks = report.removed_chunks,
            "Document deleted from knowledge base"
        );
        Ok(report)
    }

    /// 用指定知识库的嵌入引擎对任意文本批量生成向量（重嵌入路径）。
    ///
    /// 供余弦相似度比较（自动整理的聚类去重）使用；向量维度与该库
    /// 配置的嵌入模型一致。知识库不存在时返回
    /// [`KnowledgeModuleError::NotFound`]。
    pub async fn embed_texts(
        &self,
        kb_name: &str,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr.open_kb(kb_name).await.map_err(|e| match e {
            knowledge_base::KnowledgeError::NotFound(_) => {
                KnowledgeModuleError::NotFound(kb_name.to_string())
            }
            other => other.into(),
        })?;

        Ok(kb.embed_texts(texts).await?)
    }

    // ── 图谱操作 ────────────────────────────────────────────────────────────

    /// 添加结构化事实到知识图谱。
    pub async fn add_facts_to_kb(
        &self,
        kb_name: &str,
        facts: &[knowledge_base::Fact],
        index_text: bool,
    ) -> Result<Vec<knowledge_base::Fact>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        let result = kb.add_facts(facts, index_text).await?;
        info!(kb = %kb_name, fact_count = facts.len(), index_text, "Facts added to knowledge base");
        Ok(result)
    }

    /// 添加实体到知识图谱。
    pub async fn add_entities_to_kb(
        &self,
        kb_name: &str,
        entities: &[knowledge_base::Entity],
    ) -> Result<(), KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        kb.add_entities(entities).await?;
        info!(kb = %kb_name, entity_count = entities.len(), "Entities added to knowledge base");
        Ok(())
    }

    /// 查询实体相关事实。
    pub async fn query_entity_facts(
        &self,
        kb_name: &str,
        entity_name: &str,
        max_depth: u32,
    ) -> Result<Vec<knowledge_base::TraversalStep>, KnowledgeModuleError> {
        self.ensure_loaded().await?;

        let guard = self.underlying.lock().await;
        let mgr = guard.as_ref().ok_or(KnowledgeModuleError::NotInitialized)?;

        let kb = mgr
            .open_kb(kb_name)
            .await
            .map_err(|_| KnowledgeModuleError::NotFound(kb_name.to_string()))?;

        Ok(kb.query_entity_facts(entity_name, max_depth).await?)
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
        self.sync_kb_impl(name).await
    }

    /// 内部同步实现 — 调用方需先确保 `ensure_loaded()` 已完成。
    async fn sync_kb_impl(&self, name: &str) -> Result<SyncReport, KnowledgeModuleError> {
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
                    Ok(deleted) => {
                        report.removed += 1;
                        info!(
                            kb = %name,
                            path = %path,
                            doc_id = %entry.doc_id,
                            removed_chunks = deleted.removed_chunks,
                            "Deleted file missing from docs/ directory"
                        );
                    }
                    // 文档已不存在（如同内容文件共享同一 doc_id，已被前一路径删除）：
                    // 幂等跳过，不记为错误
                    Err(knowledge_base::KnowledgeError::NotFound(_)) => {
                        debug!(
                            kb = %name,
                            path = %path,
                            doc_id = %entry.doc_id,
                            "Document no longer exists, skipping deletion"
                        );
                    }
                    // 删除失败：把该路径写回新清单，下一轮同步重试 —
                    // 否则失败路径随清单保存而消失，文档永久滞留在召回面上
                    Err(e) => {
                        report.errors.push((path.clone(), e.to_string()));
                        new_manifest.files.insert(path.clone(), entry.clone());
                        warn!(
                            kb = %name,
                            path = %path,
                            error = %e,
                            "Deletion failed, keeping manifest entry for retry on next sync"
                        );
                    }
                }
            }
        }

        // 保存新清单
        new_manifest.save(&kb_dir).await?;

        report.duration_ms = start.elapsed().as_millis() as u64;
        info!(%report, "Knowledge base sync completed");

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
            match self.sync_kb_impl(&name).await {
                Ok(report) => results.push((name, report)),
                Err(e) => warn!(kb = %name, error = %e, "Sync failed, skipping"),
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
                    "File changed, re-ingesting"
                );
                // 删除旧数据（失败不阻塞，记录警告）
                if let Err(e) = kb.remove_document(&entry.doc_id).await {
                    warn!(
                        kb = %kb_name,
                        doc_id = %entry.doc_id,
                        error = %e,
                        "Failed to delete old document, continuing with new version"
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
                info!(kb = %kb_name, path = %relative_str, "New file, ingesting");
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
            default_storage_mode: Default::default(),
            helix_url: None,
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
    async fn get_and_delete_document_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(tmp.path().to_path_buf());
        km.ensure_loaded().await.unwrap();
        km.create_kb(make_test_config("test-doc-ops"))
            .await
            .unwrap();

        let doc = km
            .add_text_to_kb(
                "test-doc-ops",
                "Hello",
                "Rust is a systems programming language.",
                "ppa_semantic",
            )
            .await
            .unwrap();

        // get_document 往返：删除前可读取原文（审计/回滚的前置能力）
        let fetched = km
            .get_document("test-doc-ops", &doc.id)
            .await
            .unwrap()
            .expect("文档应存在");
        assert_eq!(fetched.id, doc.id);
        assert_eq!(fetched.title, "Hello");
        assert_eq!(fetched.content, "Rust is a systems programming language.");
        assert_eq!(fetched.source_path, "ppa_semantic");

        // 删除：报告给出 doc_id 与真实分块计数，文档随后消失
        let report = km.delete_document("test-doc-ops", &doc.id).await.unwrap();
        assert_eq!(report.doc_id, doc.id);
        assert!(report.removed_chunks >= 1);

        assert!(
            km.get_document("test-doc-ops", &doc.id)
                .await
                .unwrap()
                .is_none()
        );
        let docs = km.list_documents("test-doc-ops", 0, 10).await.unwrap();
        assert!(docs.is_empty());

        // 重复删除 → 底层 NotFound（经 Knowledge 变体透传）
        let err = km
            .delete_document("test-doc-ops", &doc.id)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                KnowledgeModuleError::Knowledge(knowledge_base::KnowledgeError::NotFound(_))
            ),
            "重复删除应报 NotFound，实际: {err}"
        );
    }

    #[tokio::test]
    async fn document_ops_on_missing_kb_report_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(tmp.path().to_path_buf());
        km.ensure_loaded().await.unwrap();

        let err = km.get_document("no-such-kb", "doc-1").await.unwrap_err();
        assert!(
            matches!(err, KnowledgeModuleError::NotFound(_)),
            "KB 不存在应报 NotFound，实际: {err}"
        );

        let err = km.delete_document("no-such-kb", "doc-1").await.unwrap_err();
        assert!(
            matches!(err, KnowledgeModuleError::NotFound(_)),
            "KB 不存在应报 NotFound，实际: {err}"
        );
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
