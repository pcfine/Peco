//! 聊天 run 的上下文压缩参数：环境变量入口与不变量钳制。
//!
//! 默认值与 `peco-server` 的同名配置对齐 —— 两个宿主对「同一段历史何时折叠」
//! 给出不同口径，会让摘要时机变成无法对照的实现细节。

use std::sync::{Arc, OnceLock};

use peco_core::agent::{
    CompactionPolicy, SUMMARY_TOKEN_CAP, StripPolicy, TRANSCRIPT_ITEM_MAX_CHARS,
    TRANSCRIPT_TOOL_OUTPUT_MIN_CAP, TurnSummarizer,
};
use tracing::warn;

/// 压缩触发阈值（估算 token，全量口径）的环境变量入口。
pub const ENV_TRIGGER_TOKENS: &str = "PECO_COMPACTION_TRIGGER_TOKENS";
/// 压缩后 verbatim 保留区目标（token）的环境变量入口。
pub const ENV_KEEP_RECENT_TOKENS: &str = "PECO_COMPACTION_KEEP_RECENT_TOKENS";
/// 摘要模型名的环境变量入口。
pub const ENV_SUMMARIZER_MODEL: &str = "PECO_SUMMARIZER_MODEL";
/// 历史轮 verbatim 保留区预算（filter 的 token 窗口）。
pub const ENV_HISTORY_TOKEN_BUDGET: &str = "PECO_HISTORY_TOKEN_BUDGET";
/// 边界观测（shadow）开关。
pub const ENV_STRIP_SHADOW: &str = "PECO_COMPACTION_STRIP_SHADOW";
/// 削减保留窗（最近 N 轮不参与削减）。
pub const ENV_STRIP_KEEP_RECENT_TURNS: &str = "PECO_COMPACTION_STRIP_KEEP_RECENT_TURNS";
/// 削减范围：历史 reasoning。
pub const ENV_STRIP_REASONING: &str = "PECO_COMPACTION_STRIP_REASONING";
/// 削减范围：历史工具调用参数。
pub const ENV_STRIP_TOOL_ARGS: &str = "PECO_COMPACTION_STRIP_TOOL_ARGS";
/// 历史工具输出截断上限（字符数，`0` = 关闭）。
pub const ENV_STRIP_TOOL_OUTPUT_MAX_CHARS: &str = "PECO_COMPACTION_STRIP_TOOL_OUTPUT_MAX_CHARS";
/// 削减生效开关。
pub const ENV_STRIP_ENFORCE: &str = "PECO_COMPACTION_STRIP_ENFORCE";
/// 边界观测表保留期（天）。
pub const ENV_CONTEXT_SHADOW_RETENTION_DAYS: &str = "PECO_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS";
/// viewable 守卫阈值（`0` = 关）。
pub const ENV_VIEW_GUARD_TOKENS: &str = "PECO_COMPACTION_VIEW_GUARD_TOKENS";
/// viewable 保留预算（`0` = 关）。
pub const ENV_KEEP_VIEW_TOKENS: &str = "PECO_COMPACTION_KEEP_VIEW_TOKENS";

const DEFAULT_TRIGGER_TOKENS: usize = 384_000;
const DEFAULT_KEEP_RECENT_TOKENS: usize = 128_000;
const DEFAULT_SUMMARIZER_MODEL: &str = "deepseek-flash";
const DEFAULT_HISTORY_TOKEN_BUDGET: usize = 128_000;
const DEFAULT_STRIP_KEEP_RECENT_TURNS: usize = 4;
const DEFAULT_CONTEXT_SHADOW_RETENTION_DAYS: u64 = 30;

/// 历史削减与边界观测参数。
///
/// 默认只观测（`shadow`）不改写：不显式开启 `enforce` 时压缩路径与引入削减前
/// 逐字节一致，观测表先攒出「若削减能省多少」的数据。
#[derive(Debug)]
pub struct CompactionStripConfig {
    /// shadow 观测：每个边界评估「若执行将削减多少」并落观测表，不改写会话。
    pub shadow: bool,
    /// 削减保留窗：只有早于最近 N 轮的已完成轮才参与削减（UI 默认页可见区恒无损）。
    pub keep_recent_turns: usize,
    /// 删除历史 reasoning 条目。
    pub strip_reasoning: bool,
    /// 清空历史工具调用参数。
    pub strip_tool_args: bool,
    /// 历史工具输出截断上限（字符数）：`0` = 关闭。
    pub tool_output_max_chars: usize,
    /// 生效开关：`true` 时边界上就地改写历史，`false` 只评估观测。
    pub enforce: bool,
    /// 边界观测表行保留期（天），过期行由启动清扫删除。
    pub retention_days: u64,
    /// viewable 守卫阈值（token）：`0` = 关（触发口径不变）。
    pub view_guard_tokens: usize,
    /// viewable 保留预算（token）：`0` = 关（保留只按全量口径）。
    pub keep_view_tokens: usize,
}

impl Default for CompactionStripConfig {
    fn default() -> Self {
        Self {
            shadow: true,
            keep_recent_turns: DEFAULT_STRIP_KEEP_RECENT_TURNS,
            strip_reasoning: true,
            strip_tool_args: true,
            tool_output_max_chars: TRANSCRIPT_ITEM_MAX_CHARS,
            enforce: false,
            retention_days: DEFAULT_CONTEXT_SHADOW_RETENTION_DAYS,
            view_guard_tokens: 0,
            keep_view_tokens: 0,
        }
    }
}

/// 一次聊天 run 的压缩参数。
#[derive(Debug)]
pub struct CompactionLimits {
    pub trigger_tokens: usize,
    pub keep_recent_tokens: usize,
    pub summarizer_model: String,
    /// 历史轮 verbatim 保留区 token 预算（不含当前轮与 pinned 摘要）。
    ///
    /// 口径：仅统计轮内 viewable 条目（User / Assistant 文本），不含 tool 输出
    /// 与 reasoning —— 与 [`Self::trigger_tokens`] 的「全量轮」口径不可直接比较。
    pub history_token_budget: usize,
    pub strip: CompactionStripConfig,
}

/// 读进程环境得到压缩参数；非法值告警后回退默认。
pub fn limits() -> CompactionLimits {
    limits_from(|name| std::env::var(name).ok())
}

/// 读取内核可注入：不改进程 env 也能验证覆盖生效，测试之间因此不会互相污染。
fn limits_from<F: Fn(&str) -> Option<String>>(get: F) -> CompactionLimits {
    let trigger_tokens = parse_or(&get, ENV_TRIGGER_TOKENS, DEFAULT_TRIGGER_TOKENS);
    let requested_keep_recent = parse_or(&get, ENV_KEEP_RECENT_TOKENS, DEFAULT_KEEP_RECENT_TOKENS);
    let keep_recent_tokens = clamp_keep_recent(trigger_tokens, requested_keep_recent);
    if keep_recent_tokens != requested_keep_recent {
        warn!(
            trigger_tokens,
            requested_keep_recent,
            keep_recent_tokens,
            "keep_recent >= trigger would compact on every turn; clamping to half the trigger"
        );
    }

    let defaults = CompactionStripConfig::default();
    CompactionLimits {
        trigger_tokens,
        keep_recent_tokens,
        summarizer_model: model_or_default(&get),
        history_token_budget: parse_or(
            &get,
            ENV_HISTORY_TOKEN_BUDGET,
            DEFAULT_HISTORY_TOKEN_BUDGET,
        ),
        strip: CompactionStripConfig {
            shadow: parse_bool_or(&get, ENV_STRIP_SHADOW, defaults.shadow),
            keep_recent_turns: parse_or(
                &get,
                ENV_STRIP_KEEP_RECENT_TURNS,
                defaults.keep_recent_turns,
            ),
            strip_reasoning: parse_bool_or(&get, ENV_STRIP_REASONING, defaults.strip_reasoning),
            strip_tool_args: parse_bool_or(&get, ENV_STRIP_TOOL_ARGS, defaults.strip_tool_args),
            tool_output_max_chars: clamp_tool_output_cap(parse_or(
                &get,
                ENV_STRIP_TOOL_OUTPUT_MAX_CHARS,
                defaults.tool_output_max_chars,
            )),
            enforce: parse_bool_or(&get, ENV_STRIP_ENFORCE, defaults.enforce),
            retention_days: parse_or(
                &get,
                ENV_CONTEXT_SHADOW_RETENTION_DAYS,
                defaults.retention_days as usize,
            ) as u64,
            view_guard_tokens: parse_or(&get, ENV_VIEW_GUARD_TOKENS, defaults.view_guard_tokens),
            keep_view_tokens: parse_or(&get, ENV_KEEP_VIEW_TOKENS, defaults.keep_view_tokens),
        },
    }
}

impl CompactionLimits {
    /// 配置不变量校验：违规值**就地钳制**并返回违规清单（不 panic）。
    ///
    /// 每次装配 run 都要跑：钳制后的值就是 [`Self::to_policy`] 的入参，钳制一次
    /// 缓存下来会让「改 env 后重启」与「运行期生效」两套口径并存。清单只供日志，
    /// 经 [`claim_config_log`] 每进程只打一次 —— 否则每条聊天 run 都刷屏。
    ///
    /// 校验顺序 V1′ → V2 → V3 → V4 → **V7 → V5 → V5′** → V6。V7 前置：守卫即将被
    /// 钳为 0 时，再按它钳 keep_view 只会留下无效告警。
    pub fn validate(&mut self) -> Vec<ConfigViolation> {
        let mut violations = Vec::new();

        // V1′：保留区目标 + 摘要估算上界 ≤ 触发阈值 —— 折叠后残留「装得下」的前提
        if self.keep_recent_tokens + SUMMARY_TOKEN_CAP > self.trigger_tokens {
            let clamped = self.trigger_tokens.saturating_sub(SUMMARY_TOKEN_CAP);
            violations.push(ConfigViolation::error(
                "V1'",
                format!(
                    "keep_recent_tokens {} + summary cap {} exceeds trigger {}; clamped to {}",
                    self.keep_recent_tokens, SUMMARY_TOKEN_CAP, self.trigger_tokens, clamped
                ),
            ));
            self.keep_recent_tokens = clamped;
            if self.trigger_tokens <= SUMMARY_TOKEN_CAP {
                violations.push(ConfigViolation::warn(
                    "V1'",
                    format!(
                        "trigger {} at or below summary cap {}; residual token bound not guaranteeable",
                        self.trigger_tokens, SUMMARY_TOKEN_CAP
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

    /// 由配置构造压缩策略：钳制后的字段就是 policy 的入参，中间不再有第二份取值。
    pub fn to_policy(&self, summarizer: Arc<dyn TurnSummarizer>) -> CompactionPolicy {
        let mut policy =
            CompactionPolicy::new(self.trigger_tokens, self.keep_recent_tokens, summarizer);
        // shadow 或 enforce 任一开启即装配削减策略（shadow 只评估、enforce 就地
        // 改写），两者都关时不扫描 —— 压缩路径与引入观测前逐字节一致
        if self.strip.shadow || self.strip.enforce {
            policy = policy.with_strip(StripPolicy {
                keep_recent_turns: self.strip.keep_recent_turns,
                strip_reasoning: self.strip.strip_reasoning,
                strip_tool_args: self.strip.strip_tool_args,
                tool_output_max_chars: self.strip.tool_output_max_chars,
                enforce: self.strip.enforce,
            });
        }
        // 0 = 关（builder 内部映射为 None）；V7 已保证「守卫 ⇒ 削减生效」
        policy.with_view_guard(self.strip.view_guard_tokens, self.strip.keep_view_tokens)
    }
}

/// 配置不变量违规：不变量编号 + 简述 + 日志级别。
///
/// 由 [`CompactionLimits::validate`] 收集；**钳制在收集时即已生效**，清单只供
/// 日志使用（每进程只打一次，见 [`claim_config_log`]）。
#[derive(Debug, Clone)]
pub struct ConfigViolation {
    /// 不变量编号（进日志字段，便于 grep 对照）。
    pub invariant: &'static str,
    /// `true` = error 级（违规值已钳制）；`false` = warn 级（仅提示，不钳制）。
    pub is_error: bool,
    /// 简述（英文，直接进日志 detail 字段）。
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

/// 配置违规日志闸门 —— 每进程一次。
static CONFIG_WARNED: OnceLock<()> = OnceLock::new();

/// 领取配置违规日志的打印权：本进程首次返回 true，之后恒 false。
pub fn claim_config_log() -> bool {
    CONFIG_WARNED.set(()).is_ok()
}

/// 保留区必须**严格小于**触发阈值。
///
/// 相等或更大时，折叠到「保留区」后总量仍不小于阈值，下一轮边界会再次触发摘要
/// 调用 —— 每轮一次元任务调用，既烧钱又一直压不干净。参数非法时宁可压得更狠，
/// 也不进入这条自激循环。
fn clamp_keep_recent(trigger_tokens: usize, keep_recent_tokens: usize) -> usize {
    if keep_recent_tokens < trigger_tokens {
        keep_recent_tokens
    } else {
        trigger_tokens / 2
    }
}

/// 校验工具输出截断上限：`0` = 关闭（合法）；非 0 必须不小于转录下界 ——
/// 低于下界时削后转录行短于单条上限、无损等式失配，回退默认。
fn clamp_tool_output_cap(value: usize) -> usize {
    if value != 0 && value < TRANSCRIPT_TOOL_OUTPUT_MIN_CAP {
        warn!(
            variable = ENV_STRIP_TOOL_OUTPUT_MAX_CHARS,
            value,
            min = TRANSCRIPT_TOOL_OUTPUT_MIN_CAP,
            "Tool output strip cap below transcript bound; using default"
        );
        TRANSCRIPT_ITEM_MAX_CHARS
    } else {
        value
    }
}

/// 空模型名会让摘要调用必然失败，按「未设置」处理。
fn model_or_default<F: Fn(&str) -> Option<String>>(get: &F) -> String {
    match get(ENV_SUMMARIZER_MODEL) {
        Some(raw) if !raw.trim().is_empty() => raw.trim().to_string(),
        Some(raw) => {
            warn!(
                variable = ENV_SUMMARIZER_MODEL,
                value = %raw,
                "Empty summarizer model; using default"
            );
            DEFAULT_SUMMARIZER_MODEL.to_string()
        }
        None => DEFAULT_SUMMARIZER_MODEL.to_string(),
    }
}

/// 读一个数值环境变量：缺失取默认值，写错告警后取默认值。
///
/// 解析失败不静默吞：「值写错却看起来生效了」是最难查的配置问题。
fn parse_or<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: usize) -> usize {
    match get(name) {
        Some(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                warn!(
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

/// 读一个布尔环境变量：缺失取默认值，写错告警后取默认值 —— 写错的开关不该
/// 静默变成 `false`，「观测忽然停了」不该由一次拼写错误解释。
fn parse_bool_or<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: bool) -> bool {
    match get(name) {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            other => {
                warn!(
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
    use peco_core::agent::{AgentError, estimate_str_tokens};
    use peco_core::session::Session;

    /// 用一个 map 充当 env：可注入读法让覆盖用例不碰进程环境。
    fn reader(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// 固定输出摘要器：装配与边界用例都不该碰真模型。
    struct FixedSummarizer;

    #[async_trait::async_trait]
    impl TurnSummarizer for FixedSummarizer {
        async fn summarize(
            &self,
            _previous: Option<&str>,
            _evicted: &str,
        ) -> Result<String, AgentError> {
            Ok("## 已做决定\n- 固定摘要".to_string())
        }

        async fn summarize_inflight(&self, _inflight: &str) -> Result<String, AgentError> {
            Ok("## 本轮已完成".to_string())
        }
    }

    /// 构造 viewable token 恰为 `turns * 300` 的会话：每轮一条 1000 字符 ASCII
    /// 用户消息（校准估算 0.3/字符），全量口径与 viewable 口径因此相等 ——
    /// 守卫用例要的正是「只看 viewable 就能和全量口径分开」。
    fn session_with_view_tokens(turns: usize) -> Session {
        let text = "a".repeat(1000);
        assert_eq!(estimate_str_tokens(&text), 300, "每轮 token 必须可手算");
        let mut session = Session::new("conv-view".to_string(), "view".to_string());
        for _ in 0..turns {
            session.start_turn(text.clone().into()).unwrap();
            session.commit_turn().unwrap();
        }
        session
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn absent_env_falls_back_to_the_server_defaults() {
        let limits = limits_from(reader(&[]));
        assert_eq!(limits.trigger_tokens, DEFAULT_TRIGGER_TOKENS);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.summarizer_model, DEFAULT_SUMMARIZER_MODEL);
        assert_eq!(limits.history_token_budget, DEFAULT_HISTORY_TOKEN_BUDGET);
    }

    /// 三个变量各自独立生效：只覆盖触发阈值与模型名时，保留区仍是默认。
    ///
    /// 触发阈值取大于默认保留区的值 —— 反过来会把默认保留区钳到一半，
    /// 那条行为由下面的钳制用例覆盖。
    #[test]
    fn env_vars_override_each_field_independently() {
        let limits = limits_from(reader(&[
            (ENV_TRIGGER_TOKENS, "500000"),
            (ENV_SUMMARIZER_MODEL, " another-model "),
            (ENV_HISTORY_TOKEN_BUDGET, "200000"),
        ]));
        assert_eq!(limits.trigger_tokens, 500_000);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.summarizer_model, "another-model");
        assert_eq!(limits.history_token_budget, 200_000);
    }

    /// 只调低触发阈值也会被钳制：默认保留区比它大，不钳就每轮折叠。
    #[test]
    fn lowering_the_trigger_below_the_default_keep_clamps_the_keep() {
        let limits = limits_from(reader(&[(ENV_TRIGGER_TOKENS, "100000")]));
        assert_eq!(limits.trigger_tokens, 100_000);
        assert_eq!(limits.keep_recent_tokens, 50_000);
    }

    /// 保留区不小于触发阈值时就地钳到一半：否则每轮边界都重新折叠。
    #[test]
    fn keep_recent_at_or_above_the_trigger_is_clamped_to_half() {
        assert_eq!(clamp_keep_recent(1000, 1000), 500);
        assert_eq!(clamp_keep_recent(1000, 5000), 500);
        assert_eq!(clamp_keep_recent(1000, 999), 999);

        let limits = limits_from(reader(&[
            (ENV_TRIGGER_TOKENS, "40000"),
            (ENV_KEEP_RECENT_TOKENS, "40000"),
        ]));
        assert_eq!(limits.trigger_tokens, 40_000);
        assert_eq!(limits.keep_recent_tokens, 20_000);
    }

    /// 写错的值退回默认，而不是变成 0 —— 0 触发阈值等于每轮都折叠。
    #[test]
    fn malformed_numbers_fall_back_to_the_defaults() {
        let limits = limits_from(reader(&[
            (ENV_TRIGGER_TOKENS, "many"),
            (ENV_KEEP_RECENT_TOKENS, "lots"),
            (ENV_HISTORY_TOKEN_BUDGET, "abc"),
            (ENV_STRIP_KEEP_RECENT_TURNS, "abc"),
            (ENV_VIEW_GUARD_TOKENS, "abc"),
        ]));
        assert_eq!(limits.trigger_tokens, DEFAULT_TRIGGER_TOKENS);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.history_token_budget, DEFAULT_HISTORY_TOKEN_BUDGET);
        assert_eq!(
            limits.strip.keep_recent_turns,
            DEFAULT_STRIP_KEEP_RECENT_TURNS
        );
        assert_eq!(limits.strip.view_guard_tokens, 0);
    }

    #[test]
    fn blank_model_name_is_treated_as_unset() {
        let limits = limits_from(reader(&[(ENV_SUMMARIZER_MODEL, "   ")]));
        assert_eq!(limits.summarizer_model, DEFAULT_SUMMARIZER_MODEL);
    }

    #[test]
    fn strip_defaults_are_shadow_only() {
        let limits = limits_from(reader(&[]));
        assert!(limits.strip.shadow, "默认必须观测");
        assert!(!limits.strip.enforce, "默认必须 legacy：只评估不改写");
        assert_eq!(limits.strip.keep_recent_turns, 4);
        assert!(limits.strip.strip_reasoning);
        assert!(limits.strip.strip_tool_args);
        assert_eq!(
            limits.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS,
            "默认上限与转录单条上限同源"
        );
        assert_eq!(limits.strip.retention_days, 30);
        assert_eq!(limits.strip.view_guard_tokens, 0, "守卫默认必须关");
        assert_eq!(limits.strip.keep_view_tokens, 0, "view 保留默认必须关");
    }

    #[test]
    fn strip_env_overrides_and_invalid_fall_back() {
        let limits = limits_from(reader(&[
            (ENV_STRIP_SHADOW, "off"),
            (ENV_STRIP_KEEP_RECENT_TURNS, "8"),
            (ENV_STRIP_REASONING, "false"),
            (ENV_STRIP_TOOL_ARGS, "0"),
            (ENV_STRIP_ENFORCE, "on"),
            (ENV_CONTEXT_SHADOW_RETENTION_DAYS, "7"),
            (ENV_VIEW_GUARD_TOKENS, "50000"),
            (ENV_KEEP_VIEW_TOKENS, "25000"),
        ]));
        assert!(!limits.strip.shadow);
        assert_eq!(limits.strip.keep_recent_turns, 8);
        assert!(!limits.strip.strip_reasoning);
        assert!(!limits.strip.strip_tool_args);
        assert!(limits.strip.enforce);
        assert_eq!(limits.strip.retention_days, 7);
        assert_eq!(limits.strip.view_guard_tokens, 50_000);
        assert_eq!(limits.strip.keep_view_tokens, 25_000);

        let limits = limits_from(reader(&[
            (ENV_STRIP_SHADOW, "maybe"),
            (ENV_STRIP_ENFORCE, "maybe"),
            (ENV_CONTEXT_SHADOW_RETENTION_DAYS, "abc"),
        ]));
        assert!(limits.strip.shadow, "写错的布尔取默认 true");
        assert!(!limits.strip.enforce, "写错的布尔取默认 false");
        assert_eq!(
            limits.strip.retention_days,
            DEFAULT_CONTEXT_SHADOW_RETENTION_DAYS
        );
    }

    #[test]
    fn tool_output_cap_below_transcript_bound_falls_back_to_default() {
        // 显式关闭合法
        let limits = limits_from(reader(&[(ENV_STRIP_TOOL_OUTPUT_MAX_CHARS, "0")]));
        assert_eq!(limits.strip.tool_output_max_chars, 0);

        // 合法放大：≥ 下界即通过
        let limits = limits_from(reader(&[(ENV_STRIP_TOOL_OUTPUT_MAX_CHARS, "4000")]));
        assert_eq!(limits.strip.tool_output_max_chars, 4000);

        // 低于下界：回退默认（非 0 保底）
        let limits = limits_from(reader(&[(ENV_STRIP_TOOL_OUTPUT_MAX_CHARS, "100")]));
        assert_eq!(
            limits.strip.tool_output_max_chars,
            TRANSCRIPT_ITEM_MAX_CHARS
        );
    }

    #[test]
    fn default_config_passes_validation() {
        let mut limits = limits_from(reader(&[]));
        let violations = limits.validate();
        assert!(
            violations.is_empty(),
            "默认配置必须零违规，实际: {violations:?}"
        );
        assert!(limits.keep_recent_tokens + SUMMARY_TOKEN_CAP <= limits.trigger_tokens);
    }

    /// V1′：钳后的保留区就是 policy 的入参 —— 断言落在 policy 的公开字段上，
    /// 而不只是 limits 自己。
    #[test]
    fn v1_prime_clamps_keep_recent_and_feeds_the_policy() {
        // 刚好越界：90_000 + 20_000 = 110_000 > 100_000
        let mut limits = limits_from(reader(&[
            (ENV_TRIGGER_TOKENS, "100000"),
            (ENV_KEEP_RECENT_TOKENS, "90000"),
        ]));
        let violations = limits.validate();
        assert_eq!(violations.len(), 1, "只应有 V1′: {violations:?}");
        assert_eq!(violations[0].invariant, "V1'");
        assert!(violations[0].is_error);
        assert_eq!(limits.keep_recent_tokens, 100_000 - SUMMARY_TOKEN_CAP);

        let policy = limits.to_policy(Arc::new(FixedSummarizer));
        assert_eq!(policy.trigger_tokens, 100_000);
        assert_eq!(
            policy.keep_recent_tokens,
            100_000 - SUMMARY_TOKEN_CAP,
            "钳后值必须真的进入 policy 构造"
        );
    }

    /// V4：守卫阈值超窗时钳到窗口宽，钳后值真的决定触发。
    ///
    /// 用 viewable token 落在 (窗口, 原始守卫] 区间的会话区分两种取值：
    /// 钳到窗口 ⇒ 触发；原样 138_000 ⇒ 132_000 的 view 不触发。
    #[test]
    fn v4_clamps_guard_to_history_window() {
        let mut limits = limits_from(reader(&[
            (ENV_STRIP_ENFORCE, "on"),
            (ENV_VIEW_GUARD_TOKENS, "138000"),
            (ENV_KEEP_VIEW_TOKENS, "64000"),
        ]));
        let violations = limits.validate();
        assert_eq!(violations.len(), 1, "只应有 V4: {violations:?}");
        assert_eq!(violations[0].invariant, "V4");
        assert_eq!(
            limits.strip.view_guard_tokens, DEFAULT_HISTORY_TOKEN_BUDGET,
            "钳到窗口宽"
        );
        assert_eq!(limits.strip.keep_view_tokens, 64_000, "keep_view 不动");

        let policy = limits.to_policy(Arc::new(FixedSummarizer));
        let mut session = session_with_view_tokens(440); // 132_000 viewable token
        let reduction = block_on(policy.apply_boundary(&mut session)).unwrap();
        assert_eq!(reduction.committed_view_tokens, 132_000);
        assert!(
            reduction.guard_triggered,
            "钳到 128_000 的守卫必须对 132_000 的 view 触发；未钳则不会"
        );
    }

    /// V7：削减未生效时守卫被钳为 0 —— 守卫不减 viewable，先于全量触发只会升频折叠。
    #[test]
    fn v7_zeroes_guard_without_enforce() {
        let mut limits = limits_from(reader(&[
            (ENV_VIEW_GUARD_TOKENS, "60000"),
            (ENV_KEEP_VIEW_TOKENS, "30000"),
        ]));
        let violations = limits.validate();
        assert_eq!(violations.len(), 1, "只应有 V7: {violations:?}");
        assert_eq!(violations[0].invariant, "V7");
        assert!(violations[0].is_error);
        assert_eq!(limits.strip.view_guard_tokens, 0, "V7 钳守卫为 0");

        let policy = limits.to_policy(Arc::new(FixedSummarizer));
        let mut session = session_with_view_tokens(204); // 61_200 viewable token
        let reduction = block_on(policy.apply_boundary(&mut session)).unwrap();
        assert_eq!(reduction.committed_view_tokens, 61_200);
        assert!(
            !reduction.guard_triggered,
            "V7 之后守卫必须关：否则 60_000 的守卫会对 61_200 触发"
        );
        assert!(
            reduction.compaction.is_none(),
            "全量口径远低于触发阈值，不该折叠"
        );
    }

    /// V2/V3/V5 是对程序化构造配置的兜底（env 路径已在读取端钳过）。
    #[test]
    fn degenerate_strip_bounds_are_clamped() {
        let mut limits = limits_from(reader(&[(ENV_STRIP_ENFORCE, "on")]));
        limits.strip.keep_recent_turns = 0;
        limits.strip.tool_output_max_chars = 100;
        limits.strip.view_guard_tokens = 80_000;
        limits.strip.keep_view_tokens = 80_000;
        let violations = limits.validate();
        let names: Vec<_> = violations.iter().map(|v| v.invariant).collect();
        assert_eq!(names, vec!["V2", "V3", "V5"], "{violations:?}");
        assert_eq!(limits.strip.keep_recent_turns, 1, "V2 钳到 1");
        assert_eq!(
            limits.strip.tool_output_max_chars, TRANSCRIPT_ITEM_MAX_CHARS,
            "V3 回退默认上限"
        );
        assert_eq!(limits.strip.keep_view_tokens, 40_000, "V5 钳到 guard/2");
        assert!(limits.strip.keep_view_tokens < limits.strip.view_guard_tokens);
    }

    /// V5′：保留区不得低于守卫的四分之一，否则退化成「只留最新 1 轮」。
    #[test]
    fn narrow_keep_view_is_clamped_to_half_guard() {
        let mut limits = limits_from(reader(&[
            (ENV_STRIP_ENFORCE, "on"),
            (ENV_VIEW_GUARD_TOKENS, "100000"),
            (ENV_KEEP_VIEW_TOKENS, "0"),
        ]));
        let violations = limits.validate();
        assert_eq!(violations.len(), 1, "只应有 V5′: {violations:?}");
        assert_eq!(violations[0].invariant, "V5'");
        assert_eq!(limits.strip.keep_view_tokens, 50_000, "钳到 guard/2");
    }

    /// V6 只提示不钳制：允许「先削减生效、观测后再开守卫」的灰度路径。
    #[test]
    fn v6_warns_enforce_without_guard() {
        let mut limits = limits_from(reader(&[(ENV_STRIP_ENFORCE, "on")]));
        let violations = limits.validate();
        assert_eq!(violations.len(), 1, "只应有 V6: {violations:?}");
        assert_eq!(violations[0].invariant, "V6");
        assert!(!violations[0].is_error, "V6 只提示不钳制");
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.strip.view_guard_tokens, 0);
    }

    /// 削减装配与否只由开关决定：两者都关时策略里没有 strip。
    #[test]
    fn strip_policy_is_attached_only_when_a_switch_is_on() {
        let mut off = limits_from(reader(&[
            (ENV_STRIP_SHADOW, "off"),
            (ENV_STRIP_ENFORCE, "off"),
        ]));
        off.validate();
        let policy = off.to_policy(Arc::new(FixedSummarizer));
        let reduction = block_on(policy.apply_boundary(&mut session_with_view_tokens(1))).unwrap();
        assert!(
            reduction.strip_evaluation.is_none(),
            "削减关闭时不应有评估值"
        );

        let mut on = limits_from(reader(&[]));
        on.validate();
        let policy = on.to_policy(Arc::new(FixedSummarizer));
        let reduction = block_on(policy.apply_boundary(&mut session_with_view_tokens(1))).unwrap();
        assert!(
            reduction.strip_evaluation.is_some(),
            "shadow 观测默认开启，必须产出评估值"
        );
    }

    #[test]
    fn config_log_gate_is_once_per_process() {
        assert!(claim_config_log(), "首次必须领到打印权");
        assert!(!claim_config_log(), "二次必须静默");
    }
}
