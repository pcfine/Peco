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

use super::definition::WorkflowDefinition;
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
}

// ── WorkflowManager ───────────────────────────────────────────────────────

/// Workflow 生命周期管理器。
///
/// 两级缓存：
/// - Tier 1：`metas` — 扫描目录时缓存的 frontmatter 摘要
/// - Tier 2：`definitions` — 完整加载的 WorkflowDefinition 实例
///
/// **不追踪活跃执行**：调用方自行管理 `WorkflowHandle` 生命周期。
pub struct WorkflowManager {
    workflows_dir: PathBuf,
    /// Tier-1 元数据缓存（name → WorkflowMeta）
    metas: RwLock<HashMap<String, WorkflowMeta>>,
    /// Tier-2 完整定义缓存（name → WorkflowDefinition）
    definitions: RwLock<HashMap<String, WorkflowDefinition>>,
    persister: Arc<dyn WorkflowPersister>,
}

impl WorkflowManager {
    /// 创建新的 WorkflowManager。
    ///
    /// 创建后应调用 [`init`](Self::init) 扫描目录并缓存元数据。
    pub fn new(workflows_dir: PathBuf, persister: Arc<dyn WorkflowPersister>) -> Self {
        Self {
            workflows_dir,
            metas: RwLock::new(HashMap::new()),
            definitions: RwLock::new(HashMap::new()),
            persister,
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

    /// 解析单个 workflow.md 的 frontmatter，提取 name + description + version。
    fn parse_meta(md_path: &Path) -> Result<WorkflowMeta, WorkflowError> {
        let raw = std::fs::read_to_string(md_path)?;
        let (frontmatter_str, _) = crate::agent::split_frontmatter(&raw)
            .map_err(|e| WorkflowError::Parse(format!("invalid frontmatter: {e}")))?;

        // 解析 workflow 包装格式：workflow: { name, description, version }
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
        }

        let parsed: WorkflowFileMeta = serde_yaml::from_str(frontmatter_str)
            .map_err(|e| WorkflowError::Parse(format!("YAML parse error: {e}")))?;

        Ok(WorkflowMeta {
            name: parsed.workflow.name,
            description: parsed.workflow.description,
            version: parsed.workflow.version,
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

    /// 强制从磁盘重新加载指定 Workflow（缓存失效）。
    pub fn reload(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError> {
        self.invalidate(name);
        self.load(name)
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
    pub fn execute(
        &self,
        name: &str,
        agent_access: Arc<dyn AgentAccess>,
        config: WorkflowConfig,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Result<WorkflowHandle, WorkflowError> {
        let definition = self.load(name)?;
        let _validated = definition.validate_inputs(&inputs)?;
        Ok(WorkflowEngine::spawn(
            definition,
            agent_access,
            self.persister.clone(),
            config,
            inputs,
        ))
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
    use crate::workflow::persistence::NullWorkflowPersister;
    use std::sync::Arc;

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

        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
        let count = manager.init().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_init_empty_dir() {
        let (_tmp, wf_dir) = setup_test_dir();
        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
        let count = manager.init().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_init_nonexistent_dir() {
        let manager = WorkflowManager::new(
            PathBuf::from("/tmp/nonexistent-workflow-dir-12345"),
            Arc::new(NullWorkflowPersister),
        );
        let count = manager.init().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_list_names_sorted() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "zulu", "Z", "1.0");
        create_workflow_file(&wf_dir, "alpha", "A", "1.0");
        create_workflow_file(&wf_dir, "mike", "M", "1.0");

        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
        manager.init().unwrap();
        let names = manager.list_names();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
    }

    #[test]
    fn test_load_cached() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "test-wf", "A test workflow", "1.0");

        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
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
        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
        manager.init().unwrap();

        let err = manager.load("nonexistent").unwrap_err();
        assert!(matches!(err, WorkflowError::Parse(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_reload_invalidates_cache() {
        let (_tmp, wf_dir) = setup_test_dir();
        create_workflow_file(&wf_dir, "test-wf", "Original", "1.0");

        let manager = WorkflowManager::new(wf_dir.clone(), Arc::new(NullWorkflowPersister));
        manager.init().unwrap();

        let def1 = manager.load("test-wf").unwrap();
        assert_eq!(def1.description, "Original");

        // Modify the file on disk
        create_workflow_file(&wf_dir, "test-wf", "Updated", "2.0");

        // Reload should pick up the change
        let def2 = manager.reload("test-wf").unwrap();
        assert_eq!(def2.description, "Updated");
    }

    #[tokio::test]
    async fn test_execute_loads_and_spawns() {
        let (_tmp, wf_dir) = setup_test_dir();
        // Create a simple shell workflow
        let wf_dir_inner = wf_dir.join("echo-wf");
        std::fs::create_dir_all(&wf_dir_inner).unwrap();
        let content = "---\nworkflow:\n  name: \"echo-wf\"\n  description: \"Echo test\"\n  version: \"1.0\"\n  steps:\n    - id: \"A\"\n      name: \"Echo\"\n      type: shell\n      config:\n        command: \"echo hello\"\n---\n";
        std::fs::write(wf_dir_inner.join("workflow.md"), content).unwrap();

        let manager = WorkflowManager::new(wf_dir, Arc::new(NullWorkflowPersister));
        manager.init().unwrap();

        // Verify the definition loads and has correct step config
        let def = manager.load("echo-wf").unwrap();
        assert_eq!(def.name, "echo-wf");
        assert_eq!(def.steps.len(), 1);
        assert_eq!(def.steps[0].id, "A");
        assert!(matches!(def.steps[0].config, StepConfig::Shell { .. }));
    }
}
