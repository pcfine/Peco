//! 知识库 Demo — 读取 PDF 文件，创建知识库并进行搜索。
//!
//! ```bash
//! cargo run --example kb_demo
//! ```

use std::path::{Path, PathBuf};

use knowledge_base::KnowledgeBaseManager;
use knowledge_base::manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig,
};

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kb_demo=info,knowledge_base=info".into()),
        )
        .init();

    // ── 准备临时目录 ──
    let tmp = tempfile::tempdir()?;
    let kb_dir = tmp.path();
    println!("📂 知识库目录: {}", kb_dir.display());

    // ── 创建管理器 ──
    let mgr = KnowledgeBaseManager::load(kb_dir).await?;

    // ── 创建知识库 ──
    let kb = mgr
        .create_kb(KbConfig {
            name: "个人档案".into(),
            description: "简历与离职证明".into(),
            embedding_model: FastembedModelTypeSerde::BGESmallZHV15,
            chunking: ChunkingStrategySerde::OverlappingWindow {
                size: 500,
                overlap: 100,
            },
            backend: BackendType::LanceDb,
            storage_path: None,
        })
        .await?;

    println!("✅ 知识库「个人档案」创建成功\n");

    // ── 导入 PDF ──
    let example_dir = find_example_dir();
    let pdf_files = ["彭琛简历.pdf", "离职证明.pdf"];

    for name in &pdf_files {
        let path = example_dir.join(name);
        if !path.exists() {
            eprintln!("⚠️  文件不存在: {}", path.display());
            continue;
        }

        print!("📄 导入: {name} ... ");
        match kb.add_file(&path).await {
            Ok(doc) => {
                let chars = doc.content.chars().count();
                println!("✅ {} 字符, id={}", chars, &doc.id[..16]);
            }
            Err(e) => eprintln!("❌ {e}"),
        }
    }

    // ── 显示统计 ──
    let infos = mgr.list_kbs().await?;
    for info in &infos {
        println!(
            "\n📊 知识库「{}」: {} 文档, {} 分块 | 模型: {} | 后端: {}",
            info.name, info.document_count, info.chunk_count, info.embedding_model, info.backend,
        );
    }

    // ── 搜索 ──
    println!("\n══════════════════════════════════════");
    println!("🔍 搜索测试");
    println!("══════════════════════════════════════");

    let queries = [
        ("彭琛", "姓名精确匹配"),
        ("武汉大学", "学历关键词"),
        ("阿里巴巴", "工作经历"),
        ("离职证明", "文档类型"),
        ("Rust 分布式", "技能关键词"),
    ];

    for (query, desc) in &queries {
        println!("\n── {desc} ──");
        println!("  查询: \"{query}\"");

        match kb.search(query, 3).await {
            Ok(results) => {
                if results.is_empty() {
                    println!("  (无结果)");
                }
                for (i, r) in results.iter().enumerate() {
                    let preview: String = r.snippet.chars().take(120).collect();
                    println!(
                        "  {}. [{:.4}] {} ({})",
                        i + 1,
                        r.score,
                        r.title,
                        r.source_path,
                    );
                    println!("     📝 {}", preview.replace('\n', " "));
                }
            }
            Err(e) => eprintln!("  ❌ {e}"),
        }
    }

    println!("\n✅ Demo 完成");
    Ok(())
}

/// 查找 example/ 目录（项目根目录下的 example/）
fn find_example_dir() -> PathBuf {
    // 从 crate 目录向上两级到项目根
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project_root = crate_dir.parent().unwrap_or(crate_dir);
    let example_dir = project_root.join("example");
    if example_dir.exists() {
        return example_dir;
    }
    // 回退：直接使用 CARGO_MANIFEST_DIR 上级
    crate_dir
        .join("../example")
        .canonicalize()
        .unwrap_or(example_dir)
}
