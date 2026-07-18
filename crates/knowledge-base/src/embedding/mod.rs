//! Fastembed 嵌入引擎 — 本地 ONNX 推理，无需外部 API。
//!
//! 实现 [`EmbeddingEngine`] trait，将 fastembed 的同步 ONNX 推理
//! 包装为异步接口（通过 `spawn_blocking`）。
//!
//! # 支持的模型
//!
//! | 模型 | 维度 | 大小 | 适用场景 |
//! |------|------|------|----------|
//! | `BGESmallZHV15` | 512 | ~100 MB | 中文优化（默认） |
//! | `BGELargeZHV15` | 1024 | ~1.3 GB | 最佳中文质量 |
//! | `AllMiniLML6V2Q` | 384 | ~80 MB | 英文快速 |
//! | `MultilingualE5Small` | 384 | ~120 MB | 多语言 |
//!
//! 首次使用会自动下载模型并缓存到 `~/.fastembed_cache/`。

use std::sync::Arc;

use crate::config::EmbeddingModelConfig;
use crate::error::KnowledgeError;
use crate::traits::EmbeddingEngine;
use fastembed::{EmbeddingModel as FastembedModel, InitOptions, TextEmbedding};

// ---------------------------------------------------------------------------
// FastembedModelType
// ---------------------------------------------------------------------------

/// 支持的嵌入模型类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastembedModelType {
    /// BGESmallZHV15 — 512 维，中文优化（默认）。
    BGESmallZHV15,
    /// BGELargeZHV15 — 1024 维，最佳中文质量。
    BGELargeZHV15,
    /// AllMiniLML6V2Q — 384 维，英文快速。
    AllMiniLML6V2Q,
    /// MultilingualE5Small — 384 维，多语言支持。
    MultilingualE5Small,
}

impl FastembedModelType {
    fn to_fastembed_model(self) -> FastembedModel {
        match self {
            FastembedModelType::BGESmallZHV15 => FastembedModel::BGESmallZHV15,
            FastembedModelType::BGELargeZHV15 => FastembedModel::BGELargeZHV15,
            FastembedModelType::AllMiniLML6V2Q => FastembedModel::AllMiniLML6V2Q,
            FastembedModelType::MultilingualE5Small => FastembedModel::MultilingualE5Small,
        }
    }

    fn ndims(self) -> usize {
        match self {
            FastembedModelType::BGESmallZHV15 => 512,
            FastembedModelType::BGELargeZHV15 => 1024,
            FastembedModelType::AllMiniLML6V2Q => 384,
            FastembedModelType::MultilingualE5Small => 384,
        }
    }

    fn is_chinese_optimized(self) -> bool {
        matches!(
            self,
            FastembedModelType::BGESmallZHV15 | FastembedModelType::BGELargeZHV15
        )
    }

    fn query_instruction(self) -> Option<&'static str> {
        if self.is_chinese_optimized() {
            Some("为这个句子生成表示以用于检索相关文章：")
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// FastembedEngine
// ---------------------------------------------------------------------------

/// Fastembed 嵌入引擎 — 实现 [`EmbeddingEngine`] trait。
///
/// # Example
///
/// ```ignore
/// use knowledge_base::embedding::{FastembedEngine, FastembedModelType};
/// use knowledge_base::traits::EmbeddingEngine;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = FastembedEngine::new(FastembedModelType::BGESmallZHV15)?;
/// let query_vec = engine.embed_query("Rust 异步编程").await?;
/// assert_eq!(query_vec.len(), 512);
/// # Ok(())
/// # }
/// ```
pub struct FastembedEngine {
    model: Arc<TextEmbedding>,
    ndims: usize,
    model_type: FastembedModelType,
}

impl FastembedEngine {
    /// 使用指定模型类型初始化嵌入引擎。
    ///
    /// 首次调用会从 HuggingFace 下载模型并缓存到本地。
    pub fn new(model_type: FastembedModelType) -> Result<Self, KnowledgeError> {
        let fastembed_model = model_type.to_fastembed_model();
        let model = TextEmbedding::try_new(InitOptions::new(fastembed_model)).map_err(|e| {
            KnowledgeError::EmbeddingError(format!("无法初始化 fastembed 模型: {e}"))
        })?;

        let ndims = model_type.ndims();
        tracing::info!(?model_type, ndims, "Fastembed 嵌入引擎初始化成功");

        Ok(Self {
            model: Arc::new(model),
            ndims,
            model_type,
        })
    }

    /// 从模型名称字符串创建引擎。
    ///
    /// 支持的名称：`"BGESmallZHV15"`、`"BGELargeZHV15"`、
    /// `"AllMiniLML6V2Q"`、`"MultilingualE5Small"`。
    pub fn from_name(name: &str) -> Result<Self, KnowledgeError> {
        let model_type = match name {
            "BGESmallZHV15" => FastembedModelType::BGESmallZHV15,
            "BGELargeZHV15" => FastembedModelType::BGELargeZHV15,
            "AllMiniLML6V2Q" => FastembedModelType::AllMiniLML6V2Q,
            "MultilingualE5Small" => FastembedModelType::MultilingualE5Small,
            other => {
                return Err(KnowledgeError::InvalidInput(format!(
                    "未知嵌入模型: {other}。支持: BGESmallZHV15, BGELargeZHV15, AllMiniLML6V2Q, MultilingualE5Small"
                )));
            }
        };
        Self::new(model_type)
    }

    /// 从 [`EmbeddingModelConfig`] 创建引擎。
    pub fn from_config(config: &EmbeddingModelConfig) -> Result<Self, KnowledgeError> {
        let model_type = if config.chinese_optimized {
            match config.ndims {
                1024 => FastembedModelType::BGELargeZHV15,
                _ => FastembedModelType::BGESmallZHV15,
            }
        } else {
            FastembedModelType::AllMiniLML6V2Q
        };
        Self::new(model_type)
    }

    /// 返回模型类型。
    pub fn model_type(&self) -> FastembedModelType {
        self.model_type
    }

    fn query_text(&self, text: &str) -> String {
        if let Some(instr) = self.model_type.query_instruction() {
            format!("{instr}{text}")
        } else {
            text.to_string()
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingEngine for FastembedEngine {
    fn ndims(&self) -> usize {
        self.ndims
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, KnowledgeError> {
        let query_text = self.query_text(text);
        let model = self.model.clone();
        let result =
            tokio::task::spawn_blocking(move || model.embed(vec![query_text.as_str()], None))
                .await
                .map_err(|e| KnowledgeError::EmbeddingError(format!("spawn_blocking 失败: {e}")))?;

        match result {
            Ok(mut vecs) => {
                let v = vecs.pop().unwrap_or_default();
                Ok(v)
            }
            Err(e) => Err(KnowledgeError::EmbeddingError(format!("查询嵌入失败: {e}"))),
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let model = self.model.clone();

        let result = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            model.embed(refs, None)
        })
        .await
        .map_err(|e| KnowledgeError::EmbeddingError(format!("spawn_blocking 失败: {e}")))?;

        result.map_err(|e| KnowledgeError::EmbeddingError(format!("批量嵌入失败: {e}")))
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_type_ndims() {
        assert_eq!(FastembedModelType::BGESmallZHV15.ndims(), 512);
        assert_eq!(FastembedModelType::BGELargeZHV15.ndims(), 1024);
        assert_eq!(FastembedModelType::AllMiniLML6V2Q.ndims(), 384);
        assert_eq!(FastembedModelType::MultilingualE5Small.ndims(), 384);
    }

    #[test]
    fn chinese_models_have_query_instruction() {
        assert!(
            FastembedModelType::BGESmallZHV15
                .query_instruction()
                .is_some()
        );
        assert!(
            FastembedModelType::BGELargeZHV15
                .query_instruction()
                .is_some()
        );
        assert!(
            FastembedModelType::AllMiniLML6V2Q
                .query_instruction()
                .is_none()
        );
    }

    #[test]
    fn from_name_valid() {
        assert!(FastembedEngine::from_name("BGESmallZHV15").is_ok());
        assert!(FastembedEngine::from_name("UnknownModel").is_err());
    }

    #[test]
    fn from_config_chinese() {
        let config = EmbeddingModelConfig::bge_large_zh();
        let engine = FastembedEngine::from_config(&config).unwrap();
        assert_eq!(engine.ndims(), 1024);
        assert_eq!(engine.model_type(), FastembedModelType::BGELargeZHV15);
    }

    #[test]
    fn from_config_english() {
        let config = EmbeddingModelConfig::default_english();
        let engine = FastembedEngine::from_config(&config).unwrap();
        assert_eq!(engine.ndims(), 384);
        assert_eq!(engine.model_type(), FastembedModelType::AllMiniLML6V2Q);
    }
}
