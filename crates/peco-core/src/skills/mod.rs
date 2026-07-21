//! Skill loading system — standards-based Skill discovery, parsing, and lifecycle.
//!
//! # Overview
//!
//! This module implements the three-tier progressive disclosure model for AI agent Skills:
//!
//! | Tier | Loaded at       | Content                          | Token cost         |
//! |------|-----------------|----------------------------------|--------------------|
//! | 1    | Startup         | `name` + `description`           | ~100 tokens/Skill  |
//! | 2    | On activation   | Full SKILL.md body               | ~3000 tokens/Skill |
//! | 3    | On reference    | scripts / references / assets    | On demand          |
//!
//! # Quick start
//!
//! ```no_run
//! use peco_core::skills::SkillRegister;
//!
//! # fn example() -> Result<(), peco_core::skills::SkillError> {
//! let mut list = SkillRegister::new("./skills");
//! list.init()?;
//!
//! for meta in list.all_meta() {
//!     println!("[{}] {}", meta.name, meta.description);
//! }
//!
//! let skill = list.activate("code-review")?;
//! println!("Skill body: {} chars", skill.body.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Skill directory structure
//!
//! ```text
//! <skill-name>/
//! ├── SKILL.md              # Required: YAML frontmatter + Markdown body
//! ├── scripts/              # Optional: executable scripts
//! ├── references/           # Optional: reference documents
//! └── assets/               # Optional: templates, images, etc.
//! ```
//!
//! # SKILL.md format
//!
//! ```markdown
//! ---
//! name: my-skill
//! description: |
//!   What this skill does.
//!   Use when the user asks for X.
//! allowed-tools:
//!   - Read
//!   - Bash
//! ---
//!
//! # My Skill
//!
//! ## Procedure
//! 1. Step one
//! 2. Step two
//! ```

pub mod config;
pub mod error;
pub mod skill_register;
pub mod loader;

// Re-export the main public types for convenience
pub use config::{Skill, SkillFrontmatter, SkillMeta, validate_name};
pub use error::SkillError;
pub use skill_register::{SkillRegister, SkillRegisterStats};
pub use loader::SkillLoader;

/// Alias for [`SkillRegister`] — legacy name kept for compatibility.
pub type SkillRegistry = SkillRegister;
