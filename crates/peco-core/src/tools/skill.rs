use super::ToolError;
use peco_derive::peco_tool;

/// Read Skill description tool.
///
/// Reads the full SKILL.md content for a registered Skill from the
/// [`GlobalSkillList`](crate::skills::GlobalSkillList). If the Skill has not
/// been activated yet (Tier 2), this tool activates it automatically.
///
/// The tool returns the complete frontmatter metadata and markdown body
/// — suitable for a model to understand what the Skill does and how to
/// carry it out.
#[peco_tool(
    name = "read_skill",
    description = "Read the full description and instructions for a skill by its name. Use this to get detailed information about what a skill does, what tools it is allowed to use, and the complete procedure to follow. Skill names must match registered skills (lowercase letters, digits, hyphens).",
    params(
        name = "The name of the skill to read (e.g., 'code-review', 'pdf-form-filler'). Must be a valid skill name that was registered during system initialization."
    )
)]
pub async fn read_skill(name: String) -> Result<String, ToolError> {
    let handler = crate::GlobalHandler::global();
    let mut registry = handler.skill_list().write().map_err(|e| {
        ToolError::ToolCallError(format!("failed to acquire skill registry lock: {e}").into())
    })?;

    let skill = registry.activate(&name).map_err(|e| {
        ToolError::ToolCallError(format!("failed to read skill '{name}': {e}").into())
    })?;

    // ── Format output: frontmatter metadata + full body ─────────────────
    let mut output = String::new();
    output.push_str(&format!("# Skill: {}\n\n", skill.frontmatter.name));
    output.push_str(&format!(
        "**Description**: {}\n\n",
        skill.frontmatter.description
    ));
    output.push_str(&format!(
        "**Allowed Tools**: {}\n",
        skill.frontmatter.allowed_tools.join(", ")
    ));
    if let Some(ref license) = skill.frontmatter.license {
        output.push_str(&format!("**License**: {license}\n"));
    }
    if let Some(ref compat) = skill.frontmatter.compatibility {
        output.push_str(&format!("**Compatibility**: {compat}\n"));
    }
    output.push_str("\n---\n\n");
    output.push_str(&skill.body);

    Ok(output)
}
