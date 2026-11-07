//! 聊天 run 的上下文压缩参数：环境变量入口与不变量钳制。
//!
//! 默认值与 `peco-server` 的同名配置对齐 —— 两个宿主对「同一段历史何时折叠」
//! 给出不同口径，会让摘要时机变成无法对照的实现细节。

use tracing::warn;

/// 压缩触发阈值（估算 token，全量口径）的环境变量入口。
pub const ENV_TRIGGER_TOKENS: &str = "PECO_COMPACTION_TRIGGER_TOKENS";
/// 压缩后 verbatim 保留区目标（token）的环境变量入口。
pub const ENV_KEEP_RECENT_TOKENS: &str = "PECO_COMPACTION_KEEP_RECENT_TOKENS";
/// 摘要模型名的环境变量入口。
pub const ENV_SUMMARIZER_MODEL: &str = "PECO_SUMMARIZER_MODEL";

const DEFAULT_TRIGGER_TOKENS: usize = 384_000;
const DEFAULT_KEEP_RECENT_TOKENS: usize = 128_000;
const DEFAULT_SUMMARIZER_MODEL: &str = "deepseek-v4-flash";

/// 一次聊天 run 的压缩参数。
#[derive(Debug)]
pub struct CompactionLimits {
    pub trigger_tokens: usize,
    pub keep_recent_tokens: usize,
    pub summarizer_model: String,
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

    CompactionLimits {
        trigger_tokens,
        keep_recent_tokens,
        summarizer_model: model_or_default(&get),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn absent_env_falls_back_to_the_server_defaults() {
        let limits = limits_from(reader(&[]));
        assert_eq!(limits.trigger_tokens, DEFAULT_TRIGGER_TOKENS);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.summarizer_model, DEFAULT_SUMMARIZER_MODEL);
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
        ]));
        assert_eq!(limits.trigger_tokens, 500_000);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert_eq!(limits.summarizer_model, "another-model");
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
        ]));
        assert_eq!(limits.trigger_tokens, DEFAULT_TRIGGER_TOKENS);
        assert_eq!(limits.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
    }

    #[test]
    fn blank_model_name_is_treated_as_unset() {
        let limits = limits_from(reader(&[(ENV_SUMMARIZER_MODEL, "   ")]));
        assert_eq!(limits.summarizer_model, DEFAULT_SUMMARIZER_MODEL);
    }
}
