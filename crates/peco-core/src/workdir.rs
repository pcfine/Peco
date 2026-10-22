//! 工作层目录（`work/`）——与 workspace 分离的可重建产出区。
//!
//! # 为什么要分家
//!
//! `<data_dir>/workspaces/<user_id>` 是**定义与记忆**（agents / skills / workflows /
//! knowledge），需要打包迁移；而 skill 与 agent 产出的图片、视频、报告是**可重建的
//! 一次性数据**，一次会话就能产出数 MB。两者混在一起会让迁移包体积失控。
//!
//! # 目录布局
//!
//! ```text
//! <data_dir>/
//! ├─ workspaces/<user_id>/     ← 定义层：迁移对象
//! └─ work/<user_id>/           ← 工作层：可弃
//!    ├─ output/                ← 产物，默认落点
//!    ├─ projects/              ← 长期工程
//!    └─ tmp/                   ← 临时文件，随时可清
//! ```
//!
//! 布局镜像 `workspaces/<user_id>` 的命名，因此不需要任何映射表：
//! `PECO_DATA_DIR` 整体换根时两个目录自动一起走。
//!
//! # 唯一推导来源
//!
//! 所有路径由 workspace 根推导，**不接受调用方各自拼**。推导要求父目录名恰为
//! `workspaces`；不满足时返回 `None`（例如单元测试的临时目录、`examples/`），
//! 此时不注入任何环境变量，行为与引入本模块之前逐字节一致。

use std::path::{Path, PathBuf};

/// workspace 根目录（定义层）。
pub const WORKSPACE_ENV: &str = "PECO_WORKSPACE";
/// 工作层根目录：`<data_dir>/work/<user_id>`。
pub const WORK_DIR_ENV: &str = "PECO_WORK_DIR";
/// 产物默认落点，skill 应写 `$PECO_OUT_DIR/<skill>/<YYYY-MM-DD>/`。
pub const OUT_DIR_ENV: &str = "PECO_OUT_DIR";
/// 长期工程目录。
pub const PROJECTS_DIR_ENV: &str = "PECO_PROJECTS_DIR";
/// 临时文件目录，随时可清。
pub const TMP_DIR_ENV: &str = "PECO_TMP_DIR";

/// workspace 根的直接父目录名 —— 推导锚点。
const WORKSPACES_DIR: &str = "workspaces";
/// 数据根下的工作层目录名。
const WORK_DIR: &str = "work";

/// 一个用户的工作层目录集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDirs {
    workspace: PathBuf,
    root: PathBuf,
    output: PathBuf,
    projects: PathBuf,
    tmp: PathBuf,
}

impl WorkDirs {
    /// 从 workspace 根推导工作层目录。
    ///
    /// 仅当路径形如 `<data_dir>/workspaces/<user_id>` 时成立；否则返回 `None`，
    /// 调用方据此跳过注入（fail-safe：宁可不给，也不写到猜出来的位置）。
    pub fn from_workspace_root(workspace: &Path) -> Option<Self> {
        let user_id = workspace.file_name()?;
        let workspaces_dir = workspace.parent()?;
        if workspaces_dir.file_name()? != WORKSPACES_DIR {
            return None;
        }
        let data_dir = workspaces_dir.parent()?;
        let root = data_dir.join(WORK_DIR).join(user_id);
        Some(Self {
            output: root.join("output"),
            projects: root.join("projects"),
            tmp: root.join("tmp"),
            root,
            workspace: workspace.to_path_buf(),
        })
    }

    /// 创建工作层目录。幂等。
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.root, &self.output, &self.projects, &self.tmp] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// 注入子进程的环境变量集合。
    ///
    /// 返回数组而非直接写 `Command`，因为 `std::process::Command` 与
    /// `tokio::process::Command` 都不能被同一个 trait 覆盖。
    pub fn envs(&self) -> [(&'static str, &Path); 5] {
        [
            (WORKSPACE_ENV, &self.workspace),
            (WORK_DIR_ENV, &self.root),
            (OUT_DIR_ENV, &self.output),
            (PROJECTS_DIR_ENV, &self.projects),
            (TMP_DIR_ENV, &self.tmp),
        ]
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    pub fn projects(&self) -> &Path {
        &self.projects
    }

    pub fn tmp(&self) -> &Path {
        &self.tmp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_from_standard_layout() {
        let wd = WorkDirs::from_workspace_root(Path::new("/data/workspaces/u1")).unwrap();
        assert_eq!(wd.workspace(), Path::new("/data/workspaces/u1"));
        assert_eq!(wd.root(), Path::new("/data/work/u1"));
        assert_eq!(wd.output(), Path::new("/data/work/u1/output"));
        assert_eq!(wd.projects(), Path::new("/data/work/u1/projects"));
        assert_eq!(wd.tmp(), Path::new("/data/work/u1/tmp"));
    }

    #[test]
    fn rejects_non_standard_layout() {
        // 测试临时目录、examples/ 下的路径都不该被当作 workspace
        assert!(WorkDirs::from_workspace_root(Path::new("/tmp/pytest-123/ws")).is_none());
        assert!(WorkDirs::from_workspace_root(Path::new("relative/ws")).is_none());
        assert!(WorkDirs::from_workspace_root(Path::new("/")).is_none());
        // 父目录名不是 workspaces
        assert!(WorkDirs::from_workspace_root(Path::new("/data/other/u1")).is_none());
    }

    #[test]
    fn ensure_creates_all_dirs() {
        let tmp = std::env::temp_dir().join(format!("peco-workdir-{}", std::process::id()));
        let ws = tmp.join("workspaces").join("u1");
        std::fs::create_dir_all(&ws).unwrap();
        let wd = WorkDirs::from_workspace_root(&ws).unwrap();
        wd.ensure().unwrap();
        for dir in [&wd.root, &wd.output, &wd.projects, &wd.tmp] {
            assert!(dir.is_dir(), "{} should exist", dir.display());
        }
        // 幂等
        wd.ensure().unwrap();
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn envs_cover_all_five_paths() {
        let wd = WorkDirs::from_workspace_root(Path::new("/data/workspaces/u1")).unwrap();
        let envs = wd.envs();
        assert_eq!(envs.len(), 5);
        let names: Vec<_> = envs.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            names,
            vec![
                WORKSPACE_ENV,
                WORK_DIR_ENV,
                OUT_DIR_ENV,
                PROJECTS_DIR_ENV,
                TMP_DIR_ENV
            ]
        );
        assert_eq!(envs[2].1, Path::new("/data/work/u1/output"));
    }
}
