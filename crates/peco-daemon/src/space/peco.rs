//! Peco 默认空间的身份：`ws_id` 由记录文件派生，常量只在记录缺失时兜底。
//!
//! 台账把「Peco 自己发起的任务」与「其它空间发起的任务」分成两组计数，两个
//! 计数口径必须看同一个 `ws_id`。这里在启动时解析一次，之后经
//! `SpaceEnv::peco` 传递 —— 让每个调用点各自读盘会给出两份答案。

use std::path::{Path, PathBuf};

use peco_core::workspace::manifest;
use tracing::warn;

/// Peco 空间所属团队，其能力根是 `$PECO_HOME/teams/peco`。
pub const PECO_TEAM_ID: &str = "peco";

/// 缺记录文件时写进 `session_id` 的种子；`ws_id` 由它派生。
pub const PECO_SESSION_SEED: &str = "peco";

/// [`PECO_SESSION_SEED`] 派生出的 `ws_id`；只在记录缺失或损坏时兜底。
pub const PECO_WS_ID_SEED: &str = "ws_peco";

/// Peco 空间的身份与落点。
#[derive(Debug, Clone)]
pub struct PecoIdentity {
    pub ws_id: String,
    pub work_dir: PathBuf,
    pub team_id: String,
    /// `true` 表示 `ws_id` 来自记录文件，`false` 表示用了种子。
    pub from_marker: bool,
}

impl PecoIdentity {
    /// 该 `ws_id` 是不是 Peco 自己的空间。
    ///
    /// 「保留空间不可删」与「清单只注入 Peco 空间」共用这一处判定：两边各写一份
    /// 字面量比较，改一处就会漏另一处。
    pub fn is_peco_space(&self, ws_id: &str) -> bool {
        ws_id == self.ws_id
    }
}

/// 解析 Peco 的身份；任何读取失败都退回种子，不 panic。
///
/// 坏记录只 warn：Peco 空间坏了不该拦住 daemon —— 台账分类退化成一个稳定值，
/// 修好记录再重启即恢复。记录派生值与种子不同时同样 warn，但以派生值为准，
/// 否则用户手改过 `session_id` 的空间会在重启后换上另一个身份。
pub fn resolve(home: &Path) -> PecoIdentity {
    let work_dir = home.join("spaces").join(PECO_TEAM_ID).join("work");
    let (ws_id, from_marker) = match manifest::read(&work_dir) {
        Ok(Some(value)) => {
            let derived = manifest::ws_id_from_session(&value.session_id);
            if derived != PECO_WS_ID_SEED {
                warn!(
                    ws_id = %derived,
                    seed = PECO_WS_ID_SEED,
                    "peco workspace id drifted from its seed; using the recorded value"
                );
            }
            (derived, true)
        }
        Ok(None) => (PECO_WS_ID_SEED.to_string(), false),
        Err(e) => {
            warn!(
                work_dir = %work_dir.display(),
                error = %e,
                "peco workspace marker unusable; falling back to the seed id"
            );
            (PECO_WS_ID_SEED.to_string(), false)
        }
    };

    PecoIdentity {
        ws_id,
        work_dir,
        team_id: PECO_TEAM_ID.to_string(),
        from_marker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_marker(home: &Path, body: &str) -> PathBuf {
        let dir = home.join("spaces").join(PECO_TEAM_ID).join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(manifest::MANIFEST_FILE), body).unwrap();
        dir
    }

    fn marker_json(team: &Path, session_id: &str) -> String {
        serde_json::json!({
            "format": "peco-workspace",
            "schema": 1,
            "name": "peco",
            "description": "",
            "team": team.to_string_lossy(),
            "session_id": session_id,
            "created_at": "2026-02-11T09:14:22.531Z",
        })
        .to_string()
    }

    #[test]
    fn seed_ws_id_is_derived_from_the_seed_session() {
        assert_eq!(
            manifest::ws_id_from_session(PECO_SESSION_SEED),
            PECO_WS_ID_SEED
        );
    }

    #[test]
    fn a_missing_marker_falls_back_to_the_seed() {
        let home = tempfile::tempdir().unwrap();
        let identity = resolve(home.path());

        assert_eq!(identity.ws_id, PECO_WS_ID_SEED);
        assert!(!identity.from_marker);
        assert_eq!(identity.team_id, PECO_TEAM_ID);
        assert_eq!(
            identity.work_dir,
            home.path().join("spaces").join("peco").join("work")
        );
    }

    #[test]
    fn a_recorded_session_drives_the_ws_id() {
        let home = tempfile::tempdir().unwrap();
        let team = home.path().join("teams").join(PECO_TEAM_ID);
        write_marker(home.path(), &marker_json(&team, "abc"));

        let identity = resolve(home.path());
        assert_eq!(identity.ws_id, "ws_abc");
        assert!(identity.from_marker, "身份必须来自记录文件");
    }

    #[test]
    fn a_corrupt_marker_falls_back_to_the_seed_without_panicking() {
        let home = tempfile::tempdir().unwrap();
        write_marker(home.path(), "{\"format\": \"peco-workspace\", ");

        let identity = resolve(home.path());
        assert_eq!(identity.ws_id, PECO_WS_ID_SEED);
        assert!(!identity.from_marker);
    }
}
