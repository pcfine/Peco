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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::info;

use crate::error::KnowledgeError;
use crate::types::SearchResult;

use super::config::{BackendType, KbConfig, KbConfigsFile, KbInfo};
use super::KnowledgeBase;

// ---------------------------------------------------------------------------
// KnowledgeBaseManager
// ---------------------------------------------------------------------------

/// 知识库管理器 — 统一入口，管理多个知识库实例。
pub struct KnowledgeBaseManager {
    /// 知识库实例映射：kb_name → KnowledgeBase
    instances: RwLock<HashMap<String, Arc<KnowledgeBase>>>,
    /// 配置映射
    configs: RwLock<HashMap<String, KbConfig>>,
    /// 数据根目录
    base_dir: PathBuf,
    /// 配置文件路径
    config_path: PathBuf,
}

impl KnowledgeBaseManager {
    /// 从指定目录加载所有知识库配置。
    ///
    /// 若配置文件不存在，将创建空配置。
    pub async fn load(base_dir: &Path) -> Result<Self, KnowledgeError> {
        tokio::fs::create_dir_all(base_dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法创建目录: {e}")))?;

        let config_path = base_dir.join("kb_configs.json");
        let configs: KbConfigsFile = if config_path.exists() {
            let data = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| KnowledgeError::InvalidInput(format!("读取配置失败: {e}")))?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            KbConfigsFile::default()
        };

        let mut config_map = HashMap::new();
        for cfg in &configs.kb_configs {
            config_map.insert(cfg.name.clone(), cfg.clone());
        }

        info!(base_dir = %base_dir.display(), kb_count = configs.kb_configs.len(), "知识库管理器已加载");

        Ok(Self {
            instances: RwLock::new(HashMap::new()),
            configs: RwLock::new(config_map),
            base_dir: base_dir.to_path_buf(),
            config_path,
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
                    "知识库 '{name}' 已存在"
                )));
            }
        }

        // 构建知识库实例
        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;

        // 保存配置
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.clone(), config.clone());
            self.persist_configs(&configs).await?;
        }

        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.clone(), kb.clone());
        }

        info!(%name, "知识库已创建");
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
            configs
                .get(name)
                .cloned()
                .ok_or_else(|| KnowledgeError::NotFound(format!("知识库 '{name}' 未找到")))?
        };

        let kb = KnowledgeBase::build(&self.base_dir, &config).await?;
        let kb = Arc::new(kb);
        {
            let mut instances = self.instances.write().await;
            instances.insert(name.to_string(), kb.clone());
        }

        info!(%name, "知识库已打开");
        Ok(kb)
    }

    /// 删除知识库及其所有数据。
    pub async fn delete_kb(&self, name: &str) -> Result<(), KnowledgeError> {
        {
            let mut instances = self.instances.write().await;
            instances.remove(name);
        }
        {
            let mut configs = self.configs.write().await;
            configs.remove(name);
            self.persist_configs(&configs).await?;
        }

        // 删除数据目录
        let kb_dir = self.base_dir.join(crate::sanitize_kb_name(name));
        if kb_dir.exists() {
            tokio::fs::remove_dir_all(&kb_dir)
                .await
                .map_err(|e| KnowledgeError::Internal(format!("删除知识库目录失败: {e}")))?;
        }

        info!(%name, "知识库已删除");
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

    /// 跨知识库搜索（并发查询所有 KB，合并结果）。
    pub async fn search_all(&self, query: &str, top_k: usize) -> Vec<(String, Vec<SearchResult>)> {
        let configs = self.configs.read().await;
        let names: Vec<String> = configs.keys().cloned().collect();
        drop(configs);

        let mut results = Vec::new();
        for name in &names {
            if let Ok(kb) = self.open_kb(name).await
                && let Ok(res) = kb.search(query, top_k).await
                && !res.is_empty()
            {
                results.push((name.clone(), res));
            }
        }
        results
    }

    /// 持久化配置到 JSON 文件。
    async fn persist_configs(
        &self,
        configs: &HashMap<String, KbConfig>,
    ) -> Result<(), KnowledgeError> {
        let cf = KbConfigsFile {
            kb_configs: configs.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&cf)
            .map_err(|e| KnowledgeError::Internal(format!("序列化配置失败: {e}")))?;
        tokio::fs::write(&self.config_path, json)
            .await
            .map_err(|e| KnowledgeError::Internal(format!("写入配置失败: {e}")))?;
        Ok(())
    }
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
