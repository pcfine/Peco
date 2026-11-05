// ============================================================================
// 进程级 SystemConfig 注入
// ============================================================================
//
// 一个进程只有一份系统配置：宿主在启动时调用 [`install_system_config`]，
// 之后任意位置的工具/工作空间都从 [`system_config`] 读取，不再层层传参。
// 首个注入生效，重复注入（含并发）不会覆盖，只记一条 warn。

use std::sync::{Arc, OnceLock};

use tracing::{info, warn};

use super::SystemConfig;

static SYSTEM_CONFIG: OnceLock<Arc<SystemConfig>> = OnceLock::new();

/// 注入进程级 [`SystemConfig`]。
///
/// 返回 `true` 表示本次注入生效；`false` 表示进程内已有配置，本次被忽略。
/// 用返回值而不是静默覆盖：同一个进程里出现两份配置说明宿主接线重复，
/// 静默换掉会让先注入方构造的工作空间与新工具读到不同凭据。
pub fn install_system_config(config: Arc<SystemConfig>) -> bool {
    match SYSTEM_CONFIG.set(config) {
        Ok(()) => {
            info!("process-level SystemConfig installed");
            true
        }
        Err(_) => {
            warn!("process-level SystemConfig already installed; the later one is ignored");
            false
        }
    }
}

/// 读取进程级 [`SystemConfig`]。宿主未注入时为 `None`。
pub fn system_config() -> Option<Arc<SystemConfig>> {
    SYSTEM_CONFIG.get().cloned()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{McpConfig, ProvidersConfig};

    fn config(tag: &str) -> Arc<SystemConfig> {
        Arc::new(SystemConfig {
            providers: ProvidersConfig {
                default_provider: tag.to_string(),
                providers: HashMap::new(),
                web_search: None,
            },
            mcp: McpConfig::empty(),
        })
    }

    /// 测试进程内前面的用例可能已经注入过：这里只断言「首个生效、不覆盖」。
    /// OnceLock 无法跨用例重置，故把断言绑在返回值语义上，不假设初始为空。
    #[test]
    fn the_first_install_wins() {
        let first = config("first");
        let installed = install_system_config(first.clone());

        let second = config("second");
        assert!(
            !install_system_config(second.clone()),
            "a second install must be rejected"
        );

        let current = system_config().expect("a config is installed");
        if installed {
            assert_eq!(current.providers.default_provider, "first");
        }
        assert!(
            !std::sync::Arc::ptr_eq(&current, &second),
            "the rejected config must never become visible"
        );
    }
}
