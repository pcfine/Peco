//! 相似度阈值标定脚本。
//!
//! 把 `ConsolidationConfig` 的 `min_cluster_cos` / `dedup_cos` 从占位默认值
//! 变成有数据来源的操作点。两步工作流：
//!
//! 1. **generate** — 读取真实记忆库，全部文档重嵌入后计算全对余弦分布，
//!    导出待人工标注的候选对（正例为主 + 少量跨阈值负例）；
//! 2. **calibrate** — 读回人工填好 0/1 标签的清单，按阈值网格计算
//!    PR 曲线，输出 `min_cluster_cos`（F1 最优）与 `dedup_cos`
//!    （precision ≥ 0.98 的最小阈值，硬删除必须高精度）候选操作点。
//!
//! # 用法
//!
//! ```text
//! # 步骤 1：导出待标注对（默认 annotation_candidates.json）
//! cargo run -p knowledge-base --example calibrate_thresholds -- \
//!     --knowledge-dir ~/.peco/workspaces/<user>/knowledge \
//!     --kb @private_memory \
//!     --mode generate --out annotation_candidates.json
//!
//! # 步骤 2：人工把每对的 label 从 null 改为 0（非重复）/ 1（重复），然后：
//! cargo run -p knowledge-base --example calibrate_thresholds -- \
//!     --knowledge-dir ~/.peco/workspaces/<user>/knowledge \
//!     --kb @private_memory \
//!     --mode calibrate --annotated annotation_candidates.json --out report.md
//! ```

use std::path::PathBuf;

use knowledge_base::KnowledgeBaseManager;
use knowledge_base::engine::{cosine_similarity, connected_component_clusters};

/// 一对候选重复 — 人工标注清单的行。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CandidatePair {
    doc_id_a: String,
    doc_id_b: String,
    title_a: String,
    title_b: String,
    cosine: f32,
    text_a: String,
    text_b: String,
    /// 人工标注：1 = 语义重复，0 = 非重复。generate 阶段为 null。
    label: Option<u8>,
}

const THRESHOLD_FLOOR: f32 = 0.50;
const THRESHOLD_CEIL: f32 = 0.99;
const NEGATIVE_SAMPLE_COUNT: usize = 100;
const EMBED_BATCH: usize = 64;
/// 聚类预览用占位阈值 — 以标定报告回填值为准。
const PREVIEW_CLUSTER_COS: f32 = 0.85;

fn main() {
    let args = parse_args();
    if let Err(e) = args {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let args = args.unwrap();
    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let result = match args.mode {
        Mode::Generate => rt.block_on(run_generate(&args)),
        Mode::Calibrate => rt.block_on(run_calibrate(&args)),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

enum Mode {
    Generate,
    Calibrate,
}

struct Args {
    knowledge_dir: PathBuf,
    kb: String,
    mode: Mode,
    out: PathBuf,
    annotated: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut knowledge_dir = None;
    let mut kb = None;
    let mut mode = Mode::Generate;
    let mut out = PathBuf::from("annotation_candidates.json");
    let mut annotated = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--knowledge-dir" => {
                knowledge_dir = Some(it.next().ok_or("--knowledge-dir 需要一个路径")?)
            }
            "--kb" => kb = Some(it.next().ok_or("--kb 需要一个名称")?),
            "--mode" => {
                mode = match it.next().ok_or("--mode 需要 generate 或 calibrate")?.as_str() {
                    "generate" => Mode::Generate,
                    "calibrate" => Mode::Calibrate,
                    other => return Err(format!("未知 mode: {other}（可选 generate | calibrate）")),
                }
            }
            "--out" => out = PathBuf::from(it.next().ok_or("--out 需要一个路径")?),
            "--annotated" => annotated = Some(PathBuf::from(it.next().ok_or("--annotated 需要一个路径")?)),
            other => return Err(format!("未知参数: {other}")),
        }
    }

    if mode_is_calibrate(&mode) {
        out = PathBuf::from("calibration_report.md");
    }

    Ok(Args {
        knowledge_dir: PathBuf::from(knowledge_dir.ok_or("缺少 --knowledge-dir（workspace 的 knowledge/ 目录）")?),
        kb: kb.ok_or("缺少 --kb（如 @private_memory）")?,
        mode,
        out,
        annotated,
    })
}

fn mode_is_calibrate(mode: &Mode) -> bool {
    matches!(mode, Mode::Calibrate)
}

/// 打开知识库并把全部文档（含原文）加载进内存。
async fn load_docs(
    kb: &std::sync::Arc<knowledge_base::KnowledgeBase>,
) -> Result<Vec<knowledge_base::Document>, String> {
    // 分页取全量摘要（无 100 条上限假设）
    let mut summaries: Vec<knowledge_base::DocumentSummary> = Vec::new();
    let (mut offset, page) = (0usize, 200usize);
    loop {
        let batch = kb
            .list_documents(offset, page)
            .await
            .map_err(|e| format!("list_documents 失败: {e}"))?;
        let done = batch.len() < page;
        summaries.extend(batch);
        if done {
            break;
        }
        offset += page;
    }
    if summaries.is_empty() {
        return Ok(Vec::new());
    }

    let mut docs = Vec::with_capacity(summaries.len());
    for s in summaries {
        let doc = kb
            .get_document(&s.id)
            .await
            .map_err(|e| format!("get_document({}) 失败: {e}", s.id))?
            .ok_or_else(|| format!("文档 {} 摘要存在但正文缺失", s.id))?;
        docs.push(doc);
    }
    Ok(docs)
}

/// 全部文档重嵌入（批量，复用 KB 自身的嵌入引擎）。
async fn embed_all(
    kb: &std::sync::Arc<knowledge_base::KnowledgeBase>,
    kb_docs: &[knowledge_base::Document],
) -> Result<Vec<Vec<f32>>, String> {
    let mut vectors = Vec::with_capacity(kb_docs.len());
    for chunk in kb_docs.chunks(EMBED_BATCH) {
        let texts: Vec<String> = chunk.iter().map(|d| d.content.clone()).collect();
        let vs = kb
            .embed_texts(&texts)
            .await
            .map_err(|e| format!("嵌入失败: {e}"))?;
        vectors.extend(vs);
    }
    Ok(vectors)
}

async fn run_generate(args: &Args) -> Result<(), String> {
    let manager = KnowledgeBaseManager::load(&args.knowledge_dir)
        .await
        .map_err(|e| format!("加载 knowledge 目录失败: {e}"))?;
    let kb = manager
        .get_kb(&args.kb)
        .await
        .ok_or_else(|| format!("知识库不存在: {}", args.kb))?;

    let docs = load_docs(&kb).await?;
    if docs.len() < 2 {
        return Err(format!("库内仅 {} 条文档，无法做全对比较", docs.len()));
    }
    println!("共 {} 条文档，开始重嵌入…", docs.len());
    let vectors = embed_all(&kb, &docs).await?;

    // 全对余弦，降序排列
    let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
    for i in 0..docs.len() {
        for j in (i + 1)..docs.len() {
            pairs.push((i, j, cosine_similarity(&vectors[i], &vectors[j])));
        }
    }
    pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // 候选对：阈值地板以上全取（上限 300），地板以下按等步长采样负例
    let mut candidates: Vec<CandidatePair> = Vec::new();
    for &(i, j, cos) in &pairs {
        if cos >= THRESHOLD_FLOOR && candidates.len() < 300 {
            candidates.push(make_pair(&docs, i, j, cos));
        }
    }
    let below: Vec<&(usize, usize, f32)> = pairs
        .iter()
        .filter(|&&(_, _, cos)| cos < THRESHOLD_FLOOR)
        .collect();
    if !below.is_empty() {
        let stride = (below.len() / NEGATIVE_SAMPLE_COUNT).max(1);
        for &&(i, j, cos) in below.iter().step_by(stride) {
            candidates.push(make_pair(&docs, i, j, cos));
        }
    }

    // 打印分布直方图，供报告引用
    print_histogram(&pairs);

    // 占位阈值下的连通分量预览（≥3 条的组才列出），帮助人工标注时聚焦
    let clusters = connected_component_clusters(&vectors, PREVIEW_CLUSTER_COS);
    let multi: Vec<&Vec<usize>> = clusters.iter().filter(|c| c.len() >= 3).collect();
    println!(
        "\n占位阈值 {PREVIEW_CLUSTER_COS} 下的聚类预览：{} 组（≥3 条）",
        multi.len()
    );
    for c in multi.iter().take(10) {
        println!("  组 [{} 条]：", c.len());
        for &idx in c.iter().take(6) {
            println!("    - {} | {}", docs[idx].id, docs[idx].title);
        }
    }

    let json = serde_json::to_string_pretty(&candidates).map_err(|e| e.to_string())?;
    std::fs::write(&args.out, json).map_err(|e| format!("写入 {} 失败: {e}", args.out.display()))?;
    println!(
        "\n已导出 {} 对候选到 {}（label 全为 null，人工标注为 0/1 后用 --mode calibrate 标定）",
        candidates.len(),
        args.out.display()
    );
    Ok(())
}

fn make_pair(docs: &[knowledge_base::Document], i: usize, j: usize, cos: f32) -> CandidatePair {
    CandidatePair {
        doc_id_a: docs[i].id.clone(),
        doc_id_b: docs[j].id.clone(),
        title_a: docs[i].title.clone(),
        title_b: docs[j].title.clone(),
        cosine: cos,
        text_a: docs[i].content.clone(),
        text_b: docs[j].content.clone(),
        label: None,
    }
}

fn print_histogram(pairs: &[(usize, usize, f32)]) {
    println!("\n全对余弦分布直方图（0.05 桶宽）：");
    let mut buckets = std::collections::BTreeMap::<i32, usize>::new();
    for &(_, _, cos) in pairs {
        *buckets.entry((cos / 0.05).floor() as i32).or_default() += 1;
    }
    for (bucket, count) in buckets {
        let lo = bucket as f32 * 0.05;
        println!("  [{lo:+.2}, {:+.2}) : {}", lo + 0.05, count);
    }
}

async fn run_calibrate(args: &Args) -> Result<(), String> {
    let annotated_path = args
        .annotated
        .as_ref()
        .ok_or("calibrate 模式需要 --annotated <已标注清单>")?;
    let raw = std::fs::read_to_string(annotated_path)
        .map_err(|e| format!("读取 {} 失败: {e}", annotated_path.display()))?;
    let pairs: Vec<CandidatePair> =
        serde_json::from_str(&raw).map_err(|e| format!("解析标注清单失败: {e}"))?;

    let labeled: Vec<&CandidatePair> = pairs.iter().filter(|p| p.label.is_some()).collect();
    let positives = labeled.iter().filter(|p| p.label == Some(1)).count();
    println!(
        "共 {} 对，其中已标注 {} 对（正例 {positives}，负例 {}）",
        pairs.len(),
        labeled.len(),
        labeled.len() - positives
    );
    if labeled.is_empty() {
        return Err("标注清单中没有已标注的对（label 全为 null）".into());
    }
    if positives < 5 {
        return Err(format!(
            "正例过少（{positives} < 5），PR 曲线不可信 — 请核对标注"
        ));
    }

    // 阈值网格 → PR 表
    let mut rows: Vec<(f32, f64, f64, f64)> = Vec::new(); // (threshold, precision, recall, f1)
    let mut t = THRESHOLD_FLOOR;
    while t <= THRESHOLD_CEIL {
        let predicted = labeled
            .iter()
            .filter(|p| p.cosine >= t)
            .collect::<Vec<_>>();
        let tp = predicted.iter().filter(|p| p.label == Some(1)).count();
        let precision = if predicted.is_empty() {
            1.0
        } else {
            tp as f64 / predicted.len() as f64
        };
        let recall = if positives == 0 {
            0.0
        } else {
            tp as f64 / positives as f64
        };
        let f1 = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };
        rows.push((t, precision, recall, f1));
        t += 0.01;
    }

    // 操作点建议
    let best_f1 = rows
        .iter()
        .max_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
        .expect("阈值网格非空");
    let dedup = rows
        .iter()
        .find(|(_, p, _, _)| *p >= 0.98)
        .unwrap_or(rows.last().expect("阈值网格非空"));

    let mut report = String::new();
    report.push_str("# 相似度阈值标定报告\n\n");
    report.push_str(&format!(
        "- 标注对总数：{}（正例 {positives}，负例 {}）\n",
        labeled.len(),
        labeled.len() - positives
    ));
    report.push_str("- 嵌入模型：KB 配置默认（bge-small-zh-v1.5, 512 维）\n\n");
    report.push_str("| 阈值 | precision | recall | F1 |\n|---|---|---|---|\n");
    for (t, p, r, f1) in &rows {
        report.push_str(&format!("| {t:.2} | {p:.3} | {r:.3} | {f1:.3} |\n"));
    }
    report.push_str(&format!(
        "\n## 建议操作点\n\n- `min_cluster_cos`（聚类分组，F1 最优）：**{:.2}**\n- `dedup_cos`（硬删除，precision ≥ 0.98 的最小阈值）：**{:.2}**\n",
        best_f1.0, dedup.0
    ));

    std::fs::write(&args.out, &report)
        .map_err(|e| format!("写入 {} 失败: {e}", args.out.display()))?;
    println!("{}", report);
    println!("报告已写入 {}", args.out.display());
    Ok(())
}
