// ============================================================================
// PecoConfig — Peco 对话配置
// ============================================================================
//
// 可选字段（compaction / environment / dynamic_context / hooks）由
// PecoManager 构造期填充 — handler 无需改动即可增减注入组件。

use std::sync::{Arc, OnceLock};

use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{
    CompactionPolicy, DynamicContext, LooperConfig, MessageFilter, SUMMARY_TOKEN_CAP,
    TRANSCRIPT_ITEM_MAX_CHARS, TRANSCRIPT_TOOL_OUTPUT_MIN_CAP, TurnSummarizer,
};
use tracing::warn;

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
/// 工具输出截断上限（见 [`CompactionStripConfig::tool_output_max_chars`]）。
pub const ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS: &str =
    "PECO_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS";
/// 生效开关（见 [`CompactionStripConfig::enforce`]）。
pub const ENV_COMPACTION_STRIP_ENFORCE: &str = "PECO_COMPACTION_STRIP_ENFORCE";
/// 边界观测表保留期（见 [`CompactionStripConfig::retention_days`]）。
pub const ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS: &str =
    "PECO_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS";
/// viewable 守卫阈值（见 [`CompactionStripConfig::view_guard_tokens`]）。
pub const ENV_COMPACTION_VIEW_GUARD_TOKENS: &str = "PECO_COMPACTION_VIEW_GUARD_TOKENS";
/// viewable 保留预算（见 [`CompactionStripConfig::keep_view_tokens`]）。
pub const ENV_COMPACTION_KEEP_VIEW_TOKENS: &str = "PECO_COMPACTION_KEEP_VIEW_TOKENS";

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
    /// 历史工具输出截断上限（字符数）：`0` = 关闭。
    ///
    /// 非 0 时不小于转录下界（削后行才不短于单条上限），违规值在读取端
    /// 回退默认。
    pub tool_output_max_chars: usize,
    /// 生效开关：`true` 时边界上就地改写历史；`false`（默认）只评估观测，
    /// legacy 路径行为与不引入削减时逐字节一致。
    pub enforce: bool,
    /// 边界观测表行保留期（天），过期行由启动清扫删除。
    pub retention_days: u64,
    /// viewable 守卫阈值（token）：`0` = 关（默认，legacy 触发口径不变）。
    /// 非 0 须 ≤ [`PecoConfig::history_token_budget`] 且须 [`Self::enforce`]
    /// （V4/V7），违规值在 [`PecoConfig::validate_compaction`] 钳制。
    pub view_guard_tokens: usize,
    /// viewable 保留预算（token）：`0` = 关（默认，保留只按全量口径）。
    /// 守卫开启时须落在 `[guard/4, guard)`（V5/V5′），违规值同样在
    /// [`PecoConfig::validate_compaction`] 钳制。
    pub keep_view_tokens: usize,
}

impl Default for CompactionStripConfig {
    fn default() -> Self {
        Self {
            shadow: true,
            keep_recent_turns: 4,
            strip_reasoning: true,
            strip_tool_args: true,
            tool_output_max_chars: TRANSCRIPT_ITEM_MAX_CHARS,
            enforce: false,
            retention_days: 30,
            view_guard_tokens: 0,
            keep_view_tokens: 0,
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
                tool_output_max_chars: clamp_tool_output_cap(env_parse(
                    &get,
                    ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
                    TRANSCRIPT_ITEM_MAX_CHARS,
                )),
                enforce: env_bool(&get, ENV_COMPACTION_STRIP_ENFORCE, false),
                retention_days: env_parse(&get, ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS, 30),
                view_guard_tokens: env_parse(&get, ENV_COMPACTION_VIEW_GUARD_TOKENS, 0),
                keep_view_tokens: env_parse(&get, ENV_COMPACTION_KEEP_VIEW_TOKENS, 0),
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

/// 校验工具输出截断上限：`0` = 关闭（合法）；非 0 必须不小于转录下界 ——
/// 低于下界时削后转录行短于单条上限、无损等式失配，回退默认。
fn clamp_tool_output_cap(value: usize) -> usize {
    if value != 0 && value < TRANSCRIPT_TOOL_OUTPUT_MIN_CAP {
        warn!(
            variable = ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
            value,
            min = TRANSCRIPT_TOOL_OUTPUT_MIN_CAP,
            "Tool output strip cap below transcript bound; using default"
        );
        TRANSCRIPT_ITEM_MAX_CHARS
    } else {
        value
    }
}

/// 配置不变量违规：不变量编号 + 简述 + 记录级别。
///
/// 由 [`PecoConfig::validate_compaction`] 收集；**钳制在收集时即已生效**，
/// 清单只供日志使用（每进程只打一次，见 [`claim_compaction_config_log`]）。
#[derive(Debug, Clone)]
pub struct ConfigViolation {
    /// 不变量编号（进日志字段，便于 grep 对照）。
    pub invariant: &'static str,
    /// `true` = error 级（违规值已钳制）；`false` = warn 级（仅提示，不钳制）。
    pub is_error: bool,
    /// 简述（英文，直接进日志 message 字段之外的 detail）。
    pub detail: String,
}

impl ConfigViolation {
    fn error(invariant: &'static str, detail: String) -> Self {
        Self {
            invariant,
            is_error: true,
            detail,
        }
    }

    fn warn(invariant: &'static str, detail: String) -> Self {
        Self {
            invariant,
            is_error: false,
            detail,
        }
    }
}

impl PecoConfig {
    /// 校验压缩 / 守卫配置不变量：违规值**就地钳制**并返回违规清单（不 panic）。
    ///
    /// 每请求执行（纯内存、无 DB）—— 钳制必须每次生效，因为它决定传给
    /// [`CompactionPolicy`] 的值；清单只用于日志，日志经
    /// [`claim_compaction_config_log`] 每进程只打一次，否则每条流连接都刷屏。
    ///
    /// 校验顺序：V1′ → V2 → V3 → V4 → **V7 → V5 → V5′** → V6。
    /// V7 前置于 V5/V5′：守卫即将被钳为 0 时，再按它钳 keep_view 只会
    /// 留下无效的告警。默认配置零违规（legacy 行为不变）；`0` = 关闭不违规。
    pub fn validate_compaction(&mut self) -> Vec<ConfigViolation> {
        let mut violations = Vec::new();

        // V1′：保留区目标 + 摘要估算上界 ≤ 触发阈值 —— 折叠后残留「装得下」的前提
        if self.compaction_keep_recent_tokens + SUMMARY_TOKEN_CAP > self.compaction_trigger_tokens {
            let clamped = self
                .compaction_trigger_tokens
                .saturating_sub(SUMMARY_TOKEN_CAP);
            violations.push(ConfigViolation::error(
                "V1'",
                format!(
                    "keep_recent_tokens {} + summary cap {} exceeds trigger {}; clamped to {}",
                    self.compaction_keep_recent_tokens,
                    SUMMARY_TOKEN_CAP,
                    self.compaction_trigger_tokens,
                    clamped
                ),
            ));
            self.compaction_keep_recent_tokens = clamped;
            if self.compaction_trigger_tokens <= SUMMARY_TOKEN_CAP {
                violations.push(ConfigViolation::warn(
                    "V1'",
                    format!(
                        "trigger {} at or below summary cap {}; residual token bound not guaranteeable",
                        self.compaction_trigger_tokens, SUMMARY_TOKEN_CAP
                    ),
                ));
            }
        }

        // V2：削减保留窗至少 1 轮 —— 0 会让削减覆盖到 UI 默认可见区
        if self.strip.keep_recent_turns < 1 {
            violations.push(ConfigViolation::error(
                "V2",
                "keep_recent_turns below 1; clamped to 1".to_string(),
            ));
            self.strip.keep_recent_turns = 1;
        }

        // V3：非 0 的输出截断上限不得低于转录下界 —— 低于则无损等式失配
        if self.strip.tool_output_max_chars != 0
            && self.strip.tool_output_max_chars < TRANSCRIPT_TOOL_OUTPUT_MIN_CAP
        {
            violations.push(ConfigViolation::error(
                "V3",
                format!(
                    "tool_output_max_chars {} below transcript bound {}; clamped to default {}",
                    self.strip.tool_output_max_chars,
                    TRANSCRIPT_TOOL_OUTPUT_MIN_CAP,
                    TRANSCRIPT_ITEM_MAX_CHARS
                ),
            ));
            self.strip.tool_output_max_chars = TRANSCRIPT_ITEM_MAX_CHARS;
        }

        // V4：守卫阈值不得超过 viewable 窗口 —— 超窗的守卫恒真，每边界空转
        if self.strip.view_guard_tokens != 0
            && self.strip.view_guard_tokens > self.history_token_budget
        {
            violations.push(ConfigViolation::error(
                "V4",
                format!(
                    "view_guard_tokens {} exceeds history window {}; clamped to window",
                    self.strip.view_guard_tokens, self.history_token_budget
                ),
            ));
            self.strip.view_guard_tokens = self.history_token_budget;
        }

        // V7：守卫不得脱离削减单独开启 —— 守卫不减 viewable，先于全量触发
        // 只会升频折叠（灰度路径只允许「先削减生效、再开守卫」）
        if self.strip.view_guard_tokens > 0 && !self.strip.enforce {
            violations.push(ConfigViolation::error(
                "V7",
                format!(
                    "view_guard_tokens {} requires strip enforce; clamped guard to 0",
                    self.strip.view_guard_tokens
                ),
            ));
            self.strip.view_guard_tokens = 0;
        }

        // V5：keep_view 须严于守卫 —— 与守卫同宽时保留区装下全部可折轮，
        // 折叠后立即再次触发（无滞回）
        if self.strip.view_guard_tokens != 0
            && self.strip.keep_view_tokens >= self.strip.view_guard_tokens
        {
            let clamped = self.strip.view_guard_tokens / 2;
            violations.push(ConfigViolation::error(
                "V5",
                format!(
                    "keep_view_tokens {} not below guard {}; clamped to {}",
                    self.strip.keep_view_tokens, self.strip.view_guard_tokens, clamped
                ),
            ));
            self.strip.keep_view_tokens = clamped;
        }

        // V5′：keep_view 不得低于守卫的四分之一 —— 太窄的保留区退化成
        // 「只留最新 1 轮」，每边界折叠却几乎不减负
        if self.strip.view_guard_tokens != 0
            && self.strip.keep_view_tokens < self.strip.view_guard_tokens / 4
        {
            let clamped = self.strip.view_guard_tokens / 2;
            violations.push(ConfigViolation::error(
                "V5'",
                format!(
                    "keep_view_tokens {} below guard/4 {}; clamped to {}",
                    self.strip.keep_view_tokens,
                    self.strip.view_guard_tokens / 4,
                    clamped
                ),
            ));
            self.strip.keep_view_tokens = clamped;
        }

        // V6：只提示不钳制 —— 削减无守卫在 tool-light 会话扩大死区，
        // 但允许作灰度路径（先削减生效、观测后再开守卫）
        if self.strip.enforce && self.strip.view_guard_tokens == 0 {
            violations.push(ConfigViolation::warn(
                "V6",
                "strip enforce without view guard; dead zone may widen on tool-light sessions"
                    .to_string(),
            ));
        }

        violations
    }
}

/// 配置违规日志闸门 —— 每进程一次。
///
/// 校验每请求跑（钳制每次生效），但日志只打一次：首次发现违规的连接
/// 把清单完整打进日志，之后的连接静默。
static COMPACTION_CONFIG_WARNED: OnceLock<()> = OnceLock::new();

/// 领取配置违规日志的打印权：本进程首次返回 true，之后恒 false。
pub fn claim_compaction_config_log() -> bool {
    COMPACTION_CONFIG_WARNED.set(()).is_ok()
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
        assert_eq!(
            cfg.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS,
            "默认上限与转录单条上限同源"
        );
        assert!(!cfg.strip.enforce, "默认必须 legacy：只评估不改写");
        assert_eq!(cfg.strip.retention_days, 30);
    }

    #[test]
    fn strip_tool_output_cap_clamps_below_bound() {
        // 显式关闭合法
        let cfg =
            PecoConfig::from_env_with(env_of(&[(ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS, "0")]));
        assert_eq!(cfg.strip.tool_output_max_chars, 0);

        // 合法放大：≥ 下界即通过
        let cfg = PecoConfig::from_env_with(env_of(&[(
            ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
            "4000",
        )]));
        assert_eq!(cfg.strip.tool_output_max_chars, 4000);

        // 恰在下界：通过
        let bound = TRANSCRIPT_TOOL_OUTPUT_MIN_CAP.to_string();
        let cfg = PecoConfig::from_env_with(env_of(&[(
            ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
            bound.as_str(),
        )]));
        assert_eq!(
            cfg.strip.tool_output_max_chars,
            TRANSCRIPT_TOOL_OUTPUT_MIN_CAP
        );

        // 低于下界：回退默认（非 0 保底）
        let cfg = PecoConfig::from_env_with(env_of(&[(
            ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
            "100",
        )]));
        assert_eq!(cfg.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS);

        // 写错取默认
        let cfg = PecoConfig::from_env_with(env_of(&[(
            ENV_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS,
            "abc",
        )]));
        assert_eq!(cfg.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS);
    }

    #[test]
    fn strip_env_overrides_and_invalid_fall_back() {
        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_SHADOW, "off"),
            (ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS, "8"),
            (ENV_COMPACTION_STRIP_REASONING, "false"),
            (ENV_COMPACTION_STRIP_TOOL_ARGS, "0"),
            (ENV_COMPACTION_STRIP_ENFORCE, "on"),
            (ENV_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS, "7"),
        ]));
        assert!(!cfg.strip.shadow);
        assert_eq!(cfg.strip.keep_recent_turns, 8);
        assert!(!cfg.strip.strip_reasoning);
        assert!(!cfg.strip.strip_tool_args);
        assert!(cfg.strip.enforce);
        assert_eq!(cfg.strip.retention_days, 7);

        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_SHADOW, "maybe"),
            (ENV_COMPACTION_STRIP_KEEP_RECENT_TURNS, "abc"),
            (ENV_COMPACTION_STRIP_ENFORCE, "maybe"),
        ]));
        assert!(cfg.strip.shadow);
        assert_eq!(cfg.strip.keep_recent_turns, 4);
        assert!(!cfg.strip.enforce, "写错的布尔取默认 false");
    }

    // ── Phase 3：viewable 守卫 env、配置不变量 V1′–V7、日志闸门 ──

    #[test]
    fn view_guard_defaults_off_and_reads_env() {
        // 默认关闭（legacy 触发口径不变）
        let cfg = PecoConfig::from_env_with(env_of(&[]));
        assert_eq!(cfg.strip.view_guard_tokens, 0, "守卫默认必须关");
        assert_eq!(cfg.strip.keep_view_tokens, 0, "view 保留默认必须关");

        let cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_VIEW_GUARD_TOKENS, "50000"),
            (ENV_COMPACTION_KEEP_VIEW_TOKENS, "25000"),
        ]));
        assert_eq!(cfg.strip.view_guard_tokens, 50_000);
        assert_eq!(cfg.strip.keep_view_tokens, 25_000);

        // 写错取默认
        let cfg = PecoConfig::from_env_with(env_of(&[(ENV_COMPACTION_VIEW_GUARD_TOKENS, "abc")]));
        assert_eq!(cfg.strip.view_guard_tokens, 0);
    }

    #[test]
    fn default_config_passes_validation() {
        let mut cfg = PecoConfig::from_env_with(env_of(&[]));
        let violations = cfg.validate_compaction();
        assert!(
            violations.is_empty(),
            "默认配置必须零违规，实际: {violations:?}"
        );
        // V1′ 默认值自检：k_recent + Σcap ≤ T
        assert!(
            cfg.compaction_keep_recent_tokens + SUMMARY_TOKEN_CAP <= cfg.compaction_trigger_tokens
        );
        assert_eq!(cfg.compaction_keep_recent_tokens, 128_000);
        assert_eq!(cfg.compaction_trigger_tokens, 384_000);
    }

    #[test]
    fn v1_prime_clamps_keep_recent_to_trigger_minus_cap() {
        // 刚好越界：90_000 + 20_000 = 110_000 > 100_000
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_TRIGGER_TOKENS, "100000"),
            (ENV_COMPACTION_KEEP_RECENT_TOKENS, "90000"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V1′: {violations:?}");
        assert_eq!(violations[0].invariant, "V1'");
        assert!(violations[0].is_error);
        assert_eq!(
            cfg.compaction_keep_recent_tokens,
            100_000 - SUMMARY_TOKEN_CAP
        );

        // 退化：触发阈值 ≤ Σcap ⇒ 钳到 0 并追加 warn 级第二条
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_TRIGGER_TOKENS, "10000"),
            (ENV_COMPACTION_KEEP_RECENT_TOKENS, "5000"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations[0].is_error);
        assert!(!violations[1].is_error, "第二条是「界不可保证」warn");
        assert_eq!(cfg.compaction_keep_recent_tokens, 0);
    }

    #[test]
    fn v2_v3_clamp_degenerate_strip_bounds() {
        // env 路径的坏值在 from_env 已被钳掉，这里直接构造 —— V3 是对
        // 程序化构造配置的兜底；空 env 内核使结果不随宿主 PECO_* 漂移
        let mut cfg = PecoConfig::from_env_with(env_of(&[]));
        cfg.strip.keep_recent_turns = 0;
        cfg.strip.tool_output_max_chars = 100;
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert_eq!(violations[0].invariant, "V2");
        assert_eq!(violations[1].invariant, "V3");
        assert_eq!(cfg.strip.keep_recent_turns, 1, "V2 钳到 1");
        assert_eq!(
            cfg.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS,
            "V3 回退默认上限"
        );
    }

    #[test]
    fn v4_clamps_guard_to_window() {
        // enforce=true 避免 V7 把守卫钳 0 干扰 V4 断言
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_ENFORCE, "on"),
            (ENV_COMPACTION_VIEW_GUARD_TOKENS, "138000"),
            (ENV_COMPACTION_KEEP_VIEW_TOKENS, "64000"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V4: {violations:?}");
        assert_eq!(violations[0].invariant, "V4");
        assert_eq!(cfg.strip.view_guard_tokens, 128_000, "鉗到窗口宽");
        assert_eq!(cfg.strip.keep_view_tokens, 64_000, "keep_view 不动");
    }

    #[test]
    fn v7_zeroes_guard_without_enforce() {
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_VIEW_GUARD_TOKENS, "60000"),
            (ENV_COMPACTION_KEEP_VIEW_TOKENS, "30000"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V7: {violations:?}");
        assert_eq!(violations[0].invariant, "V7");
        assert!(violations[0].is_error);
        assert_eq!(cfg.strip.view_guard_tokens, 0, "V7 钳守卫为 0");
        // V7 先于 V5/V5′：守卫已死，keep_view 不再按它钳制
        assert_eq!(cfg.strip.keep_view_tokens, 30_000, "keep_view 原样保留");
    }

    #[test]
    fn v5_clamps_wide_keep_view_below_guard() {
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_ENFORCE, "on"),
            (ENV_COMPACTION_VIEW_GUARD_TOKENS, "80000"),
            (ENV_COMPACTION_KEEP_VIEW_TOKENS, "80000"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V5: {violations:?}");
        assert_eq!(violations[0].invariant, "V5");
        assert_eq!(cfg.strip.keep_view_tokens, 40_000, "鉗到 guard/2");
    }

    #[test]
    fn v5_prime_clamps_narrow_keep_view_to_half_guard() {
        let mut cfg = PecoConfig::from_env_with(env_of(&[
            (ENV_COMPACTION_STRIP_ENFORCE, "on"),
            (ENV_COMPACTION_VIEW_GUARD_TOKENS, "100000"),
            (ENV_COMPACTION_KEEP_VIEW_TOKENS, "0"),
        ]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V5′: {violations:?}");
        assert_eq!(violations[0].invariant, "V5'");
        assert_eq!(cfg.strip.keep_view_tokens, 50_000, "鉗到 guard/2");
        // 鉗后仍满足 V5（严于守卫）：50_000 < 100_000
        assert!(cfg.strip.keep_view_tokens < cfg.strip.view_guard_tokens);
    }

    #[test]
    fn v6_warns_enforce_without_guard() {
        let mut cfg = PecoConfig::from_env_with(env_of(&[(ENV_COMPACTION_STRIP_ENFORCE, "on")]));
        let violations = cfg.validate_compaction();
        assert_eq!(violations.len(), 1, "只应有 V6: {violations:?}");
        assert_eq!(violations[0].invariant, "V6");
        assert!(!violations[0].is_error, "V6 只提示不鉗制");
        assert_eq!(cfg.compaction_keep_recent_tokens, 128_000, "无鉗制发生");
        assert_eq!(cfg.strip.view_guard_tokens, 0);
    }

    #[test]
    fn compaction_config_log_gate_is_once_per_process() {
        assert!(claim_compaction_config_log(), "首次必须领到打印权");
        assert!(!claim_compaction_config_log(), "二次必须静默");
    }
}
