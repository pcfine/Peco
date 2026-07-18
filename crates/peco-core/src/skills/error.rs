//! Error types for the Skill loading system.
//!
//! Following the project's error-handling strategy: individual Skill load
//! failures should not block startup — the skill list logs warnings and continues.

use std::path::PathBuf;

/// Errors that can occur during Skill discovery, parsing, and lifecycle management.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// The skills root directory does not exist or cannot be read.
    #[error("skills root directory not found or not readable: {0}")]
    RootNotFound(PathBuf),

    /// Failed to read a directory entry during discovery.
    #[error("failed to read skills directory at {path}: {source}")]
    ReadDir {
        /// The directory being scanned.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A skill directory does not contain a SKILL.md file.
    #[error("SKILL.md not found in directory: {0}")]
    SkillMdNotFound(PathBuf),

    /// Failed to read a SKILL.md file.
    #[error("failed to read SKILL.md at {path}: {source}")]
    Io {
        /// Path to the SKILL.md file.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The YAML frontmatter is missing or cannot be parsed.
    #[error("invalid frontmatter in {path}: {reason}")]
    InvalidFrontmatter {
        /// Path to the SKILL.md file.
        path: PathBuf,
        /// Human-readable reason for the parse failure.
        reason: String,
    },

    /// The `name` field in frontmatter violates naming rules.
    ///
    /// Valid names: lowercase letters, digits, hyphens; 1–64 chars;
    /// must not start/end with hyphen; no consecutive hyphens.
    #[error("invalid skill name '{name}': {reason}")]
    InvalidName {
        /// The invalid name value.
        name: String,
        /// Human-readable reason.
        reason: String,
    },

    /// The directory name does not match the `name` field in frontmatter.
    #[error("directory name '{dir}' does not match skill name '{name}' declared in SKILL.md")]
    NameMismatch {
        /// The filesystem directory name.
        dir: String,
        /// The `name` value from SKILL.md frontmatter.
        name: String,
    },

    /// Attempted to activate a Skill that was not discovered during init.
    #[error("skill '{0}' is not registered — run init() first")]
    NotRegistered(String),

    /// A Skill is already in the activated map.
    #[error("skill '{0}' is already activated")]
    AlreadyActivated(String),
}
