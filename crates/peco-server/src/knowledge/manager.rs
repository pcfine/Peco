// ============================================================================
// WebKnowledgeManager — 用户级知识库管理
// ============================================================================
//
// 核心问题：peco-core::KnowledgeManager 是全局单例，不区分用户。
// WebKnowledgeManager 为每个用户创建独立的 KnowledgeManager 实例，
// 实现用户隔离的数据存储。
//
// 架构：
//   WebKnowledgeManager
//   └── per-user KnowledgeManager (缓存)
//       └── knowledge_base::KnowledgeBaseManager
//           ├── KB "技术文档" → LanceDB
//           └── KB "法律合同" → LanceDB

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use peco_core::knowledge::KnowledgeManager;
use tokio::sync::RwLock;

use crate::error::ApiError;

/// Web 层知识库管理器 — 提供用户级隔离。
///
/// 每个用户拥有独立的 KnowledgeManager 实例，其 base_dir 指向
/// `{data_dir}/knowledge/{user_id}/`。实例按需创建并缓存在 HashMap 中。
pub struct WebKnowledgeManager {
    /// 数据根目录（来自 ServerConfig.data_dir）。
    base_dir: PathBuf,
    /// 用户级 KnowledgeManager 缓存。
    managers: RwLock<HashMap<String, Arc<KnowledgeManager>>>,
}

impl WebKnowledgeManager {
    /// 创建新的 WebKnowledgeManager。
    ///
    /// `base_dir` 是数据存储根目录（通常是 `~/.peco/`）。
    /// 各用户的知识库数据存储在 `{base_dir}/knowledge/{user_id}/` 下。
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            managers: RwLock::new(HashMap::new()),
        }
    }

    /// 获取（或创建）指定用户的 KnowledgeManager。
    ///
    /// 用户的知识库目录：`{base_dir}/knowledge/{user_id}/`。
    /// KnowledgeManager 在首次获取时创建并缓存，后续调用直接返回缓存实例。
    pub async fn get_manager(
        &self,
        user_id: &str,
    ) -> Result<Arc<KnowledgeManager>, ApiError> {
        // 检查缓存
        {
            let managers = self.managers.read().await;
            if let Some(km) = managers.get(user_id) {
                return Ok(km.clone());
            }
        }

        // 创建新实例
        let user_kb_dir = self.user_knowledge_dir(user_id);
        tokio::fs::create_dir_all(&user_kb_dir)
            .await
            .map_err(|e| {
                ApiError::Internal(format!(
                    "无法创建用户知识库目录 '{}': {e}",
                    user_kb_dir.display()
                ))
            })?;

        let km = Arc::new(KnowledgeManager::new(user_kb_dir));

        // 延迟加载底层 KnowledgeBaseManager
        km.ensure_loaded().await.map_err(|e| {
            ApiError::Internal(format!("知识库引擎初始化失败: {e}"))
        })?;

        // 写入缓存
        {
            let mut managers = self.managers.write().await;
            managers.insert(user_id.to_string(), km.clone());
        }

        tracing::info!(
            user_id = %user_id,
            kb_dir = %self.user_knowledge_dir(user_id).display(),
            "KnowledgeManager initialized for user"
        );

        Ok(km)
    }

    /// 使指定用户的 KnowledgeManager 缓存失效。
    ///
    /// 下次调用 `get_manager()` 时将重新创建实例并加载配置。
    pub async fn invalidate(&self, user_id: &str) {
        let mut managers = self.managers.write().await;
        managers.remove(user_id);
        tracing::debug!(user_id = %user_id, "KnowledgeManager cache invalidated");
    }

    /// 获取指定用户的知识库存储目录路径。
    fn user_knowledge_dir(&self, user_id: &str) -> PathBuf {
        self.base_dir.join("knowledge").join(user_id)
    }

    /// 获取指定用户的 docs 目录路径（用于文件上传）。
    pub fn user_kb_docs_dir(&self, user_id: &str, kb_name: &str) -> PathBuf {
        let sanitized = knowledge_base::sanitize_kb_name(kb_name);
        self.user_knowledge_dir(user_id).join(sanitized).join("docs")
    }
}
