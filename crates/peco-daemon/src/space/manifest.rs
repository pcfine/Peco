//! daemon 侧对 core `.workspace.json` 的薄适配。
//!
//! 记录格式只有一个实现（`peco_core::workspace::manifest`）；本文件只做
//! 「daemon 需要的派生字段」：`ws_id` 由 `session_id` 稳定派生，`team_id`
//! 由团队路径的末段派生。

use std::path::Path;

use peco_core::workspace::manifest::{self, ManifestError, WorkspaceManifest};

use crate::error::DaemonError;

pub use peco_core::workspace::manifest::MANIFEST_FILE;

/// 读记录；文件不存在返回 `None`。
pub fn read(dir: &Path) -> Result<Option<WorkspaceManifest>, DaemonError> {
    manifest::read(dir).map_err(map_error)
}

/// 原子覆盖写。
pub fn write(dir: &Path, value: &WorkspaceManifest) -> Result<(), DaemonError> {
    manifest::write(dir, value).map_err(map_error)
}

/// 只建不改：目标已存在时返回 `false`。
///
/// 引导流程据此保持幂等：并发两次启动只有一次真正写盘，已有的记录字节不变。
pub fn write_exclusive(dir: &Path, value: &WorkspaceManifest) -> Result<bool, DaemonError> {
    match manifest::write_exclusive(dir, value) {
        Ok(()) => Ok(true),
        Err(ManifestError::AlreadyExists { .. }) => Ok(false),
        Err(err) => Err(map_error(err)),
    }
}

/// 由会话 id 稳定派生 `ws_id`：marker 存活而 DB 行丢失时身份不漂移。
pub fn ws_id_of(value: &WorkspaceManifest) -> String {
    manifest::ws_id_from_session(&value.session_id)
}

/// 从 `ws_<session>` 取回会话种子。
pub fn session_of(ws_id: &str) -> &str {
    ws_id.strip_prefix("ws_").unwrap_or(ws_id)
}

/// 团队路径的末段即 `team_id`（daemon 的团队都在 `$PECO_HOME/teams/<id>`）。
pub fn team_id_of(value: &WorkspaceManifest) -> Result<String, DaemonError> {
    Path::new(&value.team)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| DaemonError::InvalidArgument {
            message: format!(
                "workspace manifest team path has no usable file name: {}",
                value.team
            ),
        })
}

/// 从 DB 行补写记录：`session_id` 取自 `ws_id`，保证之后可重建同一个身份。
///
/// 并发下若别人先写了记录，读回对方的值而不是报错 —— 同一目录只有一份记录。
pub fn ensure_for(
    dir: &Path,
    ws_id: &str,
    team_root: &Path,
    name: &str,
) -> Result<WorkspaceManifest, DaemonError> {
    let mut value = manifest::new_manifest(name, "", team_root);
    value.session_id = session_of(ws_id).to_string();

    match manifest::write_exclusive(dir, &value) {
        Ok(()) => Ok(value),
        Err(ManifestError::AlreadyExists { .. }) => {
            read(dir)?.ok_or_else(|| DaemonError::Internal {
                message: format!(
                    "workspace manifest vanished after a concurrent create at '{}'",
                    dir.display()
                ),
            })
        }
        Err(err) => Err(map_error(err)),
    }
}

fn map_error(err: ManifestError) -> DaemonError {
    match err {
        ManifestError::Corrupt { path, reason } => DaemonError::MarkerCorrupt {
            path,
            preview: reason,
        },
        ManifestError::Foreign { path, format } => DaemonError::MarkerCorrupt {
            path,
            preview: format!("foreign manifest format '{format}'"),
        },
        ManifestError::SchemaUnsupported { schema, .. } => DaemonError::MarkerSchemaUnsupported {
            schema: i64::from(schema),
        },
        ManifestError::AlreadyExists { path } => DaemonError::Internal {
            message: format!("workspace manifest already exists at '{path}'"),
        },
        ManifestError::Io { source, .. } => DaemonError::Io(source),
    }
}
