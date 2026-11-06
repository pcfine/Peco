//! daemon 的统一错误类型、稳定 code 与 HTTP 状态。
//!
//! 每个变体对应一个字符串 code：调用方（HTTP 层、CLI、测试）只依赖 code
//! 判断类别，文案随时可改。响应体形状不在这里定义 —— HTTP 层是唯一拼装点。

use axum::http::StatusCode;

/// 空间生命周期的所有失败面。
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("team '{team_id}' has no directory under teams/")]
    TeamMissing {
        team_id: String,
        available: Vec<String>,
    },

    #[error(
        "work_dir '{work_dir}' is bound to team '{current_team}', not '{requested_team}'; \
         rebind explicitly with PATCH /api/spaces/{{ws_id}}"
    )]
    TeamConflict {
        work_dir: String,
        current_team: String,
        requested_team: String,
    },

    #[error(
        "work_dir '{work_dir}' records team path '{marker_team}', which is not the daemon \
         team root '{expected}'"
    )]
    TeamPathMismatch {
        work_dir: String,
        marker_team: String,
        expected: String,
    },

    #[error(
        "marker and DB disagree on ws_id for '{work_dir}': marker={marker_ws_id} db={db_ws_id}"
    )]
    MarkerDbConflict {
        marker_ws_id: String,
        db_ws_id: String,
        work_dir: String,
    },

    #[error("marker schema {schema} is newer than the supported schema")]
    MarkerSchemaUnsupported { schema: i64 },

    #[error("marker at '{path}' is unparsable: {preview}")]
    MarkerCorrupt { path: String, preview: String },

    #[error("marker write failed at '{path}'")]
    MarkerWriteFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("work_dir '{work_dir}' is not an absolute path")]
    WorkDirNotAbsolute { work_dir: String },

    #[error("work_dir '{work_dir}' is unusable: {reason}")]
    WorkDirUnusable { work_dir: String, reason: String },

    #[error("work_dir '{work_dir}' is gone but its space row still exists")]
    WorkDirMissing { work_dir: String },

    #[error("workspace '{ws_id}' is being opened or closed")]
    WsBusy { ws_id: String },

    #[error(
        "workspace '{ws_id}' has {inflight_dispatch} dispatch and {inflight_foreign} foreign tasks inflight"
    )]
    InflightTasks {
        ws_id: String,
        inflight_dispatch: i64,
        inflight_foreign: i64,
    },

    #[error("space row '{ws_id}' not found")]
    SpaceNotFound { ws_id: String },

    #[error("workspace '{ws_id}' is indexed but not open; open it before using its agents")]
    SpaceNotOpen { ws_id: String },

    #[error("space '{ws_id}' is reserved and cannot be deleted; close it instead")]
    ReservedSpace { ws_id: String },

    #[error("task '{task_id}' not found in the dispatch ledger")]
    TaskNotFound { task_id: String },

    #[error("team '{team_id}' already exists")]
    TeamExists { team_id: String },

    #[error("team '{team_id}' has neither a directory nor an index row")]
    TeamNotFound { team_id: String },

    #[error("invalid path '{path}': {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("{message}")]
    InvalidArgument { message: String },

    #[error("{message}")]
    Internal { message: String },

    #[error("another peco-daemon is running (pid {pid}, from '{pid_file}')")]
    AlreadyRunning { pid: u32, pid_file: String },

    #[error("workspace open failed: {0}")]
    Workspace(#[from] peco_core::workspace::WorkspaceError),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),
}

impl DaemonError {
    /// 稳定 code —— 调用方按它分类，不解析文案。
    pub fn code(&self) -> &'static str {
        match self {
            Self::TeamMissing { .. } => "team_missing",
            Self::TeamConflict { .. } => "team_conflict",
            Self::TeamPathMismatch { .. } => "team_path_mismatch",
            Self::MarkerDbConflict { .. } => "marker_db_conflict",
            Self::MarkerSchemaUnsupported { .. } => "marker_schema_unsupported",
            Self::MarkerCorrupt { .. } => "marker_corrupt",
            Self::MarkerWriteFailed { .. } => "marker_write_failed",
            Self::WorkDirNotAbsolute { .. } => "work_dir_not_absolute",
            Self::WorkDirUnusable { .. } => "work_dir_unusable",
            Self::WorkDirMissing { .. } => "work_dir_missing",
            Self::WsBusy { .. } => "ws_busy",
            Self::InflightTasks { .. } => "inflight_tasks",
            Self::SpaceNotFound { .. } => "space_not_found",
            Self::SpaceNotOpen { .. } => "space_not_open",
            Self::ReservedSpace { .. } => "reserved_space",
            Self::TaskNotFound { .. } => "task_not_found",
            Self::TeamExists { .. } => "team_exists",
            Self::TeamNotFound { .. } => "team_not_found",
            Self::InvalidPath { .. } => "invalid_path",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::Internal { .. } => "internal",
            Self::AlreadyRunning { .. } => "daemon_already_running",
            Self::Workspace(_) => "workspace_open_failed",
            Self::Db(_) => "db_error",
            Self::Io(_) => "io_error",
            Self::Config(_) => "config_error",
        }
    }

    /// 错误类别对应的 HTTP 状态；只看变体，不看文案。
    pub fn http_status(&self) -> StatusCode {
        match self {
            Self::SpaceNotFound { .. } => StatusCode::NOT_FOUND,
            Self::TaskNotFound { .. } => StatusCode::NOT_FOUND,
            Self::TeamNotFound { .. } => StatusCode::NOT_FOUND,
            Self::InvalidArgument { .. }
            | Self::InvalidPath { .. }
            | Self::ReservedSpace { .. }
            | Self::WorkDirNotAbsolute { .. }
            | Self::WorkDirUnusable { .. } => StatusCode::BAD_REQUEST,
            Self::TeamMissing { .. }
            | Self::TeamExists { .. }
            | Self::TeamConflict { .. }
            | Self::TeamPathMismatch { .. }
            | Self::MarkerDbConflict { .. }
            | Self::MarkerCorrupt { .. }
            | Self::MarkerSchemaUnsupported { .. }
            | Self::WorkDirMissing { .. }
            | Self::SpaceNotOpen { .. }
            | Self::WsBusy { .. }
            | Self::InflightTasks { .. } => StatusCode::CONFLICT,
            Self::MarkerWriteFailed { .. }
            | Self::AlreadyRunning { .. }
            | Self::Workspace(_)
            | Self::Db(_)
            | Self::Io(_)
            | Self::Config(_)
            | Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个变体的 `(code, HTTP 状态)` 都被钉住：code 是调用方唯一的分类依据，
    /// 状态是 UI 唯一的分支依据，改任一处都必须先改这张表。
    #[test]
    fn every_variant_has_a_stable_code_and_status() {
        let cases = vec![
            (
                DaemonError::TeamMissing {
                    team_id: "nope".into(),
                    available: vec![],
                },
                "team_missing",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::TeamExists {
                    team_id: "nope".into(),
                },
                "team_exists",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::TeamNotFound {
                    team_id: "ghost".into(),
                },
                "team_not_found",
                StatusCode::NOT_FOUND,
            ),
            (
                DaemonError::TeamConflict {
                    work_dir: "/w".into(),
                    current_team: "research".into(),
                    requested_team: "writing".into(),
                },
                "team_conflict",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::TeamPathMismatch {
                    work_dir: "/w".into(),
                    marker_team: "/srv/teams/other".into(),
                    expected: "/h/teams/research".into(),
                },
                "team_path_mismatch",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::MarkerDbConflict {
                    marker_ws_id: "ws_a".into(),
                    db_ws_id: "ws_b".into(),
                    work_dir: "/w".into(),
                },
                "marker_db_conflict",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::MarkerSchemaUnsupported { schema: 2 },
                "marker_schema_unsupported",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::MarkerCorrupt {
                    path: "/w/.workspace.json".into(),
                    preview: "{".into(),
                },
                "marker_corrupt",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::MarkerWriteFailed {
                    path: "/w/.workspace.json".into(),
                    source: std::io::Error::other("disk full"),
                },
                "marker_write_failed",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::WorkDirNotAbsolute {
                    work_dir: "rel/path".into(),
                },
                "work_dir_not_absolute",
                StatusCode::BAD_REQUEST,
            ),
            (
                DaemonError::WorkDirUnusable {
                    work_dir: "/w".into(),
                    reason: "read-only".into(),
                },
                "work_dir_unusable",
                StatusCode::BAD_REQUEST,
            ),
            (
                DaemonError::WorkDirMissing {
                    work_dir: "/w".into(),
                },
                "work_dir_missing",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::WsBusy {
                    ws_id: "ws_a".into(),
                },
                "ws_busy",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::InflightTasks {
                    ws_id: "ws_a".into(),
                    inflight_dispatch: 2,
                    inflight_foreign: 1,
                },
                "inflight_tasks",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::SpaceNotFound {
                    ws_id: "ws_a".into(),
                },
                "space_not_found",
                StatusCode::NOT_FOUND,
            ),
            (
                DaemonError::SpaceNotOpen {
                    ws_id: "ws_a".into(),
                },
                "space_not_open",
                StatusCode::CONFLICT,
            ),
            (
                DaemonError::ReservedSpace {
                    ws_id: "ws_peco".into(),
                },
                "reserved_space",
                StatusCode::BAD_REQUEST,
            ),
            (
                DaemonError::TaskNotFound {
                    task_id: "t_a".into(),
                },
                "task_not_found",
                StatusCode::NOT_FOUND,
            ),
            (
                DaemonError::InvalidPath {
                    path: "../etc/passwd".into(),
                    reason: "escapes the space work dir".into(),
                },
                "invalid_path",
                StatusCode::BAD_REQUEST,
            ),
            (
                DaemonError::InvalidArgument {
                    message: "bad".into(),
                },
                "invalid_argument",
                StatusCode::BAD_REQUEST,
            ),
            (
                DaemonError::Internal {
                    message: "boom".into(),
                },
                "internal",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::AlreadyRunning {
                    pid: 42,
                    pid_file: "/h/daemon.pid".into(),
                },
                "daemon_already_running",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::Workspace(peco_core::workspace::WorkspaceError::WorkspaceDir(
                    "nope".into(),
                )),
                "workspace_open_failed",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::Db(sqlx::Error::RowNotFound),
                "db_error",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::Io(std::io::Error::other("x")),
                "io_error",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DaemonError::Config("bad".into()),
                "config_error",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, code, status) in cases {
            assert_eq!(error.code(), code, "{} 的 code 变了", error);
            assert_eq!(error.http_status(), status, "{} 的 HTTP 状态变了", error);
            assert!(
                !error.to_string().is_empty(),
                "{} 必须有可读文案",
                error.code()
            );
        }
    }
}
