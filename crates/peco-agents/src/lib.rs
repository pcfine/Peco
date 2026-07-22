//! Peco 内置 Workspace 模板数据提供者。
//!
//! 本 crate 仅提供编译时嵌入的模板文件数据。
//! 安装逻辑位于 `peco-core` 的 [`WorkSpace::init_from_template()`] 中。
//!
//! # 依赖方向
//!
//! ```text
//! peco-cli / peco-server → peco-agents → （无 peco-core 依赖）
//! ```
//!
//! peco-core 不依赖本 crate，保持零模板假设。

mod templates;

use std::io;

/// 所有内置模板的静态数组。
static ALL_TEMPLATES: [BuiltinTemplate; 3] = [
    templates::personal::PERSONAL,
    templates::minimal::MINIMAL,
    templates::developer::DEVELOPER,
];

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

    /// personal 模板：个人 AI 助手。
    pub fn personal() -> Self {
        templates::personal::PERSONAL
    }

    /// minimal 模板：最轻量对话。
    pub fn minimal() -> Self {
        templates::minimal::MINIMAL
    }

    /// developer 模板：开发辅助。
    pub fn developer() -> Self {
        templates::developer::DEVELOPER
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_returns_three_templates() {
        assert_eq!(BuiltinTemplate::all().len(), 3);
    }

    #[test]
    fn test_by_name_found() {
        let t = BuiltinTemplate::by_name("personal");
        assert!(t.is_some());
        assert_eq!(t.unwrap().name, "personal");
    }

    #[test]
    fn test_by_name_not_found() {
        assert!(BuiltinTemplate::by_name("nonexistent").is_none());
    }

    #[test]
    fn test_materialize_personal() {
        let t = BuiltinTemplate::personal().clone();
        let tmp = t.materialize().unwrap();
        assert!(tmp.path().join("agents/@assistant/agent.md").exists());
        assert!(tmp.path().join("agents/@memory/agent.md").exists());
        assert!(
            tmp.path()
                .join("knowledge/@private_memory/kb_config.json")
                .exists()
        );
    }

    #[test]
    fn test_materialize_minimal() {
        let t = BuiltinTemplate::minimal().clone();
        let tmp = t.materialize().unwrap();
        assert!(tmp.path().join("agents/basic-chat/agent.md").exists());
    }

    #[test]
    fn test_materialize_developer() {
        let t = BuiltinTemplate::developer().clone();
        let tmp = t.materialize().unwrap();
        assert!(
            tmp.path()
                .join("agents/@coding-assistant/agent.md")
                .exists()
        );
        assert!(tmp.path().join("agents/@memory/agent.md").exists());
        assert!(
            tmp.path()
                .join("knowledge/@project_docs/kb_config.json")
                .exists()
        );
    }
}
