// ============================================================================
// WorkflowPersister trait + NullWorkflowPersister + WorkflowSnapshot
// ============================================================================

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::definition::{StepResult, WorkflowDefinition};
use super::error::WorkflowError;

// ============================================================================
// WorkflowSnapshotState
// ============================================================================

/// 快照状态。
///
/// 不包含 `Cancelled` 变体 — 取消是终态，不可恢复。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSnapshotState {
    /// 执行中（每层完成后快照）
    Running,
    /// 暂停等待审批
    Paused,
    /// 已完成
    Completed,
    /// 已失败
    Failed,
}

// ============================================================================
// WorkflowSnapshot
// ============================================================================

/// Workflow 执行快照，支持断点续执行。
///
/// **注意**：不序列化 `TemplateContext`（含 minijinja Environment，不可序列化）。
/// 恢复时从 `step_results` 重建：`TemplateContext::new()` + 逐个 `set_step_result()`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    pub run_id: String,
    pub workflow_name: String,
    pub definition: WorkflowDefinition,
    pub state: WorkflowSnapshotState,
    /// 外部输入参数 JSON（创建执行时传入，用于恢复/审计）。
    pub inputs_json: Option<String>,
    /// 已完成步骤的结果（用于重建 TemplateContext）
    pub step_results: HashMap<String, StepResult>,
    /// 当前执行到的层级索引（0-based）
    pub current_level: usize,
    /// 总步骤数（独立于 definition.steps.len()，避免 load-then-save 时因 definition 空壳而清零）。
    pub total_steps: usize,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ============================================================================
// WorkflowPersister trait
// ============================================================================

/// Workflow 执行持久化接口。
///
/// 遵循 `SessionPersister` 的 trait 抽象模式。
/// peco-core 仅定义 trait，实现由 peco-server（SQLite）提供。
#[async_trait]
pub trait WorkflowPersister: Send + Sync {
    /// 保存 Workflow 执行快照。
    async fn save(&self, snapshot: &WorkflowSnapshot) -> Result<(), WorkflowError>;

    /// 加载 Workflow 执行快照。
    async fn load(&self, run_id: &str) -> Result<Option<WorkflowSnapshot>, WorkflowError>;

    /// 删除 Workflow 执行记录。
    async fn delete(&self, run_id: &str) -> Result<(), WorkflowError>;

    /// 列出所有 Workflow 执行记录（由 persister 内部持有用户上下文过滤）。
    async fn list(&self) -> Result<Vec<WorkflowSnapshot>, WorkflowError>;
}

// ============================================================================
// NullWorkflowPersister
// ============================================================================

/// 空持久化实现（测试/CLI 场景使用）。
///
/// 所有操作均为 no-op：save 不写盘，load 始终返回 None。
pub struct NullWorkflowPersister;

#[async_trait]
impl WorkflowPersister for NullWorkflowPersister {
    async fn save(&self, _snapshot: &WorkflowSnapshot) -> Result<(), WorkflowError> {
        Ok(())
    }

    async fn load(&self, _run_id: &str) -> Result<Option<WorkflowSnapshot>, WorkflowError> {
        Ok(None)
    }

    async fn delete(&self, _run_id: &str) -> Result<(), WorkflowError> {
        Ok(())
    }

    async fn list(&self) -> Result<Vec<WorkflowSnapshot>, WorkflowError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::{OnFailure, StepConfig, StepOutcome, StepType, WorkflowStep};
    use std::time::Duration;

    fn make_test_snapshot() -> WorkflowSnapshot {
        let step = WorkflowStep {
            id: "A".into(),
            name: "Step A".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo hello".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: None,
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        };

        let result = StepResult {
            step: step.clone(),
            outcome: StepOutcome::Success("hello".into()),
            output: Some("hello".into()),
            structured_output: None,
            duration: Duration::from_millis(100),
            attempt: 1,
        };

        let mut step_results = HashMap::new();
        step_results.insert("A".to_string(), result);

        WorkflowSnapshot {
            run_id: "test-run-id".into(),
            workflow_name: "test-workflow".into(),
            definition: WorkflowDefinition {
                name: "test-workflow".into(),
                description: "test".into(),
                version: "1.0".into(),
                timeout_seconds: None,
                inputs: HashMap::new(),
                steps: vec![step],
                body: None,
            },
            state: WorkflowSnapshotState::Completed,
            inputs_json: None,
            step_results,
            current_level: 1,
            total_steps: 1,
            started_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_snapshot_serde_roundtrip() {
        let snapshot = make_test_snapshot();
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: WorkflowSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, snapshot.run_id);
        assert_eq!(restored.workflow_name, snapshot.workflow_name);
        assert_eq!(restored.state, snapshot.state);
        assert_eq!(restored.current_level, snapshot.current_level);
        assert_eq!(restored.step_results.len(), 1);
        assert!(restored.step_results.contains_key("A"));
    }

    #[test]
    fn test_snapshot_state_serialization() {
        let states = vec![
            (WorkflowSnapshotState::Running, "running"),
            (WorkflowSnapshotState::Paused, "paused"),
            (WorkflowSnapshotState::Completed, "completed"),
            (WorkflowSnapshotState::Failed, "failed"),
        ];

        for (state, expected_str) in states {
            let json = serde_json::to_string(&state).unwrap();
            assert!(
                json.contains(expected_str),
                "Expected '{expected_str}' in JSON for {state:?}, got: {json}"
            );
            let restored: WorkflowSnapshotState = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, state);
        }
    }

    #[tokio::test]
    async fn test_null_persister_save_ok() {
        let snapshot = make_test_snapshot();
        let persister = NullWorkflowPersister;
        assert!(persister.save(&snapshot).await.is_ok());
    }

    #[tokio::test]
    async fn test_null_persister_load_none() {
        let persister = NullWorkflowPersister;
        let result = persister.load("any-id").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_null_persister_delete_ok() {
        let persister = NullWorkflowPersister;
        assert!(persister.delete("any-id").await.is_ok());
    }

    #[tokio::test]
    async fn test_null_persister_list_empty() {
        let persister = NullWorkflowPersister;
        let result = persister.list().await.unwrap();
        assert!(result.is_empty());
    }
}
