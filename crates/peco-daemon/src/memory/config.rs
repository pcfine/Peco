//! 记忆读写路径的参数：环境变量入口。
//!
//! 默认值与 `peco-server` 的同名配置对齐 —— 同一个产品在两处宿主上给出不同
//! 的召回条数与注入上限，会让「为什么这里记不住」变成无法对照的实现细节。

use tracing::warn;

/// 记忆总开关。
pub const ENV_ENABLED: &str = "PECO_MEMORY_ENABLED";
/// 提取模型（Flash 档）。
pub const ENV_MODEL: &str = "PECO_MEMORY_MODEL";
/// 读路径单次检索条数。
pub const ENV_TOP_K: &str = "PECO_MEMORY_TOP_K";
/// 读路径注入的 token 上限（估算口径，超限整行丢弃）。
pub const ENV_TOKEN_CAP: &str = "PECO_MEMORY_TOKEN_CAP";

/// 记忆 KB 名（与团队模板保持一致，不开环境变量：它同时是模板里的约定名）。
pub const KB_NAME: &str = "@private_memory";

const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const DEFAULT_TOP_K: usize = 5;
const DEFAULT_TOKEN_CAP: usize = 2000;

/// 写路径与读路径共享的一份参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    /// 总开关。`false` 时两条路径都不工作（写路径不提取、读路径不注入）。
    pub enabled: bool,
    /// 记忆知识库名。
    pub kb_name: String,
    /// 提取模型。
    pub model: String,
    /// 读路径召回条数（检索时按两倍取候选，过滤掉别的空间后再截断）。
    pub top_k: usize,
    /// 读路径注入的 token 上限。
    pub token_cap: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            kb_name: KB_NAME.to_string(),
            model: DEFAULT_MODEL.to_string(),
            top_k: DEFAULT_TOP_K,
            token_cap: DEFAULT_TOKEN_CAP,
        }
    }
}

impl MemoryConfig {
    /// 读进程环境得到参数；非法值告警后回退默认。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 读取内核可注入：不改进程 env 也能验证覆盖生效，测试之间因此不会互相污染。
    fn from_env_with<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let default = Self::default();
        Self {
            enabled: env_bool(&get, ENV_ENABLED, default.enabled),
            model: env_string(&get, ENV_MODEL, default.model),
            top_k: env_usize(&get, ENV_TOP_K, default.top_k),
            token_cap: env_usize(&get, ENV_TOKEN_CAP, default.token_cap),
            ..default
        }
    }
}

/// 布尔开关只认明确的真值；写错的值退回默认而不是静默变成 `false` ——
/// 「记忆忽然不再写入」不该由一次拼写错误解释。
fn env_bool<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: bool) -> bool {
    match get(name) {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                warn!(
                    variable = name,
                    value = %raw,
                    "Invalid boolean env var; using default"
                );
                default
            }
        },
        None => default,
    }
}

/// 空模型名会让提取调用必然失败，按「未设置」处理。
fn env_string<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: String) -> String {
    match get(name) {
        Some(raw) if !raw.trim().is_empty() => raw.trim().to_string(),
        Some(raw) => {
            warn!(
                variable = name,
                value = %raw,
                "Empty env var; using default"
            );
            default
        }
        None => default,
    }
}

/// 数值项同 [`env_bool`]：解析失败告警并退回默认。
fn env_usize<F: Fn(&str) -> Option<String>>(get: &F, name: &str, default: usize) -> usize {
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
    fn absent_env_falls_back_to_the_defaults() {
        let config = MemoryConfig::from_env_with(reader(&[]));
        assert_eq!(config, MemoryConfig::default());
        assert_eq!(config.model, "deepseek-v4-flash");
        assert_eq!(config.top_k, 5);
        assert_eq!(config.token_cap, 2000);
        assert_eq!(config.kb_name, "@private_memory");
        assert!(config.enabled);
    }

    #[test]
    fn env_vars_override_each_field_independently() {
        let config = MemoryConfig::from_env_with(reader(&[
            (ENV_ENABLED, "false"),
            (ENV_MODEL, " another-model "),
            (ENV_TOP_K, "8"),
        ]));
        assert!(!config.enabled);
        assert_eq!(config.model, "another-model");
        assert_eq!(config.top_k, 8);
        assert_eq!(config.token_cap, DEFAULT_TOKEN_CAP, "未覆盖的项保持默认");
    }

    /// 写错的值退回默认，而不是变成 0 —— `top_k = 0` 会让召回永久为空，
    /// 表现为「记忆没坏但从来不出现」。
    #[test]
    fn malformed_values_fall_back_to_the_defaults() {
        let config = MemoryConfig::from_env_with(reader(&[
            (ENV_ENABLED, "maybe"),
            (ENV_TOP_K, "many"),
            (ENV_TOKEN_CAP, "lots"),
        ]));
        assert_eq!(config, MemoryConfig::default());

        let blank = MemoryConfig::from_env_with(reader(&[(ENV_MODEL, "   ")]));
        assert_eq!(blank.model, DEFAULT_MODEL);
    }
}
