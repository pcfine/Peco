// ============================================================================
// CronScheduler — tokio-cron-scheduler 封装
// ============================================================================
//
// 内部使用 tokio::sync::Mutex<JobScheduler> 而非直接持有 JobScheduler：
// - JobScheduler::shutdown() 需要 &mut self
// - CronScheduler 通过 Arc<CronScheduler> 共享，Arc 无法提供 &mut
// - tokio::sync::Mutex 的 MutexGuard 是 Send 的，可在 .await 间安全传递
//
// JobScheduler::add / remove / start 本身接受 &self，只有 shutdown 需要 &mut。

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};
use uuid::Uuid;

use crate::state::AppState;

use super::executor;

// ============================================================================
// CronScheduler
// ============================================================================

/// Cron 任务调度器。
///
/// # 线程安全
///
/// - `inner: Mutex<JobScheduler>` — tokio::sync::Mutex，保护 shutdown 所需的 &mut 访问
/// - `job_map: RwLock<HashMap>` — 保护 task_id → job_uuid 映射，支持并发读取
pub struct CronScheduler {
    /// 内部调度器实例（tokio::sync::Mutex，跨 .await 安全）。
    inner: Mutex<JobScheduler>,
    /// task_id → job_uuid 映射，用于按 task_id 移除/更新 job。
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

    /// 注册一个新的定时任务。
    ///
    /// `Job::new_async` 的 closure 要求 `FnMut + Send + Sync + 'static`，
    /// 每次触发时 closure 被重复调用，因此所有捕获的变量在 closure 内部 clone。
    #[allow(clippy::too_many_arguments)]
    pub async fn add_task(
        &self,
        task_id: String,
        cron_expr: String,
        agent_id: String,
        user_id: String,
        prompt: String,
        pool: SqlitePool,
        state: Arc<AppState>,
    ) -> Result<Uuid, JobSchedulerError> {
        let task_id_c = task_id.clone();
        let task_id_for_log = task_id.clone();
        let job = Job::new_async(cron_expr.as_str(), move |_job_uuid, _sched| {
            // ★ 每次触发时 clone 所有捕获状态
            let task_id = task_id_c.clone();
            let agent_id = agent_id.clone();
            let user_id = user_id.clone();
            let prompt = prompt.clone();
            let pool = pool.clone();
            let state = Arc::clone(&state);

            Box::pin(async move {
                executor::execute_task(task_id, agent_id, user_id, prompt, pool, state).await;
            })
        })?;

        let job_uuid = self.inner.lock().await.add(job).await?;

        self.job_map.write().await.insert(task_id, job_uuid);
        tracing::info!(
            task_id = %task_id_for_log,
            job_uuid = %job_uuid,
            cron = %cron_expr,
            "Cron job registered"
        );

        Ok(job_uuid)
    }

    /// 从调度器中移除任务。
    pub async fn remove_task(&self, task_id: &str) -> Result<(), JobSchedulerError> {
        let job_uuid = {
            let map = self.job_map.read().await;
            map.get(task_id).copied()
        };

        match job_uuid {
            Some(uuid) => {
                self.inner.lock().await.remove(&uuid).await?;
                self.job_map.write().await.remove(task_id);
                tracing::info!(task_id = %task_id, job_uuid = %uuid, "Cron job removed");
                Ok(())
            }
            None => {
                tracing::warn!(task_id = %task_id, "Attempted to remove unknown cron job");
                Ok(())
            }
        }
    }

    /// 按 task_id 更新 cron 表达式（先移除再新增）。
    #[allow(clippy::too_many_arguments)]
    pub async fn reschedule(
        &self,
        task_id: String,
        cron_expr: String,
        agent_id: String,
        user_id: String,
        prompt: String,
        pool: SqlitePool,
        state: Arc<AppState>,
    ) -> Result<Uuid, JobSchedulerError> {
        self.remove_task(&task_id).await?;
        self.add_task(task_id, cron_expr, agent_id, user_id, prompt, pool, state)
            .await
    }

    /// 检查指定 task 是否在调度器中。
    pub async fn contains(&self, task_id: &str) -> bool {
        self.job_map.read().await.contains_key(task_id)
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
