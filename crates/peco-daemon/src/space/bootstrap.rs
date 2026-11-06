//! Peco 默认空间的幂等引导：`teams/peco/` 与 `spaces/peco/work`。
//!
//! 每一步各自收敛错误并带步骤名 warn：Peco 空间坏了不该拦住其它空间启动，
//! 而日志里必须能看出是哪一步坏了。
//!
//! 记录与知识文件「只补不改」—— 用户改过的内容在重启后必须保持原字节。
//! `agents/*/agent.md` 是例外：提示词是产品契约的一部分，模型按它写下的字段名读
//! 唤醒消息，陈旧副本会让模型去找早已不存在的字段（唤醒文本因此看起来「缺字段」），
//! 故按 `template_version` 升版覆盖，旧内容先备份到 `agent.md.bak.<旧版本>`。

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

/// 一次模板同步的结果 —— 日志字段与用例判据共用同一份口径。
#[derive(Debug)]
struct TemplateSync {
    written: usize,
    /// 因模板版本更高而被覆盖的 `agent.md` 数；其余（版本不高于现存的）计入
    /// `skipped`。
    updated: usize,
    skipped: usize,
}

/// 铺 `peco` 模板：缺的文件补上，`agents/*/agent.md` 按版本闸门覆盖。
fn install_team_template(env: &SpaceEnv) -> Result<(), DaemonError> {
    let root = env.team_root(PECO_TEAM_ID);
    // 模板文件清单只在 peco-agents 里定义一次：先整份落到临时目录，再按目录
    // 结构逐个比对，模板新增文件时这里不需要跟着改。
    let staged = BuiltinTemplate::peco().materialize()?;
    let sync = sync_template(staged.path(), &root)?;
    if sync.updated > 0 {
        info!(
            updated = sync.updated,
            skipped = sync.skipped,
            "peco team template upgraded"
        );
    } else if sync.written > 0 {
        info!(written = sync.written, "peco team template installed");
    }
    Ok(())
}

/// 补缺 + 升版；纯路径入参，用例可以直接对临时目录跑一遍。
fn sync_template(staged: &Path, root: &Path) -> Result<TemplateSync, DaemonError> {
    let written = copy_missing_files(staged, root)?;
    let (updated, skipped) = upgrade_agent_prompts(staged, root)?;
    Ok(TemplateSync {
        written,
        updated,
        skipped,
    })
}

/// `agents/*/agent.md` 的版本闸门：模板版本高于现存版本时先备份、再覆盖。
///
/// 逐个 Agent 比各自的版本，而不是看 `@master` 一个就决定全部：模板里各 Agent
/// 的版本各自演进，一个全局开关会把版本没变的那个的本地内容也顶掉。
///
/// 缺失的文件不在这里补（那是 [`copy_missing_files`] 的事）：没有现存内容就没有
/// 要备份的旧文本，也无从升版。
///
/// 返回 `(覆盖数, 跳过数)`。
fn upgrade_agent_prompts(staged: &Path, root: &Path) -> Result<(usize, usize), DaemonError> {
    let (mut updated, mut skipped) = (0, 0);
    let agents_dir = staged.join("agents");
    if !agents_dir.is_dir() {
        return Ok((updated, skipped));
    }
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let rel = Path::new("agents").join(entry.file_name()).join("agent.md");
        let (src_md, dst_md) = (staged.join(&rel), root.join(&rel));
        if !src_md.is_file() || !dst_md.is_file() {
            continue;
        }
        let existing = team::agent_md_template_version(&dst_md);
        if team::agent_md_template_version(&src_md) <= existing {
            skipped += 1;
            continue;
        }
        // 先备份再覆盖：备份写不下去就停在这里，宁可这一轮不升级，也不能让用户
        // 的旧提示词无迹可寻。
        std::fs::copy(&dst_md, dst_md.with_extension(format!("md.bak.{existing}")))?;
        std::fs::copy(&src_md, &dst_md)?;
        updated += 1;
    }
    Ok((updated, skipped))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_template() -> tempfile::TempDir {
        BuiltinTemplate::peco()
            .materialize()
            .expect("内置模板必须能落盘")
    }

    /// `agents/*/` 下的备份文件（`<agent>/<文件名>`）：升版闸门是否真的动过文件，
    /// 只有它和字节比对两条判据。
    fn backups_under(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let Ok(agents) = std::fs::read_dir(root.join("agents")) else {
            return found;
        };
        for agent in agents.flatten() {
            let dir = agent.path();
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for file in files.flatten() {
                let file_name = file.file_name().to_string_lossy().to_string();
                if file_name.contains(".bak.") {
                    found.push(format!("{name}/{file_name}"));
                }
            }
        }
        found
    }

    /// 版本没涨就不该动字节：否则用户在本机追加的提示词每次重启都消失。
    #[test]
    fn a_prompt_at_the_template_version_keeps_its_bytes() {
        let staged = staged_template();
        let root = tempfile::tempdir().unwrap();
        sync_template(staged.path(), root.path()).unwrap();

        let master = root.path().join(team::MASTER_AGENT_REL);
        let mut edited = std::fs::read_to_string(&master).unwrap();
        edited.push_str("\n<!-- 本机追加 -->\n");
        std::fs::write(&master, &edited).unwrap();

        let sync = sync_template(staged.path(), root.path()).unwrap();

        assert_eq!(sync.updated, 0, "版本不高于现存，一律不动");
        assert_eq!(
            std::fs::read_to_string(&master).unwrap(),
            edited,
            "版本相同的提示词被覆盖了"
        );
        assert!(
            backups_under(root.path()).is_empty(),
            "没覆盖就不该留备份: {:?}",
            backups_under(root.path())
        );
    }

    /// 版本更高就升版：覆盖成模板内容，同时旧文本必须能从备份里原样找回。
    #[test]
    fn a_newer_template_overwrites_after_backing_up_the_old_prompt() {
        let staged = staged_template();
        let root = tempfile::tempdir().unwrap();
        sync_template(staged.path(), root.path()).unwrap();

        let master = root.path().join(team::MASTER_AGENT_REL);
        let old = "---\nagent:\n  name: \"@master\"\n  description: \"旧版\"\n\
                   template_version: 12\n---\n\n# 旧版提示词\n";
        std::fs::write(&master, old).unwrap();
        assert_eq!(
            team::agent_md_template_version(&master),
            12,
            "夹具必须是比模板旧的版本"
        );

        let sync = sync_template(staged.path(), root.path()).unwrap();

        assert_eq!(sync.updated, 1, "只有版本落后的那个 Agent 该被覆盖");
        assert_eq!(
            std::fs::read_to_string(&master).unwrap(),
            std::fs::read_to_string(staged.path().join(team::MASTER_AGENT_REL)).unwrap(),
            "覆盖后必须是模板内容"
        );
        let backup = root.path().join("agents/@master/agent.md.bak.12");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            old,
            "旧文本必须能原样找回"
        );
    }

    /// 全新空间：整份铺开，且不留备份 —— 没有旧内容可备份。
    #[test]
    fn a_missing_template_installs_every_file_without_backups() {
        let staged = staged_template();
        let root = tempfile::tempdir().unwrap();

        let sync = sync_template(staged.path(), root.path()).unwrap();

        assert_eq!(sync.updated, 0, "全新安装没有可升级的对象");
        assert_eq!(sync.written, BuiltinTemplate::peco().files.len());
        for &(rel, content) in BuiltinTemplate::peco().files {
            assert_eq!(
                &std::fs::read(root.path().join(rel)).unwrap(),
                content,
                "{rel} 的内容与模板不一致"
            );
        }
        assert!(
            backups_under(root.path()).is_empty(),
            "全新安装不该留备份: {:?}",
            backups_under(root.path())
        );
    }
}
