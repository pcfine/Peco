//! d_dispatches 表 DAO —— 跨空间派发与同空间异步任务的台账。
//!
//! 台账生命周期长于空间行：`target_ws_id` 故意没有外键，删空间不得带走审计。

use serde::Serialize;
use sqlx::{QueryBuilder, SqlitePool};

/// `d_dispatches` 的完整行；HTTP 任务视图直接投影这组列。
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct DispatchRow {
    pub id: String,
    pub task_id: String,
    pub target_ws_id: String,
    pub target_work_dir: String,
    pub target_team_id: String,
    pub origin_ws_id: String,
    pub agent_name: String,
    pub origin_kind: String,
    pub state: String,
    pub wake_state: String,
    pub result_digest: Option<String>,
    pub created_at: String,
    pub terminal_at: Option<String>,
}

/// 列表过滤条件；每个 `None` 表示不过滤。
#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchFilter<'a> {
    pub target_ws_id: Option<&'a str>,
    pub origin_ws_id: Option<&'a str>,
    pub state: Option<&'a str>,
    pub limit: Option<i64>,
}

/// 写入一行的入参（未终态台账）。
pub struct NewDispatch<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub target_ws_id: &'a str,
    pub target_work_dir: &'a str,
    pub target_team_id: &'a str,
    pub origin_ws_id: &'a str,
    pub agent_name: &'a str,
    pub origin_kind: &'a str,
    pub state: &'a str,
    pub wake_state: &'a str,
}

/// 未终态状态清单 —— 关闭门槛、`orphan` 化与启动对账共用的唯一口径。
///
/// `input_required` / `auth_required` 不是终态：漏掉它们会让等待输入的任务在
/// 空间关闭时被销毁，而台账仍停在「在飞」状态，重启对账永远扫不到。
pub const ACTIVE_STATES: &[&str] = &["submitted", "working", "input_required", "auth_required"];

/// 等待用户输入的态：看板「等待中」只数它们，而它们在关闭门槛里仍算在飞。
pub const WAITING_STATES: &[&str] = &["input_required", "auth_required"];

/// 看板的「已完成」口径：只有真正跑完的才算成功。
const COMPLETED_STATE: &str = "completed";

/// 失败侧终态 —— 看板上单列一格，不并进「已完成」。
///
/// 把失败算成完成，看板上的成功数就不可信了；失败要显形，但不能占「已完成」
/// 的位置。
pub const FAILED_STATES: &[&str] = &["failed", "canceled", "timed_out", "rejected"];

/// 全部终态 = [`COMPLETED_STATE`] + [`FAILED_STATES`]。
///
/// 与 [`set_state`] 更新谓词里的那条清单不同源：那里额外吸收 `orphan`，目的是
/// 「不再被后续状态覆盖」；`orphan` 说的是「结果再也查不到」，它既不算完成也
/// 不算失败，故不在这张表里。
pub const TERMINAL_STATES: &[&str] = &["completed", "failed", "canceled", "timed_out", "rejected"];

/// 未终态台账的两个计数 —— 关闭门槛的唯一口径。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InflightCounts {
    /// Peco 发起的跨空间派发 + Peco 自己空间的 `run_machine_task`。
    pub dispatch: i64,
    /// 其它空间自己发起的 `run_machine_task`（外来在飞）。
    pub foreign: i64,
}

impl InflightCounts {
    /// 门槛判定用的总数；不单独查询、不单独返回，否则两个数会漂移。
    pub fn total(&self) -> i64 {
        self.dispatch + self.foreign
    }
}

/// 幂等写入：`UNIQUE(target_ws_id, task_id)` 命中即忽略，返回 1 表示本次插入。
pub async fn insert(pool: &SqlitePool, dispatch: &NewDispatch<'_>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO d_dispatches \
         (id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id, \
          agent_name, origin_kind, state, wake_state) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(target_ws_id, task_id) DO NOTHING",
    )
    .bind(dispatch.id)
    .bind(dispatch.task_id)
    .bind(dispatch.target_ws_id)
    .bind(dispatch.target_work_dir)
    .bind(dispatch.target_team_id)
    .bind(dispatch.origin_ws_id)
    .bind(dispatch.agent_name)
    .bind(dispatch.origin_kind)
    .bind(dispatch.state)
    .bind(dispatch.wake_state)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn count_by_target(pool: &SqlitePool, target_ws_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM d_dispatches WHERE target_ws_id = ?")
        .bind(target_ws_id)
        .fetch_one(pool)
        .await
}

/// 看板「今日任务」的四格计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TodayCounts {
    /// `state = 'completed'`。
    pub completed: i64,
    /// [`FAILED_STATES`] 合计。
    pub failed: i64,
    /// 在飞且不在等用户输入。
    pub active: i64,
    pub waiting: i64,
}

/// 本地日历日的四格计数，按该空间作为 target 过滤。
///
/// 「今天」取本地时区：daemon 与用户同一台机器，用 UTC 会让当地凌晨前后的
/// 任务落进别的日子。四格互斥 —— 看板是并排的列，同一任务出现在两列里会让
/// 总数对不上，故 `active` 要排掉 [`WAITING_STATES`]。
pub async fn today_counts(
    pool: &SqlitePool,
    target_ws_id: &str,
) -> Result<TodayCounts, sqlx::Error> {
    let sql = format!(
        "SELECT \
           COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN state IN ({}) THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN state IN ({}) AND state NOT IN ({}) THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN state IN ({}) THEN 1 ELSE 0 END), 0) \
         FROM d_dispatches \
         WHERE target_ws_id = ? AND date(created_at,'localtime') = date('now','localtime')",
        placeholders(FAILED_STATES),
        placeholders(ACTIVE_STATES),
        placeholders(WAITING_STATES),
        placeholders(WAITING_STATES),
    );
    let mut query = sqlx::query_as::<_, (i64, i64, i64, i64)>(&sql).bind(COMPLETED_STATE);
    for state in FAILED_STATES
        .iter()
        .chain(ACTIVE_STATES)
        .chain(WAITING_STATES)
        .chain(WAITING_STATES)
    {
        query = query.bind(*state);
    }
    let (completed, failed, active, waiting) = query.bind(target_ws_id).fetch_one(pool).await?;
    Ok(TodayCounts {
        completed,
        failed,
        active,
        waiting,
    })
}

/// 在飞台账按发起方分两组计数。
///
/// `peco_ws_id` 来自运行时解析的 Peco 身份（记录文件派生，可能漂移）：写成
/// SQL 字面量会让「谁是 Peco」在代码里出现第三份定义。
pub async fn inflight_counts(
    pool: &SqlitePool,
    target_ws_id: &str,
    peco_ws_id: &str,
) -> Result<InflightCounts, sqlx::Error> {
    Ok(InflightCounts {
        dispatch: count_inflight(pool, target_ws_id, peco_ws_id, InflightOrigin::Peco).await?,
        foreign: count_inflight(pool, target_ws_id, peco_ws_id, InflightOrigin::Foreign).await?,
    })
}

/// 两组计数的唯一差别是「谁发起的」：谓词与状态清单都在 [`count_inflight`] 里拼一次。
#[derive(Clone, Copy)]
enum InflightOrigin {
    /// Peco 发起的：跨空间派发，或 Peco 自己空间里的同空间任务。
    Peco,
    /// 其它空间自己发起的同空间任务。
    Foreign,
}

async fn count_inflight(
    pool: &SqlitePool,
    target_ws_id: &str,
    peco_ws_id: &str,
    origin: InflightOrigin,
) -> Result<i64, sqlx::Error> {
    let origin_predicate = match origin {
        InflightOrigin::Peco => "NOT (origin_kind = 'run_machine_task' AND origin_ws_id <> ?)",
        InflightOrigin::Foreign => "origin_kind = 'run_machine_task' AND origin_ws_id <> ?",
    };
    let sql = format!(
        "SELECT count(*) FROM d_dispatches \
         WHERE target_ws_id = ? AND state IN ({}) AND {origin_predicate}",
        placeholders(ACTIVE_STATES)
    );
    let mut query = sqlx::query_scalar::<_, i64>(&sql).bind(target_ws_id);
    for state in ACTIVE_STATES {
        query = query.bind(*state);
    }
    query.bind(peco_ws_id).fetch_one(pool).await
}

/// `?,?,?...` —— 占位符数量跟着状态清单走，各处不会漂移。
fn placeholders(states: &[&str]) -> String {
    vec!["?"; states.len()].join(",")
}

/// 按 `task_id` 查一行（Hub 发放的 UUID 全局唯一）。
pub async fn get_by_task_id(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<DispatchRow>, sqlx::Error> {
    sqlx::query_as::<_, DispatchRow>(
        "SELECT id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id, \
         agent_name, origin_kind, state, wake_state, result_digest, created_at, terminal_at \
         FROM d_dispatches WHERE task_id = ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
}

/// 启动对账要逐行点查的台账：只有非终态行需要判断「任务是否还活着」。
///
/// 与 [`mark_orphan`] 共用同一份状态清单 —— 两处各写一份会让「关闭时算在飞、
/// 重启时不算」这类漂移无从收敛。
pub async fn list_active(pool: &SqlitePool) -> Result<Vec<DispatchRow>, sqlx::Error> {
    let sql = format!(
        "SELECT id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id, \
         agent_name, origin_kind, state, wake_state, result_digest, created_at, terminal_at \
         FROM d_dispatches WHERE state IN ({}) ORDER BY created_at DESC, id DESC",
        placeholders(ACTIVE_STATES)
    );
    let mut query = sqlx::query_as::<_, DispatchRow>(&sql);
    for state in ACTIVE_STATES {
        query = query.bind(*state);
    }
    query.fetch_all(pool).await
}

/// 按过滤条件列出台账，最新在前。
pub async fn list(
    pool: &SqlitePool,
    filter: DispatchFilter<'_>,
) -> Result<Vec<DispatchRow>, sqlx::Error> {
    let mut builder = QueryBuilder::new(
        "SELECT id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id, \
         agent_name, origin_kind, state, wake_state, result_digest, created_at, terminal_at \
         FROM d_dispatches",
    );
    let mut has_where = false;
    if let Some(value) = filter.target_ws_id {
        push_filter_eq(&mut builder, &mut has_where, "target_ws_id", value);
    }
    if let Some(value) = filter.origin_ws_id {
        push_filter_eq(&mut builder, &mut has_where, "origin_ws_id", value);
    }
    if let Some(value) = filter.state {
        push_filter_eq(&mut builder, &mut has_where, "state", value);
    }

    builder
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(filter.limit.unwrap_or(100).clamp(1, 500));
    builder
        .build_query_as::<DispatchRow>()
        .fetch_all(pool)
        .await
}

fn push_filter_eq<'a>(
    builder: &mut QueryBuilder<'a, sqlx::Sqlite>,
    has_where: &mut bool,
    column: &str,
    value: &'a str,
) {
    if *has_where {
        builder.push(" AND ");
    } else {
        builder.push(" WHERE ");
        *has_where = true;
    }
    builder.push(column).push(" = ").push_bind(value);
}

/// 非终态行更新状态；终态（含 `orphan`）吸收后续状态，避免乱序更新回退。
pub async fn set_state(pool: &SqlitePool, task_id: &str, state: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE d_dispatches SET state = ? WHERE task_id = ? \
         AND state NOT IN ('completed','failed','canceled','timed_out','rejected','orphan')",
    )
    .bind(state)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 终态落库：状态、终态时间与结果摘要。
pub async fn finish(
    pool: &SqlitePool,
    task_id: &str,
    state: &str,
    result_digest: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE d_dispatches SET state = ?, terminal_at = datetime('now'), \
         result_digest = COALESCE(?, result_digest) \
         WHERE task_id = ? AND state <> 'orphan'",
    )
    .bind(state)
    .bind(result_digest)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 单行 orphan 化：任务已不可观察（空间关闭、Hub 消失、重订阅耗尽）。
pub async fn orphan_by_task(pool: &SqlitePool, task_id: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE d_dispatches SET state = 'orphan', terminal_at = datetime('now'), \
         wake_state = 'skipped' WHERE task_id = ?",
    )
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 在飞台账置 `orphan` 并停止唤醒，返回受影响行数。
///
/// 关闭空间会销毁唯一持有结果的 Hub，未落库的在飞任务将永远查不到结果；
/// 先标 `orphan` 是重启后对账的唯一依据。`wake_state` 同时置 `skipped`：
/// 空间都没了，再唤醒 Peco 去查只会拿到「任务不存在」。
pub async fn mark_orphan(pool: &SqlitePool, target_ws_id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET state = 'orphan', terminal_at = datetime('now'), \
         wake_state = 'skipped' \
         WHERE target_ws_id = ? AND state IN ({})",
        placeholders(ACTIVE_STATES)
    );
    let mut query = sqlx::query(&sql).bind(target_ws_id);
    for state in ACTIVE_STATES {
        query = query.bind(*state);
    }
    let result = query.execute(pool).await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 看板三格与关闭门槛看的是同一批状态：等待中的两态必须仍在在飞清单里，
    /// 否则一个等输入的任务在关闭空间时会被算成「没有在飞」。
    #[test]
    fn waiting_states_stay_inside_active_states() {
        for state in WAITING_STATES {
            assert!(
                ACTIVE_STATES.contains(state),
                "{state} 必须同时在 ACTIVE_STATES 里"
            );
        }
    }

    /// 终态清单必须等于「已完成」+ 失败侧，否则看板某一格会漏状态。
    #[test]
    fn terminal_states_are_completed_plus_failed() {
        let mut expected = vec![COMPLETED_STATE];
        expected.extend(FAILED_STATES.iter().copied());
        assert_eq!(expected, TERMINAL_STATES);
    }

    /// 四格互斥的前提：终态与在飞没有交集。
    #[test]
    fn terminal_and_active_states_do_not_overlap() {
        for state in TERMINAL_STATES {
            assert!(
                !ACTIVE_STATES.contains(state),
                "{state} 既是终态又在飞，三格会重复计数"
            );
        }
    }
}
