// ============================================================================
// 记忆双路径配置
// ============================================================================
//
// 记忆双路径的参数集中处。写路径（MemoryExtractionHook）与读路径
// （MemoryRecallContext）共享同一份 MemoryConfig，由 PecoManager 在
// 构造期装配进 PecoConfig.hooks / .dynamic_context。

/// 自动整理（巩固流水线）配置。
///
/// ConsolidationWorker 的行为参数。默认 `enabled: false`（灰度开启）——
/// 未开启时不创建 worker、不注册 cron，零开销。
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    /// 全局总开关。`false` 默认关闭。
    pub enabled: bool,
    /// per-user 单轮处理条数上限。
    pub batch_size: usize,
    /// 聚类阈值（cosine similarity）。
    ///
    /// 标定值（bge-base-zh-v1.5, 768 维, 2026-09-15, 113 对合成真值集,
    /// 人工编写待抽检）
    pub min_cluster_cos: f32,
    /// 硬去重阈值（cosine similarity）。
    ///
    /// 标定值（bge-base-zh-v1.5, 768 维, 2026-09-15, 113 对合成真值集,
    /// 人工编写待抽检）
    pub dedup_cos: f32,
    /// episodic 过期天数。
    pub episodic_ttl_days: u64,
    /// per-user 单轮 LLM 调用上限（只调 Flash 档）。
    pub max_llm_calls: usize,
    /// 每 cron tick 最多整理用户数。
    pub max_users_per_round: usize,
    /// 空闲判定阈值（秒）：距上次活动超过该值才参与本轮整理。
    pub idle_after_secs: u64,
    /// 整理 cron 表达式（默认每 30 分钟）。
    ///
    /// **6 字段**（秒 分 时 日 月 周）—— 调度器 `tokio-cron-scheduler`
    /// 内部 `Cron::with_seconds_required()`，5 字段表达式会被判为
    /// `ParseSchedule` 而注册失败。
    pub cron_expr: String,
    /// "近期召回"窗口（天）：`last_recalled_at` 距今超过该窗口视为无近期召回。
    pub recall_fresh_days: u64,
    /// 召回统计观察期（天）：统计缺失（从未召回或统计面缺损）的条目
    /// 须达到该条目龄才视为"无召回"可删 —— 统计缺失 ≠ 无召回。
    ///
    /// 与 `episodic_ttl_days` 同为条目龄门槛，二者取大者生效：默认
    /// 30 < 60（TTL），观察期被 TTL 完全覆盖，缺失统计的条目自动回落
    /// 双条件（已沉淀 + 超 TTL）；仅当运维把观察期调得比 TTL 更长时，
    /// 该分支才额外多拦一段。
    pub recall_observation_days: u64,
    /// 审计行保留期（天）：终态审计行超过该期物理清除。
    pub audit_retention_days: u64,
    /// 去重执行开关（读写双路径共用）。
    ///
    /// `false`（默认）= shadow：判定照常计算并记日志，但写路径照常写入、
    /// 读路径只重排不去重。标定报告人工抽检通过前必须保持 `false`。
    pub dedup_enforce: bool,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            batch_size: 200,
            min_cluster_cos: 0.79,
            dedup_cos: 0.88,
            episodic_ttl_days: 60,
            max_llm_calls: 20,
            max_users_per_round: 3,
            idle_after_secs: 600,
            cron_expr: "0 */30 * * * *".to_string(),
            recall_fresh_days: 14,
            recall_observation_days: 30,
            audit_retention_days: 90,
            dedup_enforce: false,
        }
    }
}

// ── 自动整理的环境变量入口 ─────────────────────────────────────────────

pub const ENV_CONSOLIDATION_ENABLED: &str = "PECO_MEMORY_CONSOLIDATION_ENABLED";
pub const ENV_CONSOLIDATION_CRON: &str = "PECO_MEMORY_CONSOLIDATION_CRON";
pub const ENV_CONSOLIDATION_BATCH_SIZE: &str = "PECO_MEMORY_CONSOLIDATION_BATCH_SIZE";
pub const ENV_CONSOLIDATION_MAX_LLM_CALLS: &str = "PECO_MEMORY_CONSOLIDATION_MAX_LLM_CALLS";
pub const ENV_CONSOLIDATION_IDLE_AFTER_SECS: &str = "PECO_MEMORY_CONSOLIDATION_IDLE_AFTER_SECS";
pub const ENV_CONSOLIDATION_DEDUP_ENFORCE: &str = "PECO_MEMORY_CONSOLIDATION_DEDUP_ENFORCE";

impl ConsolidationConfig {
    /// 从环境变量读取，未设置的字段取 [`Self::default`]。
    ///
    /// 未经 env 暴露的字段（阈值、保留期等）只能改 `Default`。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// [`Self::from_env`] 的内核：env 查找由调用方提供。
    fn from_env_with<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let default = Self::default();
        Self {
            enabled: env_bool(&get, ENV_CONSOLIDATION_ENABLED, default.enabled),
            cron_expr: env_string(&get, ENV_CONSOLIDATION_CRON, default.cron_expr),
            batch_size: env_parse(&get, ENV_CONSOLIDATION_BATCH_SIZE, default.batch_size),
            max_llm_calls: env_parse(&get, ENV_CONSOLIDATION_MAX_LLM_CALLS, default.max_llm_calls),
            idle_after_secs: env_parse(
                &get,
                ENV_CONSOLIDATION_IDLE_AFTER_SECS,
                default.idle_after_secs,
            ),
            dedup_enforce: env_bool(&get, ENV_CONSOLIDATION_DEDUP_ENFORCE, default.dedup_enforce),
            ..default
        }
    }
}

fn env_parse<T: std::str::FromStr, F: Fn(&str) -> Option<String>>(
    get: &F,
    name: &str,
    default: T,
) -> T {
    match get(name) {
        Some(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(
                    variable = name,
                    value = %raw,
                    "Invalid numeric env var; using default"
                );
                default
            }
        },
        None => default,
    }
}

fn env_string<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: String) -> String {
    match get(name) {
        Some(raw) if !raw.trim().is_empty() => raw.trim().to_string(),
        _ => default,
    }
}

fn env_bool<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: bool) -> bool {
    match get(name) {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            other => {
                tracing::warn!(
                    variable = name,
                    value = %other,
                    "Invalid boolean env var; using default"
                );
                default
            }
        },
        None => default,
    }
}

/// 记忆双路径配置。
///
/// 存储载体是 workspace 内的 `@private_memory` 知识库（personal 模板
/// 幂等安装，per-user 目录隔离，LanceDb 后端）——本配置只描述行为参数，
/// 不描述存储后端。
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// 总开关。`false` 时 PecoManager 不装配任何记忆组件（零开销）。
    pub enabled: bool,
    /// 记忆知识库名（与 personal 模板保持一致）。
    pub kb_name: String,
    /// 提取模型（Flash 档，低延迟低成本；复用主 Agent 的 provider）。
    pub model: String,
    /// 本轮对话总字符数低于该值时不提取（寒暄过滤）。
    pub analyze_min_chars: usize,
    /// 提取前检索既有记忆的条数（进入 prompt 供模型判断"是否为新信息"）。
    pub extraction_top_k: usize,
    /// 读路径单次检索条数。
    pub recall_top_k: usize,
    /// 读路径注入的 token 上限（校准估算），超出整行丢弃。
    pub injection_token_cap: usize,
    /// 单次提取调用的超时（秒）。
    pub analyzer_timeout_secs: u64,
    /// 读路径重排的 episodic 半衰期（天）。
    ///
    /// `recency_factor = 0.5^(age_days / recall_half_life_days)`，只作用于
    /// episodic（事件类记忆随时间失效）；profile/semantic 不衰减。
    pub recall_half_life_days: u64,
    /// 自动整理（巩固流水线）配置。
    pub consolidation: ConsolidationConfig,

    // ── 取代机制 · 阶段一（在线 shadow：只观测，不删除）────────────────
    /// 是否写 shadow 观测行（效果门数据源）。
    ///
    /// `false` 时提取照常、KB 照写，但不落 `memory_supersede_shadow`，
    /// 且候选召回退回单通道（见 hook 的 `build_candidates`）。
    pub supersede_shadow: bool,
    /// 是否真退役（取代执行开关）。
    ///
    /// **阶段一恒 `false`** —— 只留位：除候选召回门控外没有任何读取点，
    /// 不触发任何删除；效果门标定完成前禁止开启（fail-closed）。
    pub supersede_enforce: bool,
    /// 双通道「近期」通道每类目取几条。
    pub candidate_recent_per_category: usize,
    /// 候选总数上限。
    pub candidate_cap: usize,
    /// 单条候选展示文本的**字符**上限（近期通道对 `content` 截断）。
    pub candidate_text_cap: usize,
    /// 候选区 prompt 的 token 上限（`estimate_str_tokens` 估算，超限截断）。
    pub candidate_token_cap: usize,
    /// 单轮最多接受几条取代决策（shadow 记 `would_act`，enforce 时为意图上限）。
    pub supersede_per_turn_cap: usize,
    /// 「近期通道」全量扫描文档的上限（与 `@memory` 检索端点同口径）。
    pub shadow_scan_limit: usize,
    /// shadow 行保留期（天）。本阶段只提供清理函数，未接调度器。
    pub shadow_retention_days: u64,

    // ── 取代机制 · 阶段二（enforcement / 对账 / 保留期）──────────────────
    /// audit 中 superseded 行保留期（天），走短档。
    pub superseded_retention_days: u64,
    /// intent(done) 保留期（天），按 updated_at=落终态时刻计。
    pub intent_done_retention_days: u64,
    /// intent(failed/cancelled) 保留期（天）。
    pub intent_failed_retention_days: u64,
    /// 每轮对账领取条数上限（M3 逐条 CAS，此值只控条数）。
    pub reconcile_batch: i64,
    /// processing 陈旧回收阈值（秒）：claimed_at 早于 now−该值可被重领。
    pub reconcile_claim_timeout_secs: u64,
    /// 对账重试上界：attempts 超该值转 failed。
    pub reconcile_max_attempts: i64,
    /// audit pending 收口阈值（秒）：超时且 doc 不在 KB 才补 done。
    pub audit_pending_timeout_secs: u64,
    /// 回滚沿 successor 链的 hop 上限（S3b restore 用，本阶段先留位）。
    pub restore_walk_max_hops: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            kb_name: "@private_memory".to_string(),
            model: "deepseek-v4-flash".to_string(),
            analyze_min_chars: 50,
            extraction_top_k: 5,
            recall_top_k: 5,
            injection_token_cap: 1000,
            analyzer_timeout_secs: 10,
            recall_half_life_days: 30,
            consolidation: ConsolidationConfig::default(),
            supersede_shadow: true,
            supersede_enforce: false,
            candidate_recent_per_category: 10,
            candidate_cap: 30,
            candidate_text_cap: 200,
            candidate_token_cap: 2000,
            supersede_per_turn_cap: 5,
            shadow_scan_limit: 2000,
            shadow_retention_days: 30,
            superseded_retention_days: 30,
            intent_done_retention_days: 7,
            intent_failed_retention_days: 90,
            reconcile_batch: 50,
            reconcile_claim_timeout_secs: 300,
            reconcile_max_attempts: 3,
            audit_pending_timeout_secs: 600,
            restore_walk_max_hops: 16,
        }
    }
}

impl MemoryConfig {
    /// 在 [`Self::default`] 之上应用环境变量覆盖（目前只有 `consolidation` 子配置）。
    pub fn from_env() -> Self {
        Self::with_consolidation_config(ConsolidationConfig::from_env())
    }

    fn with_consolidation_config(consolidation: ConsolidationConfig) -> Self {
        Self {
            consolidation,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = MemoryConfig::default();
        assert!(c.enabled);
        assert_eq!(c.kb_name, "@private_memory");
        assert_eq!(c.model, "deepseek-v4-flash");
        assert_eq!(c.analyze_min_chars, 50);
        // 召回窗口 3 → 5、注入上限 800 → 1000
        assert_eq!(c.recall_top_k, 5);
        assert_eq!(c.injection_token_cap, 1000);
        assert_eq!(c.recall_half_life_days, 30);
        // 取代机制 · 阶段一：shadow 开、enforce 关（fail-closed）
        assert!(c.supersede_shadow);
        assert!(!c.supersede_enforce);
        assert_eq!(c.candidate_recent_per_category, 10);
        assert_eq!(c.candidate_cap, 30);
        assert_eq!(c.candidate_text_cap, 200);
        assert_eq!(c.candidate_token_cap, 2000);
        // 阶段一暂取 3，本轮统一为 5
        assert_eq!(c.supersede_per_turn_cap, 5);
        assert_eq!(c.shadow_scan_limit, 2000);
        assert_eq!(c.shadow_retention_days, 30);
        // 阶段二：保留期 / 对账 / 收口阈值默认值
        assert_eq!(c.superseded_retention_days, 30);
        assert_eq!(c.intent_done_retention_days, 7);
        assert_eq!(c.intent_failed_retention_days, 90);
        assert_eq!(c.reconcile_batch, 50);
        assert_eq!(c.reconcile_claim_timeout_secs, 300);
        assert_eq!(c.reconcile_max_attempts, 3);
        assert_eq!(c.audit_pending_timeout_secs, 600);
        assert_eq!(c.restore_walk_max_hops, 16);
    }

    #[test]
    fn test_consolidation_defaults() {
        let c = ConsolidationConfig::default();
        // 默认关闭 — 灰度开启，未开启时零开销
        assert!(!c.enabled);
        assert_eq!(c.batch_size, 200);
        // 0.79 / 0.88 为 bge-base-zh-v1.5 标定值（113 对合成真值集）
        assert!((c.min_cluster_cos - 0.79).abs() < f32::EPSILON);
        assert!((c.dedup_cos - 0.88).abs() < f32::EPSILON);
        assert!(c.dedup_cos > c.min_cluster_cos);
        assert_eq!(c.episodic_ttl_days, 60);
        assert_eq!(c.max_llm_calls, 20);
        assert_eq!(c.max_users_per_round, 3);
        assert_eq!(c.idle_after_secs, 600);
        // 6 字段（含秒）：调度器要求 with_seconds_required
        assert_eq!(c.cron_expr, "0 */30 * * * *");
        assert_eq!(c.recall_fresh_days, 14);
        assert_eq!(c.recall_observation_days, 30);
        assert_eq!(c.audit_retention_days, 90);
        // shadow 优先 — 标定报告人工抽检通过前保持 false
        assert!(!c.dedup_enforce);
    }

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn consolidation_from_env_unset_keeps_fail_closed_defaults() {
        let c = ConsolidationConfig::from_env_with(env_of(&[]));
        let d = ConsolidationConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.batch_size, d.batch_size);
        assert_eq!(c.max_llm_calls, d.max_llm_calls);
        assert_eq!(c.idle_after_secs, d.idle_after_secs);
        assert_eq!(c.cron_expr, d.cron_expr);
        assert!(!c.dedup_enforce);
    }

    #[test]
    fn consolidation_from_env_reads_switch_and_tuning() {
        let c = ConsolidationConfig::from_env_with(env_of(&[
            (ENV_CONSOLIDATION_ENABLED, "true"),
            (ENV_CONSOLIDATION_CRON, "0 */5 * * * *"),
            (ENV_CONSOLIDATION_BATCH_SIZE, "20"),
            (ENV_CONSOLIDATION_MAX_LLM_CALLS, "3"),
            (ENV_CONSOLIDATION_IDLE_AFTER_SECS, "60"),
            (ENV_CONSOLIDATION_DEDUP_ENFORCE, "1"),
        ]));
        assert!(c.enabled);
        assert_eq!(c.cron_expr, "0 */5 * * * *");
        assert_eq!(c.batch_size, 20);
        assert_eq!(c.max_llm_calls, 3);
        assert_eq!(c.idle_after_secs, 60);
        assert!(c.dedup_enforce);
        // 未暴露的标定字段不受 env 影响
        assert!((c.min_cluster_cos - 0.79).abs() < f32::EPSILON);
        assert!((c.dedup_cos - 0.88).abs() < f32::EPSILON);
        assert_eq!(c.episodic_ttl_days, 60);
        assert_eq!(c.audit_retention_days, 90);
    }

    #[test]
    fn consolidation_from_env_bool_accepts_common_spellings() {
        for truthy in ["true", "TRUE", " 1 ", "yes", "On"] {
            let c =
                ConsolidationConfig::from_env_with(env_of(&[(ENV_CONSOLIDATION_ENABLED, truthy)]));
            assert!(c.enabled, "{truthy:?} 应解析为 true");
        }
        for falsy in ["false", "0", "no", "off"] {
            let c =
                ConsolidationConfig::from_env_with(env_of(&[(ENV_CONSOLIDATION_ENABLED, falsy)]));
            assert!(!c.enabled, "{falsy:?} 应解析为 false");
        }
    }

    #[test]
    fn consolidation_from_env_invalid_value_falls_back_to_default() {
        let c = ConsolidationConfig::from_env_with(env_of(&[
            (ENV_CONSOLIDATION_BATCH_SIZE, "not-a-number"),
            (ENV_CONSOLIDATION_MAX_LLM_CALLS, "-3"),
            (ENV_CONSOLIDATION_DEDUP_ENFORCE, "maybe"),
        ]));
        let d = ConsolidationConfig::default();
        assert_eq!(c.batch_size, d.batch_size);
        assert_eq!(c.max_llm_calls, d.max_llm_calls);
        assert!(!c.dedup_enforce);
    }

    #[test]
    fn consolidation_from_env_blank_cron_falls_back() {
        let c = ConsolidationConfig::from_env_with(env_of(&[(ENV_CONSOLIDATION_CRON, "   ")]));
        assert_eq!(c.cron_expr, ConsolidationConfig::default().cron_expr);
    }

    #[test]
    fn memory_from_env_propagates_consolidation_switch() {
        let on =
            MemoryConfig::with_consolidation_config(ConsolidationConfig::from_env_with(env_of(&[
                (ENV_CONSOLIDATION_ENABLED, "true"),
            ])));
        assert!(on.consolidation.enabled);
        assert_eq!(on.kb_name, "@private_memory");
        assert!(on.enabled);

        let off = MemoryConfig::with_consolidation_config(ConsolidationConfig::from_env_with(
            env_of(&[]),
        ));
        assert!(!off.consolidation.enabled);
    }

    #[test]
    fn consolidation_env_var_names_are_distinct_and_namespaced() {
        let names = [
            ENV_CONSOLIDATION_ENABLED,
            ENV_CONSOLIDATION_CRON,
            ENV_CONSOLIDATION_BATCH_SIZE,
            ENV_CONSOLIDATION_MAX_LLM_CALLS,
            ENV_CONSOLIDATION_IDLE_AFTER_SECS,
            ENV_CONSOLIDATION_DEDUP_ENFORCE,
        ];
        let mut seen = std::collections::HashSet::new();
        for name in names {
            assert!(name.starts_with("PECO_MEMORY_CONSOLIDATION_"), "{name}");
            assert!(seen.insert(name), "重复的 env 变量名: {name}");
        }
    }
}
