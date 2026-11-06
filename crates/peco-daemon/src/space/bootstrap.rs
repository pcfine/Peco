//! Peco 默认空间的幂等引导：`teams/peco/` 与 `spaces/peco/work`。
//!
//! 每一步各自收敛错误并带步骤名 warn：Peco 空间坏了不该拦住其它空间启动，
//! 而日志里必须能看出是哪一步坏了。全程「只补不改」—— 用户改过的模板文件与
//! 记录文件在重启后必须保持原字节。

use std::path::Path;

use peco_agents::BuiltinTemplate;
use tracing::{info, warn};

use crate::db::teams::{self, NewTeam};
use crate::error::DaemonError;
use crate::space::open::{ensure_open_indexed, resolve_ws_id};
use crate::space::peco::{self, PECO_TEAM_ID};
use crate::space::{SpaceEnv, manifest};
use crate::team;

/// Peco 空间的人读名称。
const PECO_DISPLAY_NAME: &str = "Peco";

/// 该空间要完成的事；Peco 是常驻总管，没有一次性目标。
const PECO_DESCRIPTION: &str = "Peco 总管默认空间";

/// 幂等安装 Peco 的团队与默认空间，并在启动时打开一次。
///
/// 失败只 warn：调用方（启动链路）不因 Peco 不可用而拒绝服务其它空间。
pub async fn bootstrap_peco(env: &SpaceEnv) {
    if let Err(e) = install_team_template(env) {
        warn!(step = "install peco team template", code = e.code(), error = %e, "peco bootstrap step failed");
    }
    if let Err(e) = sync_team_index(env).await {
        warn!(step = "index peco team", code = e.code(), error = %e, "peco bootstrap step failed");
    }
    if let Err(e) = write_default_marker(env) {
        warn!(step = "write peco workspace marker", code = e.code(), error = %e, "peco bootstrap step failed");
    }
    if let Err(e) = open_default_space(env).await {
        warn!(step = "open peco workspace", code = e.code(), error = %e, "peco bootstrap step failed");
    }
}

/// 缺 `@master` 时铺 `peco` 模板；已有则整步跳过。
fn install_team_template(env: &SpaceEnv) -> Result<(), DaemonError> {
    let root = env.team_root(PECO_TEAM_ID);
    if root.join(team::MASTER_AGENT_REL).is_file() {
        return Ok(());
    }
    // 模板文件清单只在 peco-agents 里定义一次：先整份落到临时目录，再按目录
    // 结构逐个补缺，模板新增文件时这里不需要跟着改。
    let staged = BuiltinTemplate::peco().materialize()?;
    let written = copy_missing_files(staged.path(), &root)?;
    info!(written, "peco team template installed");
    Ok(())
}

/// 逐个文件「目标不存在才写」，返回写入的文件数。
fn copy_missing_files(src: &Path, dest_root: &Path) -> Result<usize, DaemonError> {
    let mut written = 0;
    let mut pending = vec![src.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let rel = path.strip_prefix(src).map_err(|e| DaemonError::Internal {
                message: format!("template file escaped its staging dir: {e}"),
            })?;
            let dest = dest_root.join(rel);
            if dest.exists() {
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &dest)?;
            written += 1;
        }
    }
    Ok(written)
}

/// 补 `team.json` 与 `teams` 索引行；已有 `team.json` 保留原字节。
async fn sync_team_index(env: &SpaceEnv) -> Result<(), DaemonError> {
    let root = env.team_root(PECO_TEAM_ID);
    if !root.is_dir() {
        return Err(DaemonError::TeamMissing {
            team_id: PECO_TEAM_ID.to_string(),
            available: env.available_teams(),
        });
    }
    // 展示名与模板版本的读入面，缺了它下一次普查会按目录名兜底。
    if !root.join("team.json").is_file() {
        let version = team::template_version_on_disk(&root).unwrap_or(0);
        team::write_team_json(&root, PECO_TEAM_ID, PECO_DISPLAY_NAME, version)?;
    }
    let (display_name, template_version) = teams::meta_from_dir(&root, PECO_TEAM_ID);
    teams::upsert(
        &env.pool,
        &NewTeam {
            id: PECO_TEAM_ID,
            display_name: &display_name,
            template_version,
        },
    )
    .await?;
    Ok(())
}

/// 缺记录文件时写一份：团队路径必须是 canonical 形式，否则打开时判
/// `team_path_mismatch`。
fn write_default_marker(env: &SpaceEnv) -> Result<(), DaemonError> {
    let work_dir = env.peco.work_dir.clone();
    std::fs::create_dir_all(&work_dir)?;
    if manifest::read(&work_dir)?.is_some() {
        return Ok(());
    }
    let team_root = std::fs::canonicalize(env.team_root(PECO_TEAM_ID))?;
    let mut value = peco_core::workspace::manifest::new_manifest(
        PECO_DISPLAY_NAME,
        PECO_DESCRIPTION,
        &team_root,
    );
    // 会话 id 是 `ws_id` 的派生种子：种子与其它空间的随机 id 不同，Peco 才能
    // 靠常量在台账与清单里被认出来。
    value.session_id = peco::PECO_SESSION_SEED.to_string();
    if manifest::write_exclusive(&work_dir, &value)? {
        info!(work_dir = %work_dir.display(), "peco workspace marker written");
    }
    Ok(())
}

/// 补 DB 行并走正常打开路径 —— Peco 空间与其它空间共用同一套决策表。
async fn open_default_space(env: &SpaceEnv) -> Result<(), DaemonError> {
    let hub = env.hub().ok_or_else(|| DaemonError::Internal {
        message: "workspace hub is not attached; skipping peco open".to_string(),
    })?;
    let ws_id = resolve_ws_id(env, &env.peco.work_dir, PECO_TEAM_ID).await?;
    ensure_open_indexed(env, &hub, &ws_id).await?;
    info!(ws_id = %ws_id, "peco workspace open");
    Ok(())
}
