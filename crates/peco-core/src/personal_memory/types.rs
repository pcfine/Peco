// ============================================================================
// PPA 数据类型 — MemoryFact, UserProfile, QueryType, MemoryOperation
// ============================================================================

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// MemoryCategory
// ============================================================================

/// 记忆分类，对应三层记忆模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Layer 1: 用户身份、角色、核心偏好
    Profile,
    /// Layer 2: 离散事实、偏好细节、知识点
    Semantic,
    /// Layer 3: 历史对话摘要、项目上下文
    Episodic,
}

// ============================================================================
// Importance
// ============================================================================

/// 记忆重要程度。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Low,
    Medium,
    High,
}

// ============================================================================
// MemoryOperation
// ============================================================================

/// 记忆操作类型 — 借鉴 Mem0 的四操作模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// 新增：全新信息，无语义等价记忆
    Add,
    /// 更新：语义等价但信息补充
    Update,
    /// 删除：与已有记忆矛盾
    Delete,
    /// 跳过：信息已存在且无变化
    Noop,
}

// ============================================================================
// MemoryFact
// ============================================================================

/// 一条个人记忆记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// 唯一标识符（格式: `mem_{YYYYmmDD_HHMMSSfff}`，如 `mem_20260719_143052_789`）
    pub id: String,
    /// 记忆分类
    pub category: MemoryCategory,
    /// 重要程度
    pub importance: Importance,
    /// 简洁的事实陈述（一句话）
    pub content: String,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 最后更新时间
    pub updated_at: DateTime<Utc>,
    /// 过期时间（None = 永不过期）
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl MemoryFact {
    /// 创建新的 MemoryFact，自动生成 ID 和时间戳。
    pub fn new(category: MemoryCategory, importance: Importance, content: String) -> Self {
        let now = Utc::now();
        let id = format!("mem_{}", now.format("%Y%m%d_%H%M%S%3f"));
        Self {
            id,
            category,
            importance,
            content,
            created_at: now,
            updated_at: now,
            expires_at: None,
        }
    }

    /// 创建新的 MemoryFact，指定 ID。
    pub fn with_id(
        id: String,
        category: MemoryCategory,
        importance: Importance,
        content: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            category,
            importance,
            content,
            created_at: now,
            updated_at: now,
            expires_at: None,
        }
    }

    /// 是否为有效记忆（未过期）。
    pub fn is_valid(&self) -> bool {
        match self.expires_at {
            Some(expiry) => Utc::now() < expiry,
            None => true,
        }
    }
}

// ============================================================================
// UserProfile
// ============================================================================

/// 用户个人资料（Layer 1 — Profile Memory）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserProfile {
    /// 用户姓名
    #[serde(default)]
    pub name: String,
    /// 用户角色
    #[serde(default)]
    pub role: String,
    /// 偏好设置
    #[serde(default)]
    pub preferences: UserPreferences,
    /// 最后更新时间
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserPreferences {
    /// 偏好的回复语言
    #[serde(default)]
    pub language: String,
    /// 偏好的回复风格
    #[serde(default)]
    pub style: String,
    /// 常用技术栈
    #[serde(default)]
    pub tech_stack: Vec<String>,
}

impl UserProfile {
    /// 格式化为一段文本，供 system prompt 注入使用。
    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        if !self.name.is_empty() {
            parts.push(format!("- 姓名: {}", self.name));
        }
        if !self.role.is_empty() {
            parts.push(format!("- 角色: {}", self.role));
        }
        if !self.preferences.language.is_empty() {
            parts.push(format!("- 回复语言: {}", self.preferences.language));
        }
        if !self.preferences.style.is_empty() {
            parts.push(format!("- 回复风格: {}", self.preferences.style));
        }
        if !self.preferences.tech_stack.is_empty() {
            parts.push(format!(
                "- 技术栈: {}",
                self.preferences.tech_stack.join(", ")
            ));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!(
                "[用户资料]\n{}\n(更新时间: {})",
                parts.join("\n"),
                self.updated_at.format("%Y-%m-%d")
            )
        }
    }
}

// ============================================================================
// QueryType
// ============================================================================

/// 查询类型 — 由 QueryClassifier 分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryType {
    /// 关于用户个人信息的查询（"我之前说过什么？"）
    PersonalQuery,
    /// 技术问题（"怎么写一个 Axum handler？"）
    TechnicalQuery,
    /// 闲聊（"你好"、"谢谢"）
    CasualChat,
    /// 默认查询 — 从宽匹配，检索 semantic
    GeneralQuery,
}

// ============================================================================
// TurnContext
// ============================================================================

/// 单轮对话上下文，用于 MemoryAnalyzer 分析。
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// 用户查询内容
    pub user_query: String,
    /// Assistant 回复内容列表（可能含多段 tool 调用后的回复）
    pub assistant_responses: Vec<String>,
}

impl TurnContext {
    /// 总字符数（用于阈值过滤）。
    pub fn total_chars(&self) -> usize {
        let user_chars = self.user_query.chars().count();
        let assistant_chars: usize = self
            .assistant_responses
            .iter()
            .map(|r| r.chars().count())
            .sum();
        user_chars + assistant_chars
    }

    /// 合并为一段文本，供 LLM 分析。
    pub fn format_for_analysis(&self) -> String {
        let mut parts = vec![format!("用户: {}", self.user_query)];
        for (i, resp) in self.assistant_responses.iter().enumerate() {
            parts.push(format!("助手[{}]: {}", i, resp));
        }
        parts.join("\n\n")
    }
}
