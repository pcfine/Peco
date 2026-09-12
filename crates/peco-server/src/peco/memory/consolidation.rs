// ============================================================================
// ConsolidationWorker — 自动记忆整理（巩固流水线）
// ============================================================================
//
// 与 Layer 2（@memory agent 手动整理）的分工：本 worker 是批量深度整理
// 的唯一执行者——机器判定（向量聚类/硬去重）+ Flash 沉淀 + TTL 清理。
// 触发方（REST 手动 / cron）只负责调用 `run_once`，不经 agent 通道。
//
// 流水线（每轮顺序）：
//   ① 候选收集：分页扫描 → 过滤 ppa_episodic/ppa_semantic → 取最近 batch_size 条
//   ② 主题聚类：候选重嵌入 → 进程内两两余弦 → 连通分量（min_cluster_cos）
//   ③ 沉淀：≥3 条 episodic 的组经 Flash 归纳为一条 semantic（原组交⑤判定）
//   ④ 硬去重：组内 cos ≥ dedup_cos 的对保留最新一条，其余硬删 + 审计
//   ⑤ TTL 清理：已沉淀 + 超 episodic_ttl_days 的 episodic 硬删 + 审计
//   ⑥ 图谱补边：图后端持久化迁移验收前显式跳过
//   ⑦ 审计与水位：删除走 outbox（pending→done/cancelled）；回写
//      memory_consolidation_state 水位与统计
//
// 安全护栏：
//   - ppa_profile 永不进候选池（① 过滤）；自动删除仅限 ④⑤ 两类
//   - ppa_semantic 只参与硬去重，不参与沉淀删除
//   - 审计先行：审计写入失败（存储不可用）→ 拒绝删除（fail-closed）
//   - 无数据不判定：时间源（created_at → title 毫秒）皆无 → 不参与 TTL
//   - 机器判定基建（嵌入/聚类）不可用 → 跳过 ②③④ 并 warn，仅执行 ①⑤⑦
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

/// 文档时间源：`metadata.created_at`（ISO 8601）优先 → title
/// `memory_{millis}_{seq}` 毫秒解析兜底 → `None`。
///
/// 注：LanceDB 后端不持久化 metadata（get_document 重建默认值），
/// 生产路径实际生效的是 title 毫秒解析 — hook 写入的标题格式保证可解析。
pub fn doc_time(doc: &knowledge_base::Document) -> Option<DateTime<Utc>> {
    if let Some(created_at) = &doc.metadata.created_at
        && let Ok(t) = DateTime::parse_from_rfc3339(created_at)
    {
        return Some(t.with_timezone(&Utc));
    }
    parse_title_millis(&doc.title).and_then(DateTime::from_timestamp_millis)
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

        // ── ① 候选收集 ─────────────────────────────────────────────
        let summaries = self.list_memory_summaries().await?;
        stats.scanned = summaries.len();
        let mut candidates: Vec<Candidate> = summaries
            .iter()
            .map(|s| Candidate {
                time: parse_title_millis(&s.title).and_then(DateTime::from_timestamp_millis),
                doc_id: s.id.clone(),
            })
            .collect();
        candidates.sort_by_key(|c| std::cmp::Reverse(c.sort_key()));
        candidates.truncate(self.config.batch_size);
        stats.candidates = candidates.len();
        tracing::info!(
            user_id = %user_id,
            kb = %self.kb_name,
            scanned = stats.scanned,
            candidates = stats.candidates,
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

        // ── ⑥ 图谱补边 — 图后端持久化迁移验收前显式跳过 ──────────────
        tracing::debug!(
            user_id = %user_id,
            "Graph edge backfill suspended until graph backend migration is accepted"
        );

        // ── ⑦ 水位与统计回写 ───────────────────────────────────────
        let stats_json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
        let now_str = now.to_rfc3339();
        crate::db::memory_consolidation_state::upsert_state(
            &self.db,
            user_id,
            Some(now_str.as_str()),
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
            llm_calls = stats.llm_calls,
            "Consolidation round finished"
        );
        Ok(stats)
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
    ) -> Result<(Vec<Vec<usize>>, Vec<knowledge_base::Document>, Vec<Vec<f32>>), String> {
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
                    let footer = format!(
                        "{MERGED_FROM_PREFIX}{}]",
                        episodic
                            .iter()
                            .map(|d| d.id.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
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
                match self.delete_with_audit(user_id, doc, REASON_DEDUP, now).await {
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

    async fn add(km: &KnowledgeManager, title: &str, content: &str, source: &str) -> String {
        km.add_text_to_kb(KB, title, content, source)
            .await
            .unwrap()
            .id
    }

    // ── 纯函数 ────────────────────────────────────────────────────────────

    #[test]
    fn parse_title_millis_extracts_first_segment() {
        assert_eq!(parse_title_millis("memory_1727500000000_3"), Some(1727500000000));
        assert_eq!(parse_title_millis("memory_1727500000000_c0"), Some(1727500000000));
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
            Some(DateTime::parse_from_rfc3339("2026-09-01T00:00:00+00:00").unwrap().with_timezone(&Utc))
        );

        // 两者皆无 → None
        doc.title = "no-timestamp".into();
        doc.metadata.created_at = None;
        assert_eq!(doc_time(&doc), None);
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
        assert_eq!(
            pick_dedup_victims(&[0, 1], &docs, &same, 0.90),
            vec![0]
        );
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
            "hiking", "piano", "astronomy", "swimming", "carpentry", "pottery",
            "gardening", "chess", "photography", "cycling",
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
        assert_eq!(stats.dedup_deleted + stats.ttl_deleted, 0, "内容互异不应有删除");

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
        assert_eq!(rows[0].content, "The user likes strong coffee in the morning.");
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
            .distill_groups(USER, &[vec![0, 1, 2], vec![3, 4, 5]], &docs, now, &mut stats)
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
    async fn ttl_deletes_only_distilled_and_expired_episodic() {
        let (_tmp, km) = make_km().await;
        let (pool, _db_tmp) = make_db().await;
        let worker = make_worker(&km, &pool, test_config());

        // 3 条已沉淀（被 footer 引用）+ 1 条未沉淀 + 1 条已过期 semantic；
        // created_at 为写入时刻（=真实当前），用 future now 使其全部「过期」
        let e1 = add(&km, "memory_1000_0", "episodic one", "ppa_episodic").await;
        let e2 = add(&km, "memory_1001_1", "episodic two", "ppa_episodic").await;
        let e3 = add(&km, "memory_1002_2", "episodic three", "ppa_episodic").await;
        add(&km, "memory_1003_3", "episodic not distilled", "ppa_episodic").await;
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

        assert_eq!(stats.ttl_deleted, 3, "仅 footer 引用且过期的 episodic 被清理");
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
