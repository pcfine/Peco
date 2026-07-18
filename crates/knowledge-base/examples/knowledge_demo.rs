//! Knowledge module demo — text ingestion + hybrid search.
//!
//! Reads PDF or plain-text files from `../../example/`, ingests them into the
//! knowledge base (InMemory backend), then runs queries.
//!
//! Usage:
//!   cargo run --example knowledge_demo
//!   cargo run --example knowledge_demo -- --text  # use built-in demo text

use std::fs;
use std::path::Path;
use std::sync::Arc;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use knowledge_base::backends::memory::InMemoryBackend;
use knowledge_base::chunking::make_chunker;
use knowledge_base::engine::{
    HybridSearchEngine, IngestionPipeline, query_analysis::RuleBasedAnalyzer,
    query_router::RuleBasedRouter,
};
use knowledge_base::error::KnowledgeError;
use knowledge_base::traits::*;
use knowledge_base::types::*;

// ---------------------------------------------------------------------------
// Fastembed-based embedding engine (Chinese-optimised)
// ---------------------------------------------------------------------------

/// Wraps a [`fastembed::TextEmbedding`] model to implement the project's
/// [`EmbeddingEngine`] trait.
///
/// Uses `BGELargeZHV15` — BAAI BGE large Chinese model (1024‑dim vectors).
/// If `BGEM3` becomes available in a future fastembed release, switch to it
/// for best‑in‑class multilingual retrieval with native sparse/dense support.
/// ONNX inference runs on CPU; the first call downloads the model from
/// HuggingFace (~1.3 GB) and caches it locally.
struct FastembedEngine {
    model: Arc<TextEmbedding>,
    ndims: usize,
}

impl FastembedEngine {
    fn new(model_name: EmbeddingModel) -> Result<Self, Box<dyn std::error::Error>> {
        let model = TextEmbedding::try_new(InitOptions::new(model_name))
            .map_err(|e| format!("failed to init fastembed model: {e}"))?;

        // Infer ndims from a test embedding.
        let test_embedding = model
            .embed(vec!["test"], None)
            .map_err(|e| format!("failed to get embedding dimension: {e}"))?;
        let ndims = test_embedding.first().map(|v| v.len()).unwrap_or(1024); // fallback for BGELargeZHV15 / BGE-M3

        tracing::info!(ndims, "Fastembed engine initialised");
        Ok(Self {
            model: Arc::new(model),
            ndims,
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingEngine for FastembedEngine {
    fn ndims(&self) -> usize {
        self.ndims
    }

    /// Embed a query with BGE instruction prefix for better retrieval quality.
    ///
    /// BGE models were trained with task‑specific instruction prefixes.
    /// Adding this prefix tells the model to produce a "query‑side" embedding
    /// (as opposed to a "passage‑side" embedding used during ingestion).
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, KnowledgeError> {
        // BGE instruction prefix for Chinese queries.
        let query_text = format!("为这个句子生成表示以用于检索相关文章：{text}");
        let model = self.model.clone();
        let result =
            tokio::task::spawn_blocking(move || model.embed(vec![query_text.as_str()], None))
                .await
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?;
        Ok(result.into_iter().next().unwrap_or_default())
    }

    /// Batch‑embed passages (no instruction prefix — raw document text).
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
        let model = self.model.clone();
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let result = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            model.embed(refs, None)
        })
        .await
        .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?
        .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?;
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// PDF extraction
// ---------------------------------------------------------------------------

fn extract_pdf_text(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let text = pdf_extract::extract_text_from_mem(&bytes)?;
    Ok(text)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knowledge_demo=info,knowledge_base=info".into()),
        )
        .init();

    let use_text_mode = std::env::args().any(|a| a == "--text");

    // ── Build backend & pipeline ──
    let backend = std::sync::Arc::new(InMemoryBackend::new());
    let embedding = std::sync::Arc::new(
        FastembedEngine::new(EmbeddingModel::BGELargeZHV15)
            .expect("Failed to init fastembed model"),
    );
    let chunker = make_chunker(ChunkingStrategy::OverlappingWindow {
        size: 500,
        overlap: 100,
    });

    let pipeline = IngestionPipeline::new(
        backend.clone() as std::sync::Arc<dyn DocumentStore>,
        Some(backend.clone() as std::sync::Arc<dyn VectorIndex>),
        Some(backend.clone() as std::sync::Arc<dyn GraphStore>),
        Some(backend.clone() as std::sync::Arc<dyn FullTextIndex>),
        embedding.clone(),
        chunker,
    );

    let search_engine = HybridSearchEngine::new(
        backend.clone() as std::sync::Arc<dyn DocumentStore>,
        Some(backend.clone() as std::sync::Arc<dyn VectorIndex>),
        Some(backend.clone() as std::sync::Arc<dyn GraphStore>),
        Some(backend.clone() as std::sync::Arc<dyn FullTextIndex>),
        embedding,
    )
    .with_query_router(Box::new(RuleBasedRouter::new()))
    .with_query_analyzer(Box::new(RuleBasedAnalyzer::new()));

    println!("=== Knowledge Module Demo ===\n");

    // ── Ingest ──
    if use_text_mode {
        ingest_demo_text(&pipeline).await?;
    } else {
        ingest_pdfs(&pipeline, &backend).await?;
    }

    // ── Run queries ──
    run_queries(&search_engine).await;

    println!("=== Demo complete ===");
    Ok(())
}

// ---------------------------------------------------------------------------
// Text demo (fast path for quick testing)
// ---------------------------------------------------------------------------

async fn ingest_demo_text(pipeline: &IngestionPipeline) -> Result<(), Box<dyn std::error::Error>> {
    let docs = vec![
        Document {
            kb_id: None,
            id:"resume".into(),
            title: "彭琛简历".into(),
            source_path: "demo/resume.txt".into(),
            content: "姓名：彭琛\n性别：男\n学历：武汉大学 计算机科学与技术 本科\n工作经历：\n- 2020-2023 阿里巴巴 高级工程师（Java 后端）\n- 2023-至今 字节跳动 资深工程师（Rust / 分布式系统）\n技能：Rust, Java, Python, Kubernetes, AWS, Linux\n项目：\n- 主导设计分布式任务调度系统，日处理10亿+任务\n- 开发高性能 RPC 框架，QPS 提升 3 倍\n语言：中文（母语），英语（流利）".into(),
            metadata: DocumentMetadata { file_type: Some("txt".into()), ..Default::default() },
        },
        Document {
            kb_id: None,
            id:"departure".into(),
            title: "离职证明".into(),
            source_path: "demo/departure.txt".into(),
            content: "离职证明\n\n兹证明 彭琛（身份证号：42010619900101XXXX）于 2020 年 7 月 1 日至 2023 年 6 月 30 日在我司担任高级工程师一职。\n\n该员工在职期间表现优秀，工作认真负责，与同事关系融洽。因其个人发展原因申请离职，我司已批准。\n\n特此证明。\n\n阿里巴巴（中国）有限公司\n2023年7月1日".into(),
            metadata: DocumentMetadata { file_type: Some("txt".into()), ..Default::default() },
        },
    ];

    for doc in docs {
        println!("📄 Ingesting: {}", doc.title);
        pipeline.ingest(doc).await?;
    }

    // Stats aren't directly accessible from IngestionPipeline, but
    // the pipeline internally uses the backend. For this demo we
    // show a summary after ingestion.
    println!("📊 Ingested 2 demo documents\n");
    Ok(())
}

// ---------------------------------------------------------------------------
// PDF demo
// ---------------------------------------------------------------------------

async fn ingest_pdfs(
    pipeline: &IngestionPipeline,
    backend: &std::sync::Arc<InMemoryBackend>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../example");
    let pdf_files: Vec<_> = ["彭琛简历.pdf", "离职证明.pdf"]
        .iter()
        .map(|name| pdf_dir.join(name))
        .filter(|p| p.exists())
        .collect();

    if pdf_files.is_empty() {
        eprintln!("No PDFs found in {}", pdf_dir.display());
        eprintln!("Falling back to --text mode.");
        return Ok(());
    }

    for pdf_path in &pdf_files {
        let file_name = pdf_path.file_name().unwrap().to_string_lossy();
        println!("📄 Ingesting: {file_name}");

        let content = match extract_pdf_text(pdf_path) {
            Ok(t) => {
                let len = t.chars().count();
                println!("  Extracted {len} chars");
                t
            }
            Err(e) => {
                eprintln!("  ⚠ Failed: {e}");
                continue;
            }
        };

        if content.trim().is_empty() {
            eprintln!("  ⚠ No text, skipping");
            continue;
        }

        let doc_id = file_name.replace('.', "_");
        let doc = Document {
            id: doc_id.clone(),
            kb_id: None,
            title: file_name.to_string(),
            source_path: pdf_path.to_string_lossy().to_string(),
            content,
            metadata: DocumentMetadata {
                file_type: Some("pdf".into()),
                ..Default::default()
            },
        };

        print!("  Ingesting... ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        pipeline.ingest(doc).await?;
        println!("✅");

        let stats = backend.stats().await?;
        println!(
            "  Store now: {} docs, {} chunks",
            stats.document_count, stats.chunk_count
        );
    }

    let stats = backend.stats().await?;
    println!(
        "\n📊 Final: {} documents, {} chunks, {} bytes\n",
        stats.document_count, stats.chunk_count, stats.total_bytes
    );
    Ok(())
}

async fn run_queries(engine: &HybridSearchEngine) {
    let queries = ["彭琛", "离职", "武汉大学", "简历", "太阳系"];

    for query in &queries {
        println!("🔍 Query: \"{query}\"");

        match engine
            .search(&SearchRequest {
                query: query.to_string(),
                top_k: 3,
                strategy: SearchStrategy::Auto,
                filters: None,
                min_confidence: None,
            })
            .await
        {
            Ok(results) => {
                if results.is_empty() {
                    println!("  (no results)\n");
                }
                for (i, r) in results.iter().enumerate() {
                    println!(
                        "  {}. [{:.4}] {} — {}",
                        i + 1,
                        r.score,
                        r.title,
                        r.source_path,
                    );
                    let preview: String = r.snippet.chars().take(100).collect();
                    println!("     📝 {}\n", preview.replace('\n', " "));
                }
            }
            Err(e) => eprintln!("  ❌ {e}\n"),
        }
    }
}
