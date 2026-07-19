// ============================================================================
// ReadSkill — 读取 Skill 完整内容（依赖注入版）
// ============================================================================

use std::pin::Pin;
use std::sync::{Arc, RwLock};

use futures::Future;
use model_provider::ToolDefinition;
use serde_json::json;

use crate::skills::SkillRegistry;
use crate::workspace::SkillProvider;

use super::{StringError, ToolDyn, ToolError};

pub struct ReadSkill {
    skill_registry: Arc<RwLock<SkillRegistry>>,
}

impl ReadSkill {
    pub fn new(skill_provider: Arc<dyn SkillProvider>) -> Self {
        Self {
            skill_registry: skill_provider.skill_registry().clone(),
        }
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

            let mut registry = self.skill_registry.write().map_err(|e| {
                ToolError::ToolCallError(
                    format!("failed to acquire skill registry lock: {e}").into(),
                )
            })?;

            let skill = registry.activate(&parsed.name).map_err(|e| {
                ToolError::ToolCallError(
                    format!("failed to read skill '{}': {e}", parsed.name).into(),
                )
            })?;

            let mut output = String::new();
            output.push_str(&format!("# Skill: {}\n\n", skill.frontmatter.name));
            output.push_str(&format!("**Description**: {}\n\n", skill.frontmatter.description));
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
