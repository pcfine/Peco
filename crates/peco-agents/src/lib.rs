//! Peco 内置 Workspace 模板数据提供者。
//!
//! 本 crate 仅提供编译时嵌入的模板文件数据。
//! 安装逻辑位于 `peco-core` 的 [`WorkSpace::init_from_template()`] 中。
//!
//! # 依赖方向
//!
//! ```text
//! peco-daemon → peco-agents → （无 peco-core 依赖）
//! ```
//!
//! peco-core 不依赖本 crate，保持零模板假设。

mod templates;

use std::io;

/// 所有内置模板的静态数组。
static ALL_TEMPLATES: [BuiltinTemplate; 2] =
    [templates::peco::PECO, templates::team_default::TEAM_DEFAULT];

/// 内置 Workspace 模板。
///
/// 模板是一个包含 `agents/` 和 `knowledge/` 子目录的文件集合。
/// 调用 [`materialize()`](Self::materialize) 将文件写入临时目录，
/// 然后将临时目录路径传递给 `WorkSpace::init_from_template()`。
#[derive(Clone, Copy)]
pub struct BuiltinTemplate {
    pub name: &'static str,
    pub description: &'static str,
    /// 模板文件：`(相对路径, 文件字节内容)`
    pub files: &'static [(&'static str, &'static [u8])],
}

impl BuiltinTemplate {
    /// 将内置模板解压到临时目录，返回目录句柄（drop 时自动清理）。
    pub fn materialize(&self) -> Result<tempfile::TempDir, io::Error> {
        let dir = tempfile::tempdir()?;
        for (rel_path, content) in self.files {
            let dest = dir.path().join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, content)?;
        }
        Ok(dir)
    }

    /// 按名称查找模板。
    pub fn by_name(name: &str) -> Option<&'static Self> {
        Self::all().iter().find(|t| t.name == name)
    }

    /// 所有可用模板。
    pub fn all() -> &'static [Self] {
        &ALL_TEMPLATES
    }

    /// peco 模板：Peco 元 Agent + 记忆管理。
    pub fn peco() -> Self {
        templates::peco::PECO
    }

    /// team_default 模板：通用专家团队（@master + 私有记忆库）。
    pub fn team_default() -> Self {
        templates::team_default::TEAM_DEFAULT
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn test_all_templates_are_listed() {
        let listed: BTreeSet<&str> = BuiltinTemplate::all().iter().map(|t| t.name).collect();
        let expected: BTreeSet<&str> = ["peco", "team_default"].into_iter().collect();
        assert_eq!(
            listed, expected,
            "内置模板清单漂移：`all()` 与 `by_name()` 是同一份数据，漏列即按名取不到"
        );
    }

    #[test]
    fn test_by_name_found() {
        let t = BuiltinTemplate::by_name("peco");
        assert!(t.is_some());
        assert_eq!(t.unwrap().name, "peco");
    }

    #[test]
    fn test_by_name_not_found() {
        assert!(BuiltinTemplate::by_name("nonexistent").is_none());
    }

    #[test]
    fn test_materialize_peco() {
        let t = BuiltinTemplate::peco();
        let tmp = t.materialize().unwrap();

        let agent_mds: Vec<_> = t
            .files
            .iter()
            .filter(|(path, _)| path.starts_with("agents/") && path.ends_with("/agent.md"))
            .collect();
        assert_eq!(agent_mds.len(), 2);
        assert!(
            agent_mds
                .iter()
                .all(|(path, _)| tmp.path().join(path).exists()),
            "所有模板 agent.md 都必须落盘"
        );
    }

    /// 记忆库由宿主在打开空间时按空间创建，模板里再铺一份团队级
    /// `@private_memory` 就等于把记忆重新变成团队共享。
    #[test]
    fn templates_ship_no_knowledge_config() {
        for template in BuiltinTemplate::all() {
            let knowledge_configs: Vec<&str> = template
                .files
                .iter()
                .map(|(path, _)| *path)
                .filter(|path| path.starts_with("knowledge/"))
                .collect();
            assert!(
                knowledge_configs.is_empty(),
                "模板 {} 携带了知识库配置: {knowledge_configs:?}",
                template.name
            );
        }
    }
}
