// ============================================================================
// ConsolidationWorker — 自动记忆整理（巩固流水线）
// ============================================================================
//
// 与 Layer 2（@memory agent 手动整理）的分工：本 worker 是批量深度整理
// 的唯一执行者——机器判定（向量聚类/硬去重）+ Flash 沉淀 + TTL 清理。
// 触发方（REST 手动 / cron）只负责调用 `run_once`，不经 agent 通道。
//
// 流水线（每轮顺序）：
//   ① 候选收集：分页扫描 → 过滤 ppa_episodic/ppa_semantic → 水位窗口选择
//   ② 主题聚类：候选重嵌入 → 进程内两两余弦 → 连通分量（min_cluster_cos）
//   ③ 沉淀：≥3 条 episodic 的组经 Flash 归纳为一条 semantic（原组交⑤判定）
//   ④ 硬去重：组内 cos ≥ dedup_cos 的对保留最新一条，其余硬删 + 审计
//   ⑤ TTL 清理：已沉淀 + 超 episodic_ttl_days 的 episodic 硬删 + 审计
//   ⑥ 审计保留期清理：超 audit_retention_days 的终态审计行物理清除
//   ⑦ 图谱补边：图后端持久化迁移验收前显式跳过
//   ⑧ 状态回写：删除走 outbox（pending→done/cancelled）；回写
//      memory_consolidation_state 水位与统计
//
// 候选水位（`last_scanned_ts`）：已覆盖区间的下界 W —— `sort_key >= W`
// 的条目至少被完整扫描过一轮，`sort_key < W` 是尚未覆盖的存量 backlog。
// 窗口先取 backlog 降序（越老越先补齐），不足部分由最新条目补齐；水位
// 单调向下推进，追平后 backlog 恒空 → 回落「取最近 batch_size 条」。
//
// 安全护栏：
//   - ppa_profile 永不进候选池（① 过滤）；自动删除仅限 ④⑤ 两类
//   - ppa_semantic 只参与硬去重，不参与沉淀删除
//   - 审计先行：审计写入失败（存储不可用）→ 拒绝删除（fail-closed）
//   - 无数据不判定：时间源（created_at → title 毫秒 → captured-at footer）
//     皆无 → 不参与 TTL
//   - 机器判定基建（嵌入/聚类）不可用 → 跳过 ②③④ 并 warn，仅执行 ①⑤⑥⑧
//   - 任一步失败 warn 后继续，不中断整轮；成本上限 max_llm_calls（Flash 档）

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use peco_core::knowledge::KnowledgeManager;
use peco_core::tools::MemoryAuditEntry;

use super::config::ConsolidationConfig;

/// 删除审计的执行者标识。
pub const DELETED_BY_WORKER: &str = "worker";

/// 硬去重删除的审计 reason。
pub const REASON_DEDUP: &str = "consolidation_dedup";

/// TTL 清理删除的审计 reason。
pub const REASON_TTL: &str = "consolidation_ttl";

/// 沉淀写入的 semantic 文档内容尾部溯源标记：解析后即得原组 doc_id
/// （TTL 判定「已沉淀」的依据）。doc_id 为 16 位 hex，逗号分隔。
///
/// 用内容 footer 而非 metadata 承载溯源：LanceDB 后端不持久化
/// metadata，footer 随内容存储跨重启可靠。
pub const MERGED_FROM_PREFIX: &str = "[merged-from: ";

/// 内容尾部捕获时刻标记（RFC 3339）：四级时间源的最后一级。
///
/// 合并写入的内容标题未必是 `memory_{millis}_{seq}` 格式（@memory agent
/// 归纳时标题自由生成），此时正文 footer 是唯一可读的时间源。
pub const CAPTURED_AT_PREFIX: &str = "[captured-at: ";

/// 一轮整理的统计（写回 `memory_consolidation_state.last_run_stats`）。
#[derive(Debug, Default, Serialize)]
pub struct RunStats {
    /// 扫描到的 episodic/semantic 文档总数。
    pub scanned: usize,
    /// 进入机器判定流程的候选条数（≤ batch_size）。
    pub candidates: usize,
    /// 聚类得到的 ≥2 条分组数。
    pub clustered_groups: usize,
    /// ③ 沉淀写入的 semantic 条数。
    pub merged: usize,
    /// ④ 硬去重删除条数。
    pub dedup_deleted: usize,
    /// ⑤ TTL 清理删除条数。
    pub ttl_deleted: usize,
    /// ⑥ 审计保留期清理删除的终态审计行数。
    pub audit_purged: usize,
    /// 本轮 Flash 调用次数。
    pub llm_calls: usize,
    /// 机器判定步骤被跳过的原因（嵌入/聚类基建不可用时非空）。
    pub machine_steps_skipped: Option<String>,
}

/// worker 整轮级错误（候选收集或水位写入不可用）。
#[derive(Debug)]
pub struct WorkerError(pub String);

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "consolidation worker: {}", self.0)
    }
}

impl std::error::Error for WorkerError {}

/// 单条候选的运行时视图。
#[derive(Debug, Clone)]
struct Candidate {
    doc_id: String,
    time: Option<DateTime<Utc>>,
}

impl Candidate {
    /// 排序键：无时刻视为最旧（保守参与机器判定，不主导"保留最新"）。
    fn sort_key(&self) -> DateTime<Utc> {
        self.time.unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }
}

/// ① 一轮候选窗口：入选候选 + 本轮推进后的水位 + 扫描基数。
#[derive(Debug)]
struct CandidateWindow {
    /// 扫描到的 episodic/semantic 文档总数。
    scanned: usize,
    /// 入选候选（≤ batch_size）。
    candidates: Vec<Candidate>,
    /// 本轮推进后的水位；无候选时保留既有水位（`None` = 从未有过水位）。
    watermark: Option<DateTime<Utc>>,
}

/// ① 候选窗口选择（纯函数）。
///
/// `watermark`（W）是已覆盖区间的下界：`sort_key >= W` 的条目至少被完整
/// 扫描过一轮（fresh），`sort_key < W` 是尚未覆盖的存量（backlog，降序
/// 即越老越先补齐）。窗口先吃 backlog，不足部分由 fresh 降序补齐 ——
/// 存量再大也不会饿死老条目，追平后 backlog 恒空即回落「取最近」。
/// 无水位（首轮）→ backlog 为空，等价于旧的「最近 batch_size 条」行为。
///
/// 新水位取 `min(旧水位, 入选最老 sort_key)`：只向下推进，已有覆盖
/// 不回退（避免下一轮重复扫描同一批 backlog 造成震荡）。
fn select_window(
    all: &[Candidate],
    watermark: Option<DateTime<Utc>>,
    batch_size: usize,
) -> CandidateWindow {
    let mut sorted: Vec<Candidate> = all.to_vec();
    sorted.sort_by_key(|c| std::cmp::Reverse(c.sort_key()));

    // partition 保序 → backlog / fresh 各自仍是降序（最新在前）
    let (backlog, fresh): (Vec<Candidate>, Vec<Candidate>) = match watermark {
        Some(w) => sorted.into_iter().partition(|c| c.sort_key() < w),
        None => (Vec::new(), sorted),
    };

    let mut candidates: Vec<Candidate> = backlog.into_iter().take(batch_size).collect();
    if candidates.len() < batch_size {
        let fill = batch_size - candidates.len();
        candidates.extend(fresh.into_iter().take(fill));
    }

    let watermark = match candidates.iter().map(|c| c.sort_key()).min() {
        Some(t_oldest) => Some(watermark.map_or(t_oldest, |w| w.min(t_oldest))),
        // 无候选：本轮不含新信息，保留既有水位
        None => watermark,
    };

    CandidateWindow {
        scanned: all.len(),
        candidates,
        watermark,
    }
}

/// 文档时间源（四级）：`metadata.created_at`（ISO 8601）→ title
/// `memory_{millis}_{seq}` 毫秒 → 正文 `[captured-at: ...]` footer → `None`。
///
/// 注：LanceDB 后端不持久化 metadata（get_document 重建默认值），
/// 生产路径实际生效的是 hook 写入的 title 毫秒解析；@memory agent 合并
/// 写入的文档标题自拟，退到 footer 一级。
pub fn doc_time(doc: &knowledge_base::Document) -> Option<DateTime<Utc>> {
    if let Some(created_at) = &doc.metadata.created_at
        && let Ok(t) = DateTime::parse_from_rfc3339(created_at)
    {
        return Some(t.with_timezone(&Utc));
    }
    parse_title_millis(&doc.title)
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| parse_captured_at(&doc.content))
}

/// 从 title `memory_{millis}_{seq}` 解析毫秒时间戳。
pub fn parse_title_millis(title: &str) -> Option<i64> {
    let rest = title.strip_prefix("memory_")?;
    let millis_str = rest.split('_').next()?;
    millis_str.parse::<i64>().ok()
}

/// TTL 到期判定（纯函数，显式传 `now`）。
///
/// 时间源不可得（`doc_time == None`）→ 不清理（无数据不判定）。
pub fn is_ttl_expired(doc: &knowledge_base::Document, now: DateTime<Utc>, ttl_days: u64) -> bool {
    doc_time(doc)
        .map(|t| now.signed_duration_since(t).num_days() >= ttl_days as i64)
        .unwrap_or(false)
}

/// 解析沉淀文档内容尾部的溯源标记，返回原组 doc_id 列表。
///
/// 取最后一条匹配行；无标记 → 空列表（视为未沉淀，TTL 保守跳过）。
pub fn parse_merged_from(content: &str) -> Vec<String> {
    content
        .lines()
        .rev()
        .find_map(|line| {
            let rest = line.trim().strip_prefix(MERGED_FROM_PREFIX)?;
            let ids = rest.strip_suffix(']')?;
            Some(
                ids.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default()
}

/// 解析内容尾部的捕获时刻标记（RFC 3339 ISO 8601）。
///
/// 取最后一条匹配行（与 `parse_merged_from` 同模式）；时间串解析失败的行
/// 忽略并继续向前找，全无合法行 → `None`（回落既有优先级链）。与
/// merged-from footer 并存时各认各的前缀，互不干扰。
pub fn parse_captured_at(content: &str) -> Option<DateTime<Utc>> {
    content.lines().rev().find_map(|line| {
        let rest = line.trim().strip_prefix(CAPTURED_AT_PREFIX)?;
        let ts = rest.strip_suffix(']')?;
        DateTime::parse_from_rfc3339(ts.trim())
            .ok()
            .map(|t| t.with_timezone(&Utc))
    })
}

/// ④ 纯函数：组内硬去重受害者挑选。
///
/// 对组内每对相似度 ≥ `dedup_cos` 的组合，较旧一条（时间源不可比时
/// content 较短一条）入受害者集合；已入集合的成员不再参与后续配对。
/// 由此组内最新成员永不成为受害者。
pub fn pick_dedup_victims(
    cluster: &[usize],
    docs: &[knowledge_base::Document],
    vectors: &[Vec<f32>],
    dedup_cos: f32,
) -> Vec<usize> {
    let mut victims: Vec<usize> = Vec::new();
    for (a_idx, &i) in cluster.iter().enumerate() {
        if victims.contains(&i) {
            continue;
        }
        for &j in &cluster[a_idx + 1..] {
            if victims.contains(&j) {
                continue;
            }
            let (Some(di), Some(dj)) = (docs.get(i), docs.get(j)) else {
                continue;
            };
            let (Some(vi), Some(vj)) = (vectors.get(i), vectors.get(j)) else {
                continue;
            };
            if knowledge_base::engine::cosine_similarity(vi, vj) < dedup_cos {
                continue;
            }
            let victim = match (doc_time(di), doc_time(dj)) {
                (Some(ti), Some(tj)) => {
                    if ti <= tj {
                        i
                    } else {
                        j
                    }
                }
                // 时间不可比 → 保留 content 更长者
                _ => {
                    if di.content.len() >= dj.content.len() {
                        j
                    } else {
                        i
                    }
                }
            };
            victims.push(victim);
        }
    }
    victims
}

/// Flash 沉淀器抽象 — 便于测试注入 mock。
#[async_trait::async_trait]
pub trait Distiller: Send + Sync {
    /// 把一组同主题记忆原文归纳为一条稳定事实文本。
    async fn distill(&self, group_texts: &[String]) -> Result<String, String>;
}

/// 基于 [`model_provider::ModelProvider`] 的沉淀器（Flash 档，关闭 reasoning）。
pub struct ModelDistiller {
    provider: Arc<dyn model_provider::ModelProvider>,
    model: String,
}

const DISTILLER_SYSTEM_PROMPT: &str = r#"You are the personal assistant's memory consolidator. You receive a group of episodic memory entries about the same topic. Distill them into ONE concise statement of the stable, long-term fact they collectively establish.

Rules:
1. Output the statement only — one or two sentences, in Chinese, stating the settled fact (not the history);
2. Do not list the inputs, do not add commentary, do not wrap in quotes or markdown;
3. If the entries contradict each other, prefer the most recent information."#;

impl ModelDistiller {
    pub fn new(provider: Arc<dyn model_provider::ModelProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

#[async_trait::async_trait]
impl Distiller for ModelDistiller {
    async fn distill(&self, group_texts: &[String]) -> Result<String, String> {
        let mut user_content = String::from("[Memory entries]\n");
        for (i, text) in group_texts.iter().enumerate() {
            user_content.push_str(&format!("{}. {text}\n", i + 1));
        }

        let request = model_provider::GenerateRequest {
            model: self.model.clone(),
            instructions: Some(DISTILLER_SYSTEM_PROMPT.to_string()),
            input: vec![Arc::new(model_provider::InputItem::Message {
                role: model_provider::Role::User,
                content: user_content.into(),
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: Some(256),
            // 沉淀是简单归纳 — 关闭 thinking 降低延迟与成本
            reasoning: Some(model_provider::ReasoningConfig {
                enabled: false,
                effort: None,
            }),
            text: None,
            additional_params: None,
        };

        let result = self
            .provider
            .generate_full(&request)
            .await
            .map_err(|e| format!("distiller model call failed: {e}"))?;

        if result.status != model_provider::ResponseStatus::Completed {
            return Err(format!(
                "distiller generation incomplete: status={:?}",
                result.status
            ));
        }

        let text: String = result
            .output
            .iter()
            .filter_map(|block| match block {
                model_provider::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("distiller returned empty output".into());
        }
        Ok(text)
    }
}

/// 自动整理 worker。
pub struct ConsolidationWorker {
    km: Arc<KnowledgeManager>,
    db: SqlitePool,
    config: ConsolidationConfig,
    kb_name: String,
    distiller: Arc<dyn Distiller>,
}

impl ConsolidationWorker {
    /// 构造 worker。`model` 为 Flash 档模型名（复用 MemoryConfig.model）。
    pub fn new(
        km: Arc<KnowledgeManager>,
        db: SqlitePool,
        kb_name: impl Into<String>,
        model: impl Into<String>,
        config: ConsolidationConfig,
        provider: Arc<dyn model_provider::ModelProvider>,
    ) -> Self {
        Self {
            km,
            db,
            config,
            kb_name: kb_name.into(),
            distiller: Arc::new(ModelDistiller::new(provider, model)),
        }
    }

    /// 注入自定义沉淀器（测试用）。
    pub fn with_distiller(mut self, distiller: Arc<dyn Distiller>) -> Self {
        self.distiller = distiller;
        self
    }

    /// 跑一轮整理。整轮级失败（候选收集/水位写入）返回 Err；
    /// 步骤级失败 warn 后继续，统计如实反映。
    pub async fn run_once(&self, user_id: &str) -> Result<RunStats, WorkerError> {
        self.run_once_at(user_id, Utc::now()).await
    }

    pub(crate) async fn run_once_at(
        &self,
        user_id: &str,
        now: DateTime<Utc>,
    ) -> Result<RunStats, WorkerError> {
        let mut stats = RunStats::default();

        // ── ① 候选收集（水位窗口）───────────────────────────────────
        let window = self.collect_candidates(user_id).await?;
        let watermark_str = window.watermark.map(|t| t.to_rfc3339());
        stats.scanned = window.scanned;
        stats.candidates = window.candidates.len();
        let candidates = window.candidates;
        tracing::info!(
            user_id = %user_id,
            kb = %self.kb_name,
            scanned = stats.scanned,
            candidates = stats.candidates,
            watermark = watermark_str.as_deref().unwrap_or("none"),
            "Consolidation round started"
        );

        // ── ②③④ 机器判定步骤（嵌入/聚类基建不可用时整体跳过）─────────
        match self.run_machine_steps(&candidates).await {
            Ok((clusters, docs, vectors)) => {
                stats.clustered_groups = clusters.iter().filter(|c| c.len() >= 2).count();
                // ③ 沉淀
                self.distill_groups(user_id, &clusters, &docs, now, &mut stats)
                    .await;
                // ④ 硬去重
                self.dedup_clusters(user_id, &clusters, &docs, &vectors, now, &mut stats)
                    .await;
            }
            Err(reason) => {
                tracing::warn!(
                    user_id = %user_id,
                    reason = %reason,
                    "Machine judgment steps skipped this round"
                );
                stats.machine_steps_skipped = Some(reason);
            }
        }

        // ── ⑤ TTL 清理（不需要向量基建，恒执行）──────────────────────
        self.ttl_cleanup(user_id, now, &mut stats).await;

        // ── ⑥ 审计保留期清理（不需要向量基建，恒执行）────────────────
        self.purge_audit(user_id, now, &mut stats).await;

        // ── ⑦ 图谱补边 — 图后端持久化迁移验收前显式跳过 ──────────────
        tracing::debug!(
            user_id = %user_id,
            "Graph edge backfill suspended until graph backend migration is accepted"
        );

        // ── ⑧ 水位与统计回写 ───────────────────────────────────────
        let stats_json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
        let now_str = now.to_rfc3339();
        crate::db::memory_consolidation_state::upsert_state(
            &self.db,
            user_id,
            watermark_str.as_deref(),
            Some(now_str.as_str()),
            Some(stats_json.as_str()),
        )
        .await
        .map_err(|e| WorkerError(format!("failed to persist consolidation state: {e}")))?;

        tracing::info!(
            user_id = %user_id,
            merged = stats.merged,
            dedup_deleted = stats.dedup_deleted,
            ttl_deleted = stats.ttl_deleted,
            audit_purged = stats.audit_purged,
            llm_calls = stats.llm_calls,
            "Consolidation round finished"
        );
        Ok(stats)
    }

    /// ① 读水位 → 全量扫描 → 窗口选择。
    async fn collect_candidates(&self, user_id: &str) -> Result<CandidateWindow, WorkerError> {
        let watermark = self.read_watermark(user_id).await;
        let summaries = self.list_memory_summaries().await?;
        let all: Vec<Candidate> = summaries
            .iter()
            .map(|s| Candidate {
                time: parse_title_millis(&s.title).and_then(DateTime::from_timestamp_millis),
                doc_id: s.id.clone(),
            })
            .collect();
        Ok(select_window(&all, watermark, self.config.batch_size))
    }

    /// 读候选水位；无行或时间戳解析失败 → `None`（按首轮窗口处理）。
    ///
    /// 读失败只 warn 不中断整轮：水位是防饥饿的调度优化，不是正确性前置，
    /// 降级为「取最近一批」不会造成数据面损伤。
    async fn read_watermark(&self, user_id: &str) -> Option<DateTime<Utc>> {
        let row = match crate::db::memory_consolidation_state::get_state(&self.db, user_id).await {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(
                    user_id = %user_id,
                    error = %e,
                    "Watermark read failed, falling back to most-recent window"
                );
                return None;
            }
        };
        row.and_then(|r| r.last_scanned_ts)
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&Utc))
    }

    /// ① 分页扫描全库，过滤出 episodic/semantic 摘要（profile 永不入池）。
    async fn list_memory_summaries(
        &self,
    ) -> Result<Vec<knowledge_base::DocumentSummary>, WorkerError> {
        const PAGE: usize = 200;
        let mut summaries = Vec::new();
        let mut offset = 0usize;
        loop {
            let batch = self
                .km
                .list_documents(&self.kb_name, offset, PAGE)
                .await
                .map_err(|e| WorkerError(format!("list_documents failed: {e}")))?;
            let done = batch.len() < PAGE;
            offset += batch.len();
            summaries.extend(
                batch
                    .into_iter()
                    .filter(|s| matches!(s.source_path.as_str(), "ppa_episodic" | "ppa_semantic")),
            );
            if done {
                break;
            }
        }
        Ok(summaries)
    }

    /// ② 一次重嵌入贯通机器判定：候选全文 + 向量 + 连通分量。
    ///
    /// 任一环节失败返回跳过原因（不视为整轮失败）。返回的 docs 与
    /// vectors 下标一一对应（并发删除的文档在抓取阶段静默剔除）。
    async fn run_machine_steps(
        &self,
        candidates: &[Candidate],
    ) -> Result<
        (
            Vec<Vec<usize>>,
            Vec<knowledge_base::Document>,
            Vec<Vec<f32>>,
        ),
        String,
    > {
        let mut docs: Vec<knowledge_base::Document> = Vec::with_capacity(candidates.len());
        for c in candidates {
            match self.km.get_document(&self.kb_name, &c.doc_id).await {
                Ok(Some(doc)) => docs.push(doc),
                Ok(None) => {} // 并发删除：本轮跳过
                Err(e) => return Err(format!("get_document({}) failed: {e}", c.doc_id)),
            }
        }
        if docs.len() < 2 {
            return Ok((Vec::new(), docs, Vec::new()));
        }
        let texts: Vec<String> = docs.iter().map(|d| d.content.clone()).collect();
        let vectors = self
            .km
            .embed_texts(&self.kb_name, &texts)
            .await
            .map_err(|e| format!("embedding unavailable: {e}"))?;
        let clusters = knowledge_base::engine::connected_component_clusters(
            &vectors,
            self.config.min_cluster_cos,
        );
        Ok((clusters, docs, vectors))
    }

    /// ③ 对 ≥3 条 episodic 的组做 Flash 归纳，写入 semantic（带溯源 footer）。
    ///
    /// 原组 episodic 不在此删除 — 交给 ⑤ 的「已沉淀 + 超 TTL」判定。
    async fn distill_groups(
        &self,
        user_id: &str,
        clusters: &[Vec<usize>],
        docs: &[knowledge_base::Document],
        now: DateTime<Utc>,
        stats: &mut RunStats,
    ) {
        for cluster in clusters {
            let episodic: Vec<&knowledge_base::Document> = cluster
                .iter()
                .filter_map(|&i| docs.get(i))
                .filter(|d| d.source_path == "ppa_episodic")
                .collect();
            if episodic.len() < 3 {
                continue;
            }
            if stats.llm_calls >= self.config.max_llm_calls {
                tracing::warn!(
                    user_id = %user_id,
                    max_llm_calls = self.config.max_llm_calls,
                    "LLM call budget exhausted, remaining groups skipped"
                );
                return;
            }

            let texts: Vec<String> = episodic.iter().map(|d| d.content.clone()).collect();
            match self.distiller.distill(&texts).await {
                Ok(content) => {
                    stats.llm_calls += 1;
                    let title = format!("memory_{}_c{}", now.timestamp_millis(), stats.merged);
                    // 双 footer：merged-from 供「已沉淀」判定，captured-at 供 TTL
                    // 时间源（标题格式被后续人工/agent 整理改动后仍有时间可读）
                    let footer = format!(
                        "{MERGED_FROM_PREFIX}{}]\n{CAPTURED_AT_PREFIX}{}]",
                        episodic
                            .iter()
                            .map(|d| d.id.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                        now.to_rfc3339()
                    );
                    match self
                        .km
                        .add_text_to_kb(
                            &self.kb_name,
                            &title,
                            &format!("{content}\n{footer}"),
                            "ppa_semantic",
                        )
                        .await
                    {
                        Ok(_) => stats.merged += 1,
                        Err(e) => tracing::warn!(
                            user_id = %user_id,
                            error = %e,
                            "Failed to write distilled memory, group kept as-is"
                        ),
                    }
                }
                Err(e) => {
                    stats.llm_calls += 1;
                    tracing::warn!(user_id = %user_id, error = %e, "Distillation call failed");
                }
            }
        }
    }

    /// ④ 组内硬去重：cos ≥ dedup_cos 的对保留较新一条，其余硬删 + 审计。
    async fn dedup_clusters(
        &self,
        user_id: &str,
        clusters: &[Vec<usize>],
        docs: &[knowledge_base::Document],
        vectors: &[Vec<f32>],
        now: DateTime<Utc>,
        stats: &mut RunStats,
    ) {
        for cluster in clusters {
            if cluster.len() < 2 {
                continue;
            }
            for victim in pick_dedup_victims(cluster, docs, vectors, self.config.dedup_cos) {
                let Some(doc) = docs.get(victim) else {
                    continue;
                };
                match self
                    .delete_with_audit(user_id, doc, REASON_DEDUP, now)
                    .await
                {
                    Ok(true) => stats.dedup_deleted += 1,
                    Ok(false) => {}
                    Err(e) => tracing::warn!(user_id = %user_id, error = %e, "Dedup delete failed"),
                }
            }
        }
    }

    /// ⑤ TTL 清理：已沉淀（被 semantic footer 引用）+ 超 TTL 的 episodic。
    ///
    /// 与水位无关 — TTL 判定需要看到老文档，每次全量扫描 episodic。
    /// 三个保守分支：时间源皆无不清理；未被沉淀引用不清理；semantic
    /// 本身不清理（只参与硬去重）。
    async fn ttl_cleanup(&self, user_id: &str, now: DateTime<Utc>, stats: &mut RunStats) {
        let summaries = match self.list_memory_summaries().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(user_id = %user_id, error = %e, "TTL cleanup scan failed");
                return;
            }
        };

        // 已沉淀集合：扫描 semantic 文档内容的溯源 footer
        let mut distilled: HashSet<String> = HashSet::new();
        for s in summaries.iter().filter(|s| s.source_path == "ppa_semantic") {
            if let Ok(Some(doc)) = self.km.get_document(&self.kb_name, &s.id).await {
                distilled.extend(parse_merged_from(&doc.content));
            }
        }

        for s in summaries.iter().filter(|s| s.source_path == "ppa_episodic") {
            let Ok(Some(doc)) = self.km.get_document(&self.kb_name, &s.id).await else {
                continue;
            };
            if !is_ttl_expired(&doc, now, self.config.episodic_ttl_days) {
                continue;
            }
            if !distilled.contains(&doc.id) {
                continue;
            }
            match self.delete_with_audit(user_id, &doc, REASON_TTL, now).await {
                Ok(true) => stats.ttl_deleted += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(user_id = %user_id, error = %e, "TTL delete failed"),
            }
        }
    }

    /// ⑥ 审计保留期清理：物理清除超出 `audit_retention_days` 的终态审计行。
    ///
    /// pending 未决行不清（DAO 契约：删除流程尚未收口，原文仍需保留）。
    /// 失败 warn 后继续 — 审计清理只影响 SQLite 体积，不参与记忆数据面，
    /// 不应让整轮整理因此失败。
    async fn purge_audit(&self, user_id: &str, now: DateTime<Utc>, stats: &mut RunStats) {
        let cutoff =
            (now - chrono::Duration::days(self.config.audit_retention_days as i64)).to_rfc3339();
        match crate::db::memory_audit::purge_older_than(&self.db, &cutoff).await {
            Ok(purged) => {
                stats.audit_purged = purged as usize;
                if purged > 0 {
                    tracing::info!(
                        user_id = %user_id,
                        purged,
                        cutoff = %cutoff,
                        "Expired audit rows purged"
                    );
                }
            }
            Err(e) => tracing::warn!(user_id = %user_id, error = %e, "Audit purge failed"),
        }
    }

    /// 审计先行删除：pending → 删除 → done / cancelled（fail-closed）。
    async fn delete_with_audit(
        &self,
        user_id: &str,
        doc: &knowledge_base::Document,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let entry = MemoryAuditEntry {
            user_id: user_id.to_string(),
            kb_name: self.kb_name.clone(),
            doc_id: doc.id.clone(),
            title: doc.title.clone(),
            content: doc.content.clone(),
            source: doc.source_path.clone(),
            reason: reason.to_string(),
            deleted_by: DELETED_BY_WORKER.to_string(),
            deleted_at: now.to_rfc3339(),
        };
        let audit_id = crate::db::memory_audit::insert_pending(&self.db, &entry)
            .await
            .map_err(|e| format!("audit write failed (fail-closed): {e}"))?;

        match self.km.delete_document(&self.kb_name, &doc.id).await {
            Ok(report) => {
                if let Err(e) = crate::db::memory_audit::mark_done(&self.db, audit_id).await {
                    tracing::warn!(
                        audit_id,
                        error = %e,
                        "Audit row left pending after successful delete"
                    );
                }
                tracing::info!(
                    user_id = %user_id,
                    doc_id = %doc.id,
                    reason = %reason,
                    removed_chunks = report.removed_chunks,
                    "Memory deleted by consolidation worker"
                );
                Ok(true)
            }
            Err(e) => {
                if let Err(mark_err) =
                    crate::db::memory_audit::mark_cancelled(&self.db, audit_id).await
                {
                    tracing::warn!(audit_id, error = %mark_err, "Failed to cancel audit row");
                }
                Err(format!("delete failed for {}: {e}", doc.id))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};

    const KB: &str = "@private_memory";
    const USER: &str = "test-user";
    /// 测试用基线毫秒时间戳（title 时间源）。
    const T0: i64 = 1727500000000;
    const T1: i64 = 1727500000001;
    const T2: i64 = 1727500000002;

    fn kb_config() -> KbConfig {
        KbConfig {
            name: KB.to_string(),
            description: "测试记忆库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
        }
    }

    async fn make_km() -> (tempfile::TempDir, Arc<KnowledgeManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(kb_config()).await.unwrap();
        (tmp, km)
    }

    async fn make_db() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 不参与生成的占位 provider — 测试经 with_distiller 注入 mock。
    struct NullProvider;

    #[async_trait::async_trait]
    impl model_provider::ModelProvider for NullProvider {
        fn name(&self) -> &str {
            "null"
        }
        async fn generate_full(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
            unreachable!("tests inject a mock distiller; provider must not be called")
        }
        async fn generate_stream(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateStream, model_provider::ProviderError> {
            Ok(model_provider::GenerateStream::new(Box::pin(
                futures::stream::empty(),
            )))
        }
    }

    /// 可编程 mock 沉淀器 — 记录每组输入。
    struct MockDistiller {
        calls: std::sync::Mutex<Vec<Vec<String>>>,
        output: String,
    }

    impl MockDistiller {
        fn new(output: &str) -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                output: output.to_string(),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl Distiller for MockDistiller {
        async fn distill(&self, group_texts: &[String]) -> Result<String, String> {
            self.calls.lock().unwrap().push(group_texts.to_vec());
            Ok(self.output.clone())
        }
    }

    fn test_config() -> ConsolidationConfig {
        ConsolidationConfig {
            // 测试用阈值：区分明显主题不聚组（<0.7），同句标点变体聚组（≈1.0）
            min_cluster_cos: 0.70,
            dedup_cos: 0.90,
            ..ConsolidationConfig::default()
        }
    }

    fn make_worker(
        km: &Arc<KnowledgeManager>,
        db: &SqlitePool,
        config: ConsolidationConfig,
    ) -> ConsolidationWorker {
        ConsolidationWorker::new(
            Arc::clone(km),
            db.clone(),
            KB,
            "deepseek-v4-flash",
            config,
            Arc::new(NullProvider),
        )
    }

    /// 构造 n 条候选：标题毫秒递增（下标越大越新，`d000` 最老）。
    fn synthetic_candidates(n: usize) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                doc_id: format!("d{i:03}"),
                time: DateTime::from_timestamp_millis(T0 + i as i64),
            })
            .collect()
    }

    /// `T0` 偏移 i 毫秒的时刻。
    fn ms(i: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(T0 + i).unwrap()
    }

    /// RFC 3339 字符串 → UTC 时刻（时间源断言用）。
    fn rfc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// 构造时间源测试文档：三个时间入口独立可设。
    fn timed_doc(created_at: Option<&str>, title: &str, content: &str) -> knowledge_base::Document {
        let mut doc = knowledge_base::Document {
            id: "d1".into(),
            kb_id: None,
            title: title.into(),
            source_path: "ppa_episodic".into(),
            content: content.into(),
            metadata: Default::default(),
        };
        doc.metadata.created_at = created_at.map(String::from);
        doc
    }

    fn window_ids(window: &CandidateWindow) -> HashSet<String> {
        window.candidates.iter().map(|c| c.doc_id.clone()).collect()
    }

    /// 读取落库的候选水位。
    async fn stored_watermark(pool: &SqlitePool, user_id: &str) -> Option<String> {
        crate::db::memory_consolidation_state::get_state(pool, user_id)
            .await
            .unwrap()
            .and_then(|r| r.last_scanned_ts)
    }

    /// 插入一条审计行并落到指定状态（`pending` 保持未决）。
    async fn insert_audit(
        pool: &SqlitePool,
        user_id: &str,
        doc_id: &str,
        deleted_at: &str,
        status: &str,
    ) -> i64 {
        let id = crate::db::memory_audit::insert_pending(
            pool,
            &MemoryAuditEntry {
                user_id: user_id.into(),
                kb_name: KB.into(),
                doc_id: doc_id.into(),
                title: format!("memory_{T0}_0"),
                content: format!("{doc_id} 的原文"),
                source: "ppa_semantic".into(),
                reason: REASON_TTL.into(),
                deleted_by: DELETED_BY_WORKER.into(),
                deleted_at: deleted_at.into(),
            },
        )
        .await
        .unwrap();
        match status {
            "done" => {
                crate::db::memory_audit::mark_done(pool, id).await.unwrap();
            }
            "cancelled" => {
                crate::db::memory_audit::mark_cancelled(pool, id)
                    .await
                    .unwrap();
            }
            _ => {}
        }
        id
    }

    async fn add(km: &KnowledgeManager, title: &str, content: &str, source: &str) -> String {
        km.add_text_to_kb(KB, title, content, source)
            .await
            .unwrap()
            .id
    }

    // ── 纯函数 ────────────────────────────────────────────────────────────

    #[test]
    fn parse_title_millis_extracts_first_segment() {
        assert_eq!(
            parse_title_millis("memory_1727500000000_3"),
            Some(1727500000000)
        );
        assert_eq!(
            parse_title_millis("memory_1727500000000_c0"),
            Some(1727500000000)
        );
        assert_eq!(parse_title_millis("memory_abc_1"), None);
        assert_eq!(parse_title_millis("meeting-notes"), None);
        assert_eq!(parse_title_millis("memory_"), None);
    }

    #[test]
    fn doc_time_prefers_created_at_then_title() {
        let mut doc = knowledge_base::Document {
            id: "d1".into(),
            kb_id: None,
            title: "memory_1727500000000_0".into(),
            source_path: "ppa_episodic".into(),
            content: "x".into(),
            metadata: Default::default(),
        };
        // 无 created_at → title 毫秒
        assert_eq!(
            doc_time(&doc),
            DateTime::from_timestamp_millis(1727500000000)
        );

        // created_at 优先
        doc.metadata.created_at = Some("2026-09-01T00:00:00+00:00".into());
        assert_eq!(
            doc_time(&doc),
            Some(
                DateTime::parse_from_rfc3339("2026-09-01T00:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        // 两者皆无 → None
        doc.title = "no-timestamp".into();
        doc.metadata.created_at = None;
        assert_eq!(doc_time(&doc), None);
    }

    #[test]
    fn doc_time_four_level_priority_matrix() {
        let footer = "[captured-at: 2020-01-01T00:00:00+00:00]";

        // 四级齐备 → created_at 胜出
        assert_eq!(
            doc_time(&timed_doc(
                Some("2026-09-01T00:00:00+00:00"),
                "memory_1727500000000_0",
                footer
            )),
            Some(rfc("2026-09-01T00:00:00+00:00"))
        );

        // created_at 非法 → 回落 title 毫秒（压过 footer）
        assert_eq!(
            doc_time(&timed_doc(
                Some("not-a-date"),
                "memory_1727500000000_0",
                footer
            )),
            DateTime::from_timestamp_millis(1727500000000)
        );
        // 无 created_at → title 毫秒胜出
        assert_eq!(
            doc_time(&timed_doc(None, "memory_1727500000000_0", footer)),
            DateTime::from_timestamp_millis(1727500000000)
        );

        // 标题不可解析（@memory 自拟标题）→ footer 兜底
        assert_eq!(
            doc_time(&timed_doc(None, "日语学习记录", footer)),
            Some(rfc("2020-01-01T00:00:00+00:00"))
        );

        // 三级皆无 → None（无数据不判定）
        assert_eq!(
            doc_time(&timed_doc(None, "日语学习记录", "普通记忆内容")),
            None
        );
    }

    #[test]
    fn parse_captured_at_takes_last_valid_line() {
        // 多条合法行 → 取最后一条
        let content =
            "事实\n[captured-at: 2020-01-01T00:00:00+00:00]\n[captured-at: 2021-02-03T04:05:06Z]";
        assert_eq!(
            parse_captured_at(content),
            Some(rfc("2021-02-03T04:05:06Z"))
        );

        // 尾部混有非法行 → 忽略并继续向前找最后一条合法行
        let mixed =
            "[captured-at: 2020-01-01T00:00:00+00:00]\n[captured-at: 不是时间]\n[captured-at: ]";
        assert_eq!(
            parse_captured_at(mixed),
            Some(rfc("2020-01-01T00:00:00+00:00"))
        );

        // 全无合法行 / 无标记 → None
        assert_eq!(parse_captured_at("事实\n[captured-at: not-a-time]"), None);
        assert_eq!(parse_captured_at("普通记忆内容"), None);
    }

    #[test]
    fn captured_at_and_merged_from_footers_coexist() {
        // 生产顺序：merged-from 在前、captured-at 在后
        let content =
            "合并事实\n[merged-from: aaaa0000,bbbb1111]\n[captured-at: 2021-02-03T04:05:06+00:00]";
        assert_eq!(parse_merged_from(content), vec!["aaaa0000", "bbbb1111"]);
        assert_eq!(
            parse_captured_at(content),
            Some(rfc("2021-02-03T04:05:06+00:00"))
        );

        // 反向排布亦各认各的前缀
        let reversed = "[captured-at: 2021-02-03T04:05:06+00:00]\n[merged-from: cccc2222]";
        assert_eq!(parse_merged_from(reversed), vec!["cccc2222"]);
        assert_eq!(
            parse_captured_at(reversed),
            Some(rfc("2021-02-03T04:05:06+00:00"))
        );
    }

    #[test]
    fn ttl_expired_requires_time_source() {
        let now = Utc::now();
        let mut doc = knowledge_base::Document {
            id: "d1".into(),
            kb_id: None,
            title: "memory_1000_0".into(),
            source_path: "ppa_episodic".into(),
            content: "x".into(),
            metadata: Default::default(),
        };
        // 极老时间戳 → 过期
        assert!(is_ttl_expired(&doc, now, 60));

        // 无时间源 → 不清理（无数据不判定）
        doc.title = "no-timestamp".into();
        assert!(!is_ttl_expired(&doc, now, 60));
    }

    #[test]
    fn parse_merged_from_reads_last_footer_line() {
        let content = "稳定事实\n[merged-from: aaaa0000,bbbb1111]";
        assert_eq!(parse_merged_from(content), vec!["aaaa0000", "bbbb1111"]);

        // 多段记忆里恰好含相似文本行 → 只认最后一条合法 footer
        let tricky = "[merged-from: not-an-id]\n事实\n[merged-from: cccc2222]";
        assert_eq!(parse_merged_from(tricky), vec!["cccc2222"]);

        assert!(parse_merged_from("普通记忆内容").is_empty());
        assert!(parse_merged_from("[merged-from: ]").is_empty());
    }

    #[test]
    fn dedup_victims_keep_newest_in_chain() {
        let now = Utc::now();
        let doc = |i: usize, title_ms: i64| knowledge_base::Document {
            id: format!("d{i}"),
            kb_id: None,
            title: format!("memory_{title_ms}_0"),
            source_path: "ppa_semantic".into(),
            content: format!("content {i}"),
            metadata: Default::default(),
        };
        // a~b ≈ 0.867、b~c ≈ 0.999、a~c ≈ 0.845（阈值 0.85）→ 传递链
        let vectors = vec![vec![1.0f32, 0.0], vec![0.87, 0.5], vec![0.87, 0.55]];
        let t = now.timestamp_millis();
        let docs = vec![doc(0, t - 3000), doc(1, t - 2000), doc(2, t - 1000)];

        // 链上依次淘汰较旧者 → 只留最新
        assert_eq!(
            pick_dedup_victims(&[0, 1, 2], &docs, &vectors, 0.85),
            vec![0, 1]
        );
    }

    #[test]
    fn dedup_victims_ignore_below_threshold_and_tiebreak_by_content() {
        let doc = |i: usize, content_len: usize| knowledge_base::Document {
            id: format!("d{i}"),
            kb_id: None,
            title: "no-timestamp".into(),
            source_path: "ppa_semantic".into(),
            content: "x".repeat(content_len),
            metadata: Default::default(),
        };
        let vectors = vec![vec![1.0f32, 0.0], vec![0.0, 1.0]];
        let docs = vec![doc(0, 10), doc(1, 20)];

        // 正交对不判重
        assert!(pick_dedup_victims(&[0, 1], &docs, &vectors, 0.90).is_empty());

        // 时间不可比 → content 较短者被淘汰
        let same = vec![vec![1.0f32, 0.0], vec![1.0, 0.0]];
        assert_eq!(pick_dedup_victims(&[0, 1], &docs, &same, 0.90), vec![0]);
    }

    #[test]
    fn select_window_catches_up_backlog_then_settles() {
        let all = synthetic_candidates(300);

        // 首轮（无水位）：回落「最近 200 条」（d100..d299），
        // 最老 100 条尚未覆盖；水位落在窗口内最老条目 d100
        let r1 = select_window(&all, None, 200);
        assert_eq!(r1.scanned, 300);
        assert_eq!(r1.candidates.len(), 200);
        assert_eq!(r1.candidates[0].doc_id, "d299");
        assert!(!window_ids(&r1).contains("d000"), "首轮不覆盖最老存量");
        assert_eq!(r1.watermark, Some(ms(100)));

        // 次轮：未覆盖的最老 100 条 backlog 全量入选，余量 100 由 fresh 补齐
        let r2 = select_window(&all, r1.watermark, 200);
        assert_eq!(r2.candidates.len(), 200);
        let ids2 = window_ids(&r2);
        for i in 0..100 {
            assert!(ids2.contains(&format!("d{i:03}")), "backlog d{i:03} 应入选");
        }
        assert!(ids2.contains("d299"), "fresh 侧补齐最新端");
        assert_eq!(r2.watermark, Some(ms(0)), "追平到最老存量");

        // 第三轮：backlog 空 → 回落现有行为（最近 200 条），水位不回跳
        let r3 = select_window(&all, r2.watermark, 200);
        assert_eq!(r3.candidates.len(), 200);
        assert_eq!(r3.candidates[0].doc_id, "d299");
        assert!(!window_ids(&r3).contains("d000"));
        assert_eq!(r3.watermark, r2.watermark);

        // 追平后稳定：再来一轮，窗口集合与水位均不变
        let r4 = select_window(&all, r3.watermark, 200);
        assert_eq!(window_ids(&r4), window_ids(&r3));
        assert_eq!(r4.watermark, r3.watermark);
    }

    #[test]
    fn select_window_edge_cases_keep_watermark() {
        // 空库：无候选 → 水位保持原值（首轮即无水位）
        let empty: Vec<Candidate> = Vec::new();
        let w = select_window(&empty, None, 200);
        assert!(w.candidates.is_empty());
        assert_eq!(w.watermark, None);
        let w = select_window(&empty, Some(ms(5)), 200);
        assert_eq!(w.watermark, Some(ms(5)), "无候选不推进水位");

        // 无时间戳候选（EPOCH）视为最老 → 降序里排在有时间戳者之后
        let all = vec![
            Candidate {
                doc_id: "no-ts".into(),
                time: None,
            },
            Candidate {
                doc_id: "t9".into(),
                time: Some(ms(9)),
            },
        ];
        let w = select_window(&all, Some(ms(10)), 1);
        assert_eq!(w.candidates[0].doc_id, "t9");
        assert_eq!(w.watermark, Some(ms(9)));

        // batch_size = 0 → 不选任何候选，水位不动
        let w = select_window(&synthetic_candidates(5), Some(ms(2)), 0);
        assert!(w.candidates.is_empty());
        assert_eq!(w.watermark, Some(ms(2)));
    }

    // ── 集成（真实嵌入 + InMemory KB + SQLite）──────────────────────────

    #[tokio::test]
    async fn run_once_scans_filters_and_persists_state() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        // 30 条混合：10 episodic / 10 semantic / 10 profile。
        // 内容主题两两互异（模板句式会因样板词占主导而意外聚组），
        // 保证机器判定不产生删除；distiller 注入 mock 保证测试封闭。
        const TOPICS: [&str; 10] = [
            "hiking",
            "piano",
            "astronomy",
            "swimming",
            "carpentry",
            "pottery",
            "gardening",
            "chess",
            "photography",
            "cycling",
        ];
        for (i, topic) in TOPICS.iter().enumerate() {
            add(
                &km,
                &format!("memory_{T0}_{i}"),
                &format!("The user went to a {topic} event last weekend and enjoyed it."),
                "ppa_episodic",
            )
            .await;
            add(
                &km,
                &format!("memory_{T1}_{i}"),
                &format!("The user owns professional {topic} equipment at home."),
                "ppa_semantic",
            )
            .await;
            add(
                &km,
                &format!("memory_{T2}_{i}"),
                &format!("The user prefers being greeted before {topic} discussions."),
                "ppa_profile",
            )
            .await;
        }
        let worker = worker.with_distiller(Arc::new(MockDistiller::new("irrelevant")));

        let stats = worker.run_once(USER).await.unwrap();
        assert_eq!(stats.scanned, 20, "profile 不入候选池");
        assert_eq!(stats.candidates, 20);
        assert_eq!(stats.machine_steps_skipped, None);
        assert_eq!(
            stats.dedup_deleted + stats.ttl_deleted,
            0,
            "内容互异不应有删除"
        );

        // 水位与统计落库
        let state = crate::db::memory_consolidation_state::get_state(&pool, USER)
            .await
            .unwrap()
            .expect("state row written");
        assert!(state.last_scanned_ts.is_some());
        assert!(state.last_run_at.is_some());
        let saved: serde_json::Value =
            serde_json::from_str(state.last_run_stats.as_deref().unwrap()).unwrap();
        assert_eq!(saved["scanned"], 20);

        // 二次运行幂等：首轮若聚出 ≥3 条 episodic 组并沉淀写入新 semantic
        // 文档，扫描基数随之 +merged（嵌入行为相关的合法增长）；仍无删除
        let stats2 = worker.run_once(USER).await.unwrap();
        assert_eq!(stats2.scanned, stats.scanned + stats.merged);
        assert_eq!(stats2.dedup_deleted + stats2.ttl_deleted, 0);
    }

    #[tokio::test]
    async fn watermark_rounds_persist_and_settle_on_existing_stock() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        // 关闭聚类（阈值 >1 恒不成组）：本轮只验证 ① 候选窗口与水位推进
        let config = ConsolidationConfig {
            min_cluster_cos: 1.1,
            dedup_cos: 1.2,
            ..test_config()
        };
        let worker = make_worker(&km, &pool, config);

        // 300 条存量：标题毫秒递增（下标越大越新），内容互异避免幂等替换
        for i in 0..300 {
            add(
                &km,
                &format!("memory_{}_{i}", T0 + i),
                &format!("The user mentioned topic number {i} during a conversation."),
                "ppa_episodic",
            )
            .await;
        }
        let ts = |i: i64| {
            DateTime::from_timestamp_millis(T0 + i)
                .unwrap()
                .to_rfc3339()
        };

        // 首轮：扫 300、选最近 200 条，水位落在窗口内最老条目
        let s1 = worker.run_once(USER).await.unwrap();
        assert_eq!(s1.scanned, 300);
        assert_eq!(s1.candidates, 200);
        assert_eq!(stored_watermark(&pool, USER).await, Some(ts(100)));

        // 次轮：未覆盖的最老 100 条补齐（backlog）+ fresh 补齐窗口 → 追平
        let s2 = worker.run_once(USER).await.unwrap();
        assert_eq!(s2.candidates, 200);
        assert_eq!(stored_watermark(&pool, USER).await, Some(ts(0)));

        // 第三轮起：backlog 空 → 回落最近 200 条；水位不回跳，连续两轮稳定
        let s3 = worker.run_once(USER).await.unwrap();
        assert_eq!(s3.candidates, 200);
        assert_eq!(stored_watermark(&pool, USER).await, Some(ts(0)));
        let s4 = worker.run_once(USER).await.unwrap();
        assert_eq!(s4.candidates, s3.candidates);
        assert_eq!(stored_watermark(&pool, USER).await, Some(ts(0)));

        // 聚类关闭 → 无沉淀/去重删除；保留期内审计无行可清
        assert_eq!(s4.merged + s4.dedup_deleted + s4.ttl_deleted, 0);
        assert_eq!(s4.audit_purged, 0);
    }

    #[tokio::test]
    async fn run_once_purges_only_expired_terminal_audit_rows() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config()); // 保留期 90 天

        let now = Utc::now();
        let expired = (now - Duration::days(120)).to_rfc3339();
        let recent = (now - Duration::days(10)).to_rfc3339();
        let old_done = insert_audit(&pool, USER, "doc-old-done", &expired, "done").await;
        let old_cancelled =
            insert_audit(&pool, USER, "doc-old-cancelled", &expired, "cancelled").await;
        let old_pending = insert_audit(&pool, USER, "doc-old-pending", &expired, "pending").await;
        let recent_done = insert_audit(&pool, USER, "doc-recent-done", &recent, "done").await;

        let stats = worker.run_once(USER).await.unwrap();
        assert_eq!(stats.audit_purged, 2, "只清保留期外的终态行");

        assert!(
            crate::db::memory_audit::get(&pool, old_done)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            crate::db::memory_audit::get(&pool, old_cancelled)
                .await
                .unwrap()
                .is_none()
        );
        // pending 未决行与保留期内行不受影响
        assert!(
            crate::db::memory_audit::get(&pool, old_pending)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            crate::db::memory_audit::get(&pool, recent_done)
                .await
                .unwrap()
                .is_some()
        );

        // 统计落库 JSON 含 audit_purged
        let state = crate::db::memory_consolidation_state::get_state(&pool, USER)
            .await
            .unwrap()
            .unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(state.last_run_stats.as_deref().unwrap()).unwrap();
        assert_eq!(saved["audit_purged"], 2);
    }

    #[tokio::test]
    async fn run_once_keeps_audit_rows_within_long_retention() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        // 保留期极大 → 截止时刻远早于任何审计行 → 零删除
        let config = ConsolidationConfig {
            audit_retention_days: 100_000,
            ..test_config()
        };
        let worker = make_worker(&km, &pool, config);

        let expired = (Utc::now() - Duration::days(120)).to_rfc3339();
        insert_audit(&pool, USER, "doc-old-done", &expired, "done").await;
        insert_audit(&pool, USER, "doc-old-pending", &expired, "pending").await;

        let stats = worker.run_once(USER).await.unwrap();
        assert_eq!(stats.audit_purged, 0);
        assert_eq!(
            crate::db::memory_audit::list_by_user(&pool, USER, 10, 0)
                .await
                .unwrap()
                .len(),
            2,
            "保留期内审计行全部保留"
        );
    }

    #[tokio::test]
    async fn dedup_deletes_older_duplicate_with_audit() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        let old_id = add(
            &km,
            "memory_1727500000000_0",
            "The user likes strong coffee in the morning.",
            "ppa_semantic",
        )
        .await;
        let new_id = add(
            &km,
            "memory_1727500000009_1",
            "The user likes strong coffee in the morning!",
            "ppa_semantic",
        )
        .await;

        let stats = worker.run_once(USER).await.unwrap();
        assert_eq!(stats.dedup_deleted, 1, "近重复对应删一条，实际: {stats:?}");
        assert!(km.get_document(KB, &old_id).await.unwrap().is_none());
        assert!(km.get_document(KB, &new_id).await.unwrap().is_some());

        // 审计 outbox：pending → done，deleted_by = worker
        let rows = crate::db::memory_audit::list_by_user(&pool, USER, 10, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "done");
        assert_eq!(rows[0].deleted_by, "worker");
        assert_eq!(rows[0].reason, "consolidation_dedup");
        assert_eq!(rows[0].doc_id, old_id);
        assert_eq!(
            rows[0].content,
            "The user likes strong coffee in the morning."
        );
    }

    #[tokio::test]
    async fn distill_respects_llm_budget_and_writes_footer() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let config = ConsolidationConfig {
            max_llm_calls: 1,
            ..test_config()
        };
        let worker = make_worker(&km, &pool, config);

        // 两组各 3 条 episodic（同轮均达标），预算只允许 1 次调用
        let docs: Vec<knowledge_base::Document> = (0..6)
            .map(|i| knowledge_base::Document {
                id: format!("e{i}"),
                kb_id: None,
                title: format!("memory_{ts}_0", ts = T0 + i as i64),
                source_path: "ppa_episodic".into(),
                content: format!("event number {i}"),
                metadata: Default::default(),
            })
            .collect();
        for d in &docs {
            km.add_text_to_kb(KB, &d.title, &d.content, "ppa_episodic")
                .await
                .unwrap();
        }
        let mock = Arc::new(MockDistiller::new("用户在系统学习日语"));
        let worker = worker.with_distiller(mock.clone());

        let mut stats = RunStats::default();
        let now = Utc::now();
        worker
            .distill_groups(
                USER,
                &[vec![0, 1, 2], vec![3, 4, 5]],
                &docs,
                now,
                &mut stats,
            )
            .await;

        assert_eq!(mock.call_count(), 1, "预算 1 次，第二组被跳过");
        assert_eq!(stats.llm_calls, 1);
        assert_eq!(stats.merged, 1);

        // 写入的 semantic 文档带溯源 footer（原组 3 条 doc_id）
        let written = km
            .list_documents(KB, 0, 50)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.source_path == "ppa_semantic")
            .expect("distilled semantic doc written");
        let doc = km.get_document(KB, &written.id).await.unwrap().unwrap();
        let from = parse_merged_from(&doc.content);
        assert_eq!(from, vec!["e0", "e1", "e2"]);
    }

    #[tokio::test]
    async fn distill_appends_captured_at_footer() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config())
            .with_distiller(Arc::new(MockDistiller::new("用户在系统学习日语")));

        let docs: Vec<knowledge_base::Document> = (0..3)
            .map(|i| knowledge_base::Document {
                id: format!("e{i}"),
                kb_id: None,
                title: format!("memory_{}_0", T0 + i as i64),
                source_path: "ppa_episodic".into(),
                content: format!("event number {i}"),
                metadata: Default::default(),
            })
            .collect();
        for d in &docs {
            km.add_text_to_kb(KB, &d.title, &d.content, "ppa_episodic")
                .await
                .unwrap();
        }

        let now = Utc::now();
        let mut stats = RunStats::default();
        worker
            .distill_groups(USER, &[vec![0, 1, 2]], &docs, now, &mut stats)
            .await;
        assert_eq!(stats.merged, 1);

        let written = km
            .list_documents(KB, 0, 50)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.source_path == "ppa_semantic")
            .expect("distilled semantic doc written");
        let doc = km.get_document(KB, &written.id).await.unwrap().unwrap();

        // 双 footer 并存：溯源在前、本轮 now 捕获时刻在后
        assert_eq!(parse_merged_from(&doc.content), vec!["e0", "e1", "e2"]);
        assert_eq!(parse_captured_at(&doc.content), Some(now));
    }

    #[tokio::test]
    async fn ttl_deletes_only_distilled_and_expired_episodic() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        // 3 条已沉淀（被 footer 引用）+ 1 条未沉淀 + 1 条已过期 semantic；
        // created_at 为写入时刻（=真实当前），用 future now 使其全部「过期」
        let e1 = add(&km, "memory_1000_0", "episodic one", "ppa_episodic").await;
        let e2 = add(&km, "memory_1001_1", "episodic two", "ppa_episodic").await;
        let e3 = add(&km, "memory_1002_2", "episodic three", "ppa_episodic").await;
        add(
            &km,
            "memory_1003_3",
            "episodic not distilled",
            "ppa_episodic",
        )
        .await;
        add(&km, "memory_1004_4", "old semantic", "ppa_semantic").await;
        add(
            &km,
            "memory_1005_5",
            &format!("merged fact\n[merged-from: {e1},{e2},{e3}]"),
            "ppa_semantic",
        )
        .await;

        let future_now = Utc::now() + Duration::days(61);
        let mut stats = RunStats::default();
        worker.ttl_cleanup(USER, future_now, &mut stats).await;

        assert_eq!(
            stats.ttl_deleted, 3,
            "仅 footer 引用且过期的 episodic 被清理"
        );
        for id in [&e1, &e2, &e3] {
            assert!(km.get_document(KB, id).await.unwrap().is_none());
        }
        // 未沉淀的 episodic 与 semantic 均保留
        assert_eq!(km.list_documents(KB, 0, 50).await.unwrap().len(), 3);

        let rows = crate::db::memory_audit::list_by_user(&pool, USER, 10, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.reason == "consolidation_ttl"));
    }

    /// 集成：@memory agent 合并写入的 episodic 标题自拟（title 毫秒不可解析），
    /// 时间源由 doc_time 四级链取 KB 层强制的 metadata.created_at（写入时刻）。
    ///
    /// 用 future 判定时钟（同 `ttl_deletes_only_distilled_and_expired_episodic`）
    /// 验证全链路：已沉淀且过期 → 清理 + 审计；未沉淀 → 保留。
    /// 注：km 写入路径强制 created_at，「created_at 缺失、仅 footer 可读」的
    /// 兜底分支在集成层不可构造，由纯函数矩阵
    /// （`doc_time_four_level_priority_matrix`）覆盖。footer 仍随内容落库，
    /// 供 created_at 元数据丢失（legacy / 导入文档）时兜底。
    #[tokio::test]
    async fn ttl_cleans_agent_merged_memory_end_to_end() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        let stale_at = (Utc::now() - Duration::days(120)).to_rfc3339();
        let merged = add(
            &km,
            "搬家的回忆",
            &format!("两次搬家的记录\n[captured-at: {stale_at}]"),
            "ppa_episodic",
        )
        .await;
        let other = add(&km, "别的记忆", "无关记录", "ppa_episodic").await;
        // 沉淀语义文档只引用前者（真实合并产物形态：双 footer）
        let semantic = add(
            &km,
            "搬家总结",
            &format!(
                "稳定事实\n[merged-from: {merged}]\n[captured-at: {}]",
                Utc::now().to_rfc3339()
            ),
            "ppa_semantic",
        )
        .await;

        // 落库核验：自拟标题 → title 毫秒不可解析；footer 随内容保留，
        // created_at 由 KB 层强制填充 —— 四级链前两级分别就位
        let stored = km.get_document(KB, &merged).await.unwrap().unwrap();
        assert_eq!(parse_title_millis(&stored.title), None);
        assert_eq!(
            parse_captured_at(&stored.content),
            Some(
                DateTime::parse_from_rfc3339(&stale_at)
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert!(stored.metadata.created_at.is_some());

        let future_now = Utc::now() + Duration::days(61);
        let mut stats = RunStats::default();
        worker.ttl_cleanup(USER, future_now, &mut stats).await;

        assert_eq!(
            stats.ttl_deleted, 1,
            "已沉淀且过期的 agent 合并记忆被清理（时间源 = created_at）"
        );
        assert!(km.get_document(KB, &merged).await.unwrap().is_none());
        assert!(km.get_document(KB, &other).await.unwrap().is_some());
        assert!(km.get_document(KB, &semantic).await.unwrap().is_some());

        let rows = crate::db::memory_audit::list_by_user(&pool, USER, 10, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].doc_id, merged);
    }

    #[tokio::test]
    async fn missing_kb_fails_the_round() {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        let err = worker.run_once(USER).await.unwrap_err();
        assert!(err.to_string().contains("list_documents"), "{err}");
    }
}
