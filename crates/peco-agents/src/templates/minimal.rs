use crate::BuiltinTemplate;

pub const MINIMAL: BuiltinTemplate = BuiltinTemplate {
    name: "minimal",
    description: "轻量对话 — 最简 agent，无知识库",
    files: &[(
        "agents/basic-chat/agent.md",
        include_bytes!("../../templates/minimal/agents/basic-chat/agent.md"),
    )],
};
