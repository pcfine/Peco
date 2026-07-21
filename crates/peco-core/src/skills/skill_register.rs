//! Skill register — lifecycle management for the three-tier loading model.
//!
//! [`SkillRegister`] is the top-level API that consumers interact with:
//!
//! 1. **Startup**: [`init()`](SkillRegister::init) scans and loads Tier-1 metadata.
//! 2. **Selection**: [`all_meta()`](SkillRegister::all_meta) provides the model with a list
//!    of available Skills for relevance matching.
//! 3. **Activation**: [`activate()`](SkillRegister::activate) loads the full Tier-2 content.
//! 4. **Resources**: Tier-3 resources (scripts, references, assets) are read on demand
//!    via [`Skill::read_resource()`](super::Skill::read_resource).

use std::collections::HashMap;
use std::path::PathBuf;

use tracing::{info, warn};

use super::config::{Skill, SkillMeta};
use super::error::SkillError;
use super::loader::SkillLoader;

// ── Stats ────────────────────────────────────────────────────────────────────

/// Summary statistics for the skill register.
#[derive(Debug, Clone, Default)]
pub struct SkillRegisterStats {
    /// Number of Skills successfully discovered and registered (Tier 1).
    pub registered: usize,
    /// Number of Skills currently activated (Tier 2).
    pub activated: usize,
    /// Number of Skill directories that failed to load during init.
    pub errors: usize,
}

// ── SkillRegister ──────────────────────────────────────────────────────────

/// Central register managing the lifecycle of all Skills in the program.
///
/// Watches a `skills_root` directory; when the root path is updated via
/// [`set_skills_root`](Self::set_skills_root), the internal Skill list is
/// automatically re-scanned and reloaded.
///
/// # Example
///
/// ```no_run
/// use peco_core::skills::SkillRegister;
///
/// # fn example() -> Result<(), peco_core::skills::SkillError> {
/// let mut list = SkillRegister::new("./skills");
/// let count = list.init()?;
/// println!("Loaded {} skills", count);
///
/// // Get metadata for model selection
/// for meta in list.all_meta() {
///     println!("  [{}] {}", meta.name, meta.description);
/// }
///
/// // Activate a specific skill
/// let skill = list.activate("pdf-form-filler")?;
/// println!("Body length: {} chars", skill.body.len());
///
/// // Update the skills root — auto-reloads the skill list
/// list.set_skills_root("./new-skills")?;
/// # Ok(())
/// # }
/// ```
pub struct SkillRegister {
    /// Tier-1 metadata keyed by Skill name.
    metas: HashMap<String, SkillMeta>,
    /// Tier-2 fully-loaded Skills keyed by Skill name.
    activated: HashMap<String, Skill>,
    /// The loader used for discovery and I/O.
    loader: SkillLoader,
    /// Number of errors encountered during [`init`](Self::init).
    error_count: usize,
}

impl SkillRegister {
    // ── Construction ─────────────────────────────────────────────────────

    /// Create a new list pointing at the given skills root directory.
    ///
    /// The list is empty until [`init()`](Self::init) is called.
    pub fn new(skills_root: impl Into<PathBuf>) -> Self {
        Self {
            metas: HashMap::new(),
            activated: HashMap::new(),
            loader: SkillLoader::new(skills_root),
            error_count: 0,
        }
    }

    /// Return the skills root path this list was created with.
    pub fn skills_root(&self) -> &PathBuf {
        &self.loader.skills_root
    }

    /// Update the skills root path and automatically re-scan for Skills.
    ///
    /// This replaces the loader's root directory, clears all registered
    /// and activated Skills, then calls [`init()`](Self::init) to reload
    /// from the new location. Activated Skills from the old root are
    /// dropped.
    ///
    /// Returns the number of Skills loaded from the new root.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use peco_core::skills::SkillRegister;
    /// # fn example() -> Result<(), peco_core::skills::SkillError> {
    /// let mut list = SkillRegister::new("./skills");
    /// list.init()?;
    ///
    /// // Later, switch to a different skills directory:
    /// let count = list.set_skills_root("/etc/peco/skills")?;
    /// println!("Reloaded {count} skills from new root");
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_skills_root(&mut self, new_root: impl Into<PathBuf>) -> Result<usize, SkillError> {
        let new_root = new_root.into();

        // Skip if the root hasn't changed
        if self.loader.skills_root == new_root {
            info!(
                "skills root unchanged ({}), skipping reload",
                new_root.display()
            );
            return Ok(self.metas.len());
        }

        info!(
            "skills root updated: {} -> {}",
            self.loader.skills_root.display(),
            new_root.display()
        );

        // Clear all state
        self.metas.clear();
        self.activated.clear();
        self.error_count = 0;

        // Update loader and re-initialize
        self.loader = SkillLoader::new(new_root);
        self.init()
    }

    // ── Tier 1: Initialisation ───────────────────────────────────────────

    /// Scan the skills root and load all Skill metadata (Tier 1).
    ///
    /// Individual Skill load failures are logged as warnings and counted
    /// in the returned stats — they do not prevent other Skills from
    /// loading or the list from operating.
    ///
    /// Returns the number of successfully registered Skills.
    pub fn init(&mut self) -> Result<usize, SkillError> {
        info!(
            "Scanning for skills in {}",
            self.loader.skills_root.display()
        );

        let (metas, errors) = self.loader.load_all_meta();

        self.error_count = errors.len();

        for meta in metas {
            info!(
                "Tier1 loaded: {} — {}",
                meta.name,
                // Truncate long descriptions in log output
                if meta.description.len() > 80 {
                    format!("{}...", &meta.description[..77])
                } else {
                    meta.description.clone()
                }
            );
            self.metas.insert(meta.name.clone(), meta);
        }

        for (_dir, err) in &errors {
            warn!("{err}");
        }

        info!(
            "Tier1 complete: {} skills loaded, {} errors",
            self.metas.len(),
            errors.len()
        );

        Ok(self.metas.len())
    }

    // ── Tier 1: Queries ──────────────────────────────────────────────────

    /// Return all registered Skill metadata (Tier 1).
    ///
    /// The returned slice is suitable for passing to a model's context
    /// so it can select relevant Skills for the current task.
    pub fn all_meta(&self) -> Vec<&SkillMeta> {
        let mut metas: Vec<_> = self.metas.values().collect();
        metas.sort_by(|a, b| a.name.cmp(&b.name));
        metas
    }

    /// Check whether a Skill with the given name has been registered.
    pub fn has_skill(&self, name: &str) -> bool {
        self.metas.contains_key(name)
    }

    /// Return the names of all registered Skills.
    pub fn skill_names(&self) -> Vec<&str> {
        let mut names: Vec<_> = self.metas.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    // ── Tier 2: Activation ───────────────────────────────────────────────

    /// Activate a Skill by loading its full content (Tier 2).
    ///
    /// If the Skill is already activated, returns a reference to the
    /// cached instance.
    ///
    /// # Errors
    ///
    /// - [`SkillError::NotRegistered`] if the Skill name was not discovered
    ///   during [`init()`](Self::init).
    /// - [`SkillError::SkillMdNotFound`], [`SkillError::Io`],
    ///   [`SkillError::InvalidFrontmatter`], etc. if loading the full
    ///   SKILL.md fails.
    pub fn activate(&mut self, name: &str) -> Result<&Skill, SkillError> {
        // Load on cache miss (mutable operations are scoped to this block).
        if !self.activated.contains_key(name) {
            // Must be registered first
            if !self.metas.contains_key(name) {
                return Err(SkillError::NotRegistered(name.to_string()));
            }

            // Load full content
            let skill = self.loader.load_skill_by_name(name)?;

            info!(
                "Tier2 activated: {} (allowed tools: [{}], {} scripts, {} refs, {} assets)",
                name,
                skill.frontmatter.allowed_tools.join(", "),
                skill.list_scripts().len(),
                skill.list_references().len(),
                skill.list_assets().len(),
            );

            self.activated.insert(name.to_string(), skill);
        }

        // At this point the entry is guaranteed to exist.
        Ok(self.activated.get(name).unwrap())
    }

    /// Check whether a Skill has been fully loaded (Tier 2).
    pub fn is_activated(&self, name: &str) -> bool {
        self.activated.contains_key(name)
    }

    /// Return a reference to an activated Skill, if available.
    pub fn get_activated(&self, name: &str) -> Option<&Skill> {
        self.activated.get(name)
    }

    // ── Tier 3: Resource Access ──────────────────────────────────────────

    /// Read a resource file from an activated Skill's directory.
    ///
    /// This is a convenience wrapper around [`Skill::read_resource`].
    pub fn read_skill_resource(
        &self,
        skill_name: &str,
        relative_path: &std::path::Path,
    ) -> Result<String, SkillError> {
        let skill = self
            .activated
            .get(skill_name)
            .ok_or_else(|| SkillError::NotRegistered(skill_name.to_string()))?;
        skill
            .read_resource(relative_path)
            .map_err(|source| SkillError::Io {
                path: skill.root_dir.join(relative_path),
                source,
            })
    }

    // ── Statistics ───────────────────────────────────────────────────────

    /// Return current list statistics.
    pub fn stats(&self) -> SkillRegisterStats {
        SkillRegisterStats {
            registered: self.metas.len(),
            activated: self.activated.len(),
            errors: self.error_count,
        }
    }
}

// ── Integration tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skills_root_accessor() {
        let list = SkillRegister::new("./my-skills");
        assert_eq!(list.skills_root(), &PathBuf::from("./my-skills"));
    }

    #[test]
    fn test_list_empty_when_root_missing() {
        let mut list = SkillRegister::new("/nonexistent/path/to/skills");
        // init should still succeed but return 0 skills
        let count = list.init().unwrap();
        assert_eq!(count, 0);
        assert!(list.all_meta().is_empty());
    }
}
