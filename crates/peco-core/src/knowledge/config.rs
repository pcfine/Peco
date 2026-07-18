//! 知识库模块级别的配置。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::error::KnowledgeModuleError;

// ---------------------------------------------------------------------------
// 默认值
// ---------------------------------------------------------------------------

fn default_kb_dir() -> PathBuf {
    // 默认：~/.peco/knowledge_bases/
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".peco")
        .join("knowledge_bases")
}

fn default_backend() -> String {
    "lancedb".into()
}

fn default_embedding_model() -> String {
    "BGESmallZHV15".into()
}

const fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// KnowledgeConfig
// ---------------------------------------------------------------------------

/// 知识库模块级别配置。
///
/// 与每个知识库独立的 [`KbConfig`](knowledge_base::KbConfig) 不同，
/// 此结构体控制模块全局行为：数据目录、默认后端、同步策略等。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeConfig {
    /// 知识库数据根目录。
    /// 默认：`~/.peco/knowledge_bases/`
    #[serde(default = "default_kb_dir")]
    pub base_dir: PathBuf,

    /// 默认存储后端。
    /// 可选 "lancedb"、"inmemory"、"helixdb"
    #[serde(default = "default_backend")]
    pub default_backend: String,

    /// 默认嵌入模型。
    /// 可选 "BGESmallZHV15"、"BGELargeZHV15"、"AllMiniLML6V2Q"、"MultilingualE5Small"
    #[serde(default = "default_embedding_model")]
    pub default_embedding_model: String,

    /// 启动时是否自动同步所有知识库。
    #[serde(default)]
    pub auto_sync_on_start: bool,

    /// sync 时是否递归扫描子目录。
    #[serde(default = "default_true")]
    pub recursive_scan: bool,

    /// 是否启用文件监听（inotify），自动检测文件变更。
    /// 若 false，需手动调用 sync。
    #[serde(default)]
    pub watch_files: bool,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            base_dir: default_kb_dir(),
            default_backend: default_backend(),
            default_embedding_model: default_embedding_model(),
            auto_sync_on_start: false,
            recursive_scan: true,
            watch_files: false,
        }
    }
}

impl KnowledgeConfig {
    /// 加载配置，优先级：环境变量 > 配置文件 > 内置默认值。
    ///
    /// 配置文件路径：`~/.peco/knowledge_config.json`
    pub fn load() -> Result<Self, KnowledgeModuleError> {
        // 1. 从内置默认值开始
        let mut cfg = Self::default();

        // 2. 尝试加载配置文件
        let config_path = cfg
            .base_dir
            .parent()
            .map(|p| p.join("knowledge_config.json"));
        if let Some(ref path) = config_path {
            if path.exists() {
                let data = std::fs::read_to_string(path).map_err(KnowledgeModuleError::Io)?;
                cfg = serde_json::from_str(&data).map_err(KnowledgeModuleError::Json)?;
            }
        }

        // 3. 环境变量覆盖（最高优先级）
        if let Ok(val) = std::env::var("PECO_KB_ROOT") {
            cfg.base_dir = PathBuf::from(val);
        }
        if let Ok(val) = std::env::var("PECO_KB_DEFAULT_BACKEND") {
            cfg.default_backend = val;
        }
        if let Ok(val) = std::env::var("PECO_KB_DEFAULT_MODEL") {
            cfg.default_embedding_model = val;
        }
        if let Ok(val) = std::env::var("PECO_KB_AUTO_SYNC") {
            cfg.auto_sync_on_start = val.to_lowercase() == "true" || val == "1";
        }
        if let Ok(val) = std::env::var("PECO_KB_WATCH") {
            cfg.watch_files = val.to_lowercase() == "true" || val == "1";
        }

        Ok(cfg)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let cfg = KnowledgeConfig::default();
        assert_eq!(cfg.default_backend, "lancedb");
        assert_eq!(cfg.default_embedding_model, "BGESmallZHV15");
        assert!(cfg.recursive_scan);
        assert!(!cfg.auto_sync_on_start);
        assert!(!cfg.watch_files);
    }
}
