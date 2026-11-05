use crate::BuiltinTemplate;

pub const TEAM_DEFAULT: BuiltinTemplate = BuiltinTemplate {
    name: "team_default",
    description: "通用专家团队模板：含 @master 与私有记忆库，可被多个工作空间共享",
    files: &[
        (
            "agents/@master/agent.md",
            include_bytes!("../../templates/team_default/agents/@master/agent.md"),
        ),
        (
            "knowledge/@private_memory/kb_config.json",
            include_bytes!("../../templates/team_default/knowledge/@private_memory/kb_config.json"),
        ),
    ],
};
