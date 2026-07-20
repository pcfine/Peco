// ============================================================================
// PersonalMemoryStore — 封装 KnowledgeManager，提供个人记忆存取
// ============================================================================
//
// 存储结构：
//   {base_dir}/personal_memory_{user_id}/
//     ├── _profile.md          # Profile 记忆（Markdown + YAML frontmatter）
//     ├── semantic/             # Semantic 记忆（每条一个文档）
//     │   ├── mem_xxx.md
//     │   └── ...
//     └── episodic/             # Episodic 摘要
//         ├── summary_xxx.md
//         └── ...
//
// 所有记忆文档通过 add_text_to_kb 写入，通过 search_kb 检索。

use std::sync::Arc;

use knowledge_base::{KbConfig, SearchResult};
use tracing::{info, warn};

use super::config::StorageConfig;
use super::types::{Importance, MemoryCategory, MemoryFact, UserProfile};
use crate::knowledge::KnowledgeManager;

/// 个人记忆存储 — 封装对 Personal KB 的读写操作。
///
/// # 线程安全
///
/// `KnowledgeManager` 内部使用 `tokio::sync::Mutex` 保护，`Arc` 共享安全。
pub struct PersonalMemoryStore {
    /// 底层知识库管理器（已绑定到用户目录）。
    km: Arc<KnowledgeManager>,
    /// Personal KB 名称（如 `personal_memory_user123`）。
    kb_name: String,
    /// 存储配置。
    #[allow(dead_code)]
    config: StorageConfig,
}

impl PersonalMemoryStore {
    /// 创建新的 PersonalMemoryStore。
    pub fn new(km: Arc<KnowledgeManager>, kb_name: String, config: StorageConfig) -> Self {
        Self {
            km,
            kb_name,
            config,
        }
    }

    /// 确保 Personal KB 已创建（幂等）。
    pub async fn ensure_kb(&self) -> Result<(), String> {
        self.km.ensure_loaded().await.map_err(|e| e.to_string())?;

        let kbs = self.km.list_kbs().await.map_err(|e| e.to_string())?;
        if kbs.iter().any(|k| k.name == self.kb_name) {
            return Ok(());
        }

        let kb_config = KbConfig {
            name: self.kb_name.clone(),
            description: "PPA 个人记忆库".into(),
            embedding_model: knowledge_base::FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: knowledge_base::ChunkingStrategySerde::FixedSize { size: 512 },
            backend: knowledge_base::BackendType::InMemory,
            storage_path: None,
        };

        self.km
            .create_kb(kb_config)
            .await
            .map_err(|e| e.to_string())?;
        info!(kb = %self.kb_name, "Personal KB created");
        Ok(())
    }

    // =========================================================================
    // Profile 操作
    // =========================================================================

    pub async fn get_profile(&self) -> Result<UserProfile, String> {
        self.ensure_kb().await?;

        let results = self
            .km
            .search_kb(&self.kb_name, "_profile", 1)
            .await
            .map_err(|e| e.to_string())?;

        if let Some(result) = results.first()
            && result.title == "_profile"
        {
            if let Ok(profile) = serde_yaml::from_str::<UserProfile>(&result.snippet) {
                return Ok(profile);
            }
            warn!("Failed to parse profile YAML, returning default");
        }

        Ok(UserProfile::default())
    }

    pub async fn update_profile(&self, profile: &UserProfile) -> Result<(), String> {
        self.ensure_kb().await?;

        let yaml = serde_yaml::to_string(profile).map_err(|e| e.to_string())?;

        self.km
            .add_text_to_kb(&self.kb_name, "_profile", &yaml, "ppa_profile")
            .await
            .map_err(|e| e.to_string())?;

        info!("Profile updated");
        Ok(())
    }

    // =========================================================================
    // 记忆操作
    // =========================================================================

    /// 保存或更新一条语义记忆。
    pub async fn save_or_update_fact(&self, fact: &MemoryFact) -> Result<(), String> {
        self.ensure_kb().await?;

        let title = format!("memory_{}", fact.id);
        let content = format_fact_content(fact);

        self.km
            .add_text_to_kb(
                &self.kb_name,
                &title,
                &content,
                &format!("ppa_{}", fact.category_name()),
            )
            .await
            .map_err(|e| e.to_string())?;

        info!(id = %fact.id, category = ?fact.category, "Memory fact saved");
        Ok(())
    }

    /// 语义向量检索。
    pub async fn search_semantic(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<MemoryFact>, String> {
        self.search_facts(query, top_k, min_score).await
    }

    /// Episodic 检索。
    pub async fn search_episodic(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<MemoryFact>, String> {
        self.search_facts(query, top_k, min_score).await
    }

    /// 通用事实检索。
    async fn search_facts(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<MemoryFact>, String> {
        self.ensure_kb().await?;

        let results = self
            .km
            .search_kb(&self.kb_name, query, top_k)
            .await
            .map_err(|e| e.to_string())?;

        let facts: Vec<MemoryFact> = results
            .into_iter()
            .filter(|r| r.score >= min_score)
            .filter_map(|r| parse_fact_from_search_result(&r))
            .collect();

        Ok(facts)
    }

    /// 标记一条记忆过期（逻辑删除）。
    pub async fn invalidate_fact(&self, fact: &MemoryFact) -> Result<(), String> {
        let content = format!("[DELETED] {}", fact.content);
        self.km
            .add_text_to_kb(
                &self.kb_name,
                &format!("deleted_{}", fact.id),
                &content,
                "ppa_deleted",
            )
            .await
            .map_err(|e| e.to_string())?;

        info!(id = %fact.id, "Memory fact invalidated");
        Ok(())
    }

    /// 获取记忆数量。
    pub async fn memory_count(&self) -> Result<usize, String> {
        self.ensure_kb().await?;
        let docs = self
            .km
            .list_documents(&self.kb_name, 0, 1000)
            .await
            .map_err(|e| e.to_string())?;
        Ok(docs.len())
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

impl MemoryFact {
    fn category_name(&self) -> &str {
        match self.category {
            MemoryCategory::Profile => "profile",
            MemoryCategory::Semantic => "semantic",
            MemoryCategory::Episodic => "episodic",
        }
    }
}

fn format_fact_content(fact: &MemoryFact) -> String {
    format!(
        "---\ncategory: {}\nimportance: {:?}\ncreated_at: {}\nid: {}\n---\n\n{}",
        fact.category_name(),
        fact.importance,
        fact.created_at.to_rfc3339(),
        fact.id,
        fact.content
    )
}

fn parse_fact_from_search_result(result: &SearchResult) -> Option<MemoryFact> {
    if result.snippet.starts_with("[DELETED]") {
        return None;
    }
    if result.title == "_profile" {
        return None;
    }

    let content = if let Some(body_start) = result.snippet.find("---\n") {
        let after_first_delim = &result.snippet[body_start + 4..];
        if let Some(body_end) = after_first_delim.find("\n---\n") {
            after_first_delim[body_end + 5..].trim().to_string()
        } else {
            result.snippet.clone()
        }
    } else {
        result.snippet.clone()
    };

    Some(MemoryFact {
        id: result.document_id.clone(),
        category: MemoryCategory::Semantic,
        importance: Importance::Medium,
        content,
        created_at: Default::default(),
        updated_at: Default::default(),
        expires_at: None,
    })
}
