//! 知识库管理器配置类型。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::embedding::FastembedModelType;
use crate::traits::ChunkingStrategy;
use crate::types::StorageMode;

// ---------------------------------------------------------------------------
// KbConfig
// ---------------------------------------------------------------------------

/// 单个知识库的完整配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbConfig {
    /// 知识库名称（唯一标识）。
    pub name: String,
    /// 描述信息。
    #[serde(default)]
    pub description: String,
    /// 嵌入模型类型。
    pub embedding_model: FastembedModelTypeSerde,
    /// 分块策略。
    pub chunking: ChunkingStrategySerde,
    /// 存储后端类型。
    pub backend: BackendType,
    /// 存储路径（LanceDB 需要，其他后端可选）。
    #[serde(default)]
    pub storage_path: Option<PathBuf>,
    /// 默认存储模式（默认为 Full）。
    #[serde(default)]
    pub default_storage_mode: StorageMode,
}

/// 后端类型（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackendType {
    /// 内存后端（测试/临时使用）。
    InMemory,
    /// LanceDB 本地持久化后端。
    #[cfg(feature = "lancedb")]
    LanceDb,
    /// HelixDB 后端（需 helixdb feature）。
    #[cfg(feature = "helixdb")]
    HelixDb,
}

// ---------------------------------------------------------------------------
// 可序列化的配置辅助类型
// ---------------------------------------------------------------------------

/// FastembedModelType 的可序列化版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FastembedModelTypeSerde {
    BGESmallZHV15,
    BGELargeZHV15,
    AllMiniLML6V2Q,
    MultilingualE5Small,
}

impl From<FastembedModelTypeSerde> for FastembedModelType {
    fn from(s: FastembedModelTypeSerde) -> Self {
        match s {
            FastembedModelTypeSerde::BGESmallZHV15 => FastembedModelType::BGESmallZHV15,
            FastembedModelTypeSerde::BGELargeZHV15 => FastembedModelType::BGELargeZHV15,
            FastembedModelTypeSerde::AllMiniLML6V2Q => FastembedModelType::AllMiniLML6V2Q,
            FastembedModelTypeSerde::MultilingualE5Small => FastembedModelType::MultilingualE5Small,
        }
    }
}

impl From<FastembedModelType> for FastembedModelTypeSerde {
    fn from(m: FastembedModelType) -> Self {
        match m {
            FastembedModelType::BGESmallZHV15 => FastembedModelTypeSerde::BGESmallZHV15,
            FastembedModelType::BGELargeZHV15 => FastembedModelTypeSerde::BGELargeZHV15,
            FastembedModelType::AllMiniLML6V2Q => FastembedModelTypeSerde::AllMiniLML6V2Q,
            FastembedModelType::MultilingualE5Small => FastembedModelTypeSerde::MultilingualE5Small,
        }
    }
}

/// ChunkingStrategy 的可序列化版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChunkingStrategySerde {
    OverlappingWindow { size: usize, overlap: usize },
    FixedSize { size: usize },
    SentenceBased { max_chars: usize },
}

impl Default for ChunkingStrategySerde {
    fn default() -> Self {
        ChunkingStrategySerde::OverlappingWindow {
            size: 800,
            overlap: 200,
        }
    }
}

impl From<ChunkingStrategySerde> for ChunkingStrategy {
    fn from(s: ChunkingStrategySerde) -> Self {
        match s {
            ChunkingStrategySerde::OverlappingWindow { size, overlap } => {
                ChunkingStrategy::OverlappingWindow { size, overlap }
            }
            ChunkingStrategySerde::FixedSize { size } => ChunkingStrategy::FixedSize { size },
            ChunkingStrategySerde::SentenceBased { max_chars } => {
                ChunkingStrategy::SentenceBased { max_chars }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// KbInfo
// ---------------------------------------------------------------------------

/// 知识库摘要信息（用于列表展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbInfo {
    pub name: String,
    pub description: String,
    pub backend: String,
    pub embedding_model: String,
    pub document_count: usize,
    pub chunk_count: usize,
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chunking_strategy() {
        let s = ChunkingStrategySerde::default();
        assert!(matches!(s, ChunkingStrategySerde::OverlappingWindow { .. }));
    }

    #[test]
    fn chunking_serde_roundtrip() {
        let json = r#"{"type":"overlapping-window","size":500,"overlap":100}"#;
        let cs: ChunkingStrategySerde = serde_json::from_str(json).unwrap();
        let strategy: ChunkingStrategy = cs.into();
        assert!(matches!(
            strategy,
            ChunkingStrategy::OverlappingWindow {
                size: 500,
                overlap: 100
            }
        ));
    }
}
