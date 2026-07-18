//! SKILL.md parser and core data structures.
//!
//! A SKILL.md file consists of two parts separated by `---` fences:
//!
//! 1. **YAML frontmatter** — typed metadata (`name`, `description`, etc.)
//! 2. **Markdown body** — instructions for the model to follow when the Skill is activated
//!
//! # Progressive disclosure tiers
//!
//! | Tier | Type        | Loaded at        | Content                              |
//! |------|-------------|------------------|--------------------------------------|
//! | 1    | `SkillMeta` | Startup          | `name` + `description` only          |
//! | 2    | `Skill`     | On activation    | Full frontmatter + Markdown body     |
//! | 3    | (files)     | On reference     | scripts / references / assets        |

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Required filename in every Skill directory.
pub const SKILL_MD_FILENAME: &str = "SKILL.md";

/// Maximum length of the `description` field (characters).
pub const MAX_DESCRIPTION_LEN: usize = 1024;

/// Minimum valid Skill name length.
pub const MIN_NAME_LEN: usize = 1;

/// Maximum valid Skill name length.
pub const MAX_NAME_LEN: usize = 64;

/// Subdirectory names that may contain Tier-3 resources.
pub const KNOWN_SUBDIRS: &[&str] = &["scripts", "references", "assets"];

// ── Frontmatter ──────────────────────────────────────────────────────────────

/// Parsed YAML frontmatter from the top of a SKILL.md file.
///
/// All fields except `name` and `description` are optional per the spec.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    /// Globally unique Skill identifier. Must match the directory name.
    pub name: String,

    /// What this Skill does and when to activate it (≤ 1024 chars).
    /// Should follow the "What + When" dual-sentence pattern.
    pub description: String,

    /// Tools the Skill is allowed to call. Host enforces this allowlist.
    #[serde(rename = "allowed-tools", default)]
    pub allowed_tools: Vec<String>,

    /// License identifier (e.g. "Apache-2.0", "MIT").
    #[serde(default)]
    pub license: Option<String>,

    /// Runtime / environment requirements (e.g. "Requires Python 3.10+").
    #[serde(default)]
    pub compatibility: Option<String>,

    /// Free-form extension metadata (version, stability, owner, tags, …).
    #[serde(default)]
    pub metadata: HashMap<String, serde_yaml::Value>,
}

// ── Tier 1: SkillMeta ────────────────────────────────────────────────────────

/// Lightweight metadata loaded at startup (Tier 1).
///
/// Only `name` and `description` are included so the model can decide
/// whether a Skill is relevant to the current task without consuming
/// the tokens required by the full Markdown body.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    /// Skill identifier (matches directory name).
    pub name: String,
    /// What + When description for LLM relevance matching.
    pub description: String,
    /// Path to the Skill directory on disk.
    pub path: PathBuf,
}

// ── Tier 2: Skill ────────────────────────────────────────────────────────────

/// A fully-loaded Skill (Tier 2).
///
/// Contains the complete frontmatter, the Markdown instruction body,
/// and paths for discovering Tier-3 resources (scripts, references, assets).
#[derive(Debug, Clone)]
pub struct Skill {
    /// Parsed YAML frontmatter with all metadata fields.
    pub frontmatter: SkillFrontmatter,
    /// Full Markdown body (instructions for the model).
    pub body: String,
    /// Absolute path to the SKILL.md file.
    pub skill_md_path: PathBuf,
    /// Absolute path to the Skill root directory.
    pub root_dir: PathBuf,
}

impl Skill {
    // ── Tier 3 helpers ───────────────────────────────────────────────────

    /// List all files under `scripts/` in this Skill directory.
    ///
    /// Returns an empty vec if the subdirectory does not exist.
    pub fn list_scripts(&self) -> Vec<PathBuf> {
        list_subdir_files(&self.root_dir, "scripts")
    }

    /// List all files under `references/` in this Skill directory.
    ///
    /// Returns an empty vec if the subdirectory does not exist.
    pub fn list_references(&self) -> Vec<PathBuf> {
        list_subdir_files(&self.root_dir, "references")
    }

    /// List all files under `assets/` in this Skill directory.
    ///
    /// Returns an empty vec if the subdirectory does not exist.
    pub fn list_assets(&self) -> Vec<PathBuf> {
        list_subdir_files(&self.root_dir, "assets")
    }

    /// Read the content of a resource file relative to the Skill root.
    ///
    /// This is the Tier-3 entry point: when the Skill body references
    /// a script or asset, call this to load it on demand.
    pub fn read_resource(&self, relative_path: &Path) -> Result<String, std::io::Error> {
        let full_path = self.root_dir.join(relative_path);
        std::fs::read_to_string(&full_path)
    }
}

// ── Name validation ──────────────────────────────────────────────────────────

/// Validate a Skill name against the spec:
///
/// - Only lowercase ASCII letters, digits, and hyphens
/// - 1–64 characters
/// - Must not start or end with a hyphen
/// - Must not contain consecutive hyphens (`--`)
pub fn validate_name(name: &str) -> Result<(), String> {
    // Length check
    if name.len() < MIN_NAME_LEN || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "name length must be between {} and {} characters, got {}",
            MIN_NAME_LEN,
            MAX_NAME_LEN,
            name.len()
        ));
    }

    // Character set + position rules
    let bytes = name.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let valid = match b {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'-' => i != 0 && i != bytes.len() - 1 && bytes.get(i.wrapping_sub(1)) != Some(&b'-'),
            _ => false,
        };
        if !valid {
            if b == b'-' {
                if i == 0 {
                    return Err(format!("name '{}' must not start with a hyphen", name));
                } else if i == bytes.len() - 1 {
                    return Err(format!("name '{}' must not end with a hyphen", name));
                } else {
                    return Err(format!(
                        "name '{}' must not contain consecutive hyphens",
                        name
                    ));
                }
            }
            return Err(format!(
                "name '{}' contains invalid character '{}' — only lowercase letters, digits, and hyphens are allowed",
                name, b as char
            ));
        }
    }

    Ok(())
}

/// Validate that the `description` field meets requirements.
pub fn validate_description(desc: &str) -> Result<(), String> {
    if desc.trim().is_empty() {
        return Err("description must not be empty".into());
    }
    if desc.len() > MAX_DESCRIPTION_LEN {
        return Err(format!(
            "description is {} characters (max {})",
            desc.len(),
            MAX_DESCRIPTION_LEN
        ));
    }
    Ok(())
}

// ── Frontmatter parsing ──────────────────────────────────────────────────────

/// Split a SKILL.md raw string into (frontmatter_str, body_str).
///
/// Delegates to the shared implementation in [`crate::agent::config::split_frontmatter`].
pub fn split_frontmatter(raw: &str) -> Result<(&str, &str), String> {
    crate::agent::split_frontmatter(raw)
        .map_err(|msg| msg.replace("file must start", "SKILL.md must start"))
}

/// Parse the full YAML frontmatter from a raw frontmatter string.
pub fn parse_frontmatter(yaml_str: &str) -> Result<SkillFrontmatter, String> {
    serde_yaml::from_str::<SkillFrontmatter>(yaml_str).map_err(|e| format!("YAML parse error: {e}"))
}

/// Parse only the `name` and `description` from frontmatter (Tier 1 optimisation).
///
/// Uses a minimal struct to avoid parsing fields that aren't needed at startup.
#[derive(Debug, Deserialize)]
struct MetaOnly {
    name: String,
    description: String,
}

/// Extract only `name` and `description` from a SKILL.md raw string.
///
/// This is faster than full parsing and avoids pulling in unused fields.
pub fn parse_meta_only(raw: &str) -> Result<(String, String), String> {
    let (frontmatter_str, _body) = split_frontmatter(raw)?;
    let meta: MetaOnly =
        serde_yaml::from_str(frontmatter_str).map_err(|e| format!("YAML parse error: {e}"))?;
    Ok((meta.name, meta.description))
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// List all files recursively under a subdirectory of the Skill root.
fn list_subdir_files(root: &Path, subdir: &str) -> Vec<PathBuf> {
    let dir = root.join(subdir);
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                // Return path relative to root for consistency
                if let Ok(rel) = path.strip_prefix(root) {
                    files.push(rel.to_path_buf());
                } else {
                    files.push(path);
                }
            }
        }
    }
    // Sort for deterministic output
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_valid_frontmatter() {
        let raw =
            "---\nname: test-skill\ndescription: A test skill\n---\n\n# Hello\n\nSome body text.";
        let (fm, body) = split_frontmatter(raw).unwrap();
        assert_eq!(fm, "name: test-skill\ndescription: A test skill");
        assert!(body.contains("# Hello"));
        assert!(!body.contains("---"));
    }

    #[test]
    fn test_split_missing_opening_delimiter() {
        let raw = "name: test\n---\nbody";
        assert!(split_frontmatter(raw).is_err());
    }

    #[test]
    fn test_split_missing_closing_delimiter() {
        let raw = "---\nname: test\nbody without closing";
        assert!(split_frontmatter(raw).is_err());
    }

    #[test]
    fn test_parse_meta_only() {
        let raw =
            "---\nname: my-skill\ndescription: Does stuff\nallowed-tools:\n  - Read\n---\n\n# Body";
        let (name, desc) = parse_meta_only(raw).unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "Does stuff");
    }

    #[test]
    fn test_validate_name_valid() {
        assert!(validate_name("pdf-form-filler").is_ok());
        assert!(validate_name("code-review").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("x4").is_ok());
    }

    #[test]
    fn test_validate_name_invalid() {
        assert!(validate_name("-start-hyphen").is_err());
        assert!(validate_name("end-hyphen-").is_err());
        assert!(validate_name("double--hyphen").is_err());
        assert!(validate_name("UPPERCASE").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err()); // > 64 chars
        assert!(validate_name("has spaces").is_err());
    }

    #[test]
    fn test_validate_description() {
        assert!(validate_description("A useful skill.").is_ok());
        assert!(validate_description("   ").is_err());
        assert!(validate_description(&"x".repeat(1025)).is_err());
    }

    #[test]
    fn test_parse_full_frontmatter() {
        let yaml = r#"
name: pdf-form-filler
description: Fill interactive PDF forms.
allowed-tools:
  - Read
  - Write
license: Apache-2.0
compatibility: Requires Python 3.10+
metadata:
  version: "1.0.0"
  stability: experimental
"#
        .trim();
        let fm = parse_frontmatter(yaml).unwrap();
        assert_eq!(fm.name, "pdf-form-filler");
        assert_eq!(fm.allowed_tools, vec!["Read", "Write"]);
        assert_eq!(fm.license.as_deref(), Some("Apache-2.0"));
    }

    #[test]
    fn test_split_empty_body() {
        let raw = "---\nname: minimal\ndescription: Minimal skill\n---\n";
        let (fm, body) = split_frontmatter(raw).unwrap();
        assert_eq!(fm, "name: minimal\ndescription: Minimal skill");
        assert!(body.is_empty() || body == "");
    }
}
