use crate::BuiltinTemplate;

pub const TEAM_DEFAULT: BuiltinTemplate = BuiltinTemplate {
    name: "team_default",
    description: "通用专家团队模板：含 @master，可被多个工作空间共享",
    files: &[(
        "agents/@master/agent.md",
        include_bytes!("../../templates/team_default/agents/@master/agent.md"),
    )],
};
