// ============================================================================
// SqliteWorkflowPersister — WorkflowPersister trait 的 SQLite 实现
// ============================================================================
//
// 每个实例绑定一个用户（构造时注入 user_id），通过内部持有 user_id
// 实现用户隔离。不同用户的 workflow 执行记录通过 WHERE user_id = ? 隔离。

use async_trait::async_trait;
use peco_core::workflow::WorkflowError;
use peco_core::workflow::{
    WorkflowDefinition, WorkflowPersister, WorkflowSnapshot, WorkflowSnapshotState,
};
use sqlx::SqlitePool;

use crate::db;

/// Workflow 执行记录的 SQLite 持久化实现。
pub struct SqliteWorkflowPersister {
    pool: SqlitePool,
    user_id: String,
}

impl SqliteWorkflowPersister {
    /// 创建指定用户的 persister 实例。
    pub fn new(pool: SqlitePool, user_id: String) -> Self {
        Self { pool, user_id }
    }
}

/// 从 WorkflowSnapshot 构建用于持久化的 JSON（剥离 definition 字段）。
///
/// definition 可在恢复时从 workflow.md 文件重新加载，无需重复存储。
fn snapshot_to_db_json(snapshot: &WorkflowSnapshot) -> serde_json::Value {
    serde_json::json!({
        "run_id": snapshot.run_id,
        "workflow_name": snapshot.workflow_name,
        "state": snapshot.state,
        "inputs_json": snapshot.inputs_json,
        "step_results": snapshot.step_results,
        "current_level": snapshot.current_level,
        "total_steps": snapshot.total_steps,
        "started_at": snapshot.started_at.to_rfc3339(),
        "updated_at": snapshot.updated_at.to_rfc3339(),
    })
}

/// 将 DB 中的快照 JSON 还原为 WorkflowSnapshot。
///
/// `workflow_name` 和 `run_id` 从 DB 列中获取（非 JSON 内）。
/// `definition` 需要调用方从文件系统补全。
fn db_json_to_snapshot(
    run_id: &str,
    workflow_name: &str,
    state: &str,
    snapshot_json: Option<&str>,
    _steps_completed: i64,
    _steps_failed: i64,
    _steps_skipped: i64,
    _total_duration_ms: Option<i64>,
    _error: Option<&str>,
    started_at: &str,
    finished_at: Option<&str>,
) -> Result<WorkflowSnapshot, WorkflowError> {
    let (step_results, current_level, inputs_json, total_steps) = if let Some(json_str) = snapshot_json {
        let parsed: serde_json::Value =
            serde_json::from_str(json_str).map_err(|e| WorkflowError::Persist(e.to_string()))?;
        let results: std::collections::HashMap<String, peco_core::workflow::StepResult> =
            serde_json::from_value(
                parsed
                    .get("step_results")
                    .cloned()
                    .unwrap_or(serde_json::json!({})),
            )
            .map_err(|e| WorkflowError::Persist(format!("step_results deserialize: {e}")))?;
        let level: usize = parsed
            .get("current_level")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let inputs: Option<String> = parsed.get("inputs_json").and_then(|v| {
            if v.is_null() {
                None
            } else {
                Some(v.to_string())
            }
        });
        let ts: usize = parsed
            .get("total_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        (results, level, inputs, ts)
    } else {
        (Default::default(), 0, None, 0)
    };

    let snapshot_state = match state {
        "running" => WorkflowSnapshotState::Running,
        "paused" => WorkflowSnapshotState::Paused,
        "completed" => WorkflowSnapshotState::Completed,
        "failed" => WorkflowSnapshotState::Failed,
        _ => WorkflowSnapshotState::Failed,
    };

    let started = started_at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .map_err(|e| WorkflowError::Persist(format!("invalid started_at '{started_at}': {e}")))?;
    let updated = finished_at
        .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
        .unwrap_or(started);

    // definition 留空 — 调用方在 load() 中从文件补全
    let definition = WorkflowDefinition {
        name: workflow_name.to_string(),
        description: String::new(),
        version: String::new(),
        timeout_seconds: None,
        inputs: Default::default(),
        steps: vec![],
        body: None,
    };

    Ok(WorkflowSnapshot {
        run_id: run_id.to_string(),
        workflow_name: workflow_name.to_string(),
        definition,
        state: snapshot_state,
        inputs_json,
        step_results,
        current_level,
        total_steps,
        started_at: started,
        updated_at: updated,
    })
}

#[async_trait]
impl WorkflowPersister for SqliteWorkflowPersister {
    async fn save(&self, snapshot: &WorkflowSnapshot) -> Result<(), WorkflowError> {
        let db_json = snapshot_to_db_json(snapshot);
        let json_str =
            serde_json::to_string(&db_json).map_err(|e| WorkflowError::Persist(e.to_string()))?;

        let status = match snapshot.state {
            WorkflowSnapshotState::Running => "running",
            WorkflowSnapshotState::Paused => "paused",
            WorkflowSnapshotState::Completed => "completed",
            WorkflowSnapshotState::Failed => "failed",
        };

        let total_steps = snapshot.total_steps as i64;
        let steps_completed = snapshot
            .step_results
            .values()
            .filter(|r| r.outcome.is_success())
            .count() as i64;
        let steps_failed = snapshot
            .step_results
            .values()
            .filter(|r| r.outcome.is_failed())
            .count() as i64;
        let steps_skipped = snapshot.step_results.len() as i64 - steps_completed - steps_failed;

        let started_str = snapshot.started_at.to_rfc3339();
        let finished_str = if matches!(
            snapshot.state,
            WorkflowSnapshotState::Completed | WorkflowSnapshotState::Failed
        ) {
            Some(snapshot.updated_at.to_rfc3339())
        } else {
            None
        };

        // UPSERT 模式：先尝试 UPDATE，无行影响则 INSERT
        let rows = sqlx::query(
            "UPDATE workflow_executions SET status = ?, steps_completed = ?, \
             steps_failed = ?, steps_skipped = ?, snapshot_json = ?, \
             total_steps = ?, finished_at = ? \
             WHERE id = ? AND user_id = ?",
        )
        .bind(status)
        .bind(steps_completed)
        .bind(steps_failed)
        .bind(steps_skipped)
        .bind(&json_str)
        .bind(total_steps)
        .bind(finished_str.as_deref())
        .bind(&snapshot.run_id)
        .bind(&self.user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| WorkflowError::Persist(format!("save workflow execution: {e}")))?
        .rows_affected();

        if rows == 0 {
            // UPSERT fallback：尝试读取已有记录的 trigger_type
            let trigger_type: String =
                sqlx::query_scalar("SELECT trigger_type FROM workflow_executions WHERE id = ?")
                    .bind(&snapshot.run_id)
                    .fetch_optional(&self.pool)
                    .await
                    .unwrap_or(None)
                    .unwrap_or_else(|| "manual".to_string());

            sqlx::query(
                "INSERT INTO workflow_executions (id, user_id, workflow_name, trigger_type, \
                 status, inputs_json, total_steps, steps_completed, steps_failed, \
                 steps_skipped, snapshot_json, started_at, finished_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&snapshot.run_id)
            .bind(&self.user_id)
            .bind(&snapshot.workflow_name)
            .bind(trigger_type)
            .bind(status)
            .bind(&snapshot.inputs_json)
            .bind(total_steps)
            .bind(steps_completed)
            .bind(steps_failed)
            .bind(steps_skipped)
            .bind(&json_str)
            .bind(&started_str)
            .bind(finished_str.as_deref())
            .execute(&self.pool)
            .await
            .map_err(|e| WorkflowError::Persist(format!("insert workflow execution: {e}")))?;
        }

        tracing::debug!(
            run_id = %snapshot.run_id,
            state = status,
            "Workflow execution persisted"
        );
        Ok(())
    }

    async fn load(&self, run_id: &str) -> Result<Option<WorkflowSnapshot>, WorkflowError> {
        let row = db::workflow_executions::find_by_id_and_user(&self.pool, run_id, &self.user_id)
            .await
            .map_err(|e| WorkflowError::Persist(format!("load workflow execution: {e}")))?;

        match row {
            Some(r) => {
                let snapshot = db_json_to_snapshot(
                    &r.id,
                    &r.workflow_name,
                    &r.status,
                    r.snapshot_json.as_deref(),
                    r.steps_completed,
                    r.steps_failed,
                    r.steps_skipped,
                    r.total_duration_ms,
                    r.error.as_deref(),
                    &r.started_at,
                    r.finished_at.as_deref(),
                )?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, run_id: &str) -> Result<(), WorkflowError> {
        sqlx::query("DELETE FROM workflow_executions WHERE id = ? AND user_id = ?")
            .bind(run_id)
            .bind(&self.user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| WorkflowError::Persist(format!("delete workflow execution: {e}")))?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<WorkflowSnapshot>, WorkflowError> {
        let rows = db::workflow_executions::list_by_user(&self.pool, &self.user_id, 0, 1000)
            .await
            .map_err(|e| WorkflowError::Persist(format!("list workflow executions: {e}")))?;

        let mut snapshots = Vec::with_capacity(rows.len());
        for r in &rows {
            let snapshot = db_json_to_snapshot(
                &r.id,
                &r.workflow_name,
                &r.status,
                r.snapshot_json.as_deref(),
                r.steps_completed,
                r.steps_failed,
                r.steps_skipped,
                r.total_duration_ms,
                r.error.as_deref(),
                &r.started_at,
                r.finished_at.as_deref(),
            )?;
            snapshots.push(snapshot);
        }

        Ok(snapshots)
    }
}
