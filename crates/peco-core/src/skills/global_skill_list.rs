//! Global skill list — lifecycle management for the three-tier loading model.
//!
//! [`GlobalSkillList`] is the top-level API that consumers interact with:
//!
//! 1. **Startup**: [`init()`](GlobalSkillList::init) scans and loads Tier-1 metadata.
//! 2. **Selection**: [`all_meta()`](GlobalSkillList::all_meta) provides the model with a list
//!    of available Skills for relevance matching.
//! 3. **Activation**: [`activate()`](GlobalSkillList::activate) loads the full Tier-2 content.
//! 4. **Resources**: Tier-3 resources (scripts, references, assets) are read on demand
//!    via [`Skill::read_resource()`](super::Skill::read_resource).

use std::collections::HashMap;
use std::path::PathBuf;

use tracing::{info, warn};

use super::config::{Skill, SkillMeta};
use super::error::SkillError;
use super::loader::SkillLoader;

// ── Stats ────────────────────────────────────────────────────────────────────

/// Summary statistics for the global skill list.
#[derive(Debug, Clone, Default)]
pub struct GlobalSkillListStats {
    /// Number of Skills successfully discovered and registered (Tier 1).
    pub registered: usize,
    /// Number of Skills currently activated (Tier 2).
    pub activated: usize,
    /// Number of Skill directories that failed to load during init.
    pub errors: usize,
}

// ── GlobalSkillList ──────────────────────────────────────────────────────────

/// Central list managing the lifecycle of all Skills in the program.
///
/// Watches a `skills_root` directory; when the root path is updated via
/// [`set_skills_root`](Self::set_skills_root), the internal Skill list is
/// automatically re-scanned and reloaded.
///
/// # Example
///
/// ```no_run
/// use peco_core::skills::GlobalSkillList;
///
/// # fn example() -> Result<(), peco_core::skills::SkillError> {
/// let mut list = GlobalSkillList::new("./skills");
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
pub struct GlobalSkillList {
    /// Tier-1 metadata keyed by Skill name.
    metas: HashMap<String, SkillMeta>,
    /// Tier-2 fully-loaded Skills keyed by Skill name.
    activated: HashMap<String, Skill>,
    /// The loader used for discovery and I/O.
    loader: SkillLoader,
    /// Number of errors encountered during [`init`](Self::init).
    error_count: usize,
}

impl GlobalSkillList {
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
    /// # use peco_core::skills::GlobalSkillList;
    /// # fn example() -> Result<(), peco_core::skills::SkillError> {
    /// let mut list = GlobalSkillList::new("./skills");
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
    pub fn stats(&self) -> GlobalSkillListStats {
        GlobalSkillListStats {
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

    /// Path to the example skills directory in the project root.
    ///
    /// Tests run with CWD at the workspace root; we also resolve relative to
    /// CARGO_MANIFEST_DIR as a fallback so tests work from any directory.
    fn example_skills_root() -> PathBuf {
        let cwd = PathBuf::from("skills");
        if cwd.is_dir() {
            return cwd;
        }
        // Fallback: resolve from the crate manifest directory
        let from_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        if from_manifest.is_dir() {
            return from_manifest;
        }
        // Last resort: try the CWD-based path anyway (let init fail gracefully)
        cwd
    }

    #[test]
    fn test_init_discovers_skills() {
        let mut list = GlobalSkillList::new(example_skills_root());
        let count = list.init().unwrap();

        // We expect at least 3 valid skills; the broken-skill is skipped.
        assert!(count >= 3, "expected at least 3 skills, got {count}");
        assert!(list.has_skill("code-review"));
        assert!(list.has_skill("pdf-form-filler"));
        assert!(list.has_skill("excel-report-builder"));

        // broken-skill has a name mismatch → should be skipped
        assert!(!list.has_skill("not-broken-skill"));
        assert!(!list.has_skill("broken-skill"));
    }

    #[test]
    fn test_all_meta_returns_sorted() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let metas = list.all_meta();
        // Should be sorted by name
        for w in metas.windows(2) {
            assert!(w[0].name <= w[1].name, "metas should be sorted");
        }

        // Verify specific metadata
        let cr = metas.iter().find(|m| m.name == "code-review").unwrap();
        assert!(cr.description.contains("code review"));
    }

    #[test]
    fn test_activate_loads_full_skill() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        assert!(!list.is_activated("code-review"));

        let skill = list.activate("code-review").unwrap();

        // Verify frontmatter fields (use skill ref while it lives)
        assert_eq!(skill.frontmatter.name, "code-review");
        assert_eq!(skill.frontmatter.allowed_tools, vec!["Read", "Bash"]);
        assert!(skill.body.contains("Code Review"));
        assert!(skill.body.contains("Procedure"));

        // Drop skill ref before calling other list methods
        let _ = skill;
        assert!(list.is_activated("code-review"));
    }

    #[test]
    fn test_activate_returns_cached_on_second_call() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        // Use scoped raw pointers to compare identity without holding borrows.
        let ptr1 = {
            let skill = list.activate("code-review").unwrap();
            skill as *const Skill
        };
        let ptr2 = {
            let skill = list.activate("code-review").unwrap();
            skill as *const Skill
        };
        // Same pointer → returned from cache
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_activate_unregistered_skill_fails() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let err = list.activate("nonexistent").unwrap_err();
        assert!(matches!(err, SkillError::NotRegistered(_)));
    }

    #[test]
    fn test_tier3_resources() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        // pdf-form-filler has scripts/
        {
            let skill = list.activate("pdf-form-filler").unwrap();
            let scripts = skill.list_scripts();
            assert_eq!(scripts.len(), 1);
            assert!(scripts[0].to_str().unwrap().contains("extract_fields.py"));
        }

        // code-review has no subdirectories
        let cr = list.activate("code-review").unwrap();
        assert!(cr.list_scripts().is_empty());
        assert!(cr.list_references().is_empty());
        assert!(cr.list_assets().is_empty());
    }

    #[test]
    fn test_read_resource() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        {
            let _skill = list.activate("pdf-form-filler").unwrap();
            // let _skill drop so we can call read_skill_resource
        }
        let content = list
            .read_skill_resource(
                "pdf-form-filler",
                &PathBuf::from("scripts/extract_fields.py"),
            )
            .unwrap();
        assert!(content.contains("extract_fields"));
        assert!(content.contains("def extract_fields"));
    }

    #[test]
    fn test_stats() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let stats = list.stats();
        assert!(stats.registered >= 3);
        assert_eq!(stats.activated, 0);
        assert!(stats.errors >= 1, "broken-skill should produce an error");

        list.activate("code-review").unwrap();
        let stats = list.stats();
        assert_eq!(stats.activated, 1);
    }

    #[test]
    fn test_skill_names() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let names = list.skill_names();
        assert!(names.contains(&"code-review"));
        assert!(names.contains(&"excel-report-builder"));
        assert!(names.contains(&"pdf-form-filler"));
        // broken-skill should not appear
        assert!(!names.contains(&"broken-skill"));
    }

    #[test]
    fn test_skills_root_accessor() {
        let list = GlobalSkillList::new("./my-skills");
        assert_eq!(list.skills_root(), &PathBuf::from("./my-skills"));
    }

    #[test]
    fn test_activate_pdf_form_filler_full() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let skill = list.activate("pdf-form-filler").unwrap();
        assert_eq!(skill.frontmatter.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(
            skill.frontmatter.compatibility.as_deref(),
            Some("Requires Python 3.10+ and pdftk")
        );
        assert_eq!(
            skill.frontmatter.allowed_tools,
            vec!["Read", "Write", "Bash"]
        );
        assert!(skill.body.contains("PDF Form Filler"));
    }

    #[test]
    fn test_activate_excel_report_builder() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();

        let skill = list.activate("excel-report-builder").unwrap();

        // Has references/
        let refs = skill.list_references();
        assert_eq!(refs.len(), 1);
        assert!(refs[0].to_str().unwrap().contains("style-guide.md"));

        // Metadata
        let meta = &skill.frontmatter.metadata;
        assert_eq!(
            meta.get("stability").and_then(|v| v.as_str()),
            Some("stable")
        );
    }

    #[test]
    fn test_list_empty_when_root_missing() {
        let mut list = GlobalSkillList::new("/nonexistent/path/to/skills");
        // init should still succeed but return 0 skills
        let count = list.init().unwrap();
        assert_eq!(count, 0);
        assert!(list.all_meta().is_empty());
    }

    // ── set_skills_root tests ─────────────────────────────────────────────

    #[test]
    fn test_set_skills_root_auto_reloads() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();
        assert!(list.has_skill("code-review"));

        // Switch to a nonexistent root — should clear and return 0
        let count = list.set_skills_root("/nonexistent/path/to/skills").unwrap();
        assert_eq!(count, 0);
        assert!(!list.has_skill("code-review"));
        assert!(list.all_meta().is_empty());
        assert_eq!(list.stats().registered, 0);
        assert_eq!(list.stats().activated, 0);
    }

    #[test]
    fn test_set_skills_root_clears_activated() {
        let mut list = GlobalSkillList::new(example_skills_root());
        list.init().unwrap();
        list.activate("code-review").unwrap();
        assert!(list.is_activated("code-review"));

        // Switch root — activated skills from old root should be dropped
        list.set_skills_root("/nonexistent/path/to/skills").unwrap();
        assert!(!list.is_activated("code-review"));
    }

    #[test]
    fn test_set_skills_root_same_path_noop() {
        let root = example_skills_root();
        let mut list = GlobalSkillList::new(&root);
        list.init().unwrap();
        list.activate("code-review").unwrap();

        // Setting the same root should be a no-op
        let count = list.set_skills_root(&root).unwrap();
        assert!(count >= 3);
        assert!(list.has_skill("code-review"));
        // Activated skills should be preserved (early return)
        assert!(list.is_activated("code-review"));
    }

    #[test]
    fn test_set_skills_root_without_init() {
        // set_skills_root should work even without prior init()
        let mut list = GlobalSkillList::new("/nonexistent/path/to/skills");
        let count = list.set_skills_root(example_skills_root()).unwrap();
        assert!(count >= 3);
        assert!(list.has_skill("code-review"));
        assert_eq!(list.skills_root(), &example_skills_root());
    }
}
