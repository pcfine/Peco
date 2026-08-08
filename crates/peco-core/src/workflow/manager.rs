// ============================================================================
// WorkflowManager — Workflow 生命周期管理
// ============================================================================
//
// 职责：
// - 扫描 workflows/ 目录，缓存 Tier-1 元数据（name + description + version）
// - 加载完整 WorkflowDefinition 并缓存（Tier-2）
// - 提供执行入口（thin wrapper around WorkflowEngine::spawn）
//
// 遵循 AgentManager 的两级缓存模式。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tracing::{debug, warn};

use crate::tools::AgentAccess;

use super::definition::{WorkflowDefinition, pre_validate_workflow_yaml};
use super::engine::{WorkflowConfig, WorkflowEngine};
use super::error::WorkflowError;
use super::handle::WorkflowHandle;
use super::persistence::WorkflowPersister;

// ── WorkflowMeta ──────────────────────────────────────────────────────────

/// Tier-1 元数据：从 workflow.md frontmatter 解析的最少信息。
#[derive(Debug, Clone)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    pub version: String,
    /// 步骤数量（从 steps 数组长度获取，0 表示定义中无 steps）。
    pub step_count: usize,
}

// ── WorkflowManager ───────────────────────────────────────────────────────

/// Workflow 生命周期管理器。
///
/// 两级缓存：
/// - Tier 1：`metas` — 扫描目录时缓存的 frontmatter 摘要
/// - Tier 2：`definitions` — 完整加载的 WorkflowDefinition 实例
///
/// **不追踪活跃执行**：调用方自行管理 `WorkflowHandle` 生命周期。
///
/// **不再持有 persister**：persister 由调用方在 `execute()` 时按用户传入。
pub struct WorkflowManager {
    workflows_dir: PathBuf,
    /// Tier-1 元数据缓存（name → WorkflowMeta）
    metas: RwLock<HashMap<String, WorkflowMeta>>,
    /// Tier-2 完整定义缓存（name → WorkflowDefinition）
    definitions: RwLock<HashMap<String, WorkflowDefinition>>,
}

impl WorkflowManager {
    /// 创建新的 WorkflowManager。
    ///
    /// 创建后应调用 [`init`](Self::init) 扫描目录并缓存元数据。
    /// persister 不再通过构造注入，改为在 [`execute`](Self::execute) 时按用户传入。
    pub fn new(workflows_dir: PathBuf) -> Self {
        Self {
            workflows_dir,
            metas: RwLock::new(HashMap::new()),
            definitions: RwLock::new(HashMap::new()),
        }
    }

    // ── 初始化 ───────────────────────────────────────────────────────

    /// 扫描 `workflows/` 目录，解析每个 `workflow.md` 的 frontmatter，
    /// 缓存 Tier-1 元数据。返回成功扫描的 Workflow 数量。
    pub fn init(&self) -> Result<usize, WorkflowError> {
        let mut metas = self
            .metas
            .write()
            .map_err(|e| WorkflowError::Persist(format!("workflow metas lock poisoned: {e}")))?;
        metas.clear();

        if !self.workflows_dir.exists() {
            return Ok(0);
        }

        let entries: Vec<_> = std::fs::read_dir(&self.workflows_dir)
            .map_err(WorkflowError::Io)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        for entry in entries {
            let md_path = entry.path().join("workflow.md");
            if !md_path.exists() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            match Self::parse_meta(&md_path) {
                Ok(meta) => {
                    debug!(workflow = %dir_name, "Workflow metadata cached");
                    metas.insert(dir_name, meta);
                }
                Err(e) => {
                    warn!(workflow = %dir_name, error = %e, "Failed to parse workflow metadata");
                }
            }
        }

        Ok(metas.len())
    }

    /// 解析单个 workflow.md 的 frontmatter，提取 name + description + version + step_count。
    fn parse_meta(md_path: &Path) -> Result<WorkflowMeta, WorkflowError> {
        let raw = std::fs::read_to_string(md_path)?;
        let (frontmatter_str, _) = crate::agent::split_frontmatter(&raw)
            .map_err(|e| WorkflowError::Parse(format!("invalid frontmatter: {e}")))?;

        // 解析 workflow 包装格式：workflow: { name, description, version, steps }
        #[derive(serde::Deserialize)]
        struct WorkflowFileMeta {
            workflow: WorkflowMetaRaw,
        }
        #[derive(serde::Deserialize)]
        struct WorkflowMetaRaw {
            name: String,
            description: String,
            #[serde(default)]
            version: String,
            #[serde(default)]
            steps: Vec<serde_yaml::Value>,
        }

        let parsed: WorkflowFileMeta = serde_yaml::from_str(frontmatter_str)
            .map_err(|e| WorkflowError::Parse(format!("YAML parse error: {e}")))?;

        Ok(WorkflowMeta {
            name: parsed.workflow.name,
            description: parsed.workflow.description,
            version: parsed.workflow.version,
            step_count: parsed.workflow.steps.len(),
        })
    }

    // ── 元数据查询 ──────────────────────────────────────────────────

    /// 重新扫描 `workflows/` 目录，刷新 Tier-1 元数据。
    /// 不清除 Tier-2 定义缓存 — 已加载的 WorkflowDefinition 实例不受影响。
    /// 返回重新发现的 Workflow 数量。
    pub fn rescan(&self) -> Result<usize, WorkflowError> {
        self.init()
    }

    /// 返回所有已缓存 Workflow 的 Tier-1 元数据列表。
    pub fn list_meta(&self) -> Vec<WorkflowMeta> {
        self.metas
            .read()
            .map(|m| {
                let mut metas: Vec<_> = m.values().cloned().collect();
                metas.sort_by(|a, b| a.name.cmp(&b.name));
                metas
            })
            .unwrap_or_default()
    }

    /// 返回所有已缓存 Workflow 的名称列表。
    pub fn list_names(&self) -> Vec<String> {
        self.list_meta().into_iter().map(|m| m.name).collect()
    }

    // ── Workflow 加载 ───────────────────────────────────────────────

    /// 加载 WorkflowDefinition（优先 Tier-2 缓存，未命中则读文件）。
    pub fn load(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError> {
        // Tier-2 缓存命中
        {
            let cache = self
                .definitions
                .read()
                .map_err(|e| WorkflowError::Persist(format!("definitions lock poisoned: {e}")))?;
            if let Some(def) = cache.get(name) {
                debug!(workflow = %name, "WorkflowDefinition cache hit");
                return Ok(def.clone());
            }
        }

        // 缓存未命中：从文件加载
        let path = self.workflows_dir.join(name).join("workflow.md");
        if !path.exists() {
            return Err(WorkflowError::Parse(format!(
                "workflow '{name}' not found at {}",
                path.display()
            )));
        }

        let def = WorkflowDefinition::from_file(&path)?;

        // 写入 Tier-2 缓存
        {
            let mut cache = self
                .definitions
                .write()
                .map_err(|e| WorkflowError::Persist(format!("definitions lock poisoned: {e}")))?;
            cache.insert(name.to_string(), def.clone());
        }

        debug!(workflow = %name, "WorkflowDefinition loaded and cached");
        Ok(def)
    }

    /// 强制从磁盘重新加载指定 Workflow（Tier-1 元数据更新 + Tier-2 缓存失效）。
    pub fn reload(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError> {
        self.refresh_one(name);
        self.load(name)
    }

    /// 刷新单个 Workflow 的缓存（Tier-2 失效 + Tier-1 元数据更新）。
    ///
    /// 从磁盘重新解析 workflow.md 的 frontmatter 并更新 Tier-1 元数据。
    /// 同时使 Tier-2 缓存失效，确保下次 [`load`](Self::load) 从磁盘重新读取完整定义。
    ///
    /// 如果 workflow.md 文件在磁盘上已不存在（如被外部删除），
    /// 则从 Tier-1 和 Tier-2 两级缓存中移除该 Workflow。
    ///
    /// 若解析失败（YAML 损坏），保留过时的 Tier-1 元数据并记录 warning —
    /// 与 `crate::agent::agent_manager::AgentManager::refresh_one` 行为一致.
    pub fn refresh_one(&self, name: &str) {
        self.invalidate(name);

        let md_path = self.workflows_dir.join(name).join("workflow.md");
        if md_path.exists() {
            match Self::parse_meta(&md_path) {
                Ok(meta) => {
                    if let Ok(mut metas) = self.metas.write() {
                        metas.insert(name.to_string(), meta);
                    }
                    debug!(workflow = %name, "Workflow metadata refreshed from disk");
                }
                Err(e) => {
                    warn!(workflow = %name, error = %e,
                        "Failed to parse workflow metadata, keeping stale cache");
                }
            }
        } else {
            // workflow.md 已被外部删除 — 从两级缓存中清理
            if let Ok(mut metas) = self.metas.write() {
                metas.remove(name);
            }
            debug!(workflow = %name, "Workflow removed from caches (file gone)");
        }
    }

    // ── 缓存管理 ────────────────────────────────────────────────────

    /// 使指定 Workflow 的 Tier-2 缓存失效（下次加载时重新解析）。
    pub fn invalidate(&self, name: &str) {
        if let Ok(mut cache) = self.definitions.write() {
            cache.remove(name);
            debug!(workflow = %name, "WorkflowDefinition cache invalidated");
        }
    }

    // ── 执行 ────────────────────────────────────────────────────────

    /// 启动 Workflow 执行。
    ///
    /// Thin wrapper：加载定义 → 验证输入 → spawn 引擎 → 返回 handle。
    /// 调用方通过 `WorkflowHandle` 消费事件和管理生命周期。
    ///
    /// `persister` 由调用方（peco-server handler 层）按用户创建并传入，
    /// 引擎通过该 persister 自动持久化快照，无需感知用户上下文。
    pub fn execute(
        &self,
        name: &str,
        agent_access: Arc<dyn AgentAccess>,
        persister: Arc<dyn WorkflowPersister>,
        config: WorkflowConfig,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Result<WorkflowHandle, WorkflowError> {
        let definition = self.load(name)?;
        let _validated = definition.validate_inputs(&inputs)?;
        Ok(WorkflowEngine::spawn(
            definition,
            agent_access,
            persister,
            config,
            inputs,
        ))
    }

    // ── 写操作 ──────────────────────────────────────────────────────
    ///
    /// 所有写操作遵循「先 I/O 后缓存」原则：
    /// 文件系统操作在锁外完成，仅在更新内存缓存时持锁，
    /// 避免 std::sync::RwLock 持锁期间阻塞 async runtime 工作线程。
    ///
    /// 校验 Workflow 名称格式。
    ///
    /// 规则：1-128 字符，仅允许 ASCII 字母、数字、下划线和连字符。
    /// 禁止目录遍历字符（`.` 和 `..`）。
    fn validate_workflow_name(name: &str) -> Result<(), WorkflowError> {
        if name.is_empty() || name.len() > 128 {
            return Err(WorkflowError::InvalidName(
                "name must be 1-128 characters".into(),
            ));
        }
        if name == "." || name == ".." {
            return Err(WorkflowError::InvalidName(
                "name must not be '.' or '..'".into(),
            ));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(WorkflowError::InvalidName(
                "name must contain only ASCII letters, digits, underscores, and hyphens".into(),
            ));
        }
        Ok(())
    }

    /// 创建新 Workflow。
    ///
    /// 流程：校验名称 → 检查目录不存在 → 解析定义（先验证后写入）→
    /// 原子写入 workflow.md → 持锁更新 metas 缓存。
    /// 所有文件 I/O 在持锁之前完成。
    pub fn create(
        &self,
        name: &str,
        yaml_content: &str,
    ) -> Result<WorkflowDefinition, WorkflowError> {
        Self::validate_workflow_name(name)?;
        let dir = self.workflows_dir.join(name);

        if dir.exists() {
            return Err(WorkflowError::AlreadyExists(format!(
                "workflow '{name}' already exists"
            )));
        }

        // 1. 预校验：在 serde 严格解析前检查 YAML 结构，给出可操作的错误信息
        pre_validate_workflow_yaml(yaml_content)?;

        // 2. 解析并验证定义（纯 CPU，先于任何文件 I/O，确保只写入合法定义）
        let definition = WorkflowDefinition::from_yaml(yaml_content)?;

        // 3. 文件 I/O（锁外完成，原子写入）
        std::fs::create_dir_all(&dir)?;
        let tmp_path = dir.join(".workflow.md.tmp");
        let final_path = dir.join("workflow.md");
        std::fs::write(&tmp_path, yaml_content)?;
        std::fs::rename(&tmp_path, &final_path)?;

        // 4. 更新缓存（持锁）
        {
            let mut metas = self
                .metas
                .write()
                .map_err(|e| WorkflowError::Persist(format!("metas lock poisoned: {e}")))?;
            metas.insert(
                name.to_string(),
                WorkflowMeta {
                    name: definition.name.clone(),
                    description: definition.description.clone(),
                    version: definition.version.clone(),
                    step_count: definition.steps.len(),
                },
            );
        }

        debug!(workflow = %name, "Workflow created");
        Ok(definition)
    }

    /// 更新已有 Workflow。
    ///
    /// 流程：检查目录存在 → 解析定义（先验证后写入）→
    /// 原子覆盖 workflow.md → 持锁更新 metas + 淘汰 definitions 缓存。
    pub fn update(
        &self,
        name: &str,
        yaml_content: &str,
    ) -> Result<WorkflowDefinition, WorkflowError> {
        let dir = self.workflows_dir.join(name);
        let final_path = dir.join("workflow.md");

        if !final_path.exists() {
            return Err(WorkflowError::Parse(format!(
                "workflow '{name}' not found at {}",
                final_path.display()
            )));
        }

        // 1. 预校验：在 serde 严格解析前检查 YAML 结构，给出可操作的错误信息
        pre_validate_workflow_yaml(yaml_content)?;

        // 2. 解析并验证定义（纯 CPU，先于文件 I/O，防止写入无效内容覆盖合法文件）
        let definition = WorkflowDefinition::from_yaml(yaml_content)?;

        // 3. 文件 I/O（锁外完成，原子覆盖）
        let tmp_path = dir.join(".workflow.md.tmp");
        std::fs::write(&tmp_path, yaml_content)?;
        std::fs::rename(&tmp_path, &final_path)?;

        // 3. 更新缓存（持锁）
        {
            // 淘汰 Tier-2
            let mut defs = self
                .definitions
                .write()
                .map_err(|e| WorkflowError::Persist(format!("definitions lock poisoned: {e}")))?;
            defs.remove(name);

            // 更新 Tier-1
            let mut metas = self
                .metas
                .write()
                .map_err(|e| WorkflowError::Persist(format!("metas lock poisoned: {e}")))?;
            metas.insert(
                name.to_string(),
                WorkflowMeta {
                    name: definition.name.clone(),
                    description: definition.description.clone(),
                    version: definition.version.clone(),
                    step_count: definition.steps.len(),
                },
            );
        }

        debug!(workflow = %name, "Workflow updated");
        Ok(definition)
    }

    /// 删除 Workflow。
    ///
    /// 流程：检查目录存在 → 删除目录 → 持锁清理 metas + definitions 缓存。
    pub fn delete(&self, name: &str) -> Result<(), WorkflowError> {
        let dir = self.workflows_dir.join(name);

        if !dir.exists() {
            return Err(WorkflowError::Parse(format!(
                "workflow '{name}' not found at {}",
                dir.display()
            )));
        }

        // 1. 文件 I/O（锁外完成）
        std::fs::remove_dir_all(&dir)?;

        // 2. 清理缓存（持锁）
        {
            let mut defs = self
                .definitions
                .write()
                .map_err(|e| WorkflowError::Persist(format!("definitions lock poisoned: {e}")))?;
            defs.remove(name);

            let mut metas = self
                .metas
                .write()
                .map_err(|e| WorkflowError::Persist(format!("metas lock poisoned: {e}")))?;
            metas.remove(name);
        }

        debug!(workflow = %name, "Workflow deleted");
        Ok(())
    }

    // ── 路径 ────────────────────────────────────────────────────────

    /// 返回 workflows 目录路径。
    pub fn workflows_dir(&self) -> &Path {
        &self.workflows_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::StepConfig;

    fn setup_test_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        (tmp, wf_dir)
    }

    fn create_workflow_file(dir: &Path, name: &str, description: &str, version: &str) {
        let wf_dir = dir.join(name);
        std::fs::create_dir_all(&wf_dir).unwrap();
        let content = format!(
            "---\nworkflow:\n  name: \"{name}\"\n  description: \"{description}\"\n  version: \"{version}\"\n  steps: []\n---\n"
        );
        std::fs::write(wf_dir.join("workflow.md"), content).unwrap();
    }

    #[test]
    fn test_init_scans_workflow_files() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "test-wf", "A test workflow", "1.0");
        create_workflow_file(&wf_dir, "another-wf", "Another one", "2.0");

        let manager = WorkflowManager::new(wf_dir);
        let count = manager.init().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_init_empty_dir() {
        let (_tmp, wf_dir) = setup_test_dir();
        let manager = WorkflowManager::new(wf_dir);
        let count = manager.init().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_init_nonexistent_dir() {
        let manager = WorkflowManager::new(PathBuf::from("/tmp/nonexistent-workflow-dir-12345"));
        let count = manager.init().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_list_names_sorted() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "zulu", "Z", "1.0");
        create_workflow_file(&wf_dir, "alpha", "A", "1.0");
        create_workflow_file(&wf_dir, "mike", "M", "1.0");

        let manager = WorkflowManager::new(wf_dir);
        manager.init().unwrap();
        let names = manager.list_names();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
    }

    #[test]
    fn test_load_cached() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "test-wf", "A test workflow", "1.0");

        let manager = WorkflowManager::new(wf_dir);
        manager.init().unwrap();

        let def1 = manager.load("test-wf").unwrap();
        let def2 = manager.load("test-wf").unwrap(); // should hit cache
        assert_eq!(def1.name, "test-wf");
        assert_eq!(def1.description, "A test workflow");
        assert_eq!(def2.name, "test-wf");
    }

    #[test]
    fn test_load_missing() {
        let (_tmp, wf_dir) = setup_test_dir();
        let manager = WorkflowManager::new(wf_dir);
        manager.init().unwrap();

        let err = manager.load("nonexistent").unwrap_err();
        assert!(matches!(err, WorkflowError::Parse(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_load_workflow_with_steps() {
        let (_tmp, wf_dir) = setup_test_dir();
        let wf_dir_inner = wf_dir.join("multi-step-wf");
        std::fs::create_dir_all(&wf_dir_inner).unwrap();
        let content = "---\nworkflow:\n  name: \"multi-step-wf\"\n  description: \"Multi-step\"\n  version: \"1.0\"\n  steps:\n    - id: \"lint\"\n      name: \"Lint\"\n      type: shell\n      config:\n        command: \"cargo clippy\"\n    - id: \"review\"\n      name: \"Review\"\n      type: agent\n      config:\n        agent: \"@reviewer\"\n        prompt: \"review code\"\n---\n";
        std::fs::write(wf_dir_inner.join("workflow.md"), content).unwrap();

        let manager = WorkflowManager::new(wf_dir);
        manager.init().unwrap();

        let def = manager.load("multi-step-wf").unwrap();
        assert_eq!(def.name, "multi-step-wf");
        assert_eq!(def.description, "Multi-step");
        assert_eq!(def.steps.len(), 2);
        assert_eq!(def.steps[0].id, "lint");
        assert!(matches!(def.steps[0].config, StepConfig::Shell { .. }));
        assert_eq!(def.steps[1].id, "review");
        assert!(matches!(def.steps[1].config, StepConfig::Agent { .. }));
    }

    // ── refresh_one / reload Tier-1 同步测试 ─────────────────────────

    #[test]
    fn test_refresh_one_new_file_adds_tier1_meta() {
        let (_tmp, wf_dir) = setup_test_dir();
        let manager = WorkflowManager::new(wf_dir.clone());
        manager.init().unwrap();
        assert_eq!(manager.list_names().len(), 0);

        // 在磁盘上直接创建新的 workflow.md（不走 create()）
        create_workflow_file(&wf_dir, "new-wf", "New workflow", "1.0");

        // refresh_one 应该发现新文件并添加到 Tier-1
        manager.refresh_one("new-wf");
        let names = manager.list_names();
        assert!(names.contains(&"new-wf".to_string()));

        // Tier-1 meta 也应正确
        let metas = manager.list_meta();
        let meta = metas.iter().find(|m| m.name == "new-wf").unwrap();
        assert_eq!(meta.description, "New workflow");
        assert_eq!(meta.version, "1.0");
    }

    #[test]
    fn test_refresh_one_updates_tier1_meta() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "test-wf", "Original", "1.0");

        let manager = WorkflowManager::new(wf_dir.clone());
        manager.init().unwrap();
        assert_eq!(manager.list_meta()[0].description, "Original");

        // 修改磁盘上的文件（description 和 version 都变了）
        create_workflow_file(&wf_dir, "test-wf", "Updated description", "2.0");

        // refresh_one 应该更新 Tier-1 meta
        manager.refresh_one("test-wf");
        let meta = &manager.list_meta()[0];
        assert_eq!(meta.description, "Updated description");
        assert_eq!(meta.version, "2.0");
    }

    #[test]
    fn test_refresh_one_deleted_file_removes_tier1_meta() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "temp-wf", "Will be deleted", "1.0");

        let manager = WorkflowManager::new(wf_dir.clone());
        manager.init().unwrap();
        assert_eq!(manager.list_names().len(), 1);

        // 删除 workflow 目录
        std::fs::remove_dir_all(wf_dir.join("temp-wf")).unwrap();

        // refresh_one 应该从 Tier-1 中移除
        manager.refresh_one("temp-wf");
        assert_eq!(manager.list_names().len(), 0);
    }

    #[test]
    fn test_refresh_one_corrupted_yaml_preserves_stale_meta() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "corrupt-wf", "Original meta", "1.0");

        let manager = WorkflowManager::new(wf_dir.clone());
        manager.init().unwrap();
        assert_eq!(manager.list_meta()[0].description, "Original meta");

        // 写入损坏的 YAML
        let md_path = wf_dir.join("corrupt-wf").join("workflow.md");
        std::fs::write(&md_path, "this is not valid yaml {{{").unwrap();

        // refresh_one 应该保留旧 meta
        manager.refresh_one("corrupt-wf");
        let metas = manager.list_meta();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].description, "Original meta");
    }

    #[test]
    fn test_reload_deleted_workflow_returns_error_and_cleans_meta() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "temp-wf", "Will be deleted", "1.0");

        let manager = WorkflowManager::new(wf_dir.clone());
        manager.init().unwrap();
        assert_eq!(manager.list_names().len(), 1);

        // 删除 workflow 目录
        std::fs::remove_dir_all(wf_dir.join("temp-wf")).unwrap();

        // reload 应该返回 error 且清理 meta
        let err = manager.reload("temp-wf").unwrap_err();
        assert!(matches!(err, WorkflowError::Parse(_)));
        assert!(err.to_string().contains("not found"));
        assert_eq!(manager.list_names().len(), 0);
    }
}
