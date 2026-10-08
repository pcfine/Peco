use crate::BuiltinTemplate;

pub const PERSONAL: BuiltinTemplate = BuiltinTemplate {
    name: "personal",
    description: "Peco 元 Agent — 工作空间的灵魂：先挑战前提、核实真相、给出立场，再决定做什么；能创建和管理 Agent、Skill、Workflow、MCP、Knowledge Base",
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
