//! `save_schedule` 工具的实现侧：把「空间 ws_id 或 label」解析成一条排期。
//!
//! 工具在 peco-core 里只认 [`ScheduleAccess`] 这个窄接口 —— cron 计算、目标解析
//! 与落库都在 daemon：core 不该知道 `workspaces` 表长什么样。

use std::sync::Arc;

use chrono::{Local, Utc};
use peco_core::tools::{NewSchedule, ScheduleAccess, ScheduleError, ScheduleInfo};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db::schedules::{self, NewTask, ScheduledTaskRow};

use super::{cron, utc_text};

/// 预览几个接下来的时刻。与 `/api/schedules/preview` 同一个数：工具回执与 HTTP
/// 视图说的是同一件事。
const PREVIEW_COUNT: usize = 3;

pub struct DaemonScheduleAccess {
    pool: SqlitePool,
}

impl DaemonScheduleAccess {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 目标解析：先当 ws_id，再当 `label` / `team_id`。
    ///
    /// 两种写法都接受，是因为模型手上可能只有「研发空间」这样的名字；而 ws_id 是
    /// 唯一确定的，两者同名时它优先。命中多个就是多义，宁可让模型问清楚，也不替
    /// 它挑一个 —— 投错空间的代价是内容出现在别人的会话里。
    async fn resolve_target(&self, target: &str) -> Result<String, ScheduleError> {
        let target = target.trim();
        if target.is_empty() {
            return Err(ScheduleError::new(
                "invalid_schedule",
                "target must not be empty".to_string(),
            ));
        }
        if let Some(ws_id) = schedules_targets::by_id(&self.pool, target)
            .await
            .map_err(db_error)?
        {
            return Ok(ws_id);
        }
        let mut hits = schedules_targets::by_name(&self.pool, target)
            .await
            .map_err(db_error)?;
        match hits.len() {
            0 => Err(ScheduleError::new(
                "space_not_found",
                format!("no workspace matches '{target}'"),
            )),
            1 => Ok(hits.remove(0)),
            _ => Err(ScheduleError::with_candidates(
                "ambiguous_target",
                format!("'{target}' matches {} workspaces", hits.len()),
                hits,
            )),
        }
    }
}

/// 目标空间的最小查询面；与 `db::workspaces` 分开，是因为这里要的只是
/// 「id 或名字 → (ws_id, label)」这一件事。
mod schedules_targets {
    use sqlx::SqlitePool;

    pub async fn by_id(pool: &SqlitePool, ws_id: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT id FROM workspaces WHERE id = ?")
            .bind(ws_id)
            .fetch_optional(pool)
            .await
    }

    /// 按人读名字找：`label` 与 `team_id` 都算（用户可能只记得团队名）。
    pub async fn by_name(pool: &SqlitePool, name: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT id FROM workspaces WHERE label = ? OR team_id = ? ORDER BY id")
            .bind(name)
            .bind(name)
            .fetch_all(pool)
            .await
    }
}

fn db_error(e: sqlx::Error) -> ScheduleError {
    ScheduleError::new("invalid_schedule", format!("db_error: {e}"))
}

#[async_trait::async_trait]
impl ScheduleAccess for DaemonScheduleAccess {
    async fn save(&self, request: NewSchedule) -> Result<ScheduleInfo, ScheduleError> {
        let name = request.name.trim();
        let prompt = request.prompt.trim();
        if name.is_empty() || prompt.is_empty() {
            return Err(ScheduleError::new(
                "invalid_schedule",
                "name and prompt must not be empty".to_string(),
            ));
        }
        // 存模型写的原文：归一化只发生在解析侧，库里因此留着它填的那串字符，
        // 改一遍再读回来不会凭空换形。
        let cron_expr = request.cron_expr.trim().to_string();
        cron::validate(&cron_expr).map_err(|e| {
            ScheduleError::new("invalid_cron", format!("invalid cron expression: {e}"))
        })?;
        let target_ws_id = self.resolve_target(&request.target).await?;

        let next = if request.enabled {
            cron::next_occurrence(&cron_expr, Local::now())
                .map(|at| utc_text(at.with_timezone(&Utc)))
                .ok()
        } else {
            None
        };
        let id = Uuid::new_v4().to_string();
        schedules::insert_task(
            &self.pool,
            &NewTask {
                id: &id,
                name,
                target_ws_id: &target_ws_id,
                prompt,
                cron_expr: &cron_expr,
                enabled: request.enabled,
                next_run_at: next.as_deref(),
            },
        )
        .await
        .map_err(db_error)?;

        let row = schedules::task_by_id(&self.pool, &id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| ScheduleError::new("invalid_schedule", "the schedule vanished"))?;
        Ok(info_of(&row, &cron_expr))
    }
}

/// 回执内容与 `GET /api/schedules` 的字段集对齐，只是不带时间戳。
fn info_of(row: &ScheduledTaskRow, cron_expr: &str) -> ScheduleInfo {
    ScheduleInfo {
        id: row.id.clone(),
        name: row.name.clone(),
        target_ws_id: row.target_ws_id.clone(),
        target_label: row.target_label.clone(),
        cron_expr: cron_expr.to_string(),
        enabled: row.enabled,
        next: cron::next_occurrences(cron_expr, Local::now(), PREVIEW_COUNT)
            .map(|times| {
                times
                    .into_iter()
                    .map(|at| utc_text(at.with_timezone(&Utc)))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 供 `main` 用的一行装配：返回 `Err` 说明进程级 OnceLock 已被别人占用。
pub fn install(pool: SqlitePool) -> Result<(), Arc<dyn ScheduleAccess>> {
    let access: Arc<dyn ScheduleAccess> = Arc::new(DaemonScheduleAccess::new(pool));
    peco_core::tools::install_schedule_access(access)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO teams (id, display_name) VALUES ('ops', 'ops'), ('writing', 'writing'); \
             INSERT INTO workspaces (id, work_dir, team_id, label) \
             VALUES ('ws_ops', '/tmp/ops', 'ops', '研发'), \
                    ('ws_write', '/tmp/write', 'writing', '写作');",
        )
        .execute(&pool)
        .await
        .unwrap();
        (pool, dir)
    }

    fn request(target: &str) -> NewSchedule {
        NewSchedule {
            name: "每日站会".into(),
            target: target.into(),
            prompt: "汇报昨天的进展".into(),
            cron_expr: "0 9 * * *".into(),
            enabled: true,
        }
    }

    /// 目标写 ws_id 或 label 都行，回执要给出落库之后的真实字段。
    #[tokio::test]
    async fn the_target_accepts_a_ws_id_or_a_label() {
        let (pool, _dir) = test_pool().await;
        let access = DaemonScheduleAccess::new(pool.clone());

        let by_id = access
            .save(request("ws_ops"))
            .await
            .expect("ws_id 必须命中");
        assert_eq!(by_id.target_ws_id, "ws_ops");
        assert_eq!(by_id.target_label, "研发");
        assert_eq!(
            by_id.cron_expr, "0 9 * * *",
            "落库的是模型写的原文，5 字段不被补成 6 字段"
        );
        assert_eq!(by_id.next.len(), 3, "回执带三个接下来的时刻");
        for pair in by_id.next.windows(2) {
            assert!(pair[0] < pair[1], "预览必须严格递增：{:?}", by_id.next);
        }

        let by_label = access.save(request("写作")).await.expect("label 必须命中");
        assert_eq!(by_label.target_ws_id, "ws_write");
        assert_eq!(by_label.target_label, "写作");

        let rows = schedules::list_tasks(&pool).await.unwrap();
        assert_eq!(rows.len(), 2, "两次保存各落一行");
        assert!(rows.iter().all(|row| row.next_run_at.is_some()));
    }

    /// 同名目标命中多个空间时必须报多义并列出候选：替模型猜一个，内容就会进错空间的
    /// 会话。
    #[tokio::test]
    async fn an_ambiguous_target_lists_the_candidates() {
        let (pool, _dir) = test_pool().await;
        // 两个空间同属一个团队：`team_id` 因此撞在一起，名字解析无法二选一。
        sqlx::raw_sql(
            "INSERT INTO workspaces (id, work_dir, team_id, label) \
             VALUES ('ws_ops_2', '/tmp/ops2', 'ops', 'ops');",
        )
        .execute(&pool)
        .await
        .unwrap();
        let access = DaemonScheduleAccess::new(pool.clone());

        let error = access
            .save(request("ops"))
            .await
            .expect_err("同团队的两个空间不能猜");

        assert_eq!(error.code, "ambiguous_target");
        assert_eq!(error.candidates, vec!["ws_ops", "ws_ops_2"]);
        assert!(
            schedules::list_tasks(&pool).await.unwrap().is_empty(),
            "报错时一行都不该落库"
        );
    }

    #[tokio::test]
    async fn an_unknown_target_is_a_space_not_found() {
        let (pool, _dir) = test_pool().await;
        let access = DaemonScheduleAccess::new(pool.clone());

        let error = access
            .save(request("不存在的空间"))
            .await
            .expect_err("必须报错");

        assert_eq!(error.code, "space_not_found");
    }

    #[tokio::test]
    async fn a_broken_cron_is_reported_before_anything_is_written() {
        let (pool, _dir) = test_pool().await;
        let access = DaemonScheduleAccess::new(pool.clone());
        let mut broken = request("ws_ops");
        broken.cron_expr = "0 0 99 * * *".into();

        let error = access.save(broken).await.expect_err("越界的小时必须被拒");

        assert_eq!(error.code, "invalid_cron");
        assert!(!error.message.is_empty(), "错误要带具体原因");
        assert!(schedules::list_tasks(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_name_or_prompt_is_an_invalid_schedule() {
        let (pool, _dir) = test_pool().await;
        let access = DaemonScheduleAccess::new(pool.clone());

        let mut blank_name = request("ws_ops");
        blank_name.name = "  ".into();
        assert_eq!(
            access
                .save(blank_name)
                .await
                .expect_err("空名字必须被拒")
                .code,
            "invalid_schedule"
        );

        let mut blank_prompt = request("ws_ops");
        blank_prompt.prompt = String::new();
        assert_eq!(
            access
                .save(blank_prompt)
                .await
                .expect_err("空正文必须被拒")
                .code,
            "invalid_schedule"
        );
    }

    /// 建一条停用的排期：没有下一次，把它打开之前调度循环不会碰它。
    #[tokio::test]
    async fn a_disabled_schedule_is_stored_without_a_next_run() {
        let (pool, _dir) = test_pool().await;
        let access = DaemonScheduleAccess::new(pool.clone());
        let mut disabled = request("ws_ops");
        disabled.enabled = false;

        let info = access.save(disabled).await.expect("停用也能建");

        assert!(!info.enabled);
        let row = schedules::task_by_id(&pool, &info.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.next_run_at, None, "停用的排期没有下一次");
        assert!(!row.enabled);
    }
}
