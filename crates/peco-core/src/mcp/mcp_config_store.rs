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

    /// 原子更新 MCP 配置。
    ///
    /// 获取写锁，调用 `f` 修改配置、校验结果、通过临时文件重命名原子写入磁盘，
    /// 然后直接更新内存中的配置。整个操作在写锁保护下完成，消除 TOCTOU 竞态。
    ///
    /// 与先 `get()` 克隆、再修改、再 `write()` + `reload()` 的分步方式不同，
    /// 此方法保证原子性且不需要回退配置。写入失败时内存状态保持不变。
    pub fn atomic_update<F>(&self, workspace_root: &Path, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut McpConfig) -> Result<(), String>,
    {
        let mut guard = self.config.write().expect("McpConfigStore RwLock poisoned");

        // 在副本上应用变更，避免失败时污染内存（写盘成功后才写回）
        let mut new_config = guard.clone();

        // 1. 在副本上应用变更
        f(&mut new_config)?;

        // 2. 校验完整配置
        new_config
            .validate()
            .map_err(|e| format!("Invalid MCP config: {e}"))?;

        // 3. 原子写入磁盘（先写临时文件再重命名）
        let path = workspace_root.join("mcpconfig.json");
        let tmp_path = workspace_root.join(".mcpconfig.json.tmp");
        let json = serde_json::to_string_pretty(&new_config)
            .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
        std::fs::write(&tmp_path, &json).map_err(|e| format!("Failed to write MCP config: {e}"))?;
        std::fs::rename(&tmp_path, &path)
            .map_err(|e| format!("Failed to commit MCP config: {e}"))?;

        // 4. 写盘成功后，才更新内存
        let count = new_config.mcp_servers.len();
        *guard = new_config;
        info!(servers = count, "MCP 配置已原子更新");
        Ok(())
    }

    /// 从工作空间根目录重新加载 MCP 配置。
    ///
    /// 读取 `{workspace_root}/mcpconfig.json`。若文件不存在，回退到 `system_mcp`。
    /// **若文件存在但解析或校验失败，保留当前内存配置不变**（而非覆盖为回退配置）。
    ///
    /// 返回新配置中 MCP 服务器的数量。失败时返回当前内存中的服务器数量。
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
                            "解析用户 mcpconfig.json 失败，保留当前内存配置"
                        );
                        return self
                            .config
                            .read()
                            .expect("McpConfigStore RwLock poisoned")
                            .mcp_servers
                            .len();
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %user_mcp_path.display(),
                        "读取用户 mcpconfig.json 失败，保留当前内存配置"
                    );
                    return self
                        .config
                        .read()
                        .expect("McpConfigStore RwLock poisoned")
                        .mcp_servers
                        .len();
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
    fn reload_invalid_json_preserves_current_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("mcpconfig.json"), "not json").unwrap();

        // 先在 store 中放入一个有效配置（模拟已有配置）
        let valid_json =
            r#"{"mcpServers": {"existing": {"transport": "stdio", "command": "echo"}}}"#;
        let existing = McpConfig::from_json_str(valid_json).unwrap();
        let store = McpConfigStore::new(existing);
        let fallback = McpConfig::empty();
        let count = store.reload(tmp.path(), &fallback);
        // 解析失败 → 保留当前内存配置（1 个 server），不覆盖为空
        assert_eq!(count, 1);
        assert!(store.get().mcp_servers.contains_key("existing"));
    }

    #[test]
    fn reload_invalid_config_preserves_current_config() {
        let tmp = tempfile::tempdir().unwrap();
        // stdio 传输缺少 command 字段 — 校验应失败
        let json = r#"{"mcpServers": {"bad-srv": {"transport": "stdio"}}}"#;
        std::fs::write(tmp.path().join("mcpconfig.json"), json).unwrap();

        // 先在 store 中放入一个有效配置
        let valid_json =
            r#"{"mcpServers": {"existing": {"transport": "stdio", "command": "echo"}}}"#;
        let existing = McpConfig::from_json_str(valid_json).unwrap();
        let store = McpConfigStore::new(existing);
        let fallback = McpConfig::empty();
        let count = store.reload(tmp.path(), &fallback);
        // 校验失败 → 保留当前内存配置（1 个 server），不覆盖为空
        assert_eq!(count, 1);
        assert!(store.get().mcp_servers.contains_key("existing"));
    }

    #[test]
    fn atomic_update_validation_failure_preserves_memory() {
        let tmp = tempfile::tempdir().unwrap();

        // 先原子写入一个有效配置
        let store = McpConfigStore::new(McpConfig::empty());
        store
            .atomic_update(tmp.path(), |config| {
                *config = McpConfig::from_json_str(
                    r#"{"mcpServers": {"existing": {"transport": "stdio", "command": "echo"}}}"#,
                )
                .unwrap();
                Ok(())
            })
            .unwrap();

        // 尝试原子写入一个无效配置：stdio 缺 command
        // 用 serde_json::from_str（不校验）构造，让它在 atomic_update 的 validate() 阶段失败
        let result = store.atomic_update(tmp.path(), |config| {
            *config =
                serde_json::from_str(r#"{"mcpServers": {"bad": {"transport": "stdio"}}}"#).unwrap();
            Ok(())
        });

        assert!(result.is_err());
        // 内存应保留 existing，不被 "bad" 污染
        let config = store.get();
        assert!(config.mcp_servers.contains_key("existing"));
        assert!(!config.mcp_servers.contains_key("bad"));
    }
}
