// ============================================================================
// Skill 工具 — ReadSkill, ListSkills, SaveSkill, DeleteSkill
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::SkillProvider;
use super::{StringError, ToolDyn, ToolError};

pub struct ReadSkill {
    skill_registry: Arc<crate::skills::SkillRegister>,
}

impl ReadSkill {
    pub fn new(skill_registry: Arc<crate::skills::SkillRegister>) -> Self {
        Self { skill_registry }
    }
}

impl ToolDyn for ReadSkill {
    fn name(&self) -> String {
        "read_skill".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_skill".to_string(),
            description: "Read the full description and instructions for a skill by its name. \
                Use this to get detailed information about what a skill does, what tools it is \
                allowed to use, and the complete procedure to follow."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the skill to read (e.g., 'code-review')."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct ReadSkillArgs {
                name: String,
            }

            let parsed: ReadSkillArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "skill name is required and cannot be empty".into(),
                ))));
            }

            let skill = self.skill_registry.activate(name).map_err(|e| {
                ToolError::ToolCallError(format!("failed to read skill '{name}': {e}").into())
            })?;

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
        })
    }
}

// ── ListSkills ──────────────────────────────────────────────────────────────

pub struct ListSkills {
    skill_registry: Arc<crate::skills::SkillRegister>,
}

impl ListSkills {
    pub fn new(skill_registry: Arc<crate::skills::SkillRegister>) -> Self {
        Self { skill_registry }
    }
}

impl ToolDyn for ListSkills {
    fn name(&self) -> String {
        "list_skills".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_skills".to_string(),
            description:
                "List all available skills in the workspace with their names and descriptions. \
                Use this to discover what skills exist before reading or modifying them."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = args;
            let metas = self.skill_registry.all_meta();
            let skills: Vec<_> = metas
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "description": m.description,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&skills)
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.to_string()))))
        })
    }
}

// ── SaveSkill ───────────────────────────────────────────────────────────────

pub struct SaveSkill {
    skill_provider: Arc<dyn SkillProvider>,
}

impl SaveSkill {
    pub fn new(skill_provider: Arc<dyn SkillProvider>) -> Self {
        Self { skill_provider }
    }
}

impl ToolDyn for SaveSkill {
    fn name(&self) -> String {
        "save_skill".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_skill".to_string(),
            description: "Create or update a skill by writing its SKILL.md file. \
                The content must be a complete SKILL.md file with YAML frontmatter \
                (name, description, allowed-tools) followed by Markdown instructions.\n\
                \n\
                Required frontmatter fields:\n\
                  name: unique skill identifier (lowercase, no spaces)\n\
                  description: what the skill does and when to use it\n\
                \n\
                Optional fields: allowed-tools, license, compatibility.\n\
                \n\
                Example:\n\
                ---\n\
                name: \"code-review\"\n\
                description: \"Review code changes for bugs and style issues\"\n\
                allowed-tools: [\"shell\", \"fetch\"]\n\
                ---\n\
                # Code Review Skill\n\
                1. Read the code changes...\n\
                \n\
                The skill becomes immediately available after creation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique skill name (lowercase letters, digits, hyphens). 1-64 chars."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete SKILL.md content: YAML frontmatter + Markdown body."
                    }
                },
                "required": ["name", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SaveSkillArgs {
                name: String,
                content: String,
            }

            let parsed: SaveSkillArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "skill name is required and cannot be empty".into(),
                ))));
            }
            if parsed.content.trim().is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "skill content is required and cannot be empty".into(),
                ))));
            }

            self.skill_provider
                .save_skill(name, &parsed.content)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save skill '{name}': {e}"
                    ))))
                })?;

            Ok(format!("Skill '{name}' saved successfully."))
        })
    }
}

// ── DeleteSkill ─────────────────────────────────────────────────────────────

pub struct DeleteSkill {
    skill_provider: Arc<dyn SkillProvider>,
}

impl DeleteSkill {
    pub fn new(skill_provider: Arc<dyn SkillProvider>) -> Self {
        Self { skill_provider }
    }
}

impl ToolDyn for DeleteSkill {
    fn name(&self) -> String {
        "delete_skill".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_skill".to_string(),
            description: "Delete a skill and its SKILL.md file. This is irreversible. \
                All files in the skill directory will be permanently removed. \
                Requires explicit confirmation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name to delete."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be explicitly set to true to confirm deletion."
                    }
                },
                "required": ["name", "confirm"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct DeleteSkillArgs {
                name: String,
                confirm: bool,
            }

            let parsed: DeleteSkillArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            if !parsed.confirm {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Deletion not confirmed. Set 'confirm' to true to proceed.".into(),
                ))));
            }

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "skill name is required and cannot be empty".into(),
                ))));
            }

            self.skill_provider.delete_skill(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to delete skill '{name}': {e}"
                ))))
            })?;

            Ok(format!("Skill '{name}' deleted successfully."))
        })
    }
}
