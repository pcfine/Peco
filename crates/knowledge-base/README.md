# knowledge-base

本地优先、多后端可插拔的 AI 知识库系统。提供文档解析、智能分块、向量嵌入、混合检索和知识图谱能力，支持多知识库管理。

## 架构

```
KnowledgeBaseManager          ← 统一入口（创建/删除/列表/搜索）
└── KnowledgeBase             ← 单知识库（解析→分块→嵌入→存储→检索）
    ├── DocumentParser        ← 文档解析（PDF / Markdown / HTML / TXT / 代码）
    ├── Chunker              ← 智能分块（滑动窗口 / 固定大小 / 按句子 / Markdown 标题）
    ├── EmbeddingEngine      ← 本地嵌入（fastembed，BGE / MiniLM / MultilingualE5）
    ├── Backend              ← 存储后端（LanceDB / InMemory / HelixDB）
    └── HybridSearchEngine   ← 混合检索（BM25 + 向量 + 图谱，RRF 融合）
```

## 快速开始

```rust
use knowledge_base::{KnowledgeBaseManager, KbConfig, BackendType};
use knowledge_base::embedding::FastembedModelType;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 加载/创建管理器
    let mut mgr = KnowledgeBaseManager::load("~/.peco/knowledge_bases").await?;

    // 2. 创建知识库
    let kb = mgr.create_kb(KbConfig {
        name: "技术文档".into(),
        description: "团队技术积累".into(),
        backend: BackendType::LanceDb,
        embedding_model: FastembedModelTypeSerde::BGESmallZHV15,
        chunking: ChunkingStrategySerde::default(),
        storage_path: None,
    }).await?;

    // 3. 导入文档（自动识别格式 → 解析 → 分块 → 嵌入 → 存储）
    kb.add_file(Path::new("./docs/rust入门.pdf")).await?;
    kb.add_file(Path::new("./docs/api设计.md")).await?;
    kb.add_directory(Path::new("./docs/培训材料/")).await?;

    // 4. 搜索
    let results = kb.search("Rust 异步编程怎么实现?", 5).await?;
    for r in &results {
        println!("[{:.3}] {} — {}", r.score, r.title, r.snippet);
    }
    Ok(())
}
```

## 功能特性

### 文档解析

自动识别文件格式并提取文本内容：

| 格式 | 状态 | 依赖 |
|------|------|------|
| PDF | ✅ | `pdf` feature |
| Markdown | ✅ | 内置 |
| HTML | ✅ | 内置 |
| TXT / 代码 | ✅ | 内置 |
| DOCX | ✅ | 内置 |

### 智能分块

| 策略 | 默认参数 | 说明 |
|------|---------|------|
| `OverlappingWindow` | 800 字符, 200 重叠 | 滑动窗口 + 句子边界对齐（推荐） |
| `FixedSize` | 固定大小 | 简单等长切分 |
| `SentenceBased` | 按最大字符数 | 按句子边界切分 |
| `MarkdownHeading` | 按最大字符数 | 按标题层级切分 |

### 嵌入模型

本地 ONNX 推理，无需 API 密钥：

| 模型 | 维度 | 大小 | 适用 |
|------|------|------|------|
| `BGESmallZHV15` | 512 | ~100 MB | 中文（默认） |
| `BGELargeZHV15` | 1024 | ~1.3 GB | 中文最佳 |
| `AllMiniLML6V2Q` | 384 | ~80 MB | 英文 |
| `MultilingualE5Small` | 384 | ~120 MB | 多语言 |

### 存储后端

| 后端 | feature | 说明 |
|------|---------|------|
| `LanceDbBackend` | `lancedb` | 本地嵌入式，BM25+向量混合搜索（默认） |
| `InMemoryBackend` | 内置 | 内存存储，测试/原型用 |
| `HelixDbBackend` | `helixdb` | HelixDB 图-向量数据库 |

### 混合检索

4 层自适应检索管道：

```
查询 → 意图分析 → 多路径并行检索 → 分数校准 → 交叉验证 → RRF 融合 → 结果
```

- **Vector 搜索**：ANN 余弦相似度
- **全文搜索**：BM25（LanceDB FTS / Tantivy）
- **图谱遍历**：CONTAINS + NEXT_CHUNK + RELATED_TO 边
- **自适应权重**：根据查询类型动态调整各路径权重

## Feature Flags

```toml
[dependencies]
knowledge-base = { path = "crates/knowledge-base" }

# 可选 feature
knowledge-base = { features = ["pdf", "lancedb", "helixdb"] }
```

| Feature | 默认 | 说明 |
|---------|------|------|
| `fastembed-embedding` | ✅ | 本地嵌入引擎 |
| `pdf` | ✅ | PDF 文档解析 |
| `lancedb` | ✅ | LanceDB 存储后端 |
| `helixdb` | — | HelixDB 后端 |

## 底层 API

除了简化的 `KnowledgeBaseManager` API，高级用户可以手动组装管道：

```rust
use std::sync::Arc;
use knowledge_base::{
    LanceDbBackend, FastembedEngine, FastembedModelType,
    IngestionPipeline, HybridSearchEngine, make_parser,
    make_chunker, ChunkingStrategy,
};

let backend = Arc::new(LanceDbBackend::connect(path, "my_kb", 1024).await?);
let embedding = Arc::new(FastembedEngine::new(FastembedModelType::BGESmallZHV15)?);
let chunker = make_chunker(ChunkingStrategy::default());

let pipeline = IngestionPipeline::new(
    backend.clone() as Arc<dyn DocumentStore>,
    Some(backend.clone() as Arc<dyn VectorIndex>),
    None, // graph_store
    Some(backend.clone() as Arc<dyn FullTextIndex>),
    embedding.clone(),
    chunker,
);

// 自定义解析 → 摄入
let parsed = make_parser(Path::new("doc.pdf"))?.parse_file(path).await?;
let doc = Document { id: "...".into(), kb_id: None, title: parsed.title, ... };
pipeline.ingest(doc).await?;
```

## 目录结构

```
src/
├── lib.rs              # crate 根，公共 API 重导出
├── types.rs            # 核心类型：Document, Chunk, SearchResult 等
├── error.rs            # KnowledgeError 枚举
├── traits/             # 抽象层：7 个 trait
│   ├── document_store.rs
│   ├── vector_index.rs
│   ├── fulltext_index.rs
│   ├── graph_store.rs
│   ├── combined_search.rs
│   ├── embedding.rs
│   └── chunker.rs
├── parsers/            # 文档解析器
│   ├── pdf.rs, markdown.rs, html.rs, txt.rs, code.rs, docx.rs
├── embedding/          # Fastembed 嵌入引擎
├── chunking/           # 分块策略实现
├── engine/             # 检索引擎
│   ├── ingestion.rs    # 摄入管道
│   ├── hybrid_search.rs # 4 层自适应检索
│   ├── fusion.rs       # RRF + 加权融合
│   ├── query_router.rs # 查询路由
│   └── query_analysis.rs # 查询意图分析
├── graph/              # 知识图谱构建
├── backends/           # 存储后端
│   ├── memory.rs       # InMemoryBackend
│   ├── lancedb/        # LanceDbBackend
│   └── helixdb/        # HelixDbBackend (feature-gated)
└── manager/            # 知识库管理器
    ├── mod.rs          # KnowledgeBaseManager + KnowledgeBase
    └── config.rs       # KbConfig 配置类型
```

## 运行测试

```bash
cargo test -p knowledge-base
```
