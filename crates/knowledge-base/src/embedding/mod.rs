//! Fastembed 嵌入引擎 — 本地 ONNX 推理，无需外部 API。
//!
//! 实现 [`EmbeddingEngine`] trait，将 fastembed 的同步 ONNX 推理
//! 包装为异步接口（通过 `spawn_blocking`）。
//!
//! # 支持的模型
//!
//! | 模型 | 维度 | 大小 | 适用场景 |
//! |------|------|------|----------|
//! | `BGEBaseZHV15` | 768 | ~400 MB | 中文优化（默认） |
//! | `BGESmallZHV15` | 512 | ~100 MB | 中文优化，轻量 |
//! | `BGELargeZHV15` | 1024 | ~1.3 GB | 最佳中文质量 |
//! | `AllMiniLML6V2Q` | 384 | ~80 MB | 英文快速 |
//! | `MultilingualE5Small` | 384 | ~120 MB | 多语言 |
//!
//! 首次使用会自动下载模型并缓存到 `~/.fastembed_cache/`。
//!
//! `BGEBaseZHV15` 不在 fastembed 的精选模型清单内，通过
//! `try_new_from_user_defined` 加载 `Xenova/bge-base-zh-v1.5` 的 ONNX
//! 导出，模型文件经 hf-hub 下载并缓存到 `~/.cache/huggingface/hub`。

use std::sync::Arc;

use crate::config::EmbeddingModelConfig;
use crate::error::KnowledgeError;
use crate::traits::EmbeddingEngine;
use fastembed::{
    EmbeddingModel as FastembedModel, InitOptions, InitOptionsUserDefined, Pooling, TextEmbedding,
    TokenizerFiles, UserDefinedEmbeddingModel,
};
use hf_hub::api::sync::Api;

// ---------------------------------------------------------------------------
// FastembedModelType
// ---------------------------------------------------------------------------

/// `Xenova/bge-base-zh-v1.5` — fastembed 精选清单未收录的中文 base 模型，
/// 经 user-defined 路径加载的 ONNX 导出仓库。
const BGE_BASE_ZH_V15_REPO: &str = "Xenova/bge-base-zh-v1.5";

/// 支持的嵌入模型类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastembedModelType {
    /// BGESmallZHV15 — 512 维，中文优化，轻量。
    BGESmallZHV15,
    /// BGEBaseZHV15 — 768 维，中文优化（默认）。
    BGEBaseZHV15,
    /// BGELargeZHV15 — 1024 维，最佳中文质量。
    BGELargeZHV15,
    /// AllMiniLML6V2Q — 384 维，英文快速。
    AllMiniLML6V2Q,
    /// MultilingualE5Small — 384 维，多语言支持。
    MultilingualE5Small,
}

impl FastembedModelType {
    fn ndims(self) -> usize {
        match self {
            FastembedModelType::BGESmallZHV15 => 512,
            FastembedModelType::BGEBaseZHV15 => 768,
            FastembedModelType::BGELargeZHV15 => 1024,
            FastembedModelType::AllMiniLML6V2Q => 384,
            FastembedModelType::MultilingualE5Small => 384,
        }
    }

    fn is_chinese_optimized(self) -> bool {
        matches!(
            self,
            FastembedModelType::BGESmallZHV15
                | FastembedModelType::BGEBaseZHV15
                | FastembedModelType::BGELargeZHV15
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

/// fastembed 内置模型的标准构造路径。
fn try_new_builtin(model: FastembedModel) -> Result<TextEmbedding, KnowledgeError> {
    TextEmbedding::try_new(InitOptions::new(model)).map_err(|e| {
        KnowledgeError::EmbeddingError(format!("Failed to initialize fastembed model: {e}"))
    })
}

/// 从 `Xenova/bge-base-zh-v1.5` 的 ONNX 导出构造嵌入模型。
///
/// fastembed 的精选模型清单未收录中文 base 变体，因此走
/// `try_new_from_user_defined`：ONNX 与 tokenizer 文件经 hf-hub
/// 下载（默认缓存于 `~/.cache/huggingface/hub`，约 400 MB）。
fn try_new_bge_base_zh() -> Result<TextEmbedding, KnowledgeError> {
    tracing::info!(
        repo = BGE_BASE_ZH_V15_REPO,
        "Loading bge-base-zh-v1.5 via user-defined path; first use downloads ~400 MB"
    );

    let api = Api::new().map_err(|e| {
        KnowledgeError::EmbeddingError(format!(
            "Failed to initialize HuggingFace API for {BGE_BASE_ZH_V15_REPO}: {e}"
        ))
    })?;
    let repo = api.model(BGE_BASE_ZH_V15_REPO.to_string());
    let read_file = |name: &str| -> Result<Vec<u8>, KnowledgeError> {
        let path = repo.get(name).map_err(|e| {
            KnowledgeError::EmbeddingError(format!(
                "Failed to fetch '{name}' from HuggingFace repo {BGE_BASE_ZH_V15_REPO}: {e}"
            ))
        })?;
        std::fs::read(&path).map_err(|e| {
            KnowledgeError::EmbeddingError(format!("Failed to read cached file {path:?}: {e}"))
        })
    };

    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read_file("tokenizer.json")?,
        config_file: read_file("config.json")?,
        special_tokens_map_file: read_file("special_tokens_map.json")?,
        tokenizer_config_file: read_file("tokenizer_config.json")?,
    };
    let model = UserDefinedEmbeddingModel::new(read_file("onnx/model.onnx")?, tokenizer_files)
        .with_pooling(Pooling::Cls);

    TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new()).map_err(|e| {
        KnowledgeError::EmbeddingError(format!(
            "Failed to initialize user-defined model {BGE_BASE_ZH_V15_REPO}: {e}"
        ))
    })
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
        let model = match model_type {
            // fastembed 精选清单未收录 bge-base-zh-v1.5，走 user-defined 加载
            FastembedModelType::BGEBaseZHV15 => try_new_bge_base_zh()?,
            FastembedModelType::BGESmallZHV15 => try_new_builtin(FastembedModel::BGESmallZHV15)?,
            FastembedModelType::BGELargeZHV15 => try_new_builtin(FastembedModel::BGELargeZHV15)?,
            FastembedModelType::AllMiniLML6V2Q => try_new_builtin(FastembedModel::AllMiniLML6V2Q)?,
            FastembedModelType::MultilingualE5Small => {
                try_new_builtin(FastembedModel::MultilingualE5Small)?
            }
        };

        let ndims = model_type.ndims();
        tracing::info!(?model_type, ndims, "Fastembed embedding engine initialized");

        Ok(Self {
            model: Arc::new(model),
            ndims,
            model_type,
        })
    }

    /// 从模型名称字符串创建引擎。
    ///
    /// 支持的名称：`"BGESmallZHV15"`、`"BGEBaseZHV15"`、`"BGELargeZHV15"`、
    /// `"AllMiniLML6V2Q"`、`"MultilingualE5Small"`。
    pub fn from_name(name: &str) -> Result<Self, KnowledgeError> {
        let model_type = match name {
            "BGESmallZHV15" => FastembedModelType::BGESmallZHV15,
            "BGEBaseZHV15" => FastembedModelType::BGEBaseZHV15,
            "BGELargeZHV15" => FastembedModelType::BGELargeZHV15,
            "AllMiniLML6V2Q" => FastembedModelType::AllMiniLML6V2Q,
            "MultilingualE5Small" => FastembedModelType::MultilingualE5Small,
            other => {
                return Err(KnowledgeError::InvalidInput(format!(
                    "Unknown embedding model: {other}. Supported: BGESmallZHV15, BGEBaseZHV15, BGELargeZHV15, AllMiniLML6V2Q, MultilingualE5Small"
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
                768 => FastembedModelType::BGEBaseZHV15,
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
                .map_err(|e| {
                    KnowledgeError::EmbeddingError(format!("spawn_blocking failed: {e}"))
                })?;

        match result {
            Ok(mut vecs) => {
                let v = vecs.pop().unwrap_or_default();
                Ok(v)
            }
            Err(e) => Err(KnowledgeError::EmbeddingError(format!(
                "Failed to embed query: {e}"
            ))),
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
        .map_err(|e| KnowledgeError::EmbeddingError(format!("spawn_blocking failed: {e}")))?;

        result.map_err(|e| KnowledgeError::EmbeddingError(format!("Failed to embed batch: {e}")))
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
        assert_eq!(FastembedModelType::BGEBaseZHV15.ndims(), 768);
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
            FastembedModelType::BGEBaseZHV15
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
    fn from_name_unknown_mentions_bge_base() {
        let err = FastembedEngine::from_name("Nope")
            .err()
            .expect("unknown model should fail")
            .to_string();
        assert!(err.contains("BGEBaseZHV15"));
    }

    /// 真实构造 base 引擎需要从 HuggingFace 下载 ~400 MB 模型，
    /// 仅在本地验证时手动运行。
    #[tokio::test]
    #[ignore = "downloads ~400 MB model on first run"]
    async fn bge_base_zh_engine_works() {
        use crate::traits::EmbeddingEngine;

        let engine = FastembedEngine::from_name("BGEBaseZHV15").unwrap();
        assert_eq!(engine.ndims(), 768);
        assert_eq!(engine.model_type(), FastembedModelType::BGEBaseZHV15);

        // 语义相似度健全性检查 — 验证 user-defined 路径的 CLS pooling 正确
        let query = "如何用 Rust 编写异步网络服务";
        let related = "Tokio 是 Rust 的异步运行时，适合编写高性能网络程序";
        let unrelated = "今天中午吃了红烧牛肉面";
        let (q, r, u) = tokio::join!(
            engine.embed_query(query),
            engine.embed_query(related),
            engine.embed_query(unrelated),
        );
        let (q, r, u) = (q.unwrap(), r.unwrap(), u.unwrap());
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let sim_related = dot(&q, &r);
        let sim_unrelated = dot(&q, &u);
        assert!(
            sim_related > sim_unrelated,
            "related ({sim_related}) should outrank unrelated ({sim_unrelated})"
        );
    }

    /// 同上 — from_config 的 768 维映射需构造真实引擎。
    #[test]
    #[ignore = "downloads ~400 MB model on first run"]
    fn from_config_base_zh() {
        let config = EmbeddingModelConfig::bge_base_zh();
        let engine = FastembedEngine::from_config(&config).unwrap();
        assert_eq!(engine.ndims(), 768);
        assert_eq!(engine.model_type(), FastembedModelType::BGEBaseZHV15);
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
