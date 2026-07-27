//! Skill register — lifecycle management for the three-tier loading model.
//!
//! [`SkillRegister`] is the top-level API that consumers interact with:
//!
//! 1. **Startup**: [`new()`](SkillRegister::new) scans and loads Tier-1 metadata.
//! 2. **Selection**: [`all_meta()`](SkillRegister::all_meta) provides the model with a list
//!    of available Skills for relevance matching.
//! 3. **Activation**: [`activate()`](SkillRegister::activate) loads the full Tier-2 content.
//! 4. **Resources**: Tier-3 resources (scripts, references, assets) are read on demand
//!    via [`Skill::read_resource()`](super::Skill::read_resource).
//!
//! All methods take `&self` — internal synchronisation is handled by an `RwLock`
//! so the register can be shared across threads via `Arc<SkillRegister>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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

// ── Inner (lock-protected state) ─────────────────────────────────────────────

struct Inner {
    /// Tier-1 metadata keyed by Skill name.
    metas: HashMap<String, SkillMeta>,
    /// Tier-2 fully-loaded Skills keyed by Skill name.
    activated: HashMap<String, Arc<Skill>>,
    /// The loader used for discovery and I/O.
    loader: SkillLoader,
    /// Number of errors encountered during initialisation.
    error_count: usize,
}

// ── SkillRegister ──────────────────────────────────────────────────────────

/// Central register managing the lifecycle of all Skills in the program.
///
/// Created via [`new()`](Self::new), which immediately scans the given
/// skills root directory and loads Tier-1 metadata. All methods are `&self`
/// — the register uses an internal `RwLock` so it can be freely shared
/// behind an `Arc`.
///
/// # Example
///
/// ```no_run
/// use peco_core::skills::SkillRegister;
///
/// # fn example() -> Result<(), peco_core::skills::SkillError> {
/// let list = SkillRegister::new("./skills")?;
/// println!("Loaded {} skills", list.stats().registered);
///
/// // Get metadata for model selection
/// for meta in list.all_meta() {
///     println!("  [{}] {}", meta.name, meta.description);
/// }
///
/// // Activate a specific skill
/// let skill = list.activate("pdf-form-filler")?;
/// println!("Body length: {} chars", skill.body.len());
/// # Ok(())
/// # }
/// ```
pub struct SkillRegister {
    inner: RwLock<Inner>,
}

impl SkillRegister {
    // ── Construction ─────────────────────────────────────────────────────

    /// Create a new register by scanning the given skills root directory.
    ///
    /// This discovers all Skill directories, loads their frontmatter (Tier 1),
    /// and makes them queryable via [`all_meta()`](Self::all_meta).
    ///
    /// Individual Skill load failures are logged as warnings — they do not
    /// prevent other Skills from loading or the register from operating.
    pub fn new(skills_root: impl Into<PathBuf>) -> Result<Self, SkillError> {
        let loader = SkillLoader::new(skills_root);

        info!(
            "Scanning for skills in {}",
            loader.skills_root.display()
        );

        let (metas, errors) = loader.load_all_meta();

        for meta in &metas {
            info!(
                "Tier1 loaded: {} — {}",
                meta.name,
                if meta.description.len() > 80 {
                    format!("{}...", &meta.description[..77])
                } else {
                    meta.description.clone()
                }
            );
        }

        for (_dir, err) in &errors {
            warn!("{err}");
        }

        let registered = metas.len();
        let error_count = errors.len();

        info!(
            "Tier1 complete: {registered} skills loaded, {error_count} errors"
        );

        let mut metas_map = HashMap::with_capacity(metas.len());
        for meta in metas {
            metas_map.insert(meta.name.clone(), meta);
        }

        Ok(Self {
            inner: RwLock::new(Inner {
                metas: metas_map,
                activated: HashMap::new(),
                loader,
                error_count,
            }),
        })
    }

    /// Create an empty register with no skills.
    ///
    /// This is a zero-scan constructor — useful as a fallback when the
    /// skills directory is unavailable, or for contexts where skills are
    /// known to be absent.
    pub fn empty() -> Self {
        Self {
            inner: RwLock::new(Inner {
                metas: HashMap::new(),
                activated: HashMap::new(),
                loader: SkillLoader::new(std::path::PathBuf::new()),
                error_count: 0,
            }),
        }
    }

    // ── Tier 1: Queries ──────────────────────────────────────────────────

    /// Return all registered Skill metadata (Tier 1) as an owned `Vec`.
    ///
    /// Suitable for passing to a model's context so it can select relevant
    /// Skills for the current task.
    pub fn all_meta(&self) -> Vec<SkillMeta> {
        let inner = self.inner.read().expect("RwLock poisoned");
        let mut metas: Vec<_> = inner.metas.values().cloned().collect();
        metas.sort_by(|a, b| a.name.cmp(&b.name));
        metas
    }

    /// Check whether a Skill with the given name has been registered.
    pub fn has_skill(&self, name: &str) -> bool {
        self.inner.read().expect("RwLock poisoned").metas.contains_key(name)
    }

    /// Return the names of all registered Skills.
    pub fn skill_names(&self) -> Vec<String> {
        let inner = self.inner.read().expect("RwLock poisoned");
        let mut names: Vec<String> = inner.metas.keys().cloned().collect();
        names.sort();
        names
    }

    // ── Tier 2: Activation ───────────────────────────────────────────────

    /// Activate a Skill by loading its full content (Tier 2).
    ///
    /// If the Skill is already activated, returns a clone of the cached
    /// `Arc<Skill>` (cheap reference-count bump).
    ///
    /// # Errors
    ///
    /// - [`SkillError::NotRegistered`] if the Skill name was not discovered
    ///   during construction.
    /// - [`SkillError::SkillMdNotFound`], [`SkillError::Io`],
    ///   [`SkillError::InvalidFrontmatter`], etc. if loading the full
    ///   SKILL.md fails.
    pub fn activate(&self, name: &str) -> Result<Arc<Skill>, SkillError> {
        let mut inner = self.inner.write().expect("RwLock poisoned");

        // Cache hit — return clone of Arc (cheap reference-count bump).
        if let Some(skill) = inner.activated.get(name) {
            return Ok(Arc::clone(skill));
        }

        // Must be registered first.
        if !inner.metas.contains_key(name) {
            return Err(SkillError::NotRegistered(name.to_string()));
        }

        let skill = inner.loader.load_skill_by_name(name)?;

        info!(
            "Tier2 activated: {} (allowed tools: [{}], {} scripts, {} refs, {} assets)",
            name,
            skill.frontmatter.allowed_tools.join(", "),
            skill.list_scripts().len(),
            skill.list_references().len(),
            skill.list_assets().len(),
        );

        let skill = Arc::new(skill);
        inner.activated.insert(name.to_string(), Arc::clone(&skill));
        Ok(skill)
    }

    /// Check whether a Skill has been fully loaded (Tier 2).
    pub fn is_activated(&self, name: &str) -> bool {
        self.inner.read().expect("RwLock poisoned").activated.contains_key(name)
    }

    /// Return a clone of the activated Skill, if available.
    pub fn get_activated(&self, name: &str) -> Option<Arc<Skill>> {
        self.inner
            .read()
            .expect("RwLock poisoned")
            .activated
            .get(name)
            .cloned()
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
        let inner = self.inner.read().expect("RwLock poisoned");
        let skill = inner
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

    /// Return current register statistics.
    pub fn stats(&self) -> SkillRegisterStats {
        let inner = self.inner.read().expect("RwLock poisoned");
        SkillRegisterStats {
            registered: inner.metas.len(),
            activated: inner.activated.len(),
            errors: inner.error_count,
        }
    }
}

// ── Integration tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_empty_when_root_missing() {
        let list = SkillRegister::new("/nonexistent/path/to/skills").unwrap();
        assert!(list.all_meta().is_empty());
        assert_eq!(list.stats().registered, 0);
    }
}
