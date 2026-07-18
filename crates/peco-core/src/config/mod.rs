// ============================================================================
// Config 模块 — 项目配置文件的读取、写入和管理
// ============================================================================
//
// 负责 providers.toml 的加载、解析、验证和写入，以及 MCP 配置的 JSON 解析。
// 仅处理配置文件 I/O，不负责构建 provider 实例。
//
// # 子模块
//
// - [`types`] — 配置数据结构（`ProvidersConfig`, `ProviderEntry`, `LlmApiParams`）
// - [`error`] — 配置相关错误类型（`ConfigError`）
// - [`loader`] — 配置加载与写入函数（providers.toml）
// - [`mcp_config`] — MCP 配置解析（`McpConfig`, `McpServerConfig`, `TransportType`）

mod error;
mod global_config;
mod loader;
mod mcp_config;
mod types;

pub use error::ConfigError;
pub use global_config::GlobalConfig;
pub use loader::{find_config_path, load_config, provider_names, save_config};
pub use mcp_config::{McpConfig, McpServerConfig, TransportType, resolve_env_vars};
pub use types::{LlmApiParams, ProviderEntry, ProvidersConfig, resolve_key};
