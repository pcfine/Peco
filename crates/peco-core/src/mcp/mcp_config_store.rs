//! MCP 配置存储 — 每个工作空间的共享持有者，支持热重载。
//!
//! [`McpConfigStore`] 是配置层的组件，与 [`McpManager`](super::McpManager)
//!（负责每个 Agent 的连接编排）相对应。它在内部 [`RwLock`] 中持有 [`McpConfig`]，
//! 调用方可通过 [`get()`](McpConfigStore::get) 获取当前配置快照，
//! 或通过 [`reload()`](McpConfigStore::reload) 从磁盘热重载。

use std::path::Path;
use std::sync::RwLock;

use tracing::info;

use crate::config::McpConfig;

// ── McpConfigStore ──────────────────────────────────────────────────────────

/// MCP 配置的共享持有者，支持热重载。
///
/// 每个 [`WorkSpace`](crate::workspace::WorkSpace) 一个实例，
/// 注入到 [`AgentManager`](crate::agent::AgentManager) 中。
/// [`Agent::from_file`](crate::agent::Agent::from_file) 通过 [`get()`](Self::get)
/// 获取快照，因此已构造的 Agent 不受热重载影响。
///
/// # 示例
///
/// ```no_run
/// use std::path::Path;
/// use peco_core::config::McpConfig;
/// use peco_core::mcp::McpConfigStore;
///
/// let store = McpConfigStore::new(McpConfig::empty());
/// let snapshot = store.get();
///
/// // 稍后：从工作空间根目录重新加载
/// let count = store.reload(Path::new("/workspace"), &McpConfig::empty());
/// println!("已配置 {count} 个 MCP 服务器");
/// ```
pub struct McpConfigStore {
    config: RwLock<McpConfig>,
}

impl McpConfigStore {
    /// 使用给定的初始配置创建新的存储实例。
    pub fn new(config: McpConfig) -> Self {
        Self {
            config: RwLock::new(config),
        }
    }

    /// 返回当前 MCP 配置的快照（克隆）。
    ///
    /// 刻意返回克隆 — 调用方获得的是时间点视图，
    /// 不会随热重载而变化，符合"已加载 Agent 不受热更新影响"的设计原则。
    pub fn get(&self) -> McpConfig {
        self.config
            .read()
            .expect("McpConfigStore RwLock poisoned")
            .clone()
    }

    /// 从工作空间根目录重新加载 MCP 配置。
    ///
    /// 读取 `{workspace_root}/mcpconfig.json`。若用户级文件不存在或解析失败，
    /// 回退到 `system_mcp`。解析时会执行校验（如 stdio 传输必须有 command 字段）。
    ///
    /// 返回新配置中 MCP 服务器的数量。
    pub fn reload(&self, workspace_root: &Path, system_mcp: &McpConfig) -> usize {
        let user_mcp_path = workspace_root.join("mcpconfig.json");

        let new_config = if user_mcp_path.exists() {
            match std::fs::read_to_string(&user_mcp_path) {
                Ok(content) => match McpConfig::from_json_str(&content) {
                    Ok(config) => config,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %user_mcp_path.display(),
                            "解析用户 mcpconfig.json 失败，使用系统回退配置"
                        );
                        system_mcp.clone()
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %user_mcp_path.display(),
                        "读取用户 mcpconfig.json 失败，使用系统回退配置"
                    );
                    system_mcp.clone()
                }
            }
        } else {
            system_mcp.clone()
        };

        let count = new_config.mcp_servers.len();
        *self.config.write().expect("McpConfigStore RwLock poisoned") = new_config;
        info!(servers = count, "MCP 配置已重新加载");
        count
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_get() {
        let config = McpConfig::empty();
        let store = McpConfigStore::new(config);
        assert!(store.get().mcp_servers.is_empty());
    }

    #[test]
    fn reload_missing_file_uses_fallback() {
        let store = McpConfigStore::new(McpConfig::empty());
        let fallback = McpConfig::empty();
        let count = store.reload(Path::new("/nonexistent/path/to/workspace"), &fallback);
        assert_eq!(count, 0);
    }

    #[test]
    fn reload_from_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let json = r#"{"mcpServers": {"test-srv": {"transport": "stdio", "command": "echo"}}}"#;
        std::fs::write(tmp.path().join("mcpconfig.json"), json).unwrap();

        let store = McpConfigStore::new(McpConfig::empty());
        let fallback = McpConfig::empty();
        let count = store.reload(tmp.path(), &fallback);
        assert_eq!(count, 1);
        assert!(store.get().mcp_servers.contains_key("test-srv"));
    }

    #[test]
    fn reload_invalid_json_uses_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("mcpconfig.json"), "not json").unwrap();

        let store = McpConfigStore::new(McpConfig::empty());
        let fallback = McpConfig::empty();
        let count = store.reload(tmp.path(), &fallback);
        // 回退到系统配置（空）
        assert_eq!(count, 0);
    }

    #[test]
    fn reload_invalid_config_uses_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        // stdio 传输缺少 command 字段 — 校验应失败
        let json = r#"{"mcpServers": {"bad-srv": {"transport": "stdio"}}}"#;
        std::fs::write(tmp.path().join("mcpconfig.json"), json).unwrap();

        let store = McpConfigStore::new(McpConfig::empty());
        let fallback = McpConfig::empty();
        let count = store.reload(tmp.path(), &fallback);
        // 校验失败，回退到系统配置
        assert_eq!(count, 0);
    }
}
