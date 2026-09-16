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
    /// 部署级 HelixDB 端点（仅 `BackendType::HelixDb` 使用）。
    ///
    /// `None` 或空串时回退：环境变量 `PECO_KB_HELIX_URL` →
    /// `http://localhost:6970`（见 [`crate::manager::knowledge_base`] 的解析顺序）。
    #[serde(default)]
    pub helix_url: Option<String>,
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
    BGEBaseZHV15,
    BGELargeZHV15,
    AllMiniLML6V2Q,
    MultilingualE5Small,
}

impl From<FastembedModelTypeSerde> for FastembedModelType {
    fn from(s: FastembedModelTypeSerde) -> Self {
        match s {
            FastembedModelTypeSerde::BGESmallZHV15 => FastembedModelType::BGESmallZHV15,
            FastembedModelTypeSerde::BGEBaseZHV15 => FastembedModelType::BGEBaseZHV15,
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
            FastembedModelType::BGEBaseZHV15 => FastembedModelTypeSerde::BGEBaseZHV15,
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

    /// kb_config.json 的磁盘契约 — 既有文件使用 kebab-case 序列化形式，
    /// 变体名与 JSON 字符串必须保持向后兼容。
    #[test]
    fn embedding_model_serde_roundtrip() {
        let cases = [
            (
                FastembedModelTypeSerde::BGESmallZHV15,
                "\"b-g-e-small-z-h-v15\"",
            ),
            (
                FastembedModelTypeSerde::BGEBaseZHV15,
                "\"b-g-e-base-z-h-v15\"",
            ),
            (
                FastembedModelTypeSerde::BGELargeZHV15,
                "\"b-g-e-large-z-h-v15\"",
            ),
            (
                FastembedModelTypeSerde::AllMiniLML6V2Q,
                "\"all-mini-l-m-l6-v2-q\"",
            ),
            (
                FastembedModelTypeSerde::MultilingualE5Small,
                "\"multilingual-e5-small\"",
            ),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let round: FastembedModelTypeSerde = serde_json::from_str(&json).unwrap();
            let model_type: FastembedModelType = round.into();
            let back: FastembedModelTypeSerde = model_type.into();
            assert_eq!(serde_json::to_string(&back).unwrap(), expected);
        }
    }

    /// 既有 kb_config.json（无 `helix_url` 字段）必须继续解析 —
    /// `#[serde(default)]` 保证旧配置向后兼容。
    #[test]
    fn kb_config_without_helix_url_parses() {
        let json = r#"{
            "name": "@private_memory",
            "description": "个人记忆知识库",
            "embedding_model": "b-g-e-base-z-h-v15",
            "chunking": {"type": "overlapping-window", "size": 800, "overlap": 200},
            "backend": "InMemory",
            "storage_path": null,
            "default_storage_mode": "full"
        }"#;
        let config: KbConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.helix_url, None);
    }

    /// `"backend": "HelixDb"` 的 kb_config.json 可解析，且 `helix_url`
    /// 缺省为 `None`（由 build 阶段的环境变量/默认值回退补齐）。
    #[cfg(feature = "helixdb")]
    #[test]
    fn helix_backend_config_parses() {
        let json = r#"{
            "name": "@private_memory",
            "description": "个人记忆知识库",
            "embedding_model": "b-g-e-base-z-h-v15",
            "chunking": {"type": "overlapping-window", "size": 800, "overlap": 200},
            "backend": "HelixDb",
            "storage_path": null,
            "default_storage_mode": "full"
        }"#;
        let config: KbConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend, BackendType::HelixDb);
        assert_eq!(config.helix_url, None);
    }

    /// `helix_url` 显式配置时按原值保留并可序列化 roundtrip。
    #[cfg(feature = "helixdb")]
    #[test]
    fn helix_url_roundtrip() {
        let json = r#"{
            "name": "@private_memory",
            "description": "个人记忆知识库",
            "embedding_model": "b-g-e-base-z-h-v15",
            "chunking": {"type": "overlapping-window", "size": 800, "overlap": 200},
            "backend": "HelixDb",
            "storage_path": null,
            "default_storage_mode": "full",
            "helix_url": "http://helix.internal:6970"
        }"#;
        let config: KbConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.helix_url.as_deref(),
            Some("http://helix.internal:6970")
        );

        let serialized = serde_json::to_string(&config).unwrap();
        let round: KbConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            round.helix_url.as_deref(),
            Some("http://helix.internal:6970")
        );
    }
}
