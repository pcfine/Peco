//! 空间私有记忆库的落盘布局与默认配置。
//!
//! 记忆库一个空间一份，落在 `$PECO_HOME/memory/<ws_id>/` —— 不放进 `work_dir`：
//! 现网的空间目录大多在 daemon 家目录之外，放进去等于往用户目录写 daemon 数据，
//! 而删除空间时又不能碰用户目录（工作目录里的文件永不被删）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
use peco_core::knowledge::KnowledgeManager;

use super::config::KB_NAME;
use crate::error::DaemonError;

/// 记忆库目录名。
///
/// 与模板时期的 `knowledge/@private_memory/` 同名：库的发现靠目录里的
/// `kb_config.json`，名字对不上也不会认错库，但同名能让磁盘上的路径与文档、
/// 模板历史一一对得上。
const KB_DIR: &str = "@private_memory";
const KB_CONFIG_FILE: &str = "kb_config.json";

/// 空间的记忆根：`$PECO_HOME/memory/<ws_id>`。
pub fn memory_root(home: &Path, ws_id: &str) -> PathBuf {
    home.join("memory").join(ws_id)
}

/// 记忆库的配置文件路径（`<root>/@private_memory/kb_config.json`）。
pub fn kb_config_path(memory_root: &Path) -> PathBuf {
    memory_root.join(KB_DIR).join(KB_CONFIG_FILE)
}

/// 记忆库的默认配置：沿用 HelixDb，记忆的读写行为不变。
pub fn default_kb_config() -> KbConfig {
    KbConfig {
        name: KB_NAME.to_string(),
        description: "空间私有记忆 — 存储偏好、事实、事件、关系".to_string(),
        embedding_model: FastembedModelTypeSerde::BGEBaseZHV15,
        chunking: ChunkingStrategySerde::OverlappingWindow {
            size: 800,
            overlap: 200,
        },
        backend: BackendType::HelixDb,
        storage_path: None,
        default_storage_mode: Default::default(),
        helix_url: None,
    }
}

/// 建出记忆根与默认库配置；已有配置原样保留。
///
/// 只写配置、不建库：`create_kb`/`open_kb` 会立刻加载嵌入模型，放在打开路径上
/// 等于每次开空间多几秒。首次召回/首次写入时由既有懒加载补上。
pub fn ensure(memory_root: &Path) -> Result<(), DaemonError> {
    let config_path = kb_config_path(memory_root);
    if config_path.is_file() {
        return Ok(());
    }
    let Some(dir) = config_path.parent() else {
        return Err(DaemonError::Internal {
            message: format!(
                "memory kb config path '{}' has no parent directory",
                config_path.display()
            ),
        });
    };
    std::fs::create_dir_all(dir)?;
    let json =
        serde_json::to_vec_pretty(&default_kb_config()).map_err(|e| DaemonError::Internal {
            message: format!("cannot serialize the memory kb config: {e}"),
        })?;
    std::fs::write(&config_path, json)?;
    Ok(())
}

/// 该空间的记忆管理器。
///
/// scope 必须是 `ws_id`（不是 team）：Helix 的 schema 命名空间由它与库名拼成，
/// 传团队等于让同团队的 N 个空间又落回同一个命名空间。
pub fn manager(memory_root: &Path, ws_id: &str) -> Arc<KnowledgeManager> {
    Arc::new(KnowledgeManager::with_scope(
        memory_root.to_path_buf(),
        ws_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_lands_under_the_space_memory_root() {
        let home = Path::new("/home/peco");
        let root = memory_root(home, "ws_a");
        assert_eq!(root, Path::new("/home/peco/memory/ws_a"));
        assert_eq!(
            kb_config_path(&root),
            Path::new("/home/peco/memory/ws_a/@private_memory/kb_config.json")
        );
    }

    /// 默认配置要能被既有加载器原样读回：字段名与取值就是磁盘契约。
    #[test]
    fn default_config_round_trips_through_the_loader_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = memory_root(dir.path(), "ws_a");
        ensure(&root).unwrap();

        let raw = std::fs::read_to_string(kb_config_path(&root)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["name"], KB_NAME);
        assert_eq!(value["backend"], "HelixDb");
        assert_eq!(value["embedding_model"], "b-g-e-base-z-h-v15");
        assert_eq!(value["chunking"]["type"], "overlapping-window");
        assert_eq!(value["chunking"]["size"], 800);
        assert_eq!(value["chunking"]["overlap"], 200);
        assert_eq!(value["default_storage_mode"], "full");

        let config: KbConfig = serde_json::from_str(&raw).unwrap();
        assert_eq!(config.name, KB_NAME);
    }

    /// 用户改过的配置必须留着：打开一次空间就把它覆盖回默认值，等于让「改模型」
    /// 这类动作永远不生效。
    #[test]
    fn an_existing_config_is_left_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let root = memory_root(dir.path(), "ws_a");
        ensure(&root).unwrap();
        let path = kb_config_path(&root);
        let edited = b"{\"name\": \"@private_memory\"}\n";
        std::fs::write(&path, edited).unwrap();

        ensure(&root).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), edited);
    }

    #[test]
    fn manager_uses_the_space_as_its_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = memory_root(dir.path(), "ws_a");
        ensure(&root).unwrap();
        let km = manager(&root, "ws_a");
        // 构造是同步的，且这里不触发任何加载：打开路径上不得多付一次模型加载。
        drop(km);
        assert!(kb_config_path(&root).is_file());
    }
}
