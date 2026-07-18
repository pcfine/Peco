//! Skill discovery and file I/O.
//!
//! [`SkillLoader`] owns the root path and provides methods for:
//!
//! - Scanning the skills directory for valid Skill subdirectories
//! - Parsing SKILL.md files at both Tier 1 (metadata) and Tier 2 (full) levels
//!
//! Errors from individual Skills are collected and logged; they do not
//! prevent other Skills from being discovered or the loader from operating.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::config::{
    SKILL_MD_FILENAME, Skill, SkillMeta, parse_frontmatter, parse_meta_only, split_frontmatter,
    validate_description, validate_name,
};
use super::error::SkillError;

// ── SkillLoader ──────────────────────────────────────────────────────────────

/// Discovers and loads Skills from a root directory.
///
/// # Example
///
/// ```no_run
/// use peco_core::skills::SkillLoader;
///
/// let loader = SkillLoader::new("./skills");
/// let (metas, _errors) = loader.load_all_meta();
/// ```
#[derive(Debug, Clone)]
pub struct SkillLoader {
    /// The root directory containing Skill subdirectories.
    pub skills_root: PathBuf,
}

impl SkillLoader {
    /// Create a new loader pointing at the given skills root directory.
    pub fn new(skills_root: impl Into<PathBuf>) -> Self {
        Self {
            skills_root: skills_root.into(),
        }
    }

    // ── Discovery ────────────────────────────────────────────────────────

    /// Discover all Skill directories under the root.
    ///
    /// A directory is considered a Skill directory if it contains a `SKILL.md` file
    /// (directly, not in a subdirectory).
    pub fn discover_skill_dirs(&self) -> Result<Vec<PathBuf>, SkillError> {
        let root = &self.skills_root;

        if !root.is_dir() {
            return Err(SkillError::RootNotFound(root.to_path_buf()));
        }

        let mut dirs = Vec::new();
        let entries = fs::read_dir(root).map_err(|e| SkillError::ReadDir {
            path: root.to_path_buf(),
            source: e,
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry in {}: {e}", root.display());
                    continue;
                }
            };

            let path = entry.path();
            if path.is_dir() && path.join(SKILL_MD_FILENAME).is_file() {
                dirs.push(path);
            }
        }

        // Sort for deterministic ordering
        dirs.sort();
        debug!(
            "Discovered {} skill directories in {}",
            dirs.len(),
            root.display()
        );
        Ok(dirs)
    }

    // ── Tier 1: Metadata ─────────────────────────────────────────────────

    /// Load metadata (name + description only) from a single Skill directory.
    ///
    /// Performs full validation: reads SKILL.md, parses name and description,
    /// validates both, and checks that the directory name matches.
    pub fn load_meta(&self, dir: &Path) -> Result<SkillMeta, SkillError> {
        let skill_md_path = dir.join(SKILL_MD_FILENAME);
        let raw = fs::read_to_string(&skill_md_path).map_err(|source| SkillError::Io {
            path: skill_md_path.clone(),
            source,
        })?;

        // Parse minimal metadata
        let (name, description) =
            parse_meta_only(&raw).map_err(|reason| SkillError::InvalidFrontmatter {
                path: skill_md_path.clone(),
                reason,
            })?;

        // Validate name format
        validate_name(&name).map_err(|reason| SkillError::InvalidName {
            name: name.clone(),
            reason,
        })?;

        // Validate description
        validate_description(&description).map_err(|reason| SkillError::InvalidFrontmatter {
            path: skill_md_path.clone(),
            reason,
        })?;

        // Check directory name matches
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if dir_name != name {
            return Err(SkillError::NameMismatch {
                dir: dir_name,
                name,
            });
        }

        Ok(SkillMeta {
            name,
            description,
            path: dir.to_path_buf(),
        })
    }

    /// Load metadata from all discovered Skill directories.
    ///
    /// Errors from individual Skills are collected and returned alongside
    /// successfully loaded metadata.
    pub fn load_all_meta(&self) -> (Vec<SkillMeta>, Vec<(PathBuf, SkillError)>) {
        let dirs = match self.discover_skill_dirs() {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed to discover skill directories: {e}");
                return (Vec::new(), vec![(self.skills_root.clone(), e)]);
            }
        };

        let mut metas = Vec::new();
        let mut errors = Vec::new();

        for dir in &dirs {
            match self.load_meta(dir) {
                Ok(meta) => {
                    debug!("Tier1 loaded: {} — {}", meta.name, meta.description);
                    metas.push(meta);
                }
                Err(e) => {
                    warn!("Failed to load skill from {}: {e}", dir.display());
                    errors.push((dir.clone(), e));
                }
            }
        }

        (metas, errors)
    }

    // ── Tier 2: Full load ────────────────────────────────────────────────

    /// Fully load a Skill from its directory.
    ///
    /// Parses the complete frontmatter and Markdown body. Performs the same
    /// validation as [`load_meta`] plus full frontmatter parsing.
    pub fn load_skill(&self, dir: &Path) -> Result<Skill, SkillError> {
        let skill_md_path = dir.join(SKILL_MD_FILENAME);

        if !skill_md_path.is_file() {
            return Err(SkillError::SkillMdNotFound(dir.to_path_buf()));
        }

        let raw = fs::read_to_string(&skill_md_path).map_err(|source| SkillError::Io {
            path: skill_md_path.clone(),
            source,
        })?;

        // Split frontmatter from body
        let (frontmatter_str, body) =
            split_frontmatter(&raw).map_err(|reason| SkillError::InvalidFrontmatter {
                path: skill_md_path.clone(),
                reason,
            })?;

        // Parse complete frontmatter
        let frontmatter = parse_frontmatter(frontmatter_str).map_err(|reason| {
            SkillError::InvalidFrontmatter {
                path: skill_md_path.clone(),
                reason,
            }
        })?;

        // Validate name
        validate_name(&frontmatter.name).map_err(|reason| SkillError::InvalidName {
            name: frontmatter.name.clone(),
            reason,
        })?;

        // Validate description
        validate_description(&frontmatter.description).map_err(|reason| {
            SkillError::InvalidFrontmatter {
                path: skill_md_path.clone(),
                reason,
            }
        })?;

        // Check directory name consistency
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if dir_name != frontmatter.name {
            return Err(SkillError::NameMismatch {
                dir: dir_name,
                name: frontmatter.name,
            });
        }

        Ok(Skill {
            frontmatter,
            body: body.to_string(),
            skill_md_path,
            root_dir: dir.to_path_buf(),
        })
    }

    /// Fully load a Skill by name (looks up `<skills_root>/<name>/SKILL.md`).
    pub fn load_skill_by_name(&self, name: &str) -> Result<Skill, SkillError> {
        let dir = self.skills_root.join(name);
        self.load_skill(&dir)
    }
}
