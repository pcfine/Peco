//! workspace 的执行工作目录 —— shell 命令的默认 cwd。
//!
//! 位置由调用方给出；`output/`、`tmp/` 是其下的约定子路径，按需自行创建。

use std::path::{Path, PathBuf};

pub const WORK_DIR_ENV: &str = "PECO_WORK_DIR";
/// 产物默认落点，skill 应写 `$PECO_OUT_DIR/<skill>/<YYYY-MM-DD>/`。
pub const OUT_DIR_ENV: &str = "PECO_OUT_DIR";
/// 临时文件目录，随时可清。
pub const TMP_DIR_ENV: &str = "PECO_TMP_DIR";

/// 一个 workspace 的执行工作目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDir {
    root: PathBuf,
    output: PathBuf,
    tmp: PathBuf,
}

impl WorkDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            output: root.join("output"),
            tmp: root.join("tmp"),
            root,
        }
    }

    /// 幂等。
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    /// 注入子进程的环境变量集合。
    ///
    /// 返回数组而非直接写 `Command`，因为 `std::process::Command` 与
    /// `tokio::process::Command` 都不能被同一个 trait 覆盖。
    pub fn envs(&self) -> [(&'static str, &Path); 3] {
        [
            (WORK_DIR_ENV, &self.root),
            (OUT_DIR_ENV, &self.output),
            (TMP_DIR_ENV, &self.tmp),
        ]
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    pub fn tmp(&self) -> &Path {
        &self.tmp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_joins_subdirs_under_root() {
        let wd = WorkDir::new("/data/work/u1");
        assert_eq!(wd.root(), Path::new("/data/work/u1"));
        assert_eq!(wd.output(), Path::new("/data/work/u1/output"));
        assert_eq!(wd.tmp(), Path::new("/data/work/u1/tmp"));
    }

    #[test]
    fn ensure_creates_only_the_root() {
        let tmp = std::env::temp_dir().join(format!("peco-workdir-{}", std::process::id()));
        let wd = WorkDir::new(tmp.join("u1"));
        wd.ensure().unwrap();
        assert!(wd.root().is_dir());
        assert!(!wd.output().exists());
        assert!(!wd.tmp().exists());
        // 幂等
        wd.ensure().unwrap();
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn envs_cover_all_three_paths() {
        let wd = WorkDir::new("/data/work/u1");
        let envs = wd.envs();
        assert_eq!(envs.len(), 3);
        let names: Vec<_> = envs.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, vec![WORK_DIR_ENV, OUT_DIR_ENV, TMP_DIR_ENV]);
        assert_eq!(envs[1].1, Path::new("/data/work/u1/output"));
    }
}
