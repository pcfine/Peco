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

pub mod config;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::Digest;
use tokio::sync::RwLock;
use tracing::info;

use crate::chunking::make_chunker;
use crate::embedding::{FastembedEngine, FastembedModelType};
use crate::engine::{HybridSearchEngine, IngestionPipeline};
use crate::error::KnowledgeError;
use crate::parsers::make_parser;
use crate::traits::*;
use crate::types::*;

use self::config::{BackendType, KbConfig, KbConfigsFile, KbInfo};

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
            if let Ok(kb) = self.open_kb(name).await {
                if let Ok(res) = kb.search(query, top_k).await {
                    if !res.is_empty() {
                        results.push((name.clone(), res));
                    }
                }
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

// ---------------------------------------------------------------------------
// KnowledgeBase
// ---------------------------------------------------------------------------

/// 单个知识库 — 封装完整的读写能力。
pub struct KnowledgeBase {
    config: KbConfig,
    pipeline: IngestionPipeline,
    search_engine: HybridSearchEngine,
}

impl KnowledgeBase {
    /// 根据配置构建知识库实例。
    async fn build(base_dir: &Path, config: &KbConfig) -> Result<Self, KnowledgeError> {
        let kb_dir = base_dir.join(crate::sanitize_kb_name(&config.name));
        tokio::fs::create_dir_all(&kb_dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法创建目录: {e}")))?;

        // 构建嵌入引擎
        let model_type: FastembedModelType = config.embedding_model.clone().into();
        let embedding = Arc::new(
            FastembedEngine::new(model_type)
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?,
        );
        let ndims = embedding.ndims();

        // 构建分块器
        let chunker = make_chunker(config.chunking.clone().into());

        // 构建存储后端
        #[cfg(feature = "lancedb")]
        let (doc_store, vector_index, fulltext_index): (
            Arc<dyn DocumentStore>,
            Option<Arc<dyn VectorIndex>>,
            Option<Arc<dyn FullTextIndex>>,
        ) = match &config.backend {
            BackendType::InMemory => {
                let be = Arc::new(crate::backends::memory::InMemoryBackend::new());
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                )
            }
            BackendType::LanceDb => {
                let be = Arc::new(
                    crate::backends::lancedb::LanceDbBackend::connect(
                        &kb_dir,
                        &crate::sanitize_kb_name(&config.name),
                        ndims,
                    )
                    .await?,
                );
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                )
            }
            #[cfg(feature = "helixdb")]
            BackendType::HelixDb => {
                // HelixDB 后端连接需要 URL 配置
                return Err(KnowledgeError::InvalidInput(
                    "HelixDB 后端需通过高级 API 配置，请使用 HelixDbBackend::connect()".into(),
                ));
            }
        };

        #[cfg(not(feature = "lancedb"))]
        let (doc_store, vector_index, fulltext_index): (
            Arc<dyn DocumentStore>,
            Option<Arc<dyn VectorIndex>>,
            Option<Arc<dyn FullTextIndex>>,
        ) = match &config.backend {
            BackendType::InMemory => {
                let be = Arc::new(crate::backends::memory::InMemoryBackend::new());
                (
                    be.clone() as Arc<dyn DocumentStore>,
                    Some(be.clone() as Arc<dyn VectorIndex>),
                    Some(be.clone() as Arc<dyn FullTextIndex>),
                )
            }
            #[cfg(feature = "helixdb")]
            BackendType::HelixDb => {
                return Err(KnowledgeError::InvalidInput(
                    "HelixDB 后端需通过高级 API 配置".into(),
                ));
            }
            _ => {
                return Err(KnowledgeError::InvalidInput(
                    "LanceDB feature 未启用".into(),
                ));
            }
        };

        // 构建 IngestionPipeline
        let pipeline = IngestionPipeline::new(
            doc_store.clone(),
            vector_index.clone(),
            None, // graph_store
            fulltext_index.clone(),
            embedding.clone(),
            chunker,
        );

        // 构建 SearchEngine
        let search_engine = HybridSearchEngine::new(
            doc_store,
            vector_index,
            None, // graph_store
            fulltext_index,
            embedding,
        );

        Ok(Self {
            config: config.clone(),
            pipeline,
            search_engine,
        })
    }

    /// 从文件添加文档（自动检测格式 → 解析 → 分块 → 嵌入 → 存储）。
    pub async fn add_file(&self, path: &Path) -> Result<Document, KnowledgeError> {
        let parser = make_parser(path)?;
        let parsed = parser.parse_file(path).await?;
        let hash = sha2::Sha256::digest(parsed.content.as_bytes());
        let doc_id = hex::encode(&hash[..8]);

        let doc = Document {
            id: doc_id,
            kb_id: Some(self.config.name.clone()),
            title: parsed.title,
            source_path: parsed.source_path,
            content: parsed.content,
            metadata: parsed.metadata,
        };

        self.pipeline.ingest(doc.clone()).await?;
        info!(kb = %self.config.name, doc_id = %doc.id, title = %doc.title, "文档已添加");
        Ok(doc)
    }

    /// 批量导入目录中所有支持的文档。
    pub async fn add_directory(&self, dir: &Path) -> Result<Vec<Document>, KnowledgeError> {
        let mut docs = Vec::new();
        let mut entries = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("读取目录失败: {e}")))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("遍历目录失败: {e}")))?
        {
            let path = entry.path();
            if path.is_file() {
                match self.add_file(&path).await {
                    Ok(doc) => docs.push(doc),
                    Err(e) => tracing::warn!(path = %path.display(), error = %e, "跳过文件"),
                }
            }
        }
        Ok(docs)
    }

    /// 直接添加文本（跳过解析）。
    pub async fn add_text(
        &self,
        title: &str,
        content: &str,
        source: &str,
    ) -> Result<Document, KnowledgeError> {
        let hash = sha2::Sha256::digest(content.as_bytes());
        let doc_id = hex::encode(&hash[..8]);

        let doc = Document {
            id: doc_id,
            kb_id: Some(self.config.name.clone()),
            title: title.to_string(),
            source_path: source.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata {
                file_type: Some("txt".into()),
                ..Default::default()
            },
        };

        self.pipeline.ingest(doc.clone()).await?;
        Ok(doc)
    }

    /// 搜索知识库。
    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        self.search_engine
            .search(&SearchRequest {
                query: query.to_string(),
                top_k,
                strategy: SearchStrategy::Auto,
                filters: Some(SearchFilters {
                    kb_id: Some(self.config.name.clone()),
                    ..Default::default()
                }),
                min_confidence: None,
            })
            .await
    }

    /// 删除文档。
    pub async fn remove_document(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        self.pipeline.delete_document(&doc_id.to_string()).await
    }

    /// 获取统计信息。
    pub async fn stats(&self) -> Result<StoreStats, KnowledgeError> {
        self.pipeline.stats().await
    }

    /// 列出文档摘要（分页）。
    pub async fn list_documents(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DocumentSummary>, KnowledgeError> {
        self.pipeline.list_documents(offset, limit).await
    }

    /// 获取知识库配置。
    pub fn config(&self) -> &KbConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

fn backend_type_str(bt: &BackendType) -> String {
    match bt {
        BackendType::InMemory => "InMemory".into(),
        #[cfg(feature = "lancedb")]
        BackendType::LanceDb => "LanceDB".into(),
        #[cfg(feature = "helixdb")]
        BackendType::HelixDb => "HelixDB".into(),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChunkingStrategy;

    #[tokio::test]
    async fn create_and_use_kb_inmemory() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = KnowledgeBaseManager::load(tmp.path()).await.unwrap();

        let kb = mgr
            .create_kb(config::KbConfig {
                name: "test-kb".into(),
                description: "测试知识库".into(),
                embedding_model: config::FastembedModelTypeSerde::AllMiniLML6V2Q,
                chunking: config::ChunkingStrategySerde::FixedSize { size: 100 },
                backend: config::BackendType::InMemory,
                storage_path: None,
            })
            .await
            .unwrap();

        // 添加文本
        let doc = kb
            .add_text("Test", "Rust is a systems programming language.", "test")
            .await
            .unwrap();
        assert_eq!(doc.title, "Test");
        assert!(doc.kb_id.is_some());

        // 搜索
        let results = kb.search("Rust programming", 3).await.unwrap();
        assert!(!results.is_empty());

        // 列出知识库
        let infos = mgr.list_kbs().await.unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "test-kb");
    }
}
