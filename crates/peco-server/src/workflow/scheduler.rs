// ============================================================================
// CronScheduler — tokio-cron-scheduler 封装（Workflow 专用）
// ============================================================================
//
// 内部使用 tokio::sync::Mutex<JobScheduler> 而非直接持有 JobScheduler：
// - JobScheduler::shutdown() 需要 &mut self
// - CronScheduler 通过 Arc<CronScheduler> 共享，Arc 无法提供 &mut
// - tokio::sync::Mutex 的 MutexGuard 是 Send 的，可在 .await 间安全传递

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};
use uuid::Uuid;

use crate::state::AppState;

// ============================================================================
// CronScheduler
// ============================================================================

/// Cron 任务调度器（Workflow 专用）。
///
/// # 线程安全
///
/// - `inner: Mutex<JobScheduler>` — tokio::sync::Mutex，保护 shutdown 所需的 &mut 访问
/// - `job_map: RwLock<HashMap>` — 保护复合键 → job_uuid 映射，支持并发读取
///
/// # 复合键
///
/// Job map 使用 `"{user_id}:{workflow_name}"` 作为键，因为 CronScheduler 是
/// AppState 中的单例，不同用户可能拥有同名 Workflow。
pub struct CronScheduler {
    /// 内部调度器实例（tokio::sync::Mutex，跨 .await 安全）。
    inner: Mutex<JobScheduler>,
    /// `"{user_id}:{workflow_name}"` → job_uuid 映射。
    job_map: RwLock<HashMap<String, Uuid>>,
}

impl CronScheduler {
    /// 创建新的 CronScheduler。
    pub async fn new() -> Result<Self, JobSchedulerError> {
        let scheduler = JobScheduler::new().await?;
        Ok(Self {
            inner: Mutex::new(scheduler),
            job_map: RwLock::new(HashMap::new()),
        })
    }

    /// 构建复合键。
    fn make_key(user_id: &str, workflow_name: &str) -> String {
        format!("{user_id}:{workflow_name}")
    }

    /// 注册一个 Workflow 定时任务。
    pub async fn add_workflow(
        &self,
        workflow_name: String,
        cron_expr: String,
        timezone: Option<String>,
        user_id: String,
        pool: SqlitePool,
        state: Arc<AppState>,
    ) -> Result<Uuid, JobSchedulerError> {
        let key = Self::make_key(&user_id, &workflow_name);
        let wf_name_c = workflow_name.clone();
        let user_id_c = user_id.clone();
        let wf_name_log = workflow_name.clone();

        let job = Job::new_async(cron_expr.as_str(), move |_job_uuid, _sched| {
            let workflow_name = wf_name_c.clone();
            let user_id = user_id_c.clone();
            let pool = pool.clone();
            let state = Arc::clone(&state);

            Box::pin(async move {
                crate::workflow::scheduler::execute_scheduled_workflow(
                    workflow_name,
                    user_id,
                    pool,
                    state,
                )
                .await;
            })
        })?;

        let job_uuid = self.inner.lock().await.add(job).await?;

        self.job_map.write().await.insert(key, job_uuid);
        tracing::info!(
            workflow = %wf_name_log,
            user_id = %user_id,
            job_uuid = %job_uuid,
            cron = %cron_expr,
            timezone = ?timezone,
            "Cron job registered for workflow"
        );

        Ok(job_uuid)
    }

    /// 从调度器中移除 workflow 定时任务。
    pub async fn remove_workflow(
        &self,
        workflow_name: &str,
        user_id: &str,
    ) -> Result<(), JobSchedulerError> {
        let key = Self::make_key(user_id, workflow_name);
        let job_uuid = {
            let map = self.job_map.read().await;
            map.get(&key).copied()
        };

        match job_uuid {
            Some(uuid) => {
                self.inner.lock().await.remove(&uuid).await?;
                self.job_map.write().await.remove(&key);
                tracing::info!(
                    workflow = %workflow_name,
                    user_id = %user_id,
                    job_uuid = %uuid,
                    "Cron job removed for workflow"
                );
                Ok(())
            }
            None => {
                tracing::warn!(
                    workflow = %workflow_name,
                    user_id = %user_id,
                    "Attempted to remove unknown cron job for workflow"
                );
                Ok(())
            }
        }
    }

    /// 更新 workflow 的 cron 表达式（先删后加）。
    pub async fn reschedule_workflow(
        &self,
        workflow_name: String,
        cron_expr: String,
        timezone: Option<String>,
        user_id: String,
        pool: SqlitePool,
        state: Arc<AppState>,
    ) -> Result<Uuid, JobSchedulerError> {
        self.remove_workflow(&workflow_name, &user_id).await?;
        self.add_workflow(workflow_name, cron_expr, timezone, user_id, pool, state)
            .await
    }

    /// 检查指定 workflow 是否已注册调度。
    pub async fn contains_workflow(&self, workflow_name: &str, user_id: &str) -> bool {
        self.job_map
            .read()
            .await
            .contains_key(&Self::make_key(user_id, workflow_name))
    }

    /// 启动调度器（在所有 job 注册完成后调用）。
    pub async fn start(&self) -> Result<(), JobSchedulerError> {
        self.inner.lock().await.start().await?;
        tracing::info!("CronScheduler started");
        Ok(())
    }

    /// 优雅关闭调度器。
    pub async fn shutdown(&self) -> Result<(), JobSchedulerError> {
        self.inner.lock().await.shutdown().await?;
        tracing::info!("CronScheduler shut down");
        Ok(())
    }

    /// 返回当前已注册的 job 数量。
    pub async fn job_count(&self) -> usize {
        self.job_map.read().await.len()
    }
}

// ============================================================================
// execute_scheduled_workflow — 由 Cron Job closure 调用
// ============================================================================

/// 由定时调度触发执行一次 Workflow。
///
/// # 流程
///
/// 1. 创建 per-user persister + 写入 workflow_executions (status=running)
/// 2. 获取 AgentAccess（通过 WorkspaceManager）
/// 3. WorkflowManager::execute() 启动引擎
/// 4. 消费事件流，引擎内部自动通过 persister 持久化
/// 5. 最终状态通过 persister.save() 写入 DB
async fn execute_scheduled_workflow(
    workflow_name: String,
    user_id: String,
    pool: SqlitePool,
    state: Arc<AppState>,
) {
    use chrono::Utc;
    use std::sync::Arc;

    let run_id = Uuid::new_v4().to_string();
    let started_at = Utc::now();

    tracing::info!(
        workflow = %workflow_name,
        user_id = %user_id,
        run_id = %run_id,
        "Scheduled workflow triggered"
    );

    // 1. 获取用户 WorkSpace
    let ws = match state.workspace_manager.get(&user_id) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "Failed to get workspace for scheduled workflow execution"
            );
            return;
        }
    };

    // 2. 加载 Workflow 定义
    let definition = match ws.workflow_manager().load(&workflow_name) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(workflow = %workflow_name, error = %e, "Failed to load workflow");
            return;
        }
    };

    // 3. 验证输入（定时触发无外部输入）
    let inputs = std::collections::HashMap::new();
    if let Err(e) = definition.validate_inputs(&inputs) {
        tracing::error!(workflow = %workflow_name, error = %e, "Input validation failed");
        return;
    }

    // 4. 创建 per-user persister + 写入初始记录
    let persister = Arc::new(crate::workflow::persister::SqliteWorkflowPersister::new(
        pool.clone(),
        user_id.clone(),
    ));
    if let Err(e) = crate::db::workflow_executions::insert(
        &pool,
        &crate::db::workflow_executions::CreateExecutionParams {
            id: run_id.clone(),
            user_id: user_id.clone(),
            workflow_name: workflow_name.clone(),
            trigger_type: "scheduled".to_string(),
            inputs_json: None,
            total_steps: definition.steps.len(),
            started_at: started_at.to_rfc3339(),
        },
    )
    .await
    {
        tracing::error!(run_id = %run_id, error = %e, "Failed to insert execution record");
        return;
    }

    // 5. 启动引擎。ws 自身实现 AgentAccess，直接传入。
    let config = peco_core::workflow::WorkflowConfig::default();
    let handle = match ws.workflow_manager().execute(
        &workflow_name,
        ws.clone(),
        persister.clone(),
        config,
        inputs,
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(workflow = %workflow_name, run_id = %run_id, error = %e, "Failed to execute workflow");
            return;
        }
    };

    // 6. 注册到 ActiveExecutions（后台消费事件，支持取消/审批/SSE）
    crate::workflow::active::insert_run(&run_id, handle).await;

    tracing::info!(
        workflow = %workflow_name,
        run_id = %run_id,
        "Scheduled workflow execution started"
    );
}
