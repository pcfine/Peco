//! 知识库管理器 — 多知识库的创建、管理和查询入口。
//!
//! # 架构
//!
//! ```text
//! KnowledgeBaseManager
//! ├── KnowledgeBase "技术文档"     (LanceDB, BGE-small-zh)
//! ├── KnowledgeBase "法律合同"     (LanceDB, BGE-large-zh)
//! └── KnowledgeBase "代码库"       (InMemory, AllMiniLML6V2Q)
//! ```
//!
//! 每个 KnowledgeBase 封装了解析器、分块器、嵌入引擎、存储后端和检索引擎。
//!
//! # 配置存储
//!
//! 配置是自包含的：每个 KB 目录内有一个 `kb_config.json` 文件。
//! `load()` 扫描 `knowledge/*/kb_config.json` 子目录来发现知识库。
//! 已弃用的中心化 `kb_configs.json` 格式在 `load()` 时自动迁移。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::KnowledgeError;
use crate::types::SearchResult;

use super::KnowledgeBase;
use super::config::{BackendType, KbConfig, KbInfo};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 每个 KB 目录下的配置文件名称。
const KB_CONFIG_FILE: &str = "kb_config.json";

// ---------------------------------------------------------------------------
// KnowledgeBaseManager
// ---------------------------------------------------------------------------

/// 知识库管理器 — 统一入口，管理多个知识库实例。
pub struct KnowledgeBaseManager {
    /// 知识库实例映射：kb_name → KnowledgeBase
    instances: RwLock<HashMap<String, Arc<KnowledgeBase>>>,
    /// 配置映射（key = 目录名，即 sanitize 后的名称）
    configs: RwLock<HashMap<String, KbConfig>>,
    /// 数据根目录（knowledge/）
    base_dir: PathBuf,
}

impl KnowledgeBaseManager {
    /// 从指定目录加载所有知识库配置。
    ///
    /// 扫描 `knowledge/*/kb_config.json` 子目录来发现知识库。
    /// 若旧的中心化 `kb_configs.json` 仍存在，会自动迁移到各 KB 目录。
    pub async fn load(base_dir: &Path) -> Result<Self, KnowledgeError> {
        tokio::fs::create_dir_all(base_dir).await.map_err(|e| {
            KnowledgeError::InvalidInput(format!("Failed to create directory: {e}"))
        })?;

        let mut config_map = HashMap::new();

        // ── 1. 扫描子目录，读取各 KB 的 kb_config.json ──────────────
        if let Ok(mut entries) = tokio::fs::read_dir(base_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let ft = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                if !ft {
                    continue;
                }
                let kb_config_path = entry.path().join(KB_CONFIG_FILE);
                if !kb_config_path.exists() {
                    continue;
                }
                if let Some(config) = read_kb_config_json(&entry.path())
                    .await
                    .unwrap_or_else(|e| {
                        warn!(
                            dir = %entry.path().display(),
                            error = %e,
                            "Failed to read kb_config.json, skipping this directory"
                        );
                        None
                    })
                {
                    // 以 config.name 为 key — 与 create_kb/open_kb/delete_kb
                    // 的查找键保持一致（这些方法均使用原始名称，而非 sanitize 后的目录名）
                    config_map.insert(config.name.clone(), config);
                }
            }
        }

        // ── 2. 旧格式迁移：kb_configs.json → 各 KB 目录 ───────────
        let legacy_path = base_dir.join("kb_configs.json");
        if legacy_path.exists()
            && let Ok(data) = tokio::fs::read_to_string(&legacy_path).await
            && let Ok(legacy) = serde_json::from_str::<LegacyKbConfigsFile>(&data)
        {
            for cfg in &legacy.kb_configs {
                let kb_dir = base_dir.join(crate::sanitize_kb_name(&cfg.name));
                if let Err(e) = tokio::fs::create_dir_all(&kb_dir).await {
                    warn!(
                        kb = %cfg.name,
                        error = %e,
                        "Migration: failed to create KB directory, skipping"
                    );
                    continue;
                }
                if let Err(e) = write_kb_config_json_file(&kb_dir, cfg).await {
                    warn!(
                        kb = %cfg.name,
                        error = %e,
                        "Migration: failed to write kb_config.json, skipping"
                    );
                    continue;
                }
                // 迁移时以 cfg.name 为 key — 与 create_kb/open_kb/delete_kb 一致
                config_map
                    .entry(cfg.name.clone())
                    .or_insert_with(|| cfg.clone());
            }
            // 备份旧文件
            let _ = tokio::fs::rename(&legacy_path, base_dir.join("kb_configs.json.bak")).await;
            info!("Migrated legacy kb_configs.json into per-KB kb_config.json files");
        }

        info!(
            base_dir = %base_dir.display(),
            kb_count = config_map.len(),
            "Knowledge base manager loaded"
        );

        Ok(Self {
            instances: RwLock::new(HashMap::new()),
            configs: RwLock::new(config_map),
            base_dir: base_dir.to_path_buf(),
        })
    }

    /// 创建新知识库。
    pub async fn create_kb(&self, config: KbConfig) -> Result<Arc<KnowledgeBase>, KnowledgeError> {
        let name = config.name.clone();

        // 检查是否已存在
        {
            let instances = self.instances.read().await;
            if instances.contains_key(&name) {
                return Err(KnowledgeError::InvalidInput(format!(
                    "Knowledge base '{name}' already exists"
                )));
            }
        }

        // 构建知识库实例
        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;

        // 将配置写入 KB 目录自身的 kb_config.json
        let kb_dir = self.base_dir.join(crate::sanitize_kb_name(&name));
        write_kb_config_json_file(&kb_dir, &config).await?;

        // 缓存到内存
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.clone(), config.clone());
        }

        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.clone(), kb.clone());
        }

        info!(%name, "Knowledge base created");
        Ok(kb)
    }

    /// 打开已有知识库。
    pub async fn open_kb(&self, name: &str) -> Result<Arc<KnowledgeBase>, KnowledgeError> {
        // 检查是否已加载
        {
            let instances = self.instances.read().await;
            if let Some(kb) = instances.get(name) {
                return Ok(kb.clone());
            }
        }

        // 从配置创建
        let config = {
            let configs = self.configs.read().await;
            configs.get(name).cloned().ok_or_else(|| {
                KnowledgeError::NotFound(format!("Knowledge base '{name}' not found"))
            })?
        };

        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;
        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.to_string(), kb.clone());
        }

        info!(%name, "Knowledge base opened");
        Ok(kb)
    }

    /// 删除知识库及其所有数据。
    ///
    /// 删除整个 KB 目录（包含 `kb_config.json` 和数据文件），
    /// 并从内存中移除。不再需要聚合持久化——配置随目录一同删除。
    pub async fn delete_kb(&self, name: &str) -> Result<(), KnowledgeError> {
        {
            let mut instances = self.instances.write().await;
            instances.remove(name);
        }
        {
            let mut configs = self.configs.write().await;
            configs.remove(name);
        }

        // 删除整个 KB 目录（配置 + 数据一起删除）
        let kb_dir = self.base_dir.join(crate::sanitize_kb_name(name));
        if kb_dir.exists() {
            tokio::fs::remove_dir_all(&kb_dir).await.map_err(|e| {
                KnowledgeError::Internal(format!("Failed to delete knowledge base directory: {e}"))
            })?;
        }

        info!(%name, "Knowledge base deleted");
        Ok(())
    }

    /// 列出所有已配置的知识库。
    pub async fn list_kbs(&self) -> Result<Vec<KbInfo>, KnowledgeError> {
        let configs = self.configs.read().await;
        let instances = self.instances.read().await;

        let mut infos = Vec::new();
        for cfg in configs.values() {
            let (doc_count, chunk_count) = if let Some(kb) = instances.get(&cfg.name) {
                match kb.stats().await {
                    Ok(s) => (s.document_count, s.chunk_count),
                    Err(_) => (0, 0),
                }
            } else {
                (0, 0)
            };

            infos.push(KbInfo {
                name: cfg.name.clone(),
                description: cfg.description.clone(),
                backend: backend_type_str(&cfg.backend),
                embedding_model: format!("{:?}", cfg.embedding_model),
                document_count: doc_count,
                chunk_count,
            });
        }
        Ok(infos)
    }

    /// 获取指定知识库。
    pub async fn get_kb(&self, name: &str) -> Option<Arc<KnowledgeBase>> {
        self.open_kb(name).await.ok()
    }

    /// 跨知识库并发搜索。
    ///
    /// 通过 [`futures::future::join_all`] 并发轮询多个异步 future，
    /// 在单个运行时线程上交替推进，实现非阻塞并发查询。
    /// 单个 KB 搜索失败时记录 warning 并跳过，不影响其他 KB。
    /// 空结果集会被过滤掉。
    pub async fn search_all(&self, query: &str, top_k: usize) -> Vec<(String, Vec<SearchResult>)> {
        let configs = self.configs.read().await;
        let names: Vec<String> = configs.keys().cloned().collect();
        drop(configs);

        // 收集异步 future（不 spawn，由 join_all 并发驱动）
        let query = query.to_string();
        let futures: Vec<_> = names
            .into_iter()
            .map(|name| {
                let q = query.clone();
                async move {
                    let kb = self.open_kb(&name).await?;
                    let res = kb.search(&q, top_k).await?;
                    let entry: Option<(String, Vec<SearchResult>)> = if res.is_empty() {
                        None
                    } else {
                        Some((name, res))
                    };
                    Ok(entry) as Result<_, KnowledgeError>
                }
            })
            .collect();

        // join_all 并发轮询所有 future（在单线程上交替推进）
        let mut results = Vec::new();
        for result in futures::future::join_all(futures).await {
            match result {
                Ok(Some(entry)) => results.push(entry),
                Ok(None) => { /* 空结果，跳过 */ }
                Err(e) => {
                    warn!(error = %e, "Failed to search knowledge base concurrently, skipping");
                }
            }
        }
        results
    }
}

// ============================================================================
// 私有辅助函数
// ============================================================================

/// 从 KB 目录读取单个 `kb_config.json`。
async fn read_kb_config_json(kb_dir: &Path) -> Result<Option<KbConfig>, KnowledgeError> {
    let path = kb_dir.join(KB_CONFIG_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let data = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| KnowledgeError::InvalidInput(format!("Failed to read kb_config.json: {e}")))?;
    let config: KbConfig = serde_json::from_str(&data).map_err(|e| {
        KnowledgeError::InvalidInput(format!("Failed to parse kb_config.json: {e}"))
    })?;
    Ok(Some(config))
}

/// 将 KB 配置写入 `kb_dir/kb_config.json`。
async fn write_kb_config_json_file(kb_dir: &Path, config: &KbConfig) -> Result<(), KnowledgeError> {
    // 确保目录存在（KnowledgeBase::build 已创建，此处作为安全网）
    tokio::fs::create_dir_all(kb_dir)
        .await
        .map_err(|e| KnowledgeError::InvalidInput(format!("Failed to create KB directory: {e}")))?;
    let path = kb_dir.join(KB_CONFIG_FILE);
    let json = serde_json::to_string_pretty(config).map_err(|e| {
        KnowledgeError::Internal(format!("Failed to serialize kb_config.json: {e}"))
    })?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| KnowledgeError::Internal(format!("Failed to write kb_config.json: {e}")))?;
    Ok(())
}

/// 旧格式 `kb_configs.json` 的反序列化结构体。
///
/// 仅在 `load()` 的自动迁移路径中使用，迁移完成后该文件会被重命名为 `.bak`。
/// 此类型不对外暴露。
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct LegacyKbConfigsFile {
    kb_configs: Vec<KbConfig>,
}

fn backend_type_str(bt: &BackendType) -> String {
    match bt {
        BackendType::InMemory => "InMemory".into(),
        #[cfg(feature = "lancedb")]
        BackendType::LanceDb => "LanceDB".into(),
        #[cfg(feature = "helixdb")]
        BackendType::HelixDb => "HelixDB".into(),
    }
}
