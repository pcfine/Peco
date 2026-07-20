// ============================================================================
// PersonalMemoryStore — 封装 KnowledgeManager，提供个人记忆存取
// ============================================================================

#![allow(dead_code)]
//
// 设计：PersonalMemoryStore 封装 peco_core::knowledge::KnowledgeManager，
// 在指定的 Personal KB（由 server 层以 per-user base_dir 创建）上操作。
// 由 server 层直接创建 per-user
// KnowledgeManager 实例并传入。
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
// Profile 通过文档标题 "_profile" 精确查找。

use std::sync::Arc;

use knowledge_base::{KbConfig, SearchResult};
use peco_core::knowledge::KnowledgeManager;
use tracing::{info, warn};

use super::config::StorageConfig;
use super::types::{Importance, MemoryCategory, MemoryFact, MemoryOperation, UserProfile};

/// Personal KB 的名称前缀。
const PERSONAL_KB_PREFIX: &str = "personal_memory";

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
    config: StorageConfig,
}

impl PersonalMemoryStore {
    /// 创建新的 PersonalMemoryStore。
    ///
    /// `km` 应为已绑定到用户专属 `base_dir` 的 KnowledgeManager 实例。
    /// `kb_name` 为 Personal KB 名称。
    pub fn new(km: Arc<KnowledgeManager>, kb_name: String, config: StorageConfig) -> Self {
        Self {
            km,
            kb_name,
            config,
        }
    }

    /// 确保 Personal KB 已创建（幂等）。
    ///
    /// 首次调用时创建 KB + 内部文档目录，后续调用直接返回。
    pub async fn ensure_kb(&self) -> Result<(), String> {
        self.km.ensure_loaded().await.map_err(|e| e.to_string())?;

        // 检查 KB 是否已存在
        let kbs = self.km.list_kbs().await.map_err(|e| e.to_string())?;
        if kbs.iter().any(|k| k.name == self.kb_name) {
            return Ok(());
        }

        // 创建 KB
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

    /// 获取用户 Profile。
    ///
    /// Profile 以 `_profile` 为标题存储在 Personal KB 中。
    /// 若不存在则返回默认空 Profile。
    pub async fn get_profile(&self) -> Result<UserProfile, String> {
        self.ensure_kb().await?;

        // 尝试通过搜索找到 _profile 文档
        let results = self
            .km
            .search_kb(&self.kb_name, "_profile", 1)
            .await
            .map_err(|e| e.to_string())?;

        if let Some(result) = results.first()
            && result.title == "_profile"
        {
            // 从 snippet 中解析 YAML
            if let Ok(profile) = serde_yaml::from_str::<UserProfile>(&result.snippet) {
                return Ok(profile);
            }
            // 兼容纯文本格式，尝试解析
            warn!("Failed to parse profile YAML, returning default");
        }

        Ok(UserProfile::default())
    }

    /// 更新 Profile。
    pub async fn update_profile(&self, profile: &UserProfile) -> Result<(), String> {
        self.ensure_kb().await?;

        let yaml = serde_yaml::to_string(profile).map_err(|e| e.to_string())?;

        // 删除旧 profile（如果存在）后写入新版本
        // 注意：当前 KnowledgeManager 没有 update/upsert 语义，
        // 我们使用 add_text_to_kb 追加，通过定期清理旧版本解决
        self.km
            .add_text_to_kb(&self.kb_name, "_profile", &yaml, "ppa_profile")
            .await
            .map_err(|e| e.to_string())?;

        info!("Profile updated");
        Ok(())
    }

    /// 检查是否需要同步 profile（在 high-importance profile 变化后调用）。
    pub async fn sync_profile(&self) -> Result<(), String> {
        // 当前实现：Profile 变化不大，仅重新写入即可
        // 未来可在此处做 dedup：清理旧的 _profile 版本，只保留最新
        Ok(())
    }

    // =========================================================================
    // Semantic 记忆操作
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
        self.search_facts(query, top_k, min_score, MemoryCategory::Semantic)
            .await
    }

    /// Episodic 检索。
    pub async fn search_episodic(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<MemoryFact>, String> {
        self.search_facts(query, top_k, min_score, MemoryCategory::Episodic)
            .await
    }

    /// 通用事实检索。
    async fn search_facts(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
        _category: MemoryCategory,
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

    // =========================================================================
    // 冲突检测
    // =========================================================================

    /// 对一条新 fact 执行冲突检测，返回应采取的操作。
    ///
    /// 通过向量检索找到最相似的已有记忆，判断：
    /// - 无语义等价记忆 → Add
    /// - 语义等价但内容补充 → Update
    /// - 与已有记忆矛盾 → Delete（标记旧记忆过期）
    /// - 完全相同 → Noop
    pub async fn detect_operation(&self, fact: &MemoryFact) -> MemoryOperation {
        // 检索最相似的已有记忆
        let existing = match self
            .search_facts(&fact.content, 1, 0.7, fact.category.clone())
            .await
        {
            Ok(facts) if !facts.is_empty() => facts.into_iter().next().unwrap(),
            _ => return MemoryOperation::Add,
        };

        // 简易相似度判断（V1 用文本比较，V2 可升级为 LLM 判断）
        let similarity = text_similarity(&fact.content, &existing.content);

        if similarity > 0.95 {
            MemoryOperation::Noop
        } else if similarity > 0.6 {
            MemoryOperation::Update
        } else if is_contradiction(&fact.content, &existing.content) {
            MemoryOperation::Delete
        } else {
            MemoryOperation::Add
        }
    }

    /// 标记一条记忆过期（逻辑删除）。
    pub async fn invalidate_fact(&self, _fact: &MemoryFact) -> Result<(), String> {
        // V1 实现：通过追加一条带有 DELETE 标记的文档来逻辑删除
        // 在检索时过滤掉已删除的记忆
        let content = format!("[DELETED] {}", _fact.content);
        self.km
            .add_text_to_kb(
                &self.kb_name,
                &format!("deleted_{}", _fact.id),
                &content,
                "ppa_deleted",
            )
            .await
            .map_err(|e| e.to_string())?;

        info!(id = %_fact.id, "Memory fact invalidated");
        Ok(())
    }

    // =========================================================================
    // 统计
    // =========================================================================

    /// 获取个人知识库中的文档总数（估算记忆数量）。
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
// 内部辅助函数
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

/// 格式化 fact 为 Markdown 内容存入知识库。
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

/// 从 SearchResult 解析 MemoryFact。
fn parse_fact_from_search_result(result: &SearchResult) -> Option<MemoryFact> {
    // 跳过已删除的记忆
    if result.snippet.starts_with("[DELETED]") {
        return None;
    }

    // 跳过 _profile 文档
    if result.title == "_profile" {
        return None;
    }

    // 从 snippet 中提取实际内容（去掉 YAML frontmatter）
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
        category: MemoryCategory::Semantic, // 默认
        importance: Importance::Medium,
        content,
        created_at: Default::default(),
        updated_at: Default::default(),
        expires_at: None,
    })
}

/// 简易文本相似度计算（Jaccard 系数）。
fn text_similarity(a: &str, b: &str) -> f64 {
    let set_a: std::collections::HashSet<_> = a.split_whitespace().collect();
    let set_b: std::collections::HashSet<_> = b.split_whitespace().collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    intersection as f64 / union as f64
}

/// 检测两句是否矛盾（V1 简易版：包含否定关键词则视为可能矛盾）。
fn is_contradiction(new_fact: &str, existing: &str) -> bool {
    let negation_words = ["不", "不是", "不喜欢", "不用", "不要", "换"];
    let has_negation = negation_words.iter().any(|w| new_fact.contains(w));

    if !has_negation {
        return false;
    }

    // 检查是否涉及同一主题
    let similarity = text_similarity(new_fact, existing);
    similarity > 0.3
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_similarity_identical() {
        let s = "用户偏好 Rust 编程语言";
        assert!(text_similarity(s, s) > 0.99);
    }

    #[test]
    fn test_text_similarity_different() {
        let a = "用户偏好 Rust 编程语言";
        let b = "用户今天吃了火锅";
        assert!(text_similarity(a, b) < 0.3);
    }

    #[test]
    fn test_text_similarity_partial() {
        let a = "用户偏好 Rust 编程语言";
        let b = "用户偏好 Python 编程语言";
        let sim = text_similarity(a, b);
        // 大部分词相同
        assert!(sim > 0.4);
    }

    #[test]
    fn test_is_contradiction_with_negation() {
        assert!(is_contradiction(
            "用户不喜欢 Axum 框架",
            "用户偏好 Axum 框架"
        ));
    }

    #[test]
    fn test_no_contradiction_without_negation() {
        assert!(!is_contradiction(
            "用户正在学习 Actix",
            "用户偏好 Axum 框架"
        ));
    }
}
