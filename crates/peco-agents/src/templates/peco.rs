use crate::BuiltinTemplate;

pub const PECO: BuiltinTemplate = BuiltinTemplate {
    name: "peco",
    description: "Peco 元 Agent — 工作空间的灵魂：先挑战前提、核实真相、给出立场，再决定做什么；能创建和管理 Agent、Skill、Workflow、MCP、Knowledge Base",
    files: &[
        (
            "agents/@master/agent.md",
            include_bytes!("../../templates/peco/agents/@master/agent.md"),
        ),
        (
            "agents/@memory/agent.md",
            include_bytes!("../../templates/peco/agents/@memory/agent.md"),
        ),
        (
            "knowledge/@private_memory/kb_config.json",
            include_bytes!("../../templates/peco/knowledge/@private_memory/kb_config.json"),
        ),
    ],
};
