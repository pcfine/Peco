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

use tracing::{debug, info, warn};

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

        info!("Scanning for skills in {}", loader.skills_root.display());

        let (metas, errors) = loader.load_all_meta();

        for meta in &metas {
            info!(
                "Tier1 loaded: {} — {}",
                meta.name,
                if meta.description.len() > 80 {
                    // 按字符边界截断，避免在多字节 UTF-8 字符中间切分导致 panic。
                    let truncated: String = meta.description.chars().take(77).collect();
                    format!("{truncated}...")
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

        info!("Tier1 complete: {registered} skills loaded, {error_count} errors");

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
        self.inner
            .read()
            .expect("RwLock poisoned")
            .metas
            .contains_key(name)
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
        self.inner
            .read()
            .expect("RwLock poisoned")
            .activated
            .contains_key(name)
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

    // ── 热重载 / 缓存管理 ────────────────────────────────────────────

    /// 重新扫描 skills 目录，刷新 Tier-1 元数据。
    ///
    /// 磁盘上已不存在的 Skill 会从 Tier-2 缓存中移除
    ///（Skill 不携带运行时状态，移除是安全的）。
    /// 仍然有效的已激活 Skill 会被保留。
    ///
    /// 返回重新扫描后发现的 Skill 数量。
    ///
    /// # 注意
    ///
    /// 此方法执行同步 I/O（`fs::read_dir` + `fs::read_to_string`）。
    /// 不在热路径上 — 调用方应仅在响应显式重载请求时调用，而非正常运行期间。
    pub fn rescan(&self) -> usize {
        let mut inner = self.inner.write().expect("RwLock poisoned");
        let (metas, errors) = inner.loader.load_all_meta();

        inner.metas.clear();
        for meta in metas {
            inner.metas.insert(meta.name.clone(), meta);
        }
        inner.error_count = errors.len();

        // 移除磁盘上已不存在的 Tier-2 条目。
        // 先收集有效名称，避免 `inner.metas` 的借用
        // 与 `activated.retain` 的闭包冲突。
        let valid_names: Vec<String> = inner.metas.keys().cloned().collect();
        inner.activated.retain(|name, _| valid_names.contains(name));

        let count = inner.metas.len();
        drop(inner);
        info!(count, "Skill 注册表已重新扫描");
        count
    }

    /// 刷新单个 Skill 的缓存数据。
    ///
    /// 使 Tier-2 缓存条目失效，以便下次调用 [`activate`](Self::activate)
    /// 时从磁盘重新加载完整的 SKILL.md。同时刷新该 Skill 的 Tier-1 元数据。
    ///
    /// 若该 Skill 在磁盘上已不存在，则同时移除 Tier-1 和 Tier-2 条目
    ///（效果等同于 [`remove_one`](Self::remove_one)）。
    pub fn refresh_one(&self, name: &str) {
        let mut inner = self.inner.write().expect("RwLock poisoned");

        // 使 Tier-2 失效
        inner.activated.remove(name);

        // 重新加载该 Skill 的 Tier-1 元数据
        let (metas, _) = inner.loader.load_all_meta();
        match metas.into_iter().find(|m| m.name == name) {
            Some(meta) => {
                inner.metas.insert(name.to_string(), meta);
                debug!(name = %name, "Skill 缓存已刷新");
            }
            None => {
                // Skill 在磁盘上已不存在 — 完全移除
                inner.metas.remove(name);
                debug!(name = %name, "Skill 已从缓存中移除（磁盘上已不存在）");
            }
        }
    }

    /// 从 Tier-1 和 Tier-2 缓存中移除某个 Skill。
    ///
    /// 当 Skill 目录被外部删除时使用此方法。
    /// 不会触碰文件系统。
    pub fn remove_one(&self, name: &str) {
        let mut inner = self.inner.write().expect("RwLock poisoned");
        inner.metas.remove(name);
        inner.activated.remove(name);
        debug!(name = %name, "Skill 已从缓存中移除");
    }

    // ── 写操作 ─────────────────────────────────────────────────────────

    /// 创建或更新一个 Skill，写入 SKILL.md 文件并刷新缓存。
    ///
    /// `content` 必须是完整的 SKILL.md 内容（YAML frontmatter + Markdown body）。
    /// 内部流程：校验名称 → 解析 YAML → 校验一致性 → 原子写入 → 刷新缓存。
    pub fn save_skill(&self, name: &str, content: &str) -> Result<(), SkillError> {
        use super::config::{
            parse_frontmatter, split_frontmatter, validate_description, validate_name,
        };

        // 1. 校验名称格式
        validate_name(name).map_err(|reason| SkillError::InvalidName {
            name: name.to_string(),
            reason,
        })?;

        // 2. 解析 frontmatter 并校验
        let (frontmatter_str, _body) =
            split_frontmatter(content).map_err(|reason| SkillError::InvalidFrontmatter {
                path: PathBuf::from(name),
                reason,
            })?;
        let fm = parse_frontmatter(frontmatter_str).map_err(|reason| {
            SkillError::InvalidFrontmatter {
                path: PathBuf::from(name),
                reason,
            }
        })?;

        // 3. 名称一致性检查
        if fm.name != name {
            return Err(SkillError::NameMismatch {
                dir: name.to_string(),
                name: fm.name,
            });
        }

        // 4. 描述字段校验
        validate_description(&fm.description).map_err(|reason| SkillError::InvalidFrontmatter {
            path: PathBuf::from(name),
            reason,
        })?;

        // 5. 原子写入文件（先写临时文件再重命名）
        let skills_root = {
            let inner = self.inner.read().expect("RwLock poisoned");
            inner.loader.skills_root.clone()
        };
        let dir = skills_root.join(name);
        std::fs::create_dir_all(&dir).map_err(|source| SkillError::Io {
            path: dir.clone(),
            source,
        })?;
        let md_path = dir.join(super::config::SKILL_MD_FILENAME);
        let tmp_path = dir.join(format!(".{}.tmp", super::config::SKILL_MD_FILENAME));
        std::fs::write(&tmp_path, content).map_err(|source| SkillError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &md_path).map_err(|source| SkillError::Io {
            path: md_path.clone(),
            source,
        })?;

        // 6. 刷新缓存
        self.refresh_one(name);

        info!(name = %name, "Skill 已保存");
        Ok(())
    }

    /// 删除 Skill 目录并清除缓存（不可逆操作）。
    pub fn delete_skill(&self, name: &str) -> Result<(), SkillError> {
        let skills_root = {
            let inner = self.inner.read().expect("RwLock poisoned");
            inner.loader.skills_root.clone()
        };
        let dir = skills_root.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|source| SkillError::Io { path: dir, source })?;
        }

        self.remove_one(name);
        info!(name = %name, "Skill 已删除");
        Ok(())
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
