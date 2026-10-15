pub mod backends;
pub mod calibration;
pub mod chunking;
pub mod config;
pub mod embedding;
pub mod engine;
pub mod error;
pub mod graph;
pub mod manager;
pub mod parsers;
pub mod traits;
pub mod types;

// 核心类型
pub use types::*;

// 隔离维度与图谱节点 label 的单源定义 —— 下游（如 peco-core）直接复用，避免各写一份
pub use types::{DEFAULT_SCOPE, ENTITY_NODE_LABEL, namespace_for};

// 错误
pub use error::KnowledgeError;

// 配置
pub use config::EmbeddingModelConfig;

// 嵌入引擎
pub use embedding::{FastembedEngine, FastembedModelType};

// 文档解析
pub use parsers::{
    DocumentFormat, DocumentParser, ParsedDocument, clean_text, make_parser,
    make_parser_for_format, parse_to_document,
};

// Traits
pub use traits::{
    Chunker, ChunkingStrategy, CombinedQuery, CombinedSearch, DocumentStore, EdgeType,
    EmbeddingEngine, FullTextEntry, FullTextHit, FullTextIndex, GraphNode, GraphStore,
    KnowledgeEdge, RrfConfig, TraversalDirection, TraversalStep, VectorEntry, VectorHit,
    VectorIndex,
};

// 引擎
pub use engine::{
    AdaptiveFusionConfig, BackendCapabilities, CrossValidation, HybridSearchEngine,
    IngestionPipeline, PathCalibration, QueryAnalysis, QueryAnalyzer, QueryIntent, QueryLength,
    QueryRouter, RuleBasedAnalyzer, adaptive_fusion_config, calibrate_path, query_adjusted_weights,
    validate_signals,
};

// 图
pub use graph::KnowledgeGraphBuilder;

// 后端
pub use backends::memory::InMemoryBackend;
pub use backends::memory_graph::MemoryGraphStore;

#[cfg(feature = "lancedb")]
pub use backends::lancedb::LanceDbBackend;

#[cfg(feature = "helixdb")]
pub use backends::helixdb::{HelixDbBackend, HelixSchema};

// 知识库管理器
pub use manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig, KbInfo,
};
pub use manager::{KnowledgeBase, KnowledgeBaseManager};

// ---------------------------------------------------------------------------
// 公共工具函数
// ---------------------------------------------------------------------------

/// 将知识库名称清洗为安全的内部标识符（目录名 / 表名）。
///
/// 只保留 ASCII 字母数字、`_`、`-`、`.`，其他字符替换为 `_`。
/// 若清洗后为空（全中文等），使用原始名称的 UTF-8 hex 摘要作为回退。
///
/// # 示例
///
/// ```
/// use knowledge_base::sanitize_kb_name;
/// assert_eq!(sanitize_kb_name("my-kb"), "my-kb");
/// assert_eq!(sanitize_kb_name("个人档案"), "kb_e4b8aae4babae6a1a3e6a188");
/// assert_eq!(sanitize_kb_name(""), "kb_default");
/// ```
///
/// 清洗后的名称仅用于内部存储，**不用于对外展示**。
/// 对外展示应使用 [`KbConfig::name`] 原始字段。
pub fn sanitize_kb_name(name: &str) -> String {
    let c: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = c.trim_matches('_').trim_matches('-').trim_matches('.');
    if trimmed.is_empty() {
        if name.is_empty() {
            return "kb_default".into();
        }
        // 名称全是非 ASCII 字符（如中文），用 hex 摘要避免冲突
        let hex_id: String = name
            .bytes()
            .take(16)
            .map(|b| format!("{:02x}", b))
            .collect();
        format!("kb_{hex_id}")
    } else {
        trimmed.to_string()
    }
}

/// 校验知识库名称：1..=64 字符，首末为 ASCII 字母/数字，其余仅 `[A-Za-z0-9._-]`，
/// `@` 开头保留给系统库。通过校验的名称满足 `sanitize_kb_name(name) == name`。
/// `Err` 为面向用户的英文说明。
pub fn validate_kb_name(name: &str) -> Result<(), String> {
    const MAX_CHARS: usize = 64;

    if name.is_empty() {
        return Err("Knowledge base name is required".into());
    }
    if name.chars().count() > MAX_CHARS {
        return Err(format!(
            "Knowledge base name must be at most {MAX_CHARS} characters"
        ));
    }
    if name.starts_with('@') {
        return Err(
            "Knowledge base name must not start with '@' (reserved for system knowledge bases)"
                .into(),
        );
    }

    let ends_are_alphanumeric = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    let chars_are_allowed = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ends_are_alphanumeric || !chars_are_allowed {
        return Err(
            "Knowledge base name must start and end with a letter or digit and may only contain letters, digits, '.', '_' or '-'"
                .into(),
        );
    }

    // 清洗后须原样保留，否则会与既有库撞同一目录
    if sanitize_kb_name(name) != name {
        return Err("Knowledge base name must remain unchanged after sanitizing".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_kb_name_accepts_names_that_survive_sanitizing() {
        for name in ["my-kb", "kb_01", "a.b-c", "A1", "a.b", "a1-b2"] {
            assert!(validate_kb_name(name).is_ok(), "{name:?} 应通过校验");
            assert_eq!(sanitize_kb_name(name), name);
        }
    }

    #[test]
    fn validate_kb_name_rejects_invalid_names() {
        let overlong = "a".repeat(65);
        for name in [
            "",
            "技术文档",
            "@x",
            "a b",
            ".hidden",
            "-x",
            "x@y",
            "a.",
            "a-",
            "a_",
            "a._-",
            "___",
            overlong.as_str(),
        ] {
            assert!(validate_kb_name(name).is_err(), "{name:?} 应被拒绝");
        }
    }

    #[test]
    fn validate_kb_name_messages_are_user_facing() {
        let reserved = validate_kb_name("@private_memory").unwrap_err();
        assert!(reserved.contains('@'));

        let invalid = validate_kb_name("技术文档").unwrap_err();
        assert!(invalid.contains("letters"));
    }
}
