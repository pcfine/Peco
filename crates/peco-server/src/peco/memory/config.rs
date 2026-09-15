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
    /// 占位默认 0.85，待标定脚本产出报告后回填操作点。
    pub min_cluster_cos: f32,
    /// 硬去重阈值（cosine similarity）。
    ///
    /// 占位默认 0.92，待标定脚本产出报告后回填操作点。
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
            min_cluster_cos: 0.85,
            dedup_cos: 0.92,
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
        // P5-T1：召回窗口 3 → 5、注入上限 800 → 1000（实测见 reports/）
        assert_eq!(c.recall_top_k, 5);
        assert_eq!(c.injection_token_cap, 1000);
        assert_eq!(c.recall_half_life_days, 30);
    }

    #[test]
    fn test_consolidation_defaults() {
        let c = ConsolidationConfig::default();
        // 默认关闭 — 灰度开启，未开启时零开销
        assert!(!c.enabled);
        assert_eq!(c.batch_size, 200);
        // 0.85 / 0.92 为占位值，以标定报告回填为准
        assert!((c.min_cluster_cos - 0.85).abs() < f32::EPSILON);
        assert!((c.dedup_cos - 0.92).abs() < f32::EPSILON);
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
}
