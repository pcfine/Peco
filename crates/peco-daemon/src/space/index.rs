//! 磁盘 → DB 的 agent 索引补齐。
//!
//! `agents/<name>/agent.md` 是唯一真相源，`agents` 表只是可重建的索引。
//! 由此得到两条必须成立的性质：磁盘上出现而库里没有的 agent 要被补进来
//! （否则模板安装、手工拷贝、外部编辑器新建的 agent 在 `GET /api/agents`
//! 里永远不可见），库里存在而磁盘上已经消失的行不能被当作真实资源。
//!
//! 只补不删：目录暂时读不到（挂载点抖动、权限变化）与「agent 被删掉」在
//! 磁盘上长得一样，删行会连带丢掉 UI 元数据；已消失的项由视图侧过滤。

use std::path::Path;

use peco_core::agent::agent_config;
use peco_core::workspace::WorkSpace;
use sqlx::SqlitePool;
use tracing::{debug, info, warn};

use crate::db::agents;
use crate::error::DaemonError;

/// agent 定义相对 `agents/<name>/` 的文件名。
const AGENT_MD: &str = "agent.md";

/// 把 `agents/` 下每个含 `agent.md` 的子目录补进 DB 索引。
///
/// 返回本次新增的行数：0 表示索引已经与磁盘一致（幂等）。索引行的 `name`
/// 取目录名而非 frontmatter 里的 `agent.name` —— 目录即寻址，`md_path` 与
/// 唯一性校验都用它。
///
/// `team_id` 必须由调用方给出：`agents_dir()` 指向的就是团队能力根，而同名
/// agent 在两个团队是不同的索引行。
pub async fn ensure_agents_indexed(
    pool: &SqlitePool,
    ws: &WorkSpace,
    team_id: &str,
) -> Result<usize, DaemonError> {
    let dir = ws.agents_dir();
    let mut inserted = 0usize;
    for (name, description) in scan_agents(&dir) {
        inserted += agents::insert_if_absent(pool, team_id, &name, &description).await? as usize;
    }
    if inserted > 0 {
        info!(
            agents_dir = %dir.display(),
            inserted,
            "unindexed agents found on disk; indexed them"
        );
    }
    Ok(inserted)
}

/// 扫 `<dir>/<name>/agent.md`，返回 `(目录名, frontmatter 描述)`。
///
/// 解析失败只降级描述，不跳过整条记录：frontmatter 写坏了仍然是用户想要的
/// agent，让它消失会让人以为文件被吞了。
///
/// 参数是 `agents/` 目录而非空间实例：团队页要在没有任何空间打开的情况下
/// 回答「这个团队装了什么」，两处各扫一遍会让描述与过滤条件漂移。
pub(crate) fn scan_agents(dir: &Path) -> Vec<(String, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!(agents_dir = %dir.display(), error = %e, "cannot list agents dir");
            return Vec::new();
        }
    };

    let mut found: Vec<(String, String)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join(AGENT_MD).is_file())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let description = description_of(&entry.path().join(AGENT_MD), &name);
            (name, description)
        })
        .collect();
    found.sort();
    found
}

fn description_of(md_path: &Path, name: &str) -> String {
    let content = match std::fs::read_to_string(md_path) {
        Ok(content) => content,
        Err(e) => {
            warn!(agent = name, error = %e, "cannot read agent.md; indexing an empty description");
            return String::new();
        }
    };
    match agent_config::parse_agent_md(&content) {
        Ok((profile, _body)) => profile.agent.description,
        Err(e) => {
            debug!(agent = name, error = %e, "agent.md frontmatter unparsable; indexing an empty description");
            String::new()
        }
    }
}
