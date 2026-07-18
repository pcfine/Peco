//! HelixDB 后端知识库 demo — 文本摄入 + 混合搜索 + 图遍历。
//!
//! 使用 HelixDB（图-向量数据库）作为存储后端，展示与
//! `knowledge_demo`（InMemory 后端）相同的摄入/搜索流程，
//! 并额外展示 HelixDB 独有的图扩展和关系查询能力。
//!
//! # 前置条件
//!
//! 需要一个运行中的 HelixDB 实例。默认连接 `http://localhost:6969`，
//! 可通过 `HELIXDB_URL` 环境变量覆盖。
//!
//! # 使用方式
//!
//! ```bash
//! # 使用内置文本 demo（不需要 PDF 文件）
//! cargo run --example helixdb_demo --features helixdb -- --text
//!
//! # 使用 PDF 文件（需要 ../../example/ 下有 PDF）
//! cargo run --example helixdb_demo --features helixdb
//!
//! # 自定义 HelixDB 地址
//! HELIXDB_URL=http://my-helixdb:6969 cargo run --example helixdb_demo --features helixdb -- --text
//!
//! # 运行后清理测试数据
//! cargo run --example helixdb_demo --features helixdb -- --text --cleanup
//! ```

use std::sync::Arc;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use knowledge_base::backends::helixdb::HelixDbBackend;
use knowledge_base::chunking::make_chunker;
use knowledge_base::engine::{
    HybridSearchEngine, IngestionPipeline, query_analysis::RuleBasedAnalyzer,
    query_router::RuleBasedRouter,
};
use knowledge_base::error::KnowledgeError;
use knowledge_base::traits::*;
use knowledge_base::types::*;

// ---------------------------------------------------------------------------
// Fastembed 嵌入引擎（中文优化，与 knowledge_demo 共用同一套逻辑）
// ---------------------------------------------------------------------------

/// 包装 [`fastembed::TextEmbedding`] 以适配 [`EmbeddingEngine`] trait。
///
/// 使用 `BGELargeZHV15` — BAAI BGE large 中文模型（1024 维向量）。
struct FastembedEngine {
    model: Arc<TextEmbedding>,
    ndims: usize,
}

impl FastembedEngine {
    fn new(model_name: EmbeddingModel) -> Result<Self, Box<dyn std::error::Error>> {
        let model = TextEmbedding::try_new(InitOptions::new(model_name))
            .map_err(|e| format!("failed to init fastembed model: {e}"))?;

        let test_embedding = model
            .embed(vec!["test"], None)
            .map_err(|e| format!("failed to get embedding dimension: {e}"))?;
        let ndims = test_embedding.first().map(|v| v.len()).unwrap_or(1024);

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

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, KnowledgeError> {
        let query_text = format!("为这个句子生成表示以用于检索相关文章：{text}");
        let model = self.model.clone();
        let result =
            tokio::task::spawn_blocking(move || model.embed(vec![query_text.as_str()], None))
                .await
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?
                .map_err(|e| KnowledgeError::EmbeddingError(e.to_string()))?;
        Ok(result.into_iter().next().unwrap_or_default())
    }

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
// 命令行参数
// ---------------------------------------------------------------------------

struct CliArgs {
    use_text_mode: bool,
    cleanup: bool,
    helixdb_url: String,
}

fn parse_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    CliArgs {
        use_text_mode: args.iter().any(|a| a == "--text"),
        cleanup: args.iter().any(|a| a == "--cleanup"),
        helixdb_url: std::env::var("HELIXDB_URL")
            .unwrap_or_else(|_| "http://localhost:6969".into()),
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "helixdb_demo=info,knowledge_base=info".into()),
        )
        .init();

    let args = parse_args();

    println!("=== HelixDB Knowledge Demo ===");
    println!("HelixDB URL: {}\n", args.helixdb_url);

    // ── 连接 HelixDB ──
    let embedding = Arc::new(
        FastembedEngine::new(EmbeddingModel::BGELargeZHV15)
            .expect("Failed to init fastembed model"),
    );

    println!("Connecting to HelixDB...");
    let backend = Arc::new(HelixDbBackend::connect(&args.helixdb_url, embedding.ndims()).await?);

    // 幂等初始化 schema（向量索引 + 全文索引）
    println!("Initialising schema (idempotent)...");
    backend.init_schema().await?;
    println!("Schema ready.\n");

    // ── 构建管道 ──
    let chunker = make_chunker(ChunkingStrategy::OverlappingWindow {
        size: 500,
        overlap: 100,
    });

    let pipeline = IngestionPipeline::new(
        backend.clone() as Arc<dyn DocumentStore>,
        Some(backend.clone() as Arc<dyn VectorIndex>),
        Some(backend.clone() as Arc<dyn GraphStore>),
        Some(backend.clone() as Arc<dyn FullTextIndex>),
        embedding.clone(),
        chunker,
    );

    // 保留一份 embedding 引用，供后续 auto_link_documents 使用
    let embed_ref = embedding.clone();

    let search_engine = HybridSearchEngine::new(
        backend.clone() as Arc<dyn DocumentStore>,
        Some(backend.clone() as Arc<dyn VectorIndex>),
        Some(backend.clone() as Arc<dyn GraphStore>),
        Some(backend.clone() as Arc<dyn FullTextIndex>),
        embedding,
    )
    .with_query_router(Box::new(RuleBasedRouter::new()))
    .with_query_analyzer(Box::new(RuleBasedAnalyzer::new()));

    // ── 检查是否已有数据（幂等摄入） ──
    let stats = backend.stats().await?;
    if stats.document_count > 0 {
        println!(
            "Found {} existing document(s), {} chunk(s).",
            stats.document_count, stats.chunk_count
        );
        if args.cleanup {
            println!("Cleaning up existing data...");
            cleanup_all(&backend).await?;
        } else {
            println!("Skipping ingestion (data already exists). Use --cleanup to reset.\n");
        }
    }

    let stats = backend.stats().await?;
    if stats.document_count == 0 {
        // ── 摄入 ──
        if args.use_text_mode {
            ingest_demo_text(&pipeline).await?;
        } else {
            ingest_pdfs(&pipeline, &backend).await?;
        }

        // ── 自动构建文档关联图 ──
        println!("\n🔗 Auto-linking documents by semantic similarity...");
        auto_link_documents(&backend, embed_ref).await?;
    }

    // ── 运行查询 ──
    run_queries(&search_engine).await;

    // ── HelixDB 特色：图遍历 ──
    run_graph_demo(&backend).await;

    // ── 清理 ──
    if args.cleanup {
        println!("\n--- Cleanup ---");
        cleanup_all(&backend).await?;
    }

    println!("\n=== Demo complete ===");
    Ok(())
}

// ---------------------------------------------------------------------------
// 文本 demo（快速测试，无需 PDF）
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
        Document {
            kb_id: None,
            id:"skills".into(),
            title: "技能与项目".into(),
            source_path: "demo/skills.txt".into(),
            content: "彭琛技能清单\n\n编程语言：Rust（精通），Java（精通），Python（熟练），Go（了解）\n\n云原生：Kubernetes（CKA 认证），Docker，AWS（Solutions Architect），Terraform\n\n数据库：MySQL，PostgreSQL，MongoDB，Redis，Elasticsearch\n\n分布式：gRPC，Kafka，分布式事务（Saga），分布式锁\n\n项目经验：\n1. 分布式任务调度平台 — 日处理 10 亿+任务，支持优先级、依赖和定时调度\n2. RPC 框架（Rust）— 基于 Tokio 异步运行时，QPS 较上代提升 3 倍\n3. 实时数据管道 — Flink + Kafka 构建，P99 延迟 < 100ms\n4. API 网关 — 基于 Envoy 的微服务网关，支持限流、鉴权、路由".into(),
            metadata: DocumentMetadata { file_type: Some("txt".into()), ..Default::default() },
        },
    ];

    for doc in docs {
        println!("📄 Ingesting: {}", doc.title);
        pipeline.ingest(doc).await?;
    }

    println!("📊 Ingested 3 demo documents\n");
    Ok(())
}

// ---------------------------------------------------------------------------
// PDF 摄入（与 knowledge_demo 共用 PDF 文件）
// ---------------------------------------------------------------------------

async fn ingest_pdfs(
    pipeline: &IngestionPipeline,
    backend: &Arc<HelixDbBackend>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../example");
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

        let content = match std::fs::read(pdf_path)
            .map_err(|e| format!("read: {e}"))
            .and_then(|bytes| {
                pdf_extract::extract_text_from_mem(&bytes).map_err(|e| format!("extract: {e}"))
            }) {
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

        // HelixDB 也支持通过 trait 的 stats() 查看状态
        let stats = backend.stats().await?;
        println!(
            "  Store now: {} docs, {} chunks",
            stats.document_count, stats.chunk_count
        );
    }

    let stats = backend.stats().await?;
    println!(
        "\n📊 Final: {} documents, {} chunks\n",
        stats.document_count, stats.chunk_count
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 混合搜索
// ---------------------------------------------------------------------------

async fn run_queries(engine: &HybridSearchEngine) {
    println!("--- Hybrid Search ---\n");

    let queries = ["彭琛", "离职", "武汉大学", "Rust", "分布式", "太阳系"];

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

// ---------------------------------------------------------------------------
// HelixDB 特色：图遍历 & 关系查询
// ---------------------------------------------------------------------------

async fn run_graph_demo(backend: &Arc<HelixDbBackend>) {
    println!("\n--- HelixDB Graph Demo ---\n");

    // 1) 列出所有文档
    println!("📋 Listed documents:");
    match backend.list(0, 20).await {
        Ok(summaries) => {
            for s in &summaries {
                println!("  - [{}] {}", s.id, s.title);
            }
        }
        Err(e) => eprintln!("  ❌ list: {e}"),
    }

    // 2) 获取单个文档的分块
    println!("\n📄 Chunks of 'resume':");
    match backend.chunks(&"resume".to_string()).await {
        Ok(chunks) => {
            for c in &chunks {
                let preview: String = c.text.chars().take(80).collect();
                println!(
                    "  chunk[{}] seq={}: {}",
                    c.id,
                    c.sequence_index,
                    preview.replace('\n', " ")
                );
            }
        }
        Err(e) => eprintln!("  ❌ chunks: {e}"),
    }

    // 3) 从 resume 出发沿 RELATED_TO 遍历
    println!("\n🔍 Traversing from 'resume' via RELATED_TO (Outgoing, depth 2):");
    match backend
        .traverse(
            "resume",
            &[EdgeType::RelatedTo],
            TraversalDirection::Outgoing,
            2,
        )
        .await
    {
        Ok(steps) => {
            if steps.is_empty() {
                println!("  (no related documents found)");
            }
            for s in &steps {
                let edge_info = s
                    .via_edge
                    .as_ref()
                    .map(|e| format!("{:?}", e))
                    .unwrap_or_else(|| "none".into());
                println!(
                    "  → [{}] distance={} via {edge_info}",
                    s.node.id, s.node.distance
                );
            }
        }
        Err(e) => eprintln!("  ❌ traverse: {e}"),
    }

    // 5) 图扩展 — 从分块出发，查找相邻文档
    //    先获取 resume 的分块 ID
    println!("\n🔍 Graph expand from resume chunks:");
    match backend.chunks(&"resume".to_string()).await {
        Ok(chunks) => {
            let chunk_ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
            if !chunk_ids.is_empty() {
                match backend
                    .expand(
                        &chunk_ids[..1], // 用第一个分块做扩展
                        &[EdgeType::RelatedTo, EdgeType::Contains],
                        2,
                    )
                    .await
                {
                    Ok(nodes) => {
                        if nodes.is_empty() {
                            println!("  (no expanded nodes)");
                        }
                        for n in &nodes {
                            println!(
                                "  → [{}] labels={:?} distance={}",
                                n.id, n.labels, n.distance
                            );
                        }
                    }
                    Err(e) => eprintln!("  ❌ expand: {e}"),
                }
            }
        }
        Err(e) => eprintln!("  ❌ chunks: {e}"),
    }

    println!();
}

// ---------------------------------------------------------------------------
// 清理
// ---------------------------------------------------------------------------

async fn cleanup_all(backend: &Arc<HelixDbBackend>) -> Result<(), Box<dyn std::error::Error>> {
    let summaries = backend.list(0, 100).await?;
    for s in &summaries {
        println!("🗑  Deleting: {}", s.id);
        backend.delete(&s.id).await?;
    }
    println!("Cleaned up {} document(s).", summaries.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// 自动关联文档（基于嵌入相似度）
// ---------------------------------------------------------------------------

/// 余弦相似度。
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// 计算文档两两之间的嵌入相似度，为相似度超过阈值的文档对创建
/// `RELATED_TO` 边，从而构建可遍历的知识图谱。
///
/// 这是核心知识图谱的结构化步骤：
/// 1. 获取所有文档及其内容
/// 2. 对每篇文档生成语义嵌入
/// 3. 计算两两余弦相似度
/// 4. 为相似度 ≥ 阈值的文档对创建 RELATED_TO 边
async fn auto_link_documents(
    backend: &Arc<HelixDbBackend>,
    embedding: Arc<FastembedEngine>,
) -> Result<(), Box<dyn std::error::Error>> {
    let summaries = backend.list(0, 100).await?;

    if summaries.len() < 2 {
        println!(
            "  (need at least 2 documents to link, got {})",
            summaries.len()
        );
        return Ok(());
    }

    // 获取每篇文档的简短摘要用于嵌入
    let mut doc_texts: Vec<(String, String)> = Vec::new(); // (id, text_for_embedding)
    for s in &summaries {
        if let Ok(Some(doc)) = backend.get(&s.id).await {
            // 取前 800 字符作为文档摘要，足够捕获主题
            let summary: String = doc.content.chars().take(800).collect();
            doc_texts.push((s.id.clone(), summary));
        }
    }

    // 批量计算嵌入
    let texts: Vec<&str> = doc_texts.iter().map(|(_, t)| t.as_str()).collect();
    let embeddings = embedding.embed_batch(&texts).await?;

    // 两两计算余弦相似度并创建 RELATED_TO 边
    const SIMILARITY_THRESHOLD: f32 = 0.60;
    let mut edge_count = 0;

    for i in 0..doc_texts.len() {
        for j in (i + 1)..doc_texts.len() {
            let sim = cosine_similarity(&embeddings[i], &embeddings[j]);
            if sim >= SIMILARITY_THRESHOLD {
                let edge = KnowledgeEdge {
                    source_id: doc_texts[i].0.clone(),
                    target_id: doc_texts[j].0.clone(),
                    edge_type: EdgeType::RelatedTo,
                    weight: sim,
                    properties: std::collections::HashMap::new(),
                };
                backend.add_edge(edge).await?;
                edge_count += 1;
                println!(
                    "  ✅ {} ↔ {} (similarity: {:.4})",
                    doc_texts[i].0, doc_texts[j].0, sim
                );
            } else {
                println!(
                    "  ⏭  {} ↔ {} (similarity: {:.4} < {})",
                    doc_texts[i].0, doc_texts[j].0, sim, SIMILARITY_THRESHOLD
                );
            }
        }
    }

    println!("  📊 Created {edge_count} RELATED_TO edges\n");
    Ok(())
}
