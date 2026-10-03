// ============================================================================
// MemoryExtractionHook — 记忆写路径（LooperHook）
// ============================================================================
//
// 每轮成功完成后，将本轮对话交给 Flash 模型提取长期记忆，写入
// @private_memory 知识库。与 compaction 的分工：compaction 解决
// "会话内上下文放不下"；本模块解决"跨会话/超长期的知识"。
//
// 非致命性：所有失败点 warn 后 return — `on_turn_complete` 无返回值，
// hook 永不影响对话主流程。
//
// 执行模型：守卫与收集在 looper 上下文内同步完成（O(1)，只看最后一轮），
// 检索、LLM 提取与 KB 写入全部 `tokio::spawn` 到后台 — turn 边界零阻塞。
// spawn 前数据全部转 owned，无借用问题；单用户场景写入乱序风险可接受。
//
// 取代机制 · 阶段一（在线 shadow）：候选召回双通道带 doc_id，提取产出的
// supersedes 决策只落 `memory_supersede_shadow` 观测表 —— KB 只 append，
// 绝不调用删除。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{InputItem, Role, Usage};
use peco_core::agent::{LooperHook, TurnFailureReason, estimate_str_tokens};
use peco_core::knowledge::KnowledgeManager;
use peco_core::session::Session;
use serde_json::json;
use tracing::{debug, info, warn};

use super::analyzer::{MemoryCandidate, MemoryCategory, MemoryFact, TurnAnalyzer};
use super::config::MemoryConfig;
use super::dedup::max_cosine;
use crate::db::memory_supersede::{ShadowRow, insert_shadow};

/// 带来源通道的候选 — shadow 快照要记 channel，analyzer 只见 [`MemoryCandidate`]。
struct MemoryCandidateWithChannel {
    id: String,
    /// KB source 标签（如 `ppa_semantic`）
    source: String,
    /// 展示文本（snippet 或截断后的正文）
    text: String,
    /// `"search"` | `"recent"` | `"both"`
    channel: &'static str,
}

impl MemoryCandidateWithChannel {
    fn to_candidate(&self) -> MemoryCandidate {
        MemoryCandidate {
            id: self.id.clone(),
            source: self.source.clone(),
            text: self.text.clone(),
        }
    }
}

/// `build_candidates` 的返回值：候选集 + 通道 A 原始结果（供近重复判定）。
struct CandidateSet {
    candidates: Vec<MemoryCandidateWithChannel>,
    /// 通道 A（search_kb）原始 `(source_path, snippet)`，**不受任何候选上限截断影响** ——
    /// 近重复判定比对集必须与旧单通道行为逐字一致。
    dedup_baseline: Vec<(String, String)>,
}

/// 记忆提取写路径。
pub struct MemoryExtractionHook {
    km: Arc<KnowledgeManager>,
    analyzer: Arc<dyn TurnAnalyzer>,
    config: MemoryConfig,
    /// shadow 观测行的落库位置（`supersede_shadow=false` 时不会被写）
    db: sqlx::SqlitePool,
    user_id: String,
}

impl MemoryExtractionHook {
    pub fn new(
        km: Arc<KnowledgeManager>,
        analyzer: Arc<dyn TurnAnalyzer>,
        config: MemoryConfig,
        db: sqlx::SqlitePool,
        user_id: String,
    ) -> Self {
        Self {
            km,
            analyzer,
            config,
            db,
            user_id,
        }
    }

    /// 从最后一轮 committed turn 收集对话转录（User/Assistant 文本，
    /// 跳过 tool 过程与 reasoning）。返回 `None` 表示无可提取内容。
    fn collect_turn_dialogue(session: &Session) -> Option<String> {
        let turn = session.committed_turns().last()?;
        let mut user_parts: Vec<Cow<'_, str>> = Vec::new();
        let mut assistant_parts: Vec<Cow<'_, str>> = Vec::new();
        for am in turn {
            if let InputItem::Message { role, content } = am.message.as_ref() {
                if content.text_view().trim().is_empty() {
                    continue;
                }
                match role {
                    Role::User => user_parts.push(content.text_view()),
                    Role::Assistant => assistant_parts.push(content.text_view()),
                    _ => {}
                }
            }
        }
        if user_parts.is_empty() {
            return None;
        }
        let mut dialogue = String::new();
        for p in user_parts {
            dialogue.push_str("User: ");
            dialogue.push_str(&p);
            dialogue.push('\n');
        }
        for p in assistant_parts {
            dialogue.push_str("Assistant: ");
            dialogue.push_str(&p);
            dialogue.push('\n');
        }
        Some(dialogue)
    }

    /// 写路径近重复判定：一次批量嵌入后，逐条求「同类目既有记忆」的
    /// 最大余弦，返回与 `facts` 等长的标记（`true` = 近重复）。
    ///
    /// 同类目 = fact 的 `ppa_{category}` 与既有 snippet 的 `source` 一致。
    /// 返回 `None` 表示嵌入基建不可用 —— 调用方降级放行全部，嵌入故障
    /// 不得阻塞写路径。
    async fn near_duplicate_flags(
        km: &KnowledgeManager,
        kb_name: &str,
        facts: &[MemoryFact],
        existing: &[(String, String)],
        dedup_cos: f32,
    ) -> Option<Vec<bool>> {
        let categories: HashSet<String> = facts.iter().map(source_of).collect();

        // 同类目既有片段：跨 fact 去重（同一片段可能被多条同类别 fact 引用），
        // 空白片段不参与嵌入
        let mut seen: HashSet<&str> = HashSet::new();
        let same_category: Vec<&(String, String)> = existing
            .iter()
            .filter(|(src, snippet)| {
                categories.contains(src)
                    && !snippet.trim().is_empty()
                    && seen.insert(snippet.as_str())
            })
            .collect();

        let mut texts: Vec<String> = facts.iter().map(|f| f.content.clone()).collect();
        texts.extend(same_category.iter().map(|(_, snippet)| snippet.clone()));

        let vectors = match km.embed_texts(kb_name, &texts).await {
            Ok(v) if v.len() == texts.len() => v,
            Ok(v) => {
                warn!(
                    expected = texts.len(),
                    got = v.len(),
                    "Embedding count mismatch, dedup check skipped"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    kb = %kb_name,
                    "Embedding unavailable, dedup check skipped (writing as-is)"
                );
                return None;
            }
        };

        let (fact_vectors, existing_vectors) = vectors.split_at(facts.len());
        let mut by_category: HashMap<String, Vec<Vec<f32>>> = HashMap::new();
        for ((src, _), vector) in same_category.iter().zip(existing_vectors) {
            by_category
                .entry(src.clone())
                .or_default()
                .push(vector.clone());
        }

        // 无同类目既有记忆 → 空候选（max_cosine 恒 0.0，不判重）
        let no_candidates: Vec<Vec<f32>> = Vec::new();
        Some(
            facts
                .iter()
                .enumerate()
                .map(|(i, fact)| {
                    let Some(vector) = fact_vectors.get(i) else {
                        return false;
                    };
                    let others = by_category.get(&source_of(fact)).unwrap_or(&no_candidates);
                    max_cosine(vector, others) >= dedup_cos
                })
                .collect(),
        )
    }

    /// 双通道候选召回（取代指针的校验集）。
    ///
    /// - 通道 A（search）：相似度检索，取 `{document_id, source_path, snippet}`；
    /// - 通道 B（recent）：全量扫描 `ppa_*` 记忆按 `created_at` 倒序、每类目取
    ///   前 N 条 —— 补相似度检索漏掉的「近期前身」（取代的主要目标）。
    ///
    /// 合并按 id 去重（同 id 双通道 → `both`，保留通道 A 的 snippet）；
    /// 任一通道失败只 warn 并用另一通道，两通道都空则空候选照常调模型。
    /// shadow 与 enforce 同时关闭时只走通道 A（候选集合与旧单通道一致，
    /// 仅新增 id/source 展示字段）。
    ///
    /// 返回 [`CandidateSet`]：`candidates` 受三个候选上限截断（供 prompt /
    /// shadow 快照），`dedup_baseline` 是通道 A 原始结果（供近重复判定），
    /// 不受截断影响。
    async fn build_candidates(
        km: &KnowledgeManager,
        config: &MemoryConfig,
        query: &str,
    ) -> CandidateSet {
        // 通道 A —— 失败只失去相似度提示，不阻断
        let search_hits: Vec<MemoryCandidateWithChannel> = match km
            .search_kb(&config.kb_name, query, config.extraction_top_k)
            .await
        {
            Ok(results) => results
                .into_iter()
                .map(|r| MemoryCandidateWithChannel {
                    id: r.document_id,
                    source: r.source_path,
                    text: r.snippet,
                    channel: "search",
                })
                .collect(),
            Err(e) => {
                warn!(
                    error = %e,
                    kb = %config.kb_name,
                    "Candidate search failed (using remaining channel)"
                );
                Vec::new()
            }
        };

        // 近重复判定比对集：通道 A 原始 (source_path, snippet)，在任何截断之前
        // 取出 —— 尾部搜索命中被候选上限挤出候选集时仍须参与判重
        let dedup_baseline: Vec<(String, String)> = search_hits
            .iter()
            .map(|c| (c.source.clone(), c.text.clone()))
            .collect();

        // 通道 B —— 仅在 shadow/enforce 至少其一时启用
        let recent_hits = if config.supersede_shadow || config.supersede_enforce {
            Self::recent_candidates(km, config).await
        } else {
            Vec::new()
        };

        // 合并：search 在前（相关性优先），同 id 覆盖为 "both"（text 保留 search 的）
        let mut merged: Vec<MemoryCandidateWithChannel> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for c in search_hits.into_iter().chain(recent_hits) {
            match seen.get(&c.id) {
                Some(&idx) => merged[idx].channel = "both",
                None => {
                    seen.insert(c.id.clone(), merged.len());
                    merged.push(c);
                }
            }
        }

        // 条数上限
        merged.truncate(config.candidate_cap);

        // token 上限：按 prompt 渲染行估算，超限即止（不放进第一条兜底 —
        // 上限是硬约束，首条超限就返回空候选）
        let mut used = 0usize;
        let mut kept: Vec<MemoryCandidateWithChannel> = Vec::new();
        for c in merged {
            let line = format!("- [{}] ({}) {}", c.id, c.source, c.text);
            let tokens = estimate_str_tokens(&line);
            if used + tokens > config.candidate_token_cap {
                break;
            }
            used += tokens;
            kept.push(c);
        }
        CandidateSet {
            candidates: kept,
            dedup_baseline,
        }
    }

    /// 通道 B（近期）：全量扫描 `ppa_*` 记忆文档，`created_at` 倒序，
    /// 按 source 分组各取 `candidate_recent_per_category` 条。
    async fn recent_candidates(
        km: &KnowledgeManager,
        config: &MemoryConfig,
    ) -> Vec<MemoryCandidateWithChannel> {
        let docs = match km
            .list_memory_documents_with_content(&config.kb_name, config.shadow_scan_limit)
            .await
        {
            Ok(docs) => docs,
            Err(e) => {
                warn!(
                    error = %e,
                    kb = %config.kb_name,
                    "Recent candidate scan failed (using remaining channel)"
                );
                return Vec::new();
            }
        };

        let mut docs: Vec<_> = docs
            .into_iter()
            .filter(|d| d.source_path.starts_with("ppa_"))
            .collect();
        // 倒序：None（无创建时间）视为最旧沉底
        docs.sort_by(|a, b| b.metadata.created_at.cmp(&a.metadata.created_at));

        let mut per_source: HashMap<String, usize> = HashMap::new();
        let mut out = Vec::new();
        for d in docs {
            let count = per_source.entry(d.source_path.clone()).or_insert(0);
            if *count >= config.candidate_recent_per_category {
                continue;
            }
            *count += 1;
            out.push(MemoryCandidateWithChannel {
                text: d.content.chars().take(config.candidate_text_cap).collect(),
                id: d.id,
                source: d.source_path,
                channel: "recent",
            });
        }
        out
    }

    /// 校验取代指针并落一条 shadow 观测行（阶段一：只记录，不执行）。
    ///
    /// - victim 必须在候选集内（A3 契约），越界的 supersedes 剔除并计 `dropped`；
    /// - analyzer 成功即写 —— **facts 为空也写**（效果门的分母）；
    /// - 写失败只 warn，不影响后续 KB 写入。
    /// - `would_act` 只按 `supersede_per_turn_cap` 做条数封顶，**未套用阶段二
    ///   的前置校验（同槽、`topic_key` 非空）** —— 标定动作率时该口径会偏高，
    ///   读数时须按此折算。
    async fn record_shadow(
        db: &sqlx::SqlitePool,
        user_id: &str,
        config: &MemoryConfig,
        candidates: &[MemoryCandidateWithChannel],
        facts: &[MemoryFact],
    ) {
        if !config.supersede_shadow {
            return;
        }

        let candidate_ids: HashSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let mut validated: Vec<MemoryFact> = Vec::with_capacity(facts.len());
        let mut items = Vec::new();
        let mut raw_count = 0usize;
        let mut dropped = 0usize;
        for (i, fact) in facts.iter().enumerate() {
            let mut kept: Vec<String> = Vec::new();
            let mut victim: Option<String> = None;
            for id in &fact.supersedes {
                raw_count += 1;
                if !candidate_ids.contains(id.as_str()) {
                    dropped += 1;
                    continue;
                }
                if victim.is_none() {
                    victim = Some(id.clone());
                }
                if !kept.iter().any(|k| k == id) {
                    kept.push(id.clone());
                }
            }
            // 越界项不入 items（只计 dropped）
            if let Some(v) = &victim {
                items.push(json!({
                    "fact_index": i,
                    "victim": v,
                }));
            }
            validated.push(MemoryFact {
                category: fact.category,
                content: fact.content.clone(),
                topic: fact.topic.clone(),
                supersedes: kept,
            });
        }

        let decisions = json!({
            "per_turn_cap": config.supersede_per_turn_cap,
            "raw_count": raw_count,
            "dropped": dropped,
            "would_act": items.len().min(config.supersede_per_turn_cap),
            "items": items,
        });
        let candidates_json = Self::candidates_snapshot(candidates);
        let facts_json: Vec<_> = validated
            .iter()
            .map(|f| {
                json!({
                    "category": f.category.as_str(),
                    "content": f.content,
                    "topic": f.topic,
                    "supersedes": f.supersedes,
                })
            })
            .collect();

        let row = ShadowRow {
            user_id: user_id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            candidates_json,
            facts_json: serde_json::to_string(&facts_json).unwrap_or_else(|_| "[]".to_string()),
            decisions_json: serde_json::to_string(&decisions).unwrap_or_else(|_| "{}".to_string()),
            acted: false,
            extracted_topic_cnt: validated.iter().filter(|f| f.topic.is_some()).count() as i64,
            episodic_cnt: validated
                .iter()
                .filter(|f| f.category == MemoryCategory::Episodic)
                .count() as i64,
        };

        match insert_shadow(db, &row).await {
            Ok(id) => debug!(shadow_id = id, user_id = %user_id, "Supersede shadow row written"),
            Err(e) => {
                warn!(error = %e, user_id = %user_id, "Supersede shadow write failed (non-fatal)")
            }
        }
    }

    /// 候选集快照 → JSON 字符串（shadow 行共用）。
    fn candidates_snapshot(candidates: &[MemoryCandidateWithChannel]) -> String {
        let v: Vec<_> = candidates
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "source": c.source,
                    "text": c.text,
                    "channel": c.channel,
                })
            })
            .collect();
        serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())
    }

    /// 落一条「提取失败」观测行（阶段一：只记录，不执行）。
    ///
    /// 与 [`Self::record_shadow`] 的区别：facts 为空、decisions 带 `error` 原因。
    /// 效果门必须看到**全部**尝试轮次（截断 / 超时 / 模型报错），否则分母系统性
    /// 偏小、动作率被高估。
    async fn record_shadow_failure(
        db: &sqlx::SqlitePool,
        user_id: &str,
        config: &MemoryConfig,
        candidates: &[MemoryCandidateWithChannel],
        kind: &str,
        detail: &str,
    ) {
        if !config.supersede_shadow {
            return;
        }
        let decisions = json!({
            "per_turn_cap": config.supersede_per_turn_cap,
            "raw_count": 0,
            "dropped": 0,
            "would_act": 0,
            "items": [],
            "error": { "kind": kind, "detail": detail },
        });
        let row = ShadowRow {
            user_id: user_id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            candidates_json: Self::candidates_snapshot(candidates),
            facts_json: "[]".to_string(),
            decisions_json: serde_json::to_string(&decisions).unwrap_or_else(|_| "{}".to_string()),
            acted: false,
            extracted_topic_cnt: 0,
            episodic_cnt: 0,
        };
        match insert_shadow(db, &row).await {
            Ok(id) => debug!(
                shadow_id = id,
                user_id = %user_id,
                error_kind = kind,
                "Supersede shadow failure row written"
            ),
            Err(e) => warn!(
                error = %e,
                user_id = %user_id,
                "Supersede shadow failure write failed (non-fatal)"
            ),
        }
    }
}

/// fact 对应的 KB source 标签（与写入时一致）。
fn source_of(fact: &MemoryFact) -> String {
    format!("ppa_{}", fact.category.as_str())
}

#[async_trait]
impl LooperHook for MemoryExtractionHook {
    async fn on_turn_complete(
        &self,
        _turn_index: usize,
        failure: Option<&TurnFailureReason>,
        _usage: &Usage,
        session: &Session,
    ) {
        // 失败轮不提取 — 回滚/中断的对话不构成可靠记忆来源
        if failure.is_some() {
            return;
        }

        let Some(dialogue) = Self::collect_turn_dialogue(session) else {
            return;
        };
        if dialogue.chars().count() < self.config.analyze_min_chars {
            return;
        }

        let km = Arc::clone(&self.km);
        let analyzer = Arc::clone(&self.analyzer);
        let config = self.config.clone();
        let db = self.db.clone();
        let user_id = self.user_id.clone();

        tokio::spawn(async move {
            // 双通道候选召回（带 doc_id）：进 prompt 供模型指 supersedes，
            // 快照供 shadow 落库。通道失败不阻断 — 只是失去该通道提示。
            let query = dialogue.chars().take(200).collect::<String>();
            let set = Self::build_candidates(&km, &config, &query).await;

            let prompt_candidates: Vec<MemoryCandidate> =
                set.candidates.iter().map(|c| c.to_candidate()).collect();

            let analyzed = tokio::time::timeout(
                std::time::Duration::from_secs(config.analyzer_timeout_secs),
                analyzer.analyze(&dialogue, &prompt_candidates),
            )
            .await;

            let facts = match analyzed {
                Ok(Ok(facts)) => facts,
                Ok(Err(e)) => {
                    warn!(error = %e, "Memory extraction failed (non-fatal)");
                    // 失败也落一行观测 —— 否则效果门的分母系统性偏小、动作率被高估
                    Self::record_shadow_failure(
                        &db,
                        &user_id,
                        &config,
                        &set.candidates,
                        "analyzer_error",
                        &e,
                    )
                    .await;
                    return;
                }
                Err(_) => {
                    warn!(
                        timeout_secs = config.analyzer_timeout_secs,
                        "Memory extraction timed out (non-fatal)"
                    );
                    Self::record_shadow_failure(
                        &db,
                        &user_id,
                        &config,
                        &set.candidates,
                        "analyzer_timeout",
                        &format!("analyzer timed out after {}s", config.analyzer_timeout_secs),
                    )
                    .await;
                    return;
                }
            };

            // 提取成功即落 shadow（空事实也写 — 效果门的分母）；失败只 warn
            Self::record_shadow(&db, &user_id, &config, &set.candidates, &facts).await;

            if facts.is_empty() {
                return;
            }

            // 写前近重复判定（shadow 下只记日志）。嵌入不可用 → None → 全部放行。
            // 比对集用 `dedup_baseline`（通道 A 原始结果）而非候选集 —— 候选集
            // 已被 candidate_cap / candidate_token_cap 截断，尾部搜索命中若被
            // 挤出候选也仍须参与判重
            let duplicates = Self::near_duplicate_flags(
                &km,
                &config.kb_name,
                &facts,
                &set.dedup_baseline,
                config.consolidation.dedup_cos,
            )
            .await
            .unwrap_or_else(|| vec![false; facts.len()]);
            let enforce = config.consolidation.dedup_enforce;

            // KB 由 personal 模板幂等安装保证存在；缺失（NotFound）按非致命处理
            let base_ts = chrono::Utc::now().timestamp_millis();
            for (i, fact) in facts.iter().enumerate() {
                let source = source_of(fact);
                if duplicates.get(i).copied().unwrap_or(false) {
                    if enforce {
                        info!(
                            kb = %config.kb_name,
                            category = fact.category.as_str(),
                            content = %fact.content,
                            "Near-duplicate memory skipped (dedup enforce)"
                        );
                        continue;
                    }
                    warn!(
                        kb = %config.kb_name,
                        category = fact.category.as_str(),
                        content = %fact.content,
                        "dedup shadow: near-duplicate memory would be suppressed (written anyway)"
                    );
                }
                let title = format!("memory_{base_ts}_{i}");
                match km
                    .add_text_to_kb(&config.kb_name, &title, &fact.content, &source)
                    .await
                {
                    Ok(_) => {
                        info!(
                            kb = %config.kb_name,
                            category = fact.category.as_str(),
                            "Memory written"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, kb = %config.kb_name, "Memory write failed (non-fatal)");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::analyzer::{MemoryCategory, MemoryFact};
    use super::*;
    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
    use peco_core::session::{MessageSource, Session};

    /// 可编程 mock 提取器 — 记录调用入参，返回预设结果。
    struct MockAnalyzer {
        result: std::sync::Mutex<Result<Vec<MemoryFact>, String>>,
        calls: std::sync::Mutex<Vec<(String, Vec<MemoryCandidate>)>>,
    }

    impl MockAnalyzer {
        fn ok(facts: Vec<MemoryFact>) -> Self {
            Self {
                result: std::sync::Mutex::new(Ok(facts)),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn err(msg: &str) -> Self {
            Self {
                result: std::sync::Mutex::new(Err(msg.to_string())),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TurnAnalyzer for MockAnalyzer {
        async fn analyze(
            &self,
            turn_dialogue: &str,
            candidates: &[MemoryCandidate],
        ) -> Result<Vec<MemoryFact>, String> {
            self.calls
                .lock()
                .unwrap()
                .push((turn_dialogue.to_string(), candidates.to_vec()));
            self.result.lock().unwrap().clone()
        }
    }

    /// 已迁移的 shadow 测试池（tempdir 须被持有到断言之后）。
    async fn migrated_pool() -> (sqlx::SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 构造挂已迁移池的 hook（shadow 表存在，user_id 固定为 test-user）。
    async fn make_hook(
        km: Arc<KnowledgeManager>,
        analyzer: Arc<dyn TurnAnalyzer>,
        config: MemoryConfig,
    ) -> (MemoryExtractionHook, sqlx::SqlitePool, tempfile::TempDir) {
        let (pool, dir) = migrated_pool().await;
        let hook =
            MemoryExtractionHook::new(km, analyzer, config, pool.clone(), "test-user".to_string());
        (hook, pool, dir)
    }

    fn make_test_kb_config(name: &str) -> KbConfig {
        KbConfig {
            name: name.to_string(),
            description: "测试记忆库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
            helix_url: None,
        }
    }

    fn make_config() -> MemoryConfig {
        MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            ..MemoryConfig::default()
        }
    }

    /// 去重门控配置：`dedup_cos` 取 0.9（与既有 consolidation 测试同档，
    /// 标点变体近重复对实测 ≈0.999）。
    fn dedup_config(enforce: bool) -> MemoryConfig {
        MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            consolidation: super::super::config::ConsolidationConfig {
                dedup_enforce: enforce,
                dedup_cos: 0.9,
                ..Default::default()
            },
            ..MemoryConfig::default()
        }
    }

    /// 已有一条既有记忆的 KB（写路径去重的比对对象）。
    async fn make_km_with_existing(
        title: &str,
        content: &str,
        source: &str,
    ) -> Arc<KnowledgeManager> {
        let km = make_km().await;
        km.add_text_to_kb("@private_memory", title, content, source)
            .await
            .unwrap();
        km
    }

    /// 多条记忆的 KB（`(title, content, source)` 三元组，按序写入）。
    async fn make_km_with_docs(docs: &[(&str, &str, &str)]) -> Arc<KnowledgeManager> {
        let km = make_km().await;
        for (title, content, source) in docs {
            km.add_text_to_kb("@private_memory", title, content, source)
                .await
                .unwrap();
        }
        km
    }

    /// 轮询等待后台任务被分析器调用（`on_turn_complete` 的 spawn 是异步的）。
    async fn wait_for_analyzer(analyzer: &MockAnalyzer) {
        for _ in 0..100 {
            if !analyzer.calls.lock().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("分析器应在超时前被调用");
    }

    async fn doc_count(km: &KnowledgeManager) -> usize {
        km.list_documents("@private_memory", 0, 100)
            .await
            .unwrap()
            .len()
    }

    /// 构造一个已提交一轮对话的 session（User + Assistant 文本）。
    fn make_session_with_turn(user: &str, assistant: &str) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        s.start_turn(user.into()).unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::Message {
                role: Role::Assistant,
                content: assistant.into(),
            },
        )
        .unwrap();
        let _ = s.commit_turn().unwrap();
        s
    }

    async fn make_km() -> Arc<KnowledgeManager> {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(make_test_kb_config("@private_memory"))
            .await
            .unwrap();
        // tempdir 由调用方持有不能释放 — leak 掉测试目录（进程退出回收）
        std::mem::forget(tmp);
        km
    }

    #[tokio::test]
    async fn test_skips_failed_turn() {
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let (hook, _pool, _dir) = make_hook(make_km().await, analyzer.clone(), make_config()).await;
        let session = make_session_with_turn(
            "这是一个足够长的用户提问内容",
            "这是一个足够长的助手回答内容",
        );

        hook.on_turn_complete(
            0,
            Some(&TurnFailureReason::Cancelled),
            &Usage::default(),
            &session,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            analyzer.calls.lock().unwrap().is_empty(),
            "失败轮不得触发提取"
        );
    }

    #[tokio::test]
    async fn test_skips_short_turns() {
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let (hook, _pool, _dir) = make_hook(make_km().await, analyzer.clone(), make_config()).await;
        // "用户: 你好\n" 共 8 字符 < analyze_min_chars(10)
        let session = make_session_with_turn("你好", "");

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            analyzer.calls.lock().unwrap().is_empty(),
            "短轮不得触发提取"
        );
    }

    #[tokio::test]
    async fn test_extracts_and_writes_to_kb() {
        let km = make_km().await;
        let facts = vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: "用户偏好简洁的回答风格".to_string(),
            topic: None,
            supersedes: vec![],
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        let (hook, _pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;
        let session = make_session_with_turn(
            "请记住：我偏好简洁的回答风格，以后所有回答都尽量精炼",
            "好的，我已记住你的偏好，之后会以简洁风格回答。",
        );

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        // spawn 的后台任务需要时间完成（含 embedding 索引），轮询等待
        for _ in 0..100 {
            let docs = km.list_documents("@private_memory", 0, 10).await.unwrap();
            if !docs.is_empty() {
                assert_eq!(docs[0].source_path, "ppa_profile");
                assert!(docs[0].title.starts_with("memory_"), "标题应带时间戳前缀");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("记忆应在超时前写入 KB");
    }

    #[tokio::test]
    async fn test_analyzer_error_is_non_fatal() {
        let km = make_km().await;
        let analyzer = Arc::new(MockAnalyzer::err("model exploded"));
        let (hook, _pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;
        let session = make_session_with_turn(
            "这是一个足够长的用户提问内容",
            "这是一个足够长的助手回答内容",
        );

        // 不应 panic、不应上抛（on_turn_complete 无返回值即编译期保证）
        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        // spawn 的后台任务需要时间，轮询等待提取器被调用（固定 sleep 会 flaky）
        for _ in 0..100 {
            if analyzer.calls.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let docs = km.list_documents("@private_memory", 0, 10).await.unwrap();
        assert!(docs.is_empty(), "提取失败时不得写入");
        assert_eq!(
            analyzer.calls.lock().unwrap().len(),
            1,
            "提取器应被调用一次"
        );
    }

    #[tokio::test]
    async fn test_analyzer_error_writes_failure_shadow_row() {
        let km = make_km().await;
        let analyzer = Arc::new(MockAnalyzer::err("model exploded"));
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;
        let session = make_session_with_turn(
            "这是一个足够长的用户提问内容",
            "这是一个足够长的助手回答内容",
        );

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        // 等失败观测行落库（spawn 后台任务）
        let mut n = 0i64;
        for _ in 0..100 {
            n = crate::db::memory_supersede::count_for_user(&pool, "test-user")
                .await
                .unwrap();
            if n >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(n, 1, "提取失败也必须落一行观测（效果门分母）");

        let decisions: String = sqlx::query_scalar(
            "SELECT decisions_json FROM memory_supersede_shadow \
             WHERE user_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind("test-user")
        .fetch_one(&pool)
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&decisions).unwrap();
        assert_eq!(v["error"]["kind"], "analyzer_error");
        assert_eq!(v["error"]["detail"], "model exploded");
        assert_eq!(v["would_act"], 0);
    }

    // ── 写路径去重（Stage 4 / 事项 5）────────────────────────────────────

    /// 既有记忆与本轮提取结果近重复（标点变体，余弦 ≈1.0）。
    const EXISTING: &str = "The user prefers concise answers when discussing Rust.";
    const NEAR_DUP: &str = "The user prefers concise answers when discussing Rust!";

    fn near_dup_facts() -> Vec<MemoryFact> {
        vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: NEAR_DUP.to_string(),
            topic: None,
            supersedes: vec![],
        }]
    }

    fn dedup_session() -> Session {
        make_session_with_turn(
            "Please remember that I prefer concise answers when discussing Rust topics.",
            "Got it — I will keep answers about Rust concise from now on.",
        )
    }

    #[tokio::test]
    async fn dedup_enforce_skips_near_duplicate_write() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        let analyzer = Arc::new(MockAnalyzer::ok(near_dup_facts()));
        let (hook, _pool, _dir) =
            make_hook(Arc::clone(&km), analyzer.clone(), dedup_config(true)).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_analyzer(&analyzer).await;
        // 判定后无写入发生 —— 留足后台任务收尾时间再断言
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        assert_eq!(
            doc_count(&km).await,
            1,
            "enforce 下近重复事实不得写入（仅存量那条）"
        );
    }

    #[tokio::test]
    async fn dedup_shadow_writes_near_duplicate() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        let analyzer = Arc::new(MockAnalyzer::ok(near_dup_facts()));
        let (hook, _pool, _dir) =
            make_hook(Arc::clone(&km), analyzer.clone(), dedup_config(false)).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        // shadow：判定照算、日志照记，但写入照常发生 → 条数增至 2
        for _ in 0..100 {
            if doc_count(&km).await == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("shadow 模式下近重复事实应照常写入");
    }

    #[tokio::test]
    async fn dedup_ignores_other_categories() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        // 同样的文本但归为 episodic → 与既有 ppa_profile 不同类目，不判重
        let analyzer = Arc::new(MockAnalyzer::ok(vec![MemoryFact {
            category: MemoryCategory::Episodic,
            content: NEAR_DUP.to_string(),
            topic: None,
            supersedes: vec![],
        }]));
        let (hook, _pool, _dir) =
            make_hook(Arc::clone(&km), analyzer.clone(), dedup_config(true)).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        for _ in 0..100 {
            if doc_count(&km).await == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("跨类目不判重，事实应照常写入");
    }

    #[tokio::test]
    async fn dedup_check_degrades_when_embedding_unavailable() {
        // KB 不存在 → embed_texts 失败 → 返回 None（调用方降级放行全部）
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        std::mem::forget(tmp);

        let flags = MemoryExtractionHook::near_duplicate_flags(
            &km,
            "@private_memory",
            &near_dup_facts(),
            &[("ppa_profile".to_string(), EXISTING.to_string())],
            0.9,
        )
        .await;
        assert!(flags.is_none(), "嵌入不可用应返回 None 供调用方降级放行");
    }

    #[tokio::test]
    async fn dedup_flags_only_same_category_near_duplicates() {
        let km = make_km().await;
        let facts = vec![
            MemoryFact {
                category: MemoryCategory::Profile,
                content: NEAR_DUP.to_string(),
                topic: None,
                supersedes: vec![],
            },
            MemoryFact {
                category: MemoryCategory::Semantic,
                content: "The user's primary programming language is Rust.".to_string(),
                topic: None,
                supersedes: vec![],
            },
        ];
        let existing = vec![
            ("ppa_profile".to_string(), EXISTING.to_string()),
            (
                "ppa_semantic".to_string(),
                "The user commutes by bicycle every day.".to_string(),
            ),
        ];

        let flags = MemoryExtractionHook::near_duplicate_flags(
            &km,
            "@private_memory",
            &facts,
            &existing,
            0.9,
        )
        .await
        .expect("KB 存在时嵌入可用");

        assert_eq!(flags, vec![true, false], "只对同类目近重复置位");
    }

    // ── 取代机制 · 阶段一（在线 shadow）────────────────────────────────────

    async fn shadow_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_supersede_shadow")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn shadow_json_col(pool: &sqlx::SqlitePool, col: &str) -> String {
        sqlx::query_scalar::<_, String>(&format!("SELECT {col} FROM memory_supersede_shadow"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// 轮询等待 shadow 行落库（record_shadow 在 spawn 的后台任务里）。
    async fn wait_for_shadow(pool: &sqlx::SqlitePool, expected: i64) {
        for _ in 0..100 {
            if shadow_count(pool).await == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("shadow 行应在超时前达到 {expected} 条");
    }

    #[tokio::test]
    async fn shadow_records_supersede_victim_from_candidates() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        let victim = km.list_documents("@private_memory", 0, 10).await.unwrap()[0]
            .id
            .clone();
        let facts = vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: "用户现在偏好详尽的回答".to_string(),
            topic: Some("answer_style".to_string()),
            supersedes: vec![victim.clone()],
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_shadow(&pool, 1).await;

        let decisions: serde_json::Value =
            serde_json::from_str(&shadow_json_col(&pool, "decisions_json").await).unwrap();
        assert_eq!(
            decisions["items"][0]["victim"].as_str(),
            Some(victim.as_str()),
            "decisions_json 应含候选集内的 victim"
        );
        assert_eq!(decisions["would_act"], 1);
        assert_eq!(decisions["raw_count"], 1);
        assert_eq!(decisions["dropped"], 0);

        // 候选快照带 id（供离线算 oracle_hit），facts_json 带 topic
        assert!(
            shadow_json_col(&pool, "candidates_json")
                .await
                .contains(&victim),
            "候选快照应含 victim id"
        );
        assert!(
            shadow_json_col(&pool, "facts_json")
                .await
                .contains("\"topic\":\"answer_style\""),
            "facts_json 应含 topic"
        );

        // KB append 照旧：既有 1 条 + 新写 1 条，victim 不被删除
        for _ in 0..100 {
            if doc_count(&km).await == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("KB 应 append 新事实，victim 保留（doc_count=2）");
    }

    #[tokio::test]
    async fn shadow_drops_supersede_outside_candidates() {
        let km = make_km().await;
        let facts = vec![MemoryFact {
            category: MemoryCategory::Semantic,
            content: "项目切换到新架构".to_string(),
            topic: Some("project_phase".to_string()),
            supersedes: vec!["doc-not-in-candidate-set".to_string()],
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_shadow(&pool, 1).await;

        let decisions: serde_json::Value =
            serde_json::from_str(&shadow_json_col(&pool, "decisions_json").await).unwrap();
        assert_eq!(decisions["raw_count"], 1);
        assert_eq!(decisions["dropped"], 1, "越界 victim 计入 dropped");
        assert!(
            decisions["items"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false),
            "越界项不得进入 items"
        );
        assert_eq!(decisions["would_act"], 0);
        // facts_json 中越界 supersedes 已被剔除（A3 契约）
        let facts_json = shadow_json_col(&pool, "facts_json").await;
        assert!(
            !facts_json.contains("doc-not-in-candidate-set"),
            "facts_json 不得保留候选集外的 supersedes: {facts_json}"
        );

        // 越界只影响决策记录 —— KB 写入照旧（本阶段零删除面）
        for _ in 0..100 {
            if doc_count(&km).await == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("候选越界不得阻断 KB append");
    }

    #[tokio::test]
    async fn shadow_disabled_writes_no_row() {
        let km = make_km().await;
        let facts = vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: "用户偏好简洁的回答风格".to_string(),
            topic: None,
            supersedes: vec![],
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        let config = MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            supersede_shadow: false,
            ..MemoryConfig::default()
        };
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), config).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        // KB 写入发生在 record_shadow 之后 —— 写入完成即证明 shadow 已被求值
        for _ in 0..100 {
            if doc_count(&km).await == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(doc_count(&km).await, 1, "shadow 关闭不影响 KB 写入");
        assert_eq!(
            shadow_count(&pool).await,
            0,
            "supersede_shadow=false 时不得写 shadow 行"
        );
    }

    #[tokio::test]
    async fn empty_facts_still_writes_shadow_row() {
        let km = make_km().await;
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), make_config()).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_shadow(&pool, 1).await;

        assert_eq!(
            shadow_json_col(&pool, "facts_json").await,
            "[]",
            "空事实轮仍落行，facts_json 为空数组（效果门分母）"
        );
    }

    #[tokio::test]
    async fn shadow_write_failure_does_not_block_kb_write() {
        let km = make_km().await;
        let facts = vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: "用户偏好简洁的回答风格".to_string(),
            topic: None,
            supersedes: vec![],
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        // 未迁移的池 — memory_supersede_shadow 不存在，insert 必失败
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        let hook = MemoryExtractionHook::new(
            Arc::clone(&km),
            analyzer.clone(),
            make_config(),
            pool,
            "test-user".to_string(),
        );

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        for _ in 0..100 {
            if doc_count(&km).await == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("shadow 写失败不得阻断 KB 写入");
    }

    // ── 候选召回：双通道 / 上限 / 降级 ────────────────────────────────────

    /// 两条与查询共享全部词元的 ppa 文档 —— 全文路径得分 1.0，search 必命中。
    const CHANNEL_DOCS: [(&str, &str, &str); 2] = [
        (
            "memory_2000_0",
            "The user prefers concise answers when discussing Rust.",
            "ppa_profile",
        ),
        (
            "memory_2001_0",
            "The user prefers concise answers when discussing Python.",
            "ppa_semantic",
        ),
    ];
    const CHANNEL_QUERY: &str = "user prefers concise answers";

    /// T1a（开）：shadow 开启时通道 B 必须贡献候选（channel != "search"），
    /// 且双通道共同命中的 id 合并为 "both"、按 id 去重。
    #[tokio::test]
    async fn channel_b_contributes_when_shadow_enabled() {
        let km = make_km_with_docs(&CHANNEL_DOCS).await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            ..MemoryConfig::default()
        };
        let set = MemoryExtractionHook::build_candidates(&km, &config, CHANNEL_QUERY).await;

        let channels: Vec<&str> = set.candidates.iter().map(|c| c.channel).collect();
        assert!(
            channels.iter().any(|c| *c != "search"),
            "shadow 开启时通道 B 应贡献候选，实际 channels: {channels:?}"
        );
        assert!(
            channels.contains(&"both"),
            "双通道共同命中的候选应合并为 both，实际 channels: {channels:?}"
        );
        let ids: HashSet<&str> = set.candidates.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            set.candidates.len(),
            "合并按 id 去重后不得有重复候选"
        );
    }

    /// T1a（关）：shadow 与 enforce 同时关闭 → 只走通道 A，候选全部
    /// channel == "search"（门控真能关）。
    #[tokio::test]
    async fn channel_b_gated_off_returns_search_only() {
        let km = make_km_with_docs(&CHANNEL_DOCS).await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            supersede_shadow: false,
            supersede_enforce: false,
            ..MemoryConfig::default()
        };
        let set = MemoryExtractionHook::build_candidates(&km, &config, CHANNEL_QUERY).await;

        assert!(!set.candidates.is_empty(), "通道 A 应召回候选");
        let channels: Vec<&str> = set.candidates.iter().map(|c| c.channel).collect();
        assert!(
            channels.iter().all(|c| *c == "search"),
            "门控关闭时不得出现 recent/both，实际 channels: {channels:?}"
        );
    }

    /// T1b：`recent_candidates` 自身语义 —— 每类目取 1 条、非 `ppa_` 前缀
    /// 过滤、channel 全为 recent、`created_at` 倒序取较晚写入的那条。
    #[tokio::test]
    async fn recent_candidates_filters_groups_and_orders_by_created_at() {
        let km = make_km().await;
        km.add_text_to_kb(
            "@private_memory",
            "memory_2100_0",
            "semantic fact alpha version",
            "ppa_semantic",
        )
        .await
        .unwrap();
        // 拉开两条同类目文档的 created_at（倒序判定依赖时间戳可分）
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        km.add_text_to_kb(
            "@private_memory",
            "memory_2101_0",
            "semantic fact beta version",
            "ppa_semantic",
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        km.add_text_to_kb(
            "@private_memory",
            "memory_2102_0",
            "episodic event gamma",
            "ppa_episodic",
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        km.add_text_to_kb(
            "@private_memory",
            "memory_2103_0",
            "manual note delta",
            "manual",
        )
        .await
        .unwrap();

        let config = MemoryConfig {
            analyze_min_chars: 10,
            candidate_recent_per_category: 1,
            ..MemoryConfig::default()
        };
        let out = MemoryExtractionHook::recent_candidates(&km, &config).await;

        assert_eq!(out.len(), 2, "每类目取 1 条 ⇒ 恰 2 条，实为 {}", out.len());
        let sources: HashSet<&str> = out.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(
            sources,
            HashSet::from(["ppa_semantic", "ppa_episodic"]),
            "非 ppa_ 前缀的文档应被过滤，实际 source: {sources:?}"
        );
        assert!(
            out.iter().all(|c| c.channel == "recent"),
            "recent_candidates 产出的 channel 应恒为 recent"
        );
        let semantic = out
            .iter()
            .find(|c| c.source == "ppa_semantic")
            .expect("应含 ppa_semantic 候选");
        assert_eq!(
            semantic.text, "semantic fact beta version",
            "created_at 倒序 ⇒ 同类目应取较晚写入的 beta"
        );
    }

    /// T2：`candidate_cap` 条数截断 —— 写入 > cap 条时返回长度 ≤ 1。
    #[tokio::test]
    async fn candidate_cap_truncates_count() {
        let km = make_km_with_docs(&[
            (
                "memory_2200_0",
                "fact number one for cap test",
                "ppa_profile",
            ),
            (
                "memory_2201_0",
                "fact number two for cap test",
                "ppa_profile",
            ),
            (
                "memory_2202_0",
                "fact number three for cap test",
                "ppa_profile",
            ),
        ])
        .await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            candidate_cap: 1,
            ..MemoryConfig::default()
        };
        let set = MemoryExtractionHook::build_candidates(&km, &config, CHANNEL_QUERY).await;

        assert!(!set.candidates.is_empty(), "通道应召回候选");
        assert!(
            set.candidates.len() <= 1,
            "candidate_cap=1 时返回长度应 ≤ 1，实为 {}",
            set.candidates.len()
        );
    }

    /// T3：`candidate_text_cap` 字符截断 —— 近期通道正文按字符数截断。
    #[tokio::test]
    async fn candidate_text_cap_truncates_recent_text() {
        let km = make_km_with_docs(&[(
            "memory_2300_0",
            "A long memory sentence that definitely exceeds eight characters.",
            "ppa_semantic",
        )])
        .await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            candidate_text_cap: 8,
            ..MemoryConfig::default()
        };
        let out = MemoryExtractionHook::recent_candidates(&km, &config).await;

        assert_eq!(out.len(), 1, "应召回唯一的 ppa 文档");
        let len = out[0].text.chars().count();
        assert!(
            len <= 8,
            "正文应截断到 candidate_text_cap=8，实为 {len} 字符"
        );
    }

    /// T4：`candidate_token_cap` 硬上限 —— 首条渲染行即超限则返回空，
    /// 不得兜底放行首条。
    #[tokio::test]
    async fn candidate_token_cap_hard_limit_returns_empty() {
        let km = make_km_with_docs(&CHANNEL_DOCS).await;
        let base = MemoryConfig {
            analyze_min_chars: 10,
            ..MemoryConfig::default()
        };
        let full = MemoryExtractionHook::build_candidates(&km, &base, CHANNEL_QUERY).await;
        assert!(
            !full.candidates.is_empty(),
            "对照：无 token 上限时应有候选，否则本测试无意义"
        );

        let config = MemoryConfig {
            analyze_min_chars: 10,
            candidate_token_cap: 1,
            ..MemoryConfig::default()
        };
        let capped = MemoryExtractionHook::build_candidates(&km, &config, CHANNEL_QUERY).await;
        assert!(
            capped.candidates.is_empty(),
            "首行超 candidate_token_cap=1 时应返回空候选（不得兜底放行）"
        );
    }

    /// N1：近重复比对集不受候选上限截断 —— 候选被 `candidate_token_cap`
    /// 截空时，`dedup_baseline` 仍含全部 `search_kb` 命中。
    #[tokio::test]
    async fn dedup_baseline_survives_candidate_token_cap() {
        let km = make_km_with_docs(&[
            (
                "memory_2400_0",
                "The user prefers concise answers about rust performance tuning.",
                "ppa_profile",
            ),
            (
                "memory_2401_0",
                "The user prefers concise answers about rust performance profiling.",
                "ppa_semantic",
            ),
            (
                "memory_2402_0",
                "The user prefers concise answers about rust performance debugging.",
                "ppa_episodic",
            ),
        ])
        .await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            candidate_token_cap: 1,
            ..MemoryConfig::default()
        };
        let set = MemoryExtractionHook::build_candidates(
            &km,
            &config,
            "user prefers concise answers about rust performance",
        )
        .await;

        assert!(
            set.candidates.is_empty(),
            "候选集应被 candidate_token_cap=1 截空（否则本测试未覆盖截断）"
        );
        assert_eq!(
            set.dedup_baseline.len(),
            config.extraction_top_k,
            "近重复比对集须含全部 search_kb 命中，不受候选上限截断"
        );
    }

    /// T5（静态）：双通道 KB 不存在 → 双双降级，返回空且不 panic。
    #[tokio::test]
    async fn build_candidates_degrades_to_empty_on_missing_kb() {
        let km = make_km().await;
        let config = MemoryConfig {
            analyze_min_chars: 10,
            kb_name: "@no_such_kb".to_string(),
            ..MemoryConfig::default()
        };
        let set = MemoryExtractionHook::build_candidates(&km, &config, CHANNEL_QUERY).await;

        assert!(set.candidates.is_empty(), "两通道均失败应返回空候选");
        assert!(set.dedup_baseline.is_empty());
    }

    /// T5（端到端）：候选为空不得跳过提取 —— analyzer 仍被调用，
    /// shadow 行照写（candidates_json == "[]"，效果门分母）。
    #[tokio::test]
    async fn missing_kb_still_runs_extraction_and_shadow() {
        let km = make_km().await;
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let config = MemoryConfig {
            analyze_min_chars: 10,
            kb_name: "@no_such_kb".to_string(),
            ..MemoryConfig::default()
        };
        let (hook, pool, _dir) = make_hook(Arc::clone(&km), analyzer.clone(), config).await;

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_shadow(&pool, 1).await;
        wait_for_analyzer(&analyzer).await;

        {
            let calls = analyzer.calls.lock().unwrap();
            assert_eq!(calls.len(), 1, "候选为空不得跳过提取");
            assert!(calls[0].1.is_empty(), "双通道降级后候选为空");
        }
        assert_eq!(
            shadow_json_col(&pool, "candidates_json").await,
            "[]",
            "shadow 行应记录空候选快照"
        );
    }
}
