// ============================================================================
// .workspace.json — 工作空间记录文件的唯一实现
// ============================================================================
//
// 一个工作空间目录里的 `.workspace.json` 只记录「它属于哪个团队、叫什么、
// 要做什么、用哪个会话、何时创建」。`work_dir` 不入文件 —— 它就是这个文件
// 所在目录，写进去只会在目录被移动后变成第二份会漂移的事实。
//
// 本模块是 README/daemon/server/工具共用的唯一读写点：任何一方自己再解析
// JSON 都会让字段语义分叉。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 记录文件名。
pub const MANIFEST_FILE: &str = ".workspace.json";
/// 判别串：与历史上任何同名 marker（尤其是 daemon 旧 `ws_id/user_id` 形态）区分。
pub const MANIFEST_FORMAT: &str = "peco-workspace";
/// 当前格式版本；更高版本由读取方拒绝，绝不猜语义。
pub const MANIFEST_SCHEMA: u32 = 1;

/// 工作空间记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    /// 固定为 [`MANIFEST_FORMAT`]。
    pub format: String,
    pub schema: u32,
    /// 人读名称。
    pub name: String,
    /// 这个工作空间要完成的具体任务描述。
    pub description: String,
    /// 团队能力根的绝对路径（含 agents/skills/knowledge/workflows）。
    pub team: String,
    /// 工作空间会话 id；daemon 侧也以它作为 `ws_id` 的稳定种子。
    pub session_id: String,
    /// ISO 8601 / RFC 3339 时刻。
    pub created_at: String,
}

/// marker 读写的错误。
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("workspace manifest at '{path}' is corrupt: {reason}")]
    Corrupt { path: String, reason: String },
    #[error("workspace manifest at '{path}' has unknown format '{format}'")]
    Foreign { path: String, format: String },
    #[error("workspace manifest at '{path}' uses unsupported schema {schema}")]
    SchemaUnsupported { path: String, schema: u32 },
    #[error("workspace manifest already exists at '{path}'")]
    AlreadyExists { path: String },
    #[error("workspace manifest io error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

fn io_error(path: &Path, source: std::io::Error) -> ManifestError {
    ManifestError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// marker 的绝对落点。
pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILE)
}

/// 读取 marker。文件不存在返回 `Ok(None)`；坏 JSON / 外来格式 / 高版本都报错。
///
/// 「文件不存在」与「文件不是本格式」刻意分开：前者由调用方决定是补写还是
/// 拒绝，后者必须暴露给用户，否则把别的工具写的文件当成自己的会静默覆盖。
pub fn read(dir: &Path) -> Result<Option<WorkspaceManifest>, ManifestError> {
    let path = manifest_path(dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(&path, e)),
    };

    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| ManifestError::Corrupt {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

    let format = value
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if format != MANIFEST_FORMAT {
        return Err(ManifestError::Foreign {
            path: path.display().to_string(),
            format: format.to_string(),
        });
    }

    let schema = value
        .get("schema")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    if schema != MANIFEST_SCHEMA as u64 {
        return Err(ManifestError::SchemaUnsupported {
            path: path.display().to_string(),
            schema: u32::try_from(schema).unwrap_or(u32::MAX),
        });
    }

    serde_json::from_value(value)
        .map(Some)
        .map_err(|e| ManifestError::Corrupt {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
}

/// 新建一份记录：生成 `session_id` 与 `created_at`。
pub fn new_manifest(name: &str, description: &str, team: &Path) -> WorkspaceManifest {
    WorkspaceManifest {
        format: MANIFEST_FORMAT.to_string(),
        schema: MANIFEST_SCHEMA,
        name: name.to_string(),
        description: description.to_string(),
        team: team.to_string_lossy().into_owned(),
        session_id: new_session_id(),
        created_at: now_rfc3339(),
    }
}

/// 原子覆盖写：同目录 tmp → `fsync` → `rename`。
///
/// 掉电最坏留下「没有 marker」，不会留下半个 JSON；失败路径删除 tmp。
pub fn write(dir: &Path, manifest: &WorkspaceManifest) -> Result<(), ManifestError> {
    let path = manifest_path(dir);
    let tmp = tmp_path(dir);
    write_tmp(&tmp, manifest)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error(&path, e));
    }
    Ok(())
}

/// 只建不改：目标已存在时返回 [`ManifestError::AlreadyExists`]。
///
/// `rename` 会覆盖目标，故独占语义必须用 `hard_link`（目标存在即失败）。
pub fn write_exclusive(dir: &Path, manifest: &WorkspaceManifest) -> Result<(), ManifestError> {
    let path = manifest_path(dir);
    let tmp = tmp_path(dir);
    write_tmp(&tmp, manifest)?;
    match std::fs::hard_link(&tmp, &path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Err(ManifestError::AlreadyExists {
                    path: path.display().to_string(),
                })
            } else {
                Err(io_error(&path, e))
            }
        }
    }
}

fn write_tmp(tmp: &Path, manifest: &WorkspaceManifest) -> Result<(), ManifestError> {
    let body = serde_json::to_vec_pretty(manifest).map_err(|e| ManifestError::Corrupt {
        path: tmp.display().to_string(),
        reason: e.to_string(),
    })?;
    let mut file = std::fs::File::create(tmp).map_err(|e| io_error(tmp, e))?;
    file.write_all(&body).map_err(|e| io_error(tmp, e))?;
    file.write_all(b"\n").map_err(|e| io_error(tmp, e))?;
    file.sync_all().map_err(|e| io_error(tmp, e))?;
    Ok(())
}

fn tmp_path(dir: &Path) -> PathBuf {
    dir.join(format!(
        "{MANIFEST_FILE}.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

/// 新工作空间 id，`ws_` 前缀让日志与台账里一眼可辨。
pub fn new_ws_id() -> String {
    ws_id_from_session(&new_session_id())
}

/// 新会话 id（uuid simple）。
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 由会话 id 派生稳定的 `ws_id`：marker 存活而 DB 行丢失时身份不漂移。
pub fn ws_id_from_session(session_id: &str) -> String {
    format!("ws_{}", session_id.trim())
}

/// 当前时刻（毫秒精度 RFC 3339）。
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WorkspaceManifest {
        new_manifest("demo", "把迁移做完", Path::new("/srv/teams/research"))
    }

    #[test]
    fn write_then_read_roundtrips_and_hides_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = sample();

        write(dir.path(), &manifest).unwrap();

        assert_eq!(read(dir.path()).unwrap(), Some(manifest));
        let raw = std::fs::read_to_string(manifest_path(dir.path())).unwrap();
        assert!(!raw.contains("work_dir"), "work_dir 不入文件: {raw}");
        assert!(raw.contains(MANIFEST_FORMAT));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件: {leftovers:?}");
    }

    #[test]
    fn a_missing_manifest_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), None);
    }

    #[test]
    fn a_foreign_marker_is_rejected_instead_of_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            r#"{"schema":1,"ws_id":"ws_1","team":"research","session_id":"s"}"#,
        )
        .unwrap();

        let err = read(dir.path()).unwrap_err();
        assert!(matches!(err, ManifestError::Foreign { .. }), "{err}");
    }

    #[test]
    fn a_newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            format!(r#"{{"format":"{MANIFEST_FORMAT}","schema":2,"name":"n","description":"d","team":"/t","session_id":"s","created_at":"now"}}"#),
        )
        .unwrap();

        let err = read(dir.path()).unwrap_err();
        assert!(
            matches!(err, ManifestError::SchemaUnsupported { schema: 2, .. }),
            "{err}"
        );
    }

    #[test]
    fn broken_json_and_missing_fields_are_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(manifest_path(dir.path()), "{ not json").unwrap();
        assert!(matches!(
            read(dir.path()).unwrap_err(),
            ManifestError::Corrupt { .. }
        ));

        std::fs::write(
            manifest_path(dir.path()),
            format!(r#"{{"format":"{MANIFEST_FORMAT}","schema":1}}"#),
        )
        .unwrap();
        assert!(matches!(
            read(dir.path()).unwrap_err(),
            ManifestError::Corrupt { .. }
        ));
    }

    #[test]
    fn exclusive_write_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = sample();
        write_exclusive(dir.path(), &first).unwrap();
        let before = std::fs::read(manifest_path(dir.path())).unwrap();

        let mut second = sample();
        second.name = "another".to_string();
        let err = write_exclusive(dir.path(), &second).unwrap_err();

        assert!(matches!(err, ManifestError::AlreadyExists { .. }), "{err}");
        assert_eq!(
            std::fs::read(manifest_path(dir.path())).unwrap(),
            before,
            "独占写失败后原文件必须原字节不变"
        );
    }

    #[test]
    fn the_same_session_derives_the_same_ws_id() {
        assert_eq!(ws_id_from_session("abc"), "ws_abc");
        assert_eq!(ws_id_from_session("abc"), ws_id_from_session("abc"));
        assert_ne!(ws_id_from_session("abc"), ws_id_from_session("def"));
        assert!(new_ws_id().starts_with("ws_"));
    }
}
