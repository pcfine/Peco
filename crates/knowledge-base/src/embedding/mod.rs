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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
// 进程级模型缓存
// ---------------------------------------------------------------------------

/// 模型类型 → 已加载模型。一个进程只持有每种模型的一份。
type ModelCache = Mutex<HashMap<FastembedModelType, Arc<TextEmbedding>>>;

/// 空间记忆库按空间各建一份，若每次 `FastembedEngine::new` 都重新加载，
/// N 个空间就是 N 份 bge 常驻（每份数百 MB）。缓存键是模型类型，因此
/// 量化模型与全精度模型可以共存，不会互相顶掉。
static ENGINES: OnceLock<ModelCache> = OnceLock::new();

fn engines() -> &'static ModelCache {
    ENGINES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取缓存中的模型；缺失时调用 `load` 加载并写回。
///
/// 加载失败**不写缓存**：下载被墙或磁盘写满是一次性故障，缓存这个失败会让
/// 整个进程再也建不出知识库。
///
/// 加载期间持锁 —— 多空间并发首次加载时只有一次真正加载，其余等待复用，
/// 顺带消除两个 KB 同时初始化抢同一份模型缓存导致的偶发失败。
///
/// `V` 泛型化只服务于测试：计数与失败重试的断言不该为了跑一次用例去下载模型。
fn cached_model<V, F>(
    cache: &Mutex<HashMap<FastembedModelType, Arc<V>>>,
    model_type: FastembedModelType,
    load: F,
) -> Result<Arc<V>, KnowledgeError>
where
    F: FnOnce() -> Result<V, KnowledgeError>,
{
    // 持锁期间若 `load` panic，锁会带毒；缓存内容本身仍然自洽，继续用。
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(model) = guard.get(&model_type) {
        return Ok(Arc::clone(model));
    }
    let model = Arc::new(load()?);
    guard.insert(model_type, Arc::clone(&model));
    Ok(model)
}

/// 真正加载模型（下载 / 反序列化），只在缓存缺失时执行。
fn load_model(model_type: FastembedModelType) -> Result<TextEmbedding, KnowledgeError> {
    match model_type {
        // fastembed 精选清单未收录 bge-base-zh-v1.5，走 user-defined 加载
        FastembedModelType::BGEBaseZHV15 => try_new_bge_base_zh(),
        FastembedModelType::BGESmallZHV15 => try_new_builtin(FastembedModel::BGESmallZHV15),
        FastembedModelType::BGELargeZHV15 => try_new_builtin(FastembedModel::BGELargeZHV15),
        FastembedModelType::AllMiniLML6V2Q => try_new_builtin(FastembedModel::AllMiniLML6V2Q),
        FastembedModelType::MultilingualE5Small => {
            try_new_builtin(FastembedModel::MultilingualE5Small)
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
    /// 引擎按模型类型在进程内共享：同一模型类型重复构造只加载一次。
    /// 首次调用会从 HuggingFace 下载模型并缓存到本地。
    pub fn new(model_type: FastembedModelType) -> Result<Self, KnowledgeError> {
        let ndims = model_type.ndims();
        let model = cached_model(engines(), model_type, || {
            let model = load_model(model_type)?;
            tracing::info!(?model_type, ndims, "Fastembed embedding engine initialized");
            Ok(model)
        })?;

        Ok(Self {
            model,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 同一模型类型只加载一次：计数断言不引入第二个真模型。
    ///
    /// 用 `cached_model` 的泛型值类型做计数替身 —— 直接对 `FastembedEngine::new`
    /// 计数就得为用例下载模型，无网环境必红。
    #[test]
    fn same_model_type_loads_once() {
        let cache: Mutex<HashMap<FastembedModelType, Arc<usize>>> = Mutex::new(HashMap::new());
        let loads = AtomicUsize::new(0);

        let first = cached_model(&cache, FastembedModelType::BGESmallZHV15, || {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(1usize)
        })
        .expect("首次加载应成功");
        let second = cached_model(&cache, FastembedModelType::BGESmallZHV15, || {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(2usize)
        })
        .expect("命中缓存应成功");

        assert_eq!(loads.load(Ordering::SeqCst), 1, "同一模型类型不得重复加载");
        assert!(Arc::ptr_eq(&first, &second), "两次构造必须共享同一份模型");

        // 换模型类型才允许再加载一次
        let other = cached_model(&cache, FastembedModelType::BGELargeZHV15, || {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(3usize)
        })
        .expect("另一模型类型应加载");
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert_eq!(*other, 3);
    }

    /// 加载失败不写缓存：下次仍会重试，而不是把一次性故障固化成永久失败。
    #[test]
    fn failed_load_is_not_cached() {
        let cache: Mutex<HashMap<FastembedModelType, Arc<u8>>> = Mutex::new(HashMap::new());

        let err = cached_model(&cache, FastembedModelType::MultilingualE5Small, || {
            Err(KnowledgeError::EmbeddingError("模型下载失败".into()))
        })
        .expect_err("加载失败应返回错误");
        assert!(err.to_string().contains("模型下载失败"));
        assert!(cache.lock().unwrap().is_empty(), "失败结果不得进缓存");

        let ok = cached_model(&cache, FastembedModelType::MultilingualE5Small, || Ok(7u8))
            .expect("重试应成功");
        assert_eq!(*ok, 7);
    }

    /// 真实的 `FastembedEngine::new` 也必须共享同一份模型实例。
    ///
    /// 依赖本地模型缓存；取不到模型（无网且无缓存）时跳过，避免环境相关的红灯。
    #[test]
    fn engine_instances_share_one_model() {
        let (first, second) = match (
            FastembedEngine::new(FastembedModelType::AllMiniLML6V2Q),
            FastembedEngine::new(FastembedModelType::AllMiniLML6V2Q),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return,
        };
        assert!(
            Arc::ptr_eq(&first.model, &second.model),
            "同一模型类型的引擎必须复用同一份 TextEmbedding"
        );
        assert_eq!(first.ndims(), 384);
        assert_eq!(second.model_type(), FastembedModelType::AllMiniLML6V2Q);
    }

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
