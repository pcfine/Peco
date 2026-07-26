use crate::BuiltinTemplate;

pub const PERSONAL: BuiltinTemplate = BuiltinTemplate {
    name: "personal",
    description: "个人 AI 助手 — 含记忆管理和私人知识库",
    files: &[
        (
            "agents/@assistant/agent.md",
            include_bytes!("../../templates/personal/agents/@assistant/agent.md"),
        ),
        (
            "agents/@memory/agent.md",
            include_bytes!("../../templates/personal/agents/@memory/agent.md"),
        ),
        (
            "knowledge/@private_memory/kb_config.json",
            include_bytes!("../../templates/personal/knowledge/@private_memory/kb_config.json"),
        ),
    ],
};
