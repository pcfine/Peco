use crate::BuiltinTemplate;

pub const DEVELOPER: BuiltinTemplate = BuiltinTemplate {
    name: "developer",
    description: "开发辅助 — 含编码助手、记忆管理和项目文档知识库",
    files: &[
        (
            "agents/coding-assistant/agent.md",
            include_bytes!("../../templates/developer/agents/coding-assistant/agent.md"),
        ),
        (
            "agents/memory/agent.md",
            include_bytes!("../../templates/developer/agents/memory/agent.md"),
        ),
        (
            "knowledge/project_docs/kb_config.json",
            include_bytes!("../../templates/developer/knowledge/project_docs/kb_config.json"),
        ),
    ],
};
