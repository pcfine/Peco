// ============================================================================
// Config 错误类型
// ============================================================================

use std::path::PathBuf;

/// 配置文件读取和解析过程中的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 读取文件失败
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    /// TOML 解析失败
    #[error("failed to parse TOML config: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// TOML 序列化失败
    #[error("failed to serialize TOML config: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// 未找到配置文件（providers.toml 默认路径搜索）
    #[error("providers.toml not found at any standard location")]
    ConfigNotFound,

    /// 指定路径的配置文件不存在
    #[error("config file not found: {0}")]
    ConfigFileNotFound(PathBuf),

    /// JSON 解析失败（MCP 配置等）
    #[error("failed to parse JSON config: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// 配置验证失败
    #[error("config validation error: {0}")]
    Validation(String),

    /// 指定的 provider 不存在
    #[error("provider '{0}' not found in config; available providers: check provider_names()")]
    ProviderNotFound(String),

    /// 缺少必填字段
    #[error("missing required field '{0}' for provider '{1}'")]
    MissingField(String, String),

    /// 环境变量未设置
    #[error("environment variable referenced in '{0}' not set: {1}")]
    EnvVar(String, #[source] std::env::VarError),
}
