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
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tracing::{debug, info, warn};

use super::config::{Skill, SkillMeta, SkillResourceFile};
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
        info!(count, "Skill registry rescanned");
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
                debug!(name = %name, "Skill cache refreshed");
            }
            None => {
                // Skill 在磁盘上已不存在 — 完全移除
                inner.metas.remove(name);
                debug!(name = %name, "Skill removed from cache (no longer on disk)");
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
        debug!(name = %name, "Skill removed from cache");
    }

    // ── 写操作 ─────────────────────────────────────────────────────────

    /// 创建或更新一个 Skill，写入 SKILL.md 文件并刷新缓存。
    ///
    /// `content` 必须是完整的 SKILL.md 内容（YAML frontmatter + Markdown body）。
    /// 等价于不带资源文件的 [`save_skill_bundle`](Self::save_skill_bundle)。
    pub fn save_skill(&self, name: &str, content: &str) -> Result<(), SkillError> {
        self.save_skill_bundle(name, content, &[])
    }

    /// 一次性写入 SKILL.md 与其 Tier-3 资源文件（`scripts` / `references` / `assets`）。
    ///
    /// 内部流程：校验名称 → 解析 YAML → 校验一致性 → **校验全部资源路径** →
    /// 原子写入 SKILL.md → 原子写入资源文件 → 刷新缓存。
    ///
    /// 所有校验都在任何落盘之前完成：任一校验失败时磁盘保持原样，
    /// 不会留下半写入的技能目录。
    ///
    /// `scripts/` 下的文件在 Unix 上会被赋予可执行位（`0o755`），使
    /// `read_skill` 返回的路径可以按相对路径直接执行。
    pub fn save_skill_bundle(
        &self,
        name: &str,
        content: &str,
        files: &[SkillResourceFile],
    ) -> Result<(), SkillError> {
        use super::config::{
            SKILL_MD_FILENAME, parse_frontmatter, split_frontmatter, validate_description,
            validate_name, validate_resource_path,
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

        // 5. 校验全部资源路径（先校验、后落盘）
        let mut resources: Vec<(PathBuf, &str)> = Vec::with_capacity(files.len());
        for file in files {
            let rel = validate_resource_path(&file.path).map_err(|reason| {
                SkillError::InvalidResourcePath {
                    path: file.path.clone(),
                    reason,
                }
            })?;
            resources.push((rel, file.content.as_str()));
        }

        // 6. 原子写入 SKILL.md
        let skills_root = {
            let inner = self.inner.read().expect("RwLock poisoned");
            inner.loader.skills_root.clone()
        };
        let dir = skills_root.join(name);
        std::fs::create_dir_all(&dir).map_err(|source| SkillError::Io {
            path: dir.clone(),
            source,
        })?;
        atomic_write(&dir.join(SKILL_MD_FILENAME), content)?;

        // 7. 原子写入资源文件
        for (rel, file_content) in resources {
            let target = dir.join(&rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            atomic_write(&target, file_content)?;

            // scripts/ 下的文件赋予可执行位
            #[cfg(unix)]
            {
                let is_script = rel
                    .components()
                    .next()
                    .map(|c| c.as_os_str() == std::ffi::OsStr::new("scripts"))
                    .unwrap_or(false);
                if is_script {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
                }
            }

            debug!(skill = %name, path = %rel.display(), "Skill resource written");
        }

        // 8. 刷新缓存
        self.refresh_one(name);

        info!(name = %name, files = files.len(), "Skill saved");
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
        info!(name = %name, "Skill deleted");
        Ok(())
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// 原子写入：先写同目录下的临时文件，再 `rename` 覆盖目标。
///
/// `rename` 在同一文件系统内是原子操作，因此读者要么看到旧内容，要么看到
/// 完整的新内容，不会读到半截文件。
fn atomic_write(path: &Path, content: &str) -> Result<(), SkillError> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("resource");
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&tmp_path, content).map_err(|source| SkillError::Io {
        path: tmp_path.clone(),
        source,
    })?;
    std::fs::rename(&tmp_path, path).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

// ── Integration tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillResourceFile;
    use std::path::Path;

    const DEMO_SKILL: &str =
        "---\nname: demo\ndescription: A demo skill for tests\n---\n\n# Demo\n\nDo things.\n";

    #[test]
    fn test_list_empty_when_root_missing() {
        let list = SkillRegister::new("/nonexistent/path/to/skills").unwrap();
        assert!(list.all_meta().is_empty());
        assert_eq!(list.stats().registered, 0);
    }

    #[test]
    fn save_skill_bundle_writes_resources_and_marks_scripts_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = SkillRegister::new(tmp.path()).unwrap();

        let files = vec![
            SkillResourceFile {
                path: "scripts/run.sh".into(),
                content: "#!/bin/sh\necho hi\n".into(),
            },
            SkillResourceFile {
                path: "references/notes.md".into(),
                content: "# Notes\n".into(),
            },
        ];
        reg.save_skill_bundle("demo", DEMO_SKILL, &files).unwrap();

        let skill = reg.activate("demo").unwrap();
        assert_eq!(skill.list_scripts(), vec![PathBuf::from("scripts/run.sh")]);
        assert_eq!(
            skill.list_references(),
            vec![PathBuf::from("references/notes.md")]
        );
        assert!(skill.list_assets().is_empty());
        assert_eq!(
            skill.read_resource(Path::new("scripts/run.sh")).unwrap(),
            "#!/bin/sh\necho hi\n"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join("demo/scripts/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "scripts must be executable");
        }
    }

    #[test]
    fn save_skill_bundle_is_all_or_nothing_on_bad_path() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = SkillRegister::new(tmp.path()).unwrap();

        let files = vec![
            SkillResourceFile {
                path: "scripts/ok.sh".into(),
                content: "ok".into(),
            },
            SkillResourceFile {
                path: "../../escape.txt".into(),
                content: "nope".into(),
            },
        ];
        let err = reg
            .save_skill_bundle("demo", DEMO_SKILL, &files)
            .unwrap_err();
        assert!(
            matches!(err, SkillError::InvalidResourcePath { .. }),
            "got {err:?}"
        );

        // 校验失败时不应落下任何文件
        assert!(!tmp.path().join("demo/SKILL.md").exists());
        assert!(!tmp.path().join("demo/scripts/ok.sh").exists());
        assert!(!tmp.path().join("escape.txt").exists());
    }

    #[test]
    fn save_skill_bundle_rejects_name_mismatch_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = SkillRegister::new(tmp.path()).unwrap();
        let bad = "---\nname: other\ndescription: mismatch\n---\n";
        let err = reg.save_skill_bundle("demo", bad, &[]).unwrap_err();
        assert!(
            matches!(err, SkillError::NameMismatch { .. }),
            "got {err:?}"
        );
        assert!(!tmp.path().join("demo/SKILL.md").exists());
    }

    #[test]
    fn save_skill_accepts_a_fresh_skill_and_refreshes_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = SkillRegister::new(tmp.path()).unwrap();
        assert!(!reg.has_skill("demo"));

        reg.save_skill("demo", DEMO_SKILL).unwrap();
        assert!(reg.has_skill("demo"));
        assert_eq!(reg.stats().registered, 1);
    }
}
