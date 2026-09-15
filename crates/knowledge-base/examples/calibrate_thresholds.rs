//! 相似度阈值标定脚本。
//!
//! 把 `ConsolidationConfig` 的 `min_cluster_cos` / `dedup_cos` 从占位默认值
//! 变成有数据来源的操作点。三个模式：
//!
//! 1. **generate** — 读取真实记忆库，全部文档重嵌入后计算全对余弦分布，
//!    导出待人工标注的候选对（正例为主 + 少量跨阈值负例）；
//! 2. **calibrate** — 读回人工填好 0/1 标签的清单，按阈值网格计算
//!    PR 曲线，输出 `min_cluster_cos`（F1 最优）与 `dedup_cos`
//!    （precision ≥ 0.98 的最小阈值，硬删除必须高精度）候选操作点。
//! 3. **synthetic** — 真实库无近重复样本时的替代路径：读入人工编写的合成
//!    真值对 CSV，嵌入后逐对算余弦，产出与 generate 同 schema 的标注 JSON
//!    （label 已填），再交给 calibrate 出报告。
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
//!
//! # 替代路径：合成标定集（真实库无近重复样本时）
//! cargo run -p knowledge-base --example calibrate_thresholds -- \
//!     --mode synthetic --pairs reports/data/synthetic_calibration_pairs.csv \
//!     --out /tmp/t1/annotated_synthetic.json
//! # 换 base 模型重跑同一合成集（模型名随标注 JSON 透传给报告）：
//! cargo run -p knowledge-base --example calibrate_thresholds -- \
//!     --mode synthetic --model base --pairs reports/data/synthetic_calibration_pairs.csv \
//!     --out /tmp/t1/annotated_synthetic_base.json
//! # 然后复用既有 calibrate 模式出报告：
//! cargo run -p knowledge-base --example calibrate_thresholds -- \
//!     --knowledge-dir <任意非空库> --kb @private_memory --mode calibrate \
//!     --annotated /tmp/t1/annotated_synthetic.json --out reports/p2-t3-synthetic-calibration.md
//! ```
//!
//! 三个模式的 `--out` 均可省略，省略时取各自默认值：generate →
//! `annotation_candidates.json`，calibrate → `calibration_report.md`，
//! synthetic → `annotated_synthetic.json`。
//!
//! 注：calibrate 模式只消费标注 JSON 里的 cosine 字段，不重嵌入，
//! `--knowledge-dir` / `--kb` 在该模式下仅为兼容而接受（可省略）。
//!
//! 注：`--model` 只影响 synthetic 模式（决定临时库的嵌入引擎）。在其它模式下
//! 显式传入会报错 — 那两种模式的嵌入要么由真实库配置决定，要么根本不重嵌入，
//! 静默忽略会让「以为跑了 base」的错误结论混进报告。

use std::collections::HashMap;
use std::path::PathBuf;

use knowledge_base::KnowledgeBaseManager;
use knowledge_base::calibration::parse_labeled_pairs;
use knowledge_base::engine::{connected_component_clusters, cosine_similarity};
use knowledge_base::manager::config::{
    BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig,
};

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
    /// 产出这批余弦的嵌入模型名。仅 synthetic 模式写入（generate 走真实库配置，
    /// 由 KB 自己记录）；`skip_serializing_if` 保证 generate 的导出格式不变，
    /// calibrate 读到旧文件（无此字段）时退回默认描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

const THRESHOLD_FLOOR: f32 = 0.50;
const THRESHOLD_CEIL: f32 = 0.99;
const NEGATIVE_SAMPLE_COUNT: usize = 100;
const EMBED_BATCH: usize = 64;
/// 聚类预览阈值：与回填后的 min_cluster_cos 标定值保持一致（0.77，见
/// reports/p2-t3-synthetic-calibration.md），预览才有「放量后真实效果」的参考价值。
const PREVIEW_CLUSTER_COS: f32 = 0.77;
/// synthetic 模式临时知识库名（用完即删，不落用户 workspace）。
const SYNTHETIC_KB: &str = "@synthetic_calibration";

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
        Mode::Synthetic => rt.block_on(run_synthetic(&args)),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

enum Mode {
    Generate,
    Calibrate,
    Synthetic,
}

/// synthetic 模式的嵌入模型选项 — 决定临时知识库用哪套权重算余弦。
///
/// 同一合成集换模型重跑即可得到同尺对比：small（512 维，fastembed 内置清单）
/// 与 base（768 维，走 `Xenova/bge-base-zh-v1.5` 的 user-defined ONNX 路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SyntheticModel {
    /// 默认 small — 与回填到 KB 配置的模型一致，保持既有行为不变。
    #[default]
    Small,
    Base,
}

impl SyntheticModel {
    /// 人类可读模型名 — 写进标注 JSON 并透传到标定报告，供两份报告对照。
    fn name(self) -> &'static str {
        match self {
            SyntheticModel::Small => "bge-small-zh-v1.5",
            SyntheticModel::Base => "bge-base-zh-v1.5",
        }
    }

    /// 模型对应的 KB 嵌入配置枚举 — `KnowledgeBase` 据此构造嵌入引擎。
    fn kb_model(self) -> FastembedModelTypeSerde {
        match self {
            SyntheticModel::Small => FastembedModelTypeSerde::BGESmallZHV15,
            SyntheticModel::Base => FastembedModelTypeSerde::BGEBaseZHV15,
        }
    }
}

struct Args {
    /// 仅 generate 模式需要。
    knowledge_dir: Option<PathBuf>,
    /// 仅 generate 模式需要。
    kb: Option<String>,
    /// 仅 synthetic 模式需要。
    pairs: Option<PathBuf>,
    mode: Mode,
    /// 仅 synthetic 模式生效。
    model: SyntheticModel,
    out: PathBuf,
    annotated: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut knowledge_dir = None;
    let mut kb = None;
    let mut pairs = None;
    let mut mode = Mode::Generate;
    let mut model = SyntheticModel::default();
    let mut model_given = false;
    let mut out = None;
    let mut annotated = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--knowledge-dir" => {
                knowledge_dir = Some(it.next().ok_or("--knowledge-dir 需要一个路径")?)
            }
            "--kb" => kb = Some(it.next().ok_or("--kb 需要一个名称")?),
            "--pairs" => pairs = Some(PathBuf::from(it.next().ok_or("--pairs 需要一个路径")?)),
            "--mode" => {
                mode = match it
                    .next()
                    .ok_or("--mode 需要 generate、calibrate 或 synthetic")?
                    .as_str()
                {
                    "generate" => Mode::Generate,
                    "calibrate" => Mode::Calibrate,
                    "synthetic" => Mode::Synthetic,
                    other => {
                        return Err(format!(
                            "未知 mode: {other}（可选 generate | calibrate | synthetic）"
                        ));
                    }
                }
            }
            "--model" => {
                model = match it.next().ok_or("--model 需要 small 或 base")?.as_str() {
                    "small" => SyntheticModel::Small,
                    "base" => SyntheticModel::Base,
                    other => {
                        return Err(format!("未知 model: {other}（可选 small | base）"));
                    }
                };
                model_given = true;
            }
            "--out" => out = Some(PathBuf::from(it.next().ok_or("--out 需要一个路径")?)),
            "--annotated" => {
                annotated = Some(PathBuf::from(it.next().ok_or("--annotated 需要一个路径")?))
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }

    let out = match mode {
        Mode::Generate => out.unwrap_or_else(|| PathBuf::from("annotation_candidates.json")),
        Mode::Calibrate => out.unwrap_or_else(|| PathBuf::from("calibration_report.md")),
        Mode::Synthetic => out.unwrap_or_else(|| PathBuf::from("annotated_synthetic.json")),
    };

    // 只有 generate 需要真实知识库；calibrate 只读标注 JSON，synthetic 只读 CSV
    if matches!(mode, Mode::Generate) {
        if knowledge_dir.is_none() {
            return Err("缺少 --knowledge-dir（workspace 的 knowledge/ 目录）".into());
        }
        if kb.is_none() {
            return Err("缺少 --kb（如 @private_memory）".into());
        }
    }
    // generate 的嵌入模型由真实库配置决定，calibrate 根本不重嵌入 — 两种模式下
    // 显式传 --model 都无处生效，报错而不是静默忽略
    if model_given && !matches!(mode, Mode::Synthetic) {
        return Err("--model 仅对 synthetic 模式有效".into());
    }

    Ok(Args {
        knowledge_dir: knowledge_dir.map(PathBuf::from),
        kb,
        pairs,
        mode,
        model,
        out,
        annotated,
    })
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
    let knowledge_dir = args
        .knowledge_dir
        .as_ref()
        .ok_or("generate 模式需要 --knowledge-dir（workspace 的 knowledge/ 目录）")?;
    let kb_name = args
        .kb
        .as_ref()
        .ok_or("generate 模式需要 --kb（如 @private_memory）")?;

    let manager = KnowledgeBaseManager::load(knowledge_dir)
        .await
        .map_err(|e| format!("加载 knowledge 目录失败: {e}"))?;
    let kb = manager
        .get_kb(kb_name)
        .await
        .ok_or_else(|| format!("知识库不存在: {kb_name}"))?;

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
    std::fs::write(&args.out, json)
        .map_err(|e| format!("写入 {} 失败: {e}", args.out.display()))?;
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
        model: None,
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
        let predicted = labeled.iter().filter(|p| p.cosine >= t).collect::<Vec<_>>();
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
    // 标注 JSON 自带模型名（synthetic 写入）时如实标注；旧文件无此字段，
    // 退回 generate 时代的默认描述
    match pairs.iter().find_map(|p| p.model.as_deref()) {
        Some(m) => report.push_str(&format!("- 嵌入模型：{m}（合成集实算余弦）\n\n")),
        None => report.push_str("- 嵌入模型：KB 配置默认（bge-base-zh-v1.5, 768 维）\n\n"),
    }
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

/// synthetic 模式：合成真值对 CSV → 嵌入 → 余弦 → 标注 JSON（label 已填）。
///
/// 产出的 JSON 与 generate 模式同 schema，可直接喂给 calibrate 模式；
/// calibrate 只读其中的 cosine 字段，不重嵌入。
async fn run_synthetic(args: &Args) -> Result<(), String> {
    let pairs_path = args
        .pairs
        .as_ref()
        .ok_or("synthetic 模式需要 --pairs <CSV 路径>")?;
    let raw = std::fs::read_to_string(pairs_path)
        .map_err(|e| format!("读取 {} 失败: {e}", pairs_path.display()))?;
    let rows = parse_labeled_pairs(&raw)
        .map_err(|e| format!("解析 {} 失败: {e}", pairs_path.display()))?;

    let positives = rows.iter().filter(|p| p.label == 1).count();
    println!(
        "已解析 {} 对合成样本（{}）：正例 {positives}，负例 {}",
        rows.len(),
        pairs_path.display(),
        rows.len() - positives
    );

    // 两列文本合并去重 — 同一段文本只嵌入一次；owner 记录首个使用者，供嵌入失败时报出 pair_id
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut owner: HashMap<String, String> = HashMap::new();
    let mut texts: Vec<String> = Vec::new();
    for p in &rows {
        for t in [&p.text_a, &p.text_b] {
            if index.contains_key(t) {
                continue;
            }
            index.insert(t.clone(), texts.len());
            owner.insert(t.clone(), p.pair_id.clone());
            texts.push(t.clone());
        }
    }
    println!("去重后待嵌入文本 {} 条", texts.len());
    println!("嵌入模型：{}（--model）", args.model.name());

    // 临时库：只为借它的嵌入引擎（InMemory 后端，不落盘向量表），用完连目录一起删
    let temp_dir = make_temp_dir()?;
    let _guard = TempDirGuard(temp_dir.clone());
    let manager = KnowledgeBaseManager::load(&temp_dir)
        .await
        .map_err(|e| format!("创建临时知识库目录失败: {e}"))?;
    let kb = manager
        .create_kb(KbConfig {
            name: SYNTHETIC_KB.into(),
            description: "合成标定集临时库（用完即删）".into(),
            embedding_model: args.model.kb_model(),
            chunking: ChunkingStrategySerde::default(),
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
        })
        .await
        .map_err(|e| format!("创建临时知识库失败: {e}"))?;

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(EMBED_BATCH) {
        let batch: Vec<String> = chunk.to_vec();
        let vs = kb.embed_texts(&batch).await.map_err(|e| {
            let pair_id = owner.get(&chunk[0]).map(String::as_str).unwrap_or("?");
            format!(
                "嵌入失败（pair_id {pair_id} 起的一批 {} 条，样本 {:?}）: {e}",
                chunk.len(),
                truncate_chars(&chunk[0], 40)
            )
        })?;
        vectors.extend(vs);
    }

    let mut out: Vec<CandidatePair> = Vec::with_capacity(rows.len());
    for p in &rows {
        let va = lookup_vector(&vectors, &index, &p.text_a, &p.pair_id)?;
        let vb = lookup_vector(&vectors, &index, &p.text_b, &p.pair_id)?;
        out.push(CandidatePair {
            doc_id_a: format!("{}_a", p.pair_id),
            doc_id_b: format!("{}_b", p.pair_id),
            title_a: p.category.clone(),
            title_b: p.category.clone(),
            cosine: cosine_similarity(va, vb),
            text_a: p.text_a.clone(),
            text_b: p.text_b.clone(),
            label: Some(p.label),
            model: Some(args.model.name().to_string()),
        });
    }

    print_synthetic_summary(&out, args.model);

    let json = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    std::fs::write(&args.out, json)
        .map_err(|e| format!("写入 {} 失败: {e}", args.out.display()))?;
    println!(
        "\n已导出 {} 对（label 已填）到 {}，可用 --mode calibrate 出阈值报告",
        out.len(),
        args.out.display()
    );
    Ok(())
}

/// 取某段文本的向量（缺失即为内部错误 — 去重阶段应保证全覆盖）。
fn lookup_vector<'a>(
    vectors: &'a [Vec<f32>],
    index: &HashMap<String, usize>,
    text: &str,
    pair_id: &str,
) -> Result<&'a [f32], String> {
    let i = index
        .get(text)
        .ok_or_else(|| format!("内部错误：pair_id {pair_id} 的文本未参与嵌入"))?;
    vectors
        .get(*i)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("内部错误：pair_id {pair_id} 的向量下标 {i} 越界"))
}

/// 按 label 分组打印 cosine 分布 — 正例均值应明显高于负例，用于快速 sanity check。
///
/// 模型名打在首行：同一合成集换模型重跑时，两份输出靠这一行区分。
fn print_synthetic_summary(pairs: &[CandidatePair], model: SyntheticModel) {
    println!("\n合成标定集 cosine 统计：");
    println!("  模型：{}", model.name());
    println!("  总对数：{}", pairs.len());
    let mut means = [0.0f32; 2];
    for (label, name) in [(1u8, "正例"), (0u8, "负例")] {
        let cos: Vec<f32> = pairs
            .iter()
            .filter(|p| p.label == Some(label))
            .map(|p| p.cosine)
            .collect();
        if cos.is_empty() {
            println!("  {name}（label={label}）：0 对");
            continue;
        }
        let min = cos.iter().copied().fold(f32::INFINITY, f32::min);
        let max = cos.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mean = cos.iter().sum::<f32>() / cos.len() as f32;
        means[label as usize] = mean;
        println!(
            "  {name}（label={label}）：{} 对 | min {min:.4} | max {max:.4} | 均值 {mean:.4}",
            cos.len()
        );
    }
    if means[0] > 0.0 && means[1] > 0.0 && means[1] <= means[0] {
        println!("  ⚠ 正例均值未高于负例均值，标定集区分度可疑 — 请核对标注或样本");
    }
}

/// 在系统临时目录下建一个唯一子目录（进程 id + 纳秒时间戳）。
fn make_temp_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "peco_synthetic_calib_{}_{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建临时目录 {} 失败: {e}", dir.display()))?;
    Ok(dir)
}

/// 离开作用域时递归删除临时目录（含 KB 写出的 kb_config.json）。
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("warn: 清理临时目录 {} 失败: {e}", self.0.display());
        }
    }
}

/// 按字符（而非字节）截断，避免在多字节 UTF-8 边界上断开。
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}
