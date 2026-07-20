// ============================================================================
// PpaConfig — PPA 全量配置
// ============================================================================
//
// NOTE: This module defines the full PPA configuration schema. Many items are
// currently only consumed by peco-server or planned for future use.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// PPA 总配置。
///
/// 在 peco-server 构建 LooperConfig 前从配置文件或环境变量加载，
/// 注入到 PpaDynamicContext 和 PpaMemoryHook 中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PpaConfig {
    /// 是否启用 PPA 模块。
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// 记忆分析配置。
    #[serde(default)]
    pub analyzer: AnalyzerConfig,

    /// 记忆检索配置。
    #[serde(default)]
    pub retrieval: RetrievalConfig,

    /// 存储配置。
    #[serde(default)]
    pub storage: StorageConfig,
}

impl Default for PpaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            analyzer: AnalyzerConfig::default(),
            retrieval: RetrievalConfig::default(),
            storage: StorageConfig::default(),
        }
    }
}

// ============================================================================
// AnalyzerConfig
// ============================================================================

/// 记忆分析器配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerConfig {
    /// 分析用的轻量模型名称。
    #[serde(default = "default_analyzer_model")]
    pub model: String,

    /// 最小分析字符数（过短对话不触发分析）。
    #[serde(default = "default_min_turn_chars")]
    pub min_turn_chars: usize,

    /// 每隔 N 轮分析一次（1 = 每轮都分析）。
    #[serde(default = "default_analyze_interval")]
    pub analyze_interval: usize,

    /// 分析超时（秒）。
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// 每轮最多提取的记忆数。
    #[serde(default = "default_max_facts_per_turn")]
    pub max_facts_per_turn: usize,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        Self {
            model: default_analyzer_model(),
            min_turn_chars: default_min_turn_chars(),
            analyze_interval: default_analyze_interval(),
            timeout_secs: default_timeout_secs(),
            max_facts_per_turn: default_max_facts_per_turn(),
        }
    }
}

fn default_analyzer_model() -> String {
    "deepseek-v4-flash".to_string()
}
fn default_min_turn_chars() -> usize {
    50
}
fn default_analyze_interval() -> usize {
    1
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_max_facts_per_turn() -> usize {
    5
}

// ============================================================================
// RetrievalConfig
// ============================================================================

/// 记忆检索配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// 是否启用自动检索（关闭后仅 Tool `recall` 可用）。
    #[serde(default = "default_auto_retrieve")]
    pub auto_retrieve: bool,

    /// Semantic 记忆检索数量。
    #[serde(default = "default_semantic_top_k")]
    pub semantic_top_k: usize,

    /// Episodic 记忆检索数量。
    #[serde(default = "default_episodic_top_k")]
    pub episodic_top_k: usize,

    /// Profile 是否始终注入。
    #[serde(default = "default_profile_always")]
    pub profile_always: bool,

    /// 最低相关度阈值（0.0 ~ 1.0）。
    #[serde(default = "default_min_relevance_score")]
    pub min_relevance_score: f32,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            auto_retrieve: default_auto_retrieve(),
            semantic_top_k: default_semantic_top_k(),
            episodic_top_k: default_episodic_top_k(),
            profile_always: default_profile_always(),
            min_relevance_score: default_min_relevance_score(),
        }
    }
}

fn default_auto_retrieve() -> bool {
    true
}
fn default_semantic_top_k() -> usize {
    3
}
fn default_episodic_top_k() -> usize {
    2
}
fn default_profile_always() -> bool {
    true
}
fn default_min_relevance_score() -> f32 {
    0.6
}

// ============================================================================
// StorageConfig
// ============================================================================

/// 存储配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Semantic 记忆数量上限（超过后 LRU 淘汰）。
    #[serde(default = "default_max_semantic_memories")]
    pub max_semantic_memories: usize,

    /// Episodic 摘要数量上限。
    #[serde(default = "default_max_episodic_summaries")]
    pub max_episodic_summaries: usize,

    /// 每 N 轮对话后触发摘要压缩。
    #[serde(default = "default_auto_summarize_turns")]
    pub auto_summarize_turns: usize,

    /// 时间衰减半衰期（天），0 = 不衰减。
    #[serde(default = "default_time_decay_half_life_days")]
    pub time_decay_half_life_days: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_semantic_memories: default_max_semantic_memories(),
            max_episodic_summaries: default_max_episodic_summaries(),
            auto_summarize_turns: default_auto_summarize_turns(),
            time_decay_half_life_days: default_time_decay_half_life_days(),
        }
    }
}

fn default_max_semantic_memories() -> usize {
    500
}
fn default_max_episodic_summaries() -> usize {
    50
}
fn default_auto_summarize_turns() -> usize {
    10
}
fn default_time_decay_half_life_days() -> u32 {
    30
}

fn default_enabled() -> bool {
    true
}
