// ============================================================================
// PecoConfig — Peco 对话配置
// ============================================================================
//
// 可选字段（compaction / environment / dynamic_context / hooks）由
// PecoManager 构造期填充 — handler 无需改动即可增减注入组件。

use std::sync::Arc;

use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{
    CompactionPolicy, DynamicContext, LooperConfig, MessageFilter, TurnSummarizer,
};

use super::memory::MemoryConfig;

/// Peco 对话配置。
#[derive(Clone)]
pub struct PecoConfig {
    /// 事件通道缓冲区大小
    pub event_buffer: usize,
    /// 历史轮 verbatim 保留区 token 预算（不含当前轮与 pinned 摘要）。
    ///
    /// 口径：仅统计轮内 viewable 条目（User / Assistant 文本），
    /// 不含 tool 输出与 reasoning。与 [`Self::compaction_trigger_tokens`]
    /// 的「全量轮」口径不同 — 两个数值不可直接比较。
    ///
    /// 取代早期的 `max_history_messages` 消息条数窗口 — 中文场景下
    /// 按条数截断的 token 波动过大，预算控制必须基于校准 token 估算。
    ///
    /// 默认值可用 env `PECO_HISTORY_TOKEN_BUDGET` 覆盖（[`Self::from_env`]）。
    pub history_token_budget: usize,
    /// 上下文滚动压缩触发阈值（估算 token）。
    ///
    /// 口径：pinned 摘要 + 全部 committed 轮的**全量** token
    /// （含 tool 输出与 reasoning）。与 [`Self::history_token_budget`]
    /// 的「仅 viewable 文本」口径不同。
    ///
    /// 默认值可用 env `PECO_COMPACTION_TRIGGER_TOKENS` 覆盖（[`Self::from_env`]）。
    pub compaction_trigger_tokens: usize,
    /// 压缩后 verbatim 保留区目标 token。
    ///
    /// 默认值可用 env `PECO_COMPACTION_KEEP_RECENT_TOKENS` 覆盖（[`Self::from_env`]）。
    pub compaction_keep_recent_tokens: usize,
    /// 摘要模型名（Flash 档，低延迟低成本）。
    ///
    /// 撞上 `max_iterations` 时合成的收尾报告共用同一个模型 —— 两者都是「读一遍既有
    /// 内容、产出短文本」的元任务，只是提示词不同。
    pub summarizer_model: String,
    /// 历史削减（strip）与边界观测配置。
    pub strip: CompactionStripConfig,
    /// 记忆双路径配置（写路径提取 hook + 读路径召回）。
    pub memory: MemoryConfig,
    /// 统一重发上限（`0` = 关闭）。见 `LooperConfig::retry_limit`。
    ///
    /// 默认值经 `LooperConfig::from_env()` 读取（env `PECO_RETRY_LIMIT`）。
    pub retry_limit: u32,
    /// 截断重发的输出预算抬升目标（token）。
    /// 见 `LooperConfig::retry_output_budget`。
    ///
    /// **模型真实上限低于该值时，抬升后的重试请求会被网关拒（400）** —— 那时
    /// 把本值调到模型的上限之内，或直接把 `retry_limit` 置 0。
    /// 默认值经 `LooperConfig::from_env()` 读取（env `PECO_RETRY_OUTPUT_BUDGET`）。
    pub retry_output_budget: u32,
    /// 重发退避起始延迟（毫秒）。
    /// 默认值经 `LooperConfig::from_env()` 读取（env `PECO_RETRY_BASE_DELAY_MS`）。
    pub retry_base_delay_ms: u64,
    /// 重发退避延迟上限（毫秒）。
    /// 默认值经 `LooperConfig::from_env()` 读取（env `PECO_RETRY_MAX_DELAY_MS`）。
    pub retry_max_delay_ms: u64,

    // ── 以下由 PecoManager 构造期填充 ──────────────────────
    /// 上下文滚动压缩策略。由 `PecoManager` 基于主 Agent 的 provider 构建。
    pub compaction: Option<Arc<CompactionPolicy>>,
    /// 轮末收尾报告合成器。由 `PecoManager` 装配为与压缩摘要器**同一个**实例
    /// （见 `TurnSummarizer::summarize_inflight`）。
    /// 未装配（`None`）时撞上 `max_iterations` 走固定中断说明。
    pub epilogue: Option<Arc<dyn TurnSummarizer>>,
    /// 环境上下文（恒定前缀）：用户身份、工作空间路径、日期平台等。
    /// 由 `PecoManager` 在构造时经 `EnvironmentInfo::render()` 求值一次填入。
    pub environment: Option<String>,
    /// 动态上下文（读路径）：每次用户 query 前自动检索并注入。
    /// 记忆体系装配为 `MemoryRecallContext`（见 `super::memory`）。
    pub dynamic_context: Option<Arc<dyn DynamicContext>>,
    /// Looper 钩子（写路径）：每轮完成后触发记忆提取、token 用量记录等。
    /// 记忆体系装配为 `MemoryExtractionHook`（见 `super::memory`）。
    /// 钩子按注册顺序执行，相互独立。
    pub hooks: Vec<Arc<dyn LooperHook>>,
}

// ── 三个 token 预算的环境变量入口 ─────────────────────────────
//
// 未暴露给 env 的字段（`event_buffer` / `summarizer_model`）只能改默认值。

/// 历史轮 verbatim 保留区预算（见 [`PecoConfig::history_token_budget`]）。
pub const ENV_HISTORY_TOKEN_BUDGET: &str = "PECO_HISTORY_TOKEN_BUDGET";
/// 上下文滚动压缩触发阈值（见 [`PecoConfig::compaction_trigger_tokens`]）。
pub const ENV_COMPACTION_TRIGGER_TOKENS: &str = "PECO_COMPACTION_TRIGGER_TOKENS";
/// 压缩后 verbatim 保留区目标（见 [`PecoConfig::compaction_keep_recent_tokens`]）。
pub const ENV_COMPACTION_KEEP_RECENT_TOKENS: &str = "PECO_COMPACTION_KEEP_RECENT_TOKENS";

// ── 历史削减（strip）与边界观测的环境变量入口 ─────────────────

/// shadow 观测开关（见 [`CompactionStripConfig::shadow`]）。
pub const ENV_COMPACTION_STRIP_SHADOW: &str = "PECO_COMPACTION_STRIP_SHADOW";
/// 削减保留窗（见 [`CompactionStripConfig::keep_recent_turns`]）。
pub const ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS: &str = "PECO_COMPACTION_STRIP_KEEP_RECENT_TURNS";
/// 削减范围：reasoning（见 [`CompactionStripConfig::strip_reasoning`]）。
pub const ENV_COMPACTION_STRIP_REASONING: &str = "PECO_COMPACTION_STRIP_REASONING";
/// 削减范围：工具调用参数（见 [`CompactionStripConfig::strip_tool_args`]）。
pub const ENV_COMPACTION_STRIP_TOOL_ARGS: &str = "PECO_COMPACTION_STRIP_TOOL_ARGS";
/// 边界观测表保留期（见 [`CompactionStripConfig::retention_days`]）。
pub const ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS: &str =
    "PECO_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS";

/// 历史削减与边界观测配置。
///
/// 默认只观测（`shadow`）不改写 —— 不装配削减策略时压缩路径与引入前
/// 逐字节一致；生效开关随削减路径接入。
#[derive(Debug, Clone)]
pub struct CompactionStripConfig {
    /// shadow 观测：每个边界评估「若执行将削减多少」并落观测表，不改写会话。
    pub shadow: bool,
    /// 削减保留窗：只有早于最近 N 轮的已完成轮才参与削减（UI 默认页可见区恒无损）。
    pub keep_recent_turns: usize,
    /// 削减范围：删除历史 reasoning 条目。
    pub strip_reasoning: bool,
    /// 削减范围：清空历史工具调用参数。
    pub strip_tool_args: bool,
    /// 边界观测表行保留期（天），过期行由启动清扫删除。
    pub retention_days: u64,
}

impl Default for CompactionStripConfig {
    fn default() -> Self {
        Self {
            shadow: true,
            keep_recent_turns: 4,
            strip_reasoning: true,
            strip_tool_args: true,
            retention_days: 30,
        }
    }
}

impl Default for PecoConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl PecoConfig {
    /// 从环境变量读取三个 token 预算，未设置的字段取本文件中的字面默认。
    ///
    /// 与 `ConsolidationConfig::from_env` 同范式：读取内核可注入，
    /// 便于在不触碰进程 env 的前提下单测。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    fn from_env_with<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        // 重试四字段的默认值委托 from_env —— env 读取单点在 peco-core。
        let retry = LooperConfig::from_env();
        Self {
            event_buffer: 256,
            history_token_budget: env_parse(&get, ENV_HISTORY_TOKEN_BUDGET, 128_000),
            compaction_trigger_tokens: env_parse(&get, ENV_COMPACTION_TRIGGER_TOKENS, 384_000),
            compaction_keep_recent_tokens: env_parse(
                &get,
                ENV_COMPACTION_KEEP_RECENT_TOKENS,
                128_000,
            ),
            summarizer_model: "deepseek-v4-flash".to_string(),
            strip: CompactionStripConfig {
                shadow: env_bool(&get, ENV_COMPACTION_STRIP_SHADOW, true),
                keep_recent_turns: env_parse(&get, ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS, 4),
                strip_reasoning: env_bool(&get, ENV_COMPACTION_STRIP_REASONING, true),
                strip_tool_args: env_bool(&get, ENV_COMPACTION_STRIP_TOOL_ARGS, true),
                retention_days: env_parse(&get, ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS, 30),
            },
            memory: MemoryConfig::from_env(),
            retry_limit: retry.retry_limit,
            retry_output_budget: retry.retry_output_budget,
            retry_base_delay_ms: retry.retry_base_delay_ms,
            retry_max_delay_ms: retry.retry_max_delay_ms,
            compaction: None,
            epilogue: None,
            environment: None,
            dynamic_context: None,
            hooks: Vec::new(),
        }
    }
}

impl PecoConfig {
    /// 从 PecoConfig 构建 LooperConfig。
    ///
    /// 重试四字段以 `self` 为可覆盖项显式转发（缺省来源即
    /// [`LooperConfig::from_env`]），其余字段同样以 `from_env()` 为底。
    pub fn to_looper_config(&self, message_filter: Arc<dyn MessageFilter>) -> LooperConfig {
        LooperConfig {
            event_buffer: self.event_buffer,
            persist_on_failure: true,
            environment: self.environment.clone(),
            dynamic_context: self.dynamic_context.clone(),
            hooks: self.hooks.clone(),
            message_filter: Some(message_filter),
            compaction: self.compaction.clone(),
            epilogue: self.epilogue.clone(),
            retry_limit: self.retry_limit,
            retry_output_budget: self.retry_output_budget,
            retry_base_delay_ms: self.retry_base_delay_ms,
            retry_max_delay_ms: self.retry_max_delay_ms,
            ..LooperConfig::from_env()
        }
    }
}

/// 读一个数值环境变量：缺失取默认值，写错告警后取默认值。
///
/// 与 `memory::config` 的同名辅助函数同语义 —— 解析失败不静默吞，
/// 「值写错却看起来生效了」是最难查的配置问题。
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

/// 读一个布尔环境变量：缺失取默认值，写错告警后取默认值。
///
/// 与 `memory::config` 的同名辅助函数同语义 —— 解析失败不静默吞，
/// 「值写错却看起来生效了」是最难查的配置问题。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn token_budgets_use_defaults_when_env_absent() {
        let cfg = PecoConfig::from_env_with(env_of(&[]));
        assert_eq!(cfg.history_token_budget, 128_000);
        assert_eq!(cfg.compaction_trigger_tokens, 384_000);
        assert_eq!(cfg.compaction_keep_recent_tokens, 128_000);
    }

    #[test]
    fn token_budgets_read_env_overrides() {
        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_HISTORY_TOKEN_BUDGET, "200000"),
            (ENV_COMPACTION_TRIGGER_TOKENS, "500000"),
            (ENV_COMPACTION_KEEP_RECENT_TOKENS, "150000"),
        ]));
        assert_eq!(cfg.history_token_budget, 200_000);
        assert_eq!(cfg.compaction_trigger_tokens, 500_000);
        assert_eq!(cfg.compaction_keep_recent_tokens, 150_000);
    }

    #[test]
    fn token_budget_invalid_env_falls_back_to_default() {
        let cfg = PecoConfig::from_env_with(env_of(&[(ENV_COMPACTION_TRIGGER_TOKENS, "abc")]));
        assert_eq!(cfg.compaction_trigger_tokens, 384_000);
    }

    #[test]
    fn strip_defaults_are_shadow_only() {
        let cfg = PecoConfig::from_env_with(env_of(&[]));
        assert!(cfg.strip.shadow);
        assert_eq!(cfg.strip.keep_recent_turns, 4);
        assert!(cfg.strip.strip_reasoning);
        assert!(cfg.strip.strip_tool_args);
        assert_eq!(cfg.strip.retention_days, 30);
    }

    #[test]
    fn strip_env_overrides_and_invalid_fall_back() {
        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_SHADOW, "off"),
            (ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS, "8"),
            (ENV_COMPACTION_STRIP_REASONING, "false"),
            (ENV_COMPACTION_STRIP_TOOL_ARGS, "0"),
            (ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS, "7"),
        ]));
        assert!(!cfg.strip.shadow);
        assert_eq!(cfg.strip.keep_recent_turns, 8);
        assert!(!cfg.strip.strip_reasoning);
        assert!(!cfg.strip.strip_tool_args);
        assert_eq!(cfg.strip.retention_days, 7);

        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_SHADOW, "maybe"),
            (ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS, "abc"),
        ]));
        assert!(cfg.strip.shadow);
        assert_eq!(cfg.strip.keep_recent_turns, 4);
    }
}
