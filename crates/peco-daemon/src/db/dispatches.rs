//! d_dispatches 表 DAO —— 跨空间派发与同空间异步任务的台账。
//!
//! 台账生命周期长于空间行：`target_ws_id` 故意没有外键，删空间不得带走审计。

use std::time::Duration;

use serde::Serialize;
use sqlx::{QueryBuilder, SqlitePool};

pub use crate::db::dispatch_state::{WakeSkipReason, WakeState};

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
    pub task_state: String,
    pub wake_state: String,
    /// 仅 `wake_state='skipped'` 时非空；历史行为 `None`（成因不可考）。
    pub wake_skip_reason: Option<String>,
    pub result_digest: Option<String>,
    pub created_at: String,
    pub terminal_at: Option<String>,
}

/// 列表过滤条件；每个 `None` 表示不过滤。
#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchFilter<'a> {
    pub target_ws_id: Option<&'a str>,
    pub origin_ws_id: Option<&'a str>,
    pub task_state: Option<&'a str>,
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
    pub task_state: &'a str,
    pub wake_state: WakeState,
    /// 只在 `wake_state = Skipped` 时给值 —— reason 非空当且仅当已跳过。
    pub wake_skip_reason: Option<WakeSkipReason>,
}

/// 未终态状态清单 —— 关闭门槛、`orphan` 化与启动对账共用的唯一口径。
///
/// `input_required` / `auth_required` 不是终态：漏掉它们会让等待输入的任务在
/// 空间关闭时被销毁，而台账仍停在「在飞」状态，重启对账永远扫不到。
pub const TASK_ACTIVE_STATES: &[&str] =
    &["submitted", "working", "input_required", "auth_required"];

/// 等待用户输入的态：看板「等待中」只数它们，而它们在关闭门槛里仍算在飞。
pub const TASK_WAITING_STATES: &[&str] = &["input_required", "auth_required"];

/// 看板的「已完成」口径：只有真正跑完的才算成功。
const TASK_COMPLETED_STATE: &str = "completed";

/// 失败侧终态 —— 看板上单列一格，不并进「已完成」。
///
/// 把失败算成完成，看板上的成功数就不可信了；失败要显形，但不能占「已完成」
/// 的位置。
pub const TASK_FAILED_STATES: &[&str] = &["failed", "canceled", "timed_out", "rejected"];

/// 全部终态 = [`TASK_COMPLETED_STATE`] + [`TASK_FAILED_STATES`]。
///
/// 与 [`set_task_state`] 更新谓词里的那条清单不同源：那里额外吸收 `orphan`，目的是
/// 「不再被后续状态覆盖」；`orphan` 说的是「结果再也查不到」，它既不算完成也
/// 不算失败，故不在这张表里。
pub const TASK_TERMINAL_STATES: &[&str] =
    &["completed", "failed", "canceled", "timed_out", "rejected"];

/// daemon 专有的任务态：派发方再也拿不到结果，既不算完成也不算失败。
///
/// 只在这里定义一次：唤醒侧合成消息时也要认它，两处各写一个字面量会让
/// 「orphan 行给固定文案」在多一处地方悄悄失效。
pub const TASK_ORPHAN_STATE: &str = "orphan";

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
          agent_name, origin_kind, task_state, wake_state, wake_skip_reason) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
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
    .bind(dispatch.task_state)
    .bind(dispatch.wake_state.as_str())
    .bind(dispatch.wake_skip_reason.map(WakeSkipReason::as_str))
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
    /// `task_state = 'completed'`。
    pub completed: i64,
    /// [`TASK_FAILED_STATES`] 合计。
    pub failed: i64,
    /// 在飞且不在等用户输入。
    pub active: i64,
    pub waiting: i64,
}

/// 本地日历日的四格计数，按该空间作为 target 过滤。
///
/// 「今天」取本地时区：daemon 与用户同一台机器，用 UTC 会让当地凌晨前后的
/// 任务落进别的日子。四格互斥 —— 看板是并排的列，同一任务出现在两列里会让
/// 总数对不上，故 `active` 要排掉 [`TASK_WAITING_STATES`]。
pub async fn today_counts(
    pool: &SqlitePool,
    target_ws_id: &str,
) -> Result<TodayCounts, sqlx::Error> {
    let sql = format!(
        "SELECT \
           COALESCE(SUM(CASE WHEN task_state = ? THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN task_state IN ({}) THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN task_state IN ({}) AND task_state NOT IN ({}) THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN task_state IN ({}) THEN 1 ELSE 0 END), 0) \
         FROM d_dispatches \
         WHERE target_ws_id = ? AND date(created_at,'localtime') = date('now','localtime')",
        placeholders(TASK_FAILED_STATES),
        placeholders(TASK_ACTIVE_STATES),
        placeholders(TASK_WAITING_STATES),
        placeholders(TASK_WAITING_STATES),
    );
    let mut query = sqlx::query_as::<_, (i64, i64, i64, i64)>(&sql).bind(TASK_COMPLETED_STATE);
    for state in TASK_FAILED_STATES
        .iter()
        .chain(TASK_ACTIVE_STATES)
        .chain(TASK_WAITING_STATES)
        .chain(TASK_WAITING_STATES)
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
         WHERE target_ws_id = ? AND task_state IN ({}) AND {origin_predicate}",
        placeholders(TASK_ACTIVE_STATES)
    );
    let mut query = sqlx::query_scalar::<_, i64>(&sql).bind(target_ws_id);
    for state in TASK_ACTIVE_STATES {
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
         agent_name, origin_kind, task_state, wake_state, wake_skip_reason, \
         result_digest, created_at, terminal_at \
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
         agent_name, origin_kind, task_state, wake_state, wake_skip_reason, \
         result_digest, created_at, terminal_at \
         FROM d_dispatches WHERE task_state IN ({}) ORDER BY created_at DESC, id DESC",
        placeholders(TASK_ACTIVE_STATES)
    );
    let mut query = sqlx::query_as::<_, DispatchRow>(&sql);
    for state in TASK_ACTIVE_STATES {
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
         agent_name, origin_kind, task_state, wake_state, wake_skip_reason, \
         result_digest, created_at, terminal_at \
         FROM d_dispatches",
    );
    let mut has_where = false;
    if let Some(value) = filter.target_ws_id {
        push_filter_eq(&mut builder, &mut has_where, "target_ws_id", value);
    }
    if let Some(value) = filter.origin_ws_id {
        push_filter_eq(&mut builder, &mut has_where, "origin_ws_id", value);
    }
    if let Some(value) = filter.task_state {
        push_filter_eq(&mut builder, &mut has_where, "task_state", value);
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
pub async fn set_task_state(
    pool: &SqlitePool,
    task_id: &str,
    state: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE d_dispatches SET task_state = ? WHERE task_id = ? \
         AND task_state NOT IN ('completed','failed','canceled','timed_out','rejected','orphan')",
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
        "UPDATE d_dispatches SET task_state = ?, terminal_at = datetime('now'), \
         result_digest = COALESCE(?, result_digest) \
         WHERE task_id = ? AND task_state <> 'orphan'",
    )
    .bind(state)
    .bind(result_digest)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 单行 orphan 化：任务已不可观察（空间关闭、Hub 消失、重订阅耗尽）。
///
/// 原因只在行还没被跳过时补写：一行可能是先因来源不符被跳过、之后才失联的，
/// 覆盖掉 `not_peco_origin` 会把「为什么本来就不该唤醒」换成次要成因。
pub async fn orphan_by_task(pool: &SqlitePool, task_id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET task_state = '{orphan}', terminal_at = datetime('now'), \
         wake_state = '{skipped}', \
         wake_skip_reason = COALESCE(wake_skip_reason, '{orphaned}') \
         WHERE task_id = ?",
        orphan = TASK_ORPHAN_STATE,
        skipped = WakeState::Skipped.as_str(),
        orphaned = WakeSkipReason::Orphaned.as_str(),
    );
    let result = sqlx::query(&sql).bind(task_id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 在飞台账置 `orphan` 并停止唤醒，返回受影响行数。
///
/// 关闭空间会销毁唯一持有结果的 Hub，未落库的在飞任务将永远查不到结果；
/// 先标 `orphan` 是重启后对账的唯一依据。`wake_state` 同时置 `skipped`：
/// 空间都没了，再唤醒 Peco 去查只会拿到「任务不存在」。
///
/// 但只对还没投出去的行改唤醒态。已 `delivering` / `delivered` 的行已经有
/// 一条唤醒消息在途或已送达 —— 把它倒回 `skipped`，台账就变成了「消息发出过，
/// 却记着没发」，重投与去重都失去依据。状态本身照旧一律 orphan 化。
///
/// 两个 CASE 都读旧行值：SQLite 在写任何列之前求值 `SET` 里的全部表达式，故原因
/// 与唤醒态看到的是同一份更新前的 `wake_state`。原因那列排在前，是为了即使实现
/// 换成逐列生效，判据也不会先被本语句写下的 `skipped` 覆盖掉。
pub async fn mark_orphan(pool: &SqlitePool, target_ws_id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET task_state = '{orphan}', terminal_at = datetime('now'), \
         wake_skip_reason = CASE WHEN wake_state IN ('{pending}','{failed}') \
         THEN '{space_closed}' ELSE wake_skip_reason END, \
         wake_state = CASE WHEN wake_state IN ('{pending}','{failed}') THEN '{skipped}' \
         ELSE wake_state END \
         WHERE target_ws_id = ? AND task_state IN ({active})",
        orphan = TASK_ORPHAN_STATE,
        pending = WakeState::Pending.as_str(),
        failed = WakeState::Failed.as_str(),
        space_closed = WakeSkipReason::SpaceClosed.as_str(),
        skipped = WakeState::Skipped.as_str(),
        active = placeholders(TASK_ACTIVE_STATES),
    );
    let mut query = sqlx::query(&sql).bind(target_ws_id);
    for state in TASK_ACTIVE_STATES {
        query = query.bind(*state);
    }
    let result = query.execute(pool).await?;
    Ok(result.rows_affected())
}

// ============================================================================
// 唤醒队列：wake_state 的五态迁移
// ============================================================================
//
// `pending → delivering → delivered / failed`，外加三条回收路径（启动回收、
// 60s 超时、failed 重排）。全部是条件更新：并发投递与事件重放的幂等都靠
// 「条件不满足就是 0 行」，不在内存里另立一把锁。

/// 待唤醒的行，最老的在前；仍在退避窗口里的行不返回，留给后面的 tick。
///
/// 排序两列缺一不可：同一秒写入的成批行 `created_at` 相同，只按它排序会让
/// 「先到先投」退化成随机顺序。
///
/// 退避放在这里而不是调用方：取批与认领必须是同一套判据，否则「取到了但认领
/// 不到」会把一批有效行挡在队列里空转一轮。
pub async fn list_pending(
    pool: &SqlitePool,
    limit: i64,
    backoff: RetryBackoff,
) -> Result<Vec<DispatchRow>, sqlx::Error> {
    let sql = format!(
        "SELECT id, task_id, target_ws_id, target_work_dir, target_team_id, origin_ws_id, \
         agent_name, origin_kind, task_state, wake_state, wake_skip_reason, \
         result_digest, created_at, terminal_at \
         FROM d_dispatches WHERE wake_state = '{pending}' AND {} ORDER BY created_at, id LIMIT ?",
        backoff_predicate(backoff),
        pending = WakeState::Pending.as_str(),
    );
    sqlx::query_as::<_, DispatchRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// 退避参数：`tick` 是首次重试的间隔，`cap` 是指数增长的上限。
///
/// 两个数都归唤醒侧（见 [`crate::wake`]），台账层只把它们翻成 SQL —— 在这里各
/// 写一个数字，`wake::TICK` 一改就先漂移。
#[derive(Debug, Clone, Copy)]
pub struct RetryBackoff {
    pub tick: Duration,
    pub cap: Duration,
}

/// 「距上次尝试已过退避」的 SQL 谓词，`list_pending` 与 [`claim`] 共用。
///
/// 退避 = `min(cap, tick * 2^(attempts-1))`。SQLite 没有幂运算，`1 << n` 等价且
/// 不引入第二份公式；`MAX(..., 0)` 挡住「attempts 为 0 时移负数」这一格。
///
/// 没有认领时间的行不受限：从没试过，以及「输给用户后回滚」的行（attempts 已
/// 退回、时间清空）都不该因为退避白等一轮。
fn backoff_predicate(backoff: RetryBackoff) -> String {
    format!(
        "(wake_claimed_at IS NULL OR wake_attempts < 1 \
         OR datetime(wake_claimed_at, '+' || \
            MIN({cap}, {tick} * (1 << MAX(wake_attempts - 1, 0))) || ' seconds') \
            <= datetime('now'))",
        cap = backoff.cap.as_secs(),
        tick = backoff.tick.as_secs(),
    )
}

/// 认领一行：`pending → delivering` 并计一次尝试；返回 1 表示本次拿到它。
///
/// 按主键 `id` 定位而不用 `task_id`：`UNIQUE` 是 `(target_ws_id, task_id)`，同一个
/// task 在两个目标空间各有一行时，按 `task_id` 更新会一次改两行、返回 2，调用方
/// 于是永远认为「没认到」。
///
/// `WHERE wake_state = 'pending'` 是并发与重放的全部防线：第二个认领者拿到 0 行，
/// 于是同一行不会被投递两遍（双进程、事件重放、tick 重入都靠它）。退避谓词同理
/// —— 它与取批共用一套判据，重排刚写回的 `wake_claimed_at` 在这里挡住抖动。
pub async fn claim(pool: &SqlitePool, id: &str, backoff: RetryBackoff) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{delivering}', wake_skip_reason = NULL, \
         wake_attempts = wake_attempts + 1, wake_claimed_at = datetime('now') \
         WHERE id = ? AND wake_state = '{pending}' AND {}",
        backoff_predicate(backoff),
        delivering = WakeState::Delivering.as_str(),
        pending = WakeState::Pending.as_str(),
    );
    let result = sqlx::query(&sql).bind(id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 投递成功：写 `delivered_at`。
///
/// `delivered` 的意思是「已经交给运行器」，不是「agent 已经处理完」—— 那一轮
/// 之后出什么错都不改台账，真相在 `messages` 表里。
///
/// 与 [`claim`] 一样按主键定位：`task_id` 只在同一目标空间内唯一。
pub async fn mark_delivered(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{delivered}', wake_skip_reason = NULL, \
         delivered_at = datetime('now') \
         WHERE id = ? AND wake_state = '{delivering}'",
        delivered = WakeState::Delivered.as_str(),
        delivering = WakeState::Delivering.as_str(),
    );
    let result = sqlx::query(&sql).bind(id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 投递失败：落 `failed`，由 [`skip_exhausted`] / [`reschedule_failed`] 决定重排还是放弃。
pub async fn mark_failed(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{failed}', wake_skip_reason = NULL \
         WHERE id = ? AND wake_state = '{delivering}'",
        failed = WakeState::Failed.as_str(),
        delivering = WakeState::Delivering.as_str(),
    );
    let result = sqlx::query(&sql).bind(id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 认领后输给用户（会话被前台占用）：退回待投递，并把这 1 次尝试退回去。
///
/// 回滚而不记失败，是因为「忙」说明的是时机不对，不是这条唤醒投不出去：把两者
/// 算成同一件事，用户连续打开会话几次就足以让一条本来能送达的唤醒被跳过。
pub async fn release_claim(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{pending}', wake_skip_reason = NULL, \
         wake_claimed_at = NULL, wake_attempts = MAX(wake_attempts - 1, 0) \
         WHERE id = ? AND wake_state = '{delivering}'",
        pending = WakeState::Pending.as_str(),
        delivering = WakeState::Delivering.as_str(),
    );
    let result = sqlx::query(&sql).bind(id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 启动回收：上个进程留下的 `delivering` 行没有人再会把它落地，全部退回待投递。
///
/// 语义是 at-least-once：这一行可能已经投出去过，但停在 `delivering` 说明没有
/// 送达的证据。重投一条带 `task_id` 的可见消息，比让它永远卡住要好。
pub async fn requeue_all_delivering(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{pending}', wake_skip_reason = NULL, \
         wake_claimed_at = NULL \
         WHERE wake_state = '{delivering}'",
        pending = WakeState::Pending.as_str(),
        delivering = WakeState::Delivering.as_str(),
    );
    let result = sqlx::query(&sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 运行期超时回收：认领后超过 `older_than` 仍停在 `delivering`，认领方已经死了。
///
/// 时限由唤醒侧给定，不在这里再写一个数字 —— 两处各写一个，改一处就会漂移。
pub async fn requeue_stale_delivering(
    pool: &SqlitePool,
    older_than: Duration,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{pending}', wake_skip_reason = NULL, \
         wake_claimed_at = NULL \
         WHERE wake_state = '{delivering}' \
         AND datetime(wake_claimed_at) <= datetime('now', ?)",
        pending = WakeState::Pending.as_str(),
        delivering = WakeState::Delivering.as_str(),
    );
    let result = sqlx::query(&sql)
        .bind(format!("-{} seconds", older_than.as_secs()))
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 尝试次数用尽：落 `skipped` 终态，不再重排。
pub async fn skip_exhausted(pool: &SqlitePool, max_attempts: i64) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{skipped}', wake_skip_reason = '{exhausted}' \
         WHERE wake_state = '{failed}' AND wake_attempts >= ?",
        skipped = WakeState::Skipped.as_str(),
        exhausted = WakeSkipReason::AttemptsExhausted.as_str(),
        failed = WakeState::Failed.as_str(),
    );
    let result = sqlx::query(&sql).bind(max_attempts).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 还有剩余尝试的行退回待投递。
///
/// `wake_claimed_at` 故意留着：它记的是「上一次尝试是什么时候」，而重试退避正是
/// 按它算的。清空它等于宣布「这一行从没试过」，下一轮 tick 立刻重投，把 10 次
/// 预算在十几秒内烧光。超时回收只看 `delivering` 行，不受影响。
pub async fn reschedule_failed(pool: &SqlitePool, max_attempts: i64) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET wake_state = '{pending}', wake_skip_reason = NULL \
         WHERE wake_state = '{failed}' AND wake_attempts < ?",
        pending = WakeState::Pending.as_str(),
        failed = WakeState::Failed.as_str(),
    );
    let result = sqlx::query(&sql).bind(max_attempts).execute(pool).await?;
    Ok(result.rows_affected())
}

/// 启动对账发现的 orphan 行：任务随上个进程一起消失了。
///
/// 与 [`orphan_by_task`] 只差 `wake_state`：运行期失联（订阅断了、Hub 不认识这个
/// 任务）已无结果可查，故一并停掉唤醒；重启对账的行本来就在等唤醒，保留待投递
/// 才能让 Peco 知道「你派出去的那条断了」。
pub async fn orphan_by_task_on_restart(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE d_dispatches SET task_state = 'orphan', terminal_at = datetime('now') \
         WHERE task_id = ?",
    )
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 重启对账的孤儿行必须留在待唤醒队列 —— 这正是它和运行期失联的区别：
    /// 前者没被通知过，后者（`orphan_by_task`）已经没有结果可查。
    #[tokio::test]
    async fn restart_orphan_keeps_the_row_in_the_wake_queue() {
        let (pool, _dir) = test_pool().await;
        insert(
            &pool,
            &NewDispatch {
                id: "d1",
                task_id: "t1",
                target_ws_id: "ws_a",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "working",
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
            },
        )
        .await
        .unwrap();

        orphan_by_task_on_restart(&pool, "t1").await.unwrap();

        let (state, wake_state) = sqlx::query_as::<_, (String, String)>(
            "SELECT task_state, wake_state FROM d_dispatches WHERE task_id = ?",
        )
        .bind("t1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "orphan");
        assert_eq!(wake_state, "pending", "重启对账的孤儿行仍要唤醒");
    }

    /// 播种一行待唤醒台账。
    async fn seed_wake_row(pool: &SqlitePool, id: &str, task_id: &str) {
        insert(
            pool,
            &NewDispatch {
                id,
                task_id,
                target_ws_id: "ws_a",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "completed",
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
            },
        )
        .await
        .unwrap();
    }

    /// 把一行推进到「第 `attempts` 次尝试、认领时间在 `claimed_ago` 之前」。
    async fn arrange_claimed(
        pool: &SqlitePool,
        id: &str,
        attempts: i64,
        claimed_ago: Option<&str>,
    ) {
        sqlx::query(
            "UPDATE d_dispatches SET wake_state = 'pending', wake_attempts = ?, \
             wake_claimed_at = CASE WHEN ? IS NULL THEN NULL ELSE datetime('now', ?) END \
             WHERE id = ?",
        )
        .bind(attempts)
        .bind(claimed_ago)
        .bind(claimed_ago)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn short_backoff() -> RetryBackoff {
        RetryBackoff {
            tick: Duration::from_secs(5),
            cap: Duration::from_secs(300),
        }
    }

    /// 退避 = `min(cap, tick * 2^(attempts-1))`：第 3 次尝试之后要等 20s。
    ///
    /// 两档都断 —— 只断「没到期认不到」的话，一个恒假的谓词（永远认不到）也绿。
    #[tokio::test]
    async fn claim_waits_out_the_retry_backoff() {
        let (pool, _dir) = test_pool().await;
        seed_wake_row(&pool, "d1", "t1").await;

        arrange_claimed(&pool, "d1", 3, Some("-10 seconds")).await;
        assert!(
            list_pending(&pool, 5, short_backoff())
                .await
                .unwrap()
                .is_empty(),
            "退避窗口里的行不得进队列"
        );
        assert_eq!(
            claim(&pool, "d1", short_backoff()).await.unwrap(),
            0,
            "退避窗口里认领不到"
        );

        arrange_claimed(&pool, "d1", 3, Some("-30 seconds")).await;
        assert_eq!(
            claim(&pool, "d1", short_backoff()).await.unwrap(),
            1,
            "过了退避就能认领"
        );
    }

    /// 第 7 次起触顶：第 10 次尝试等 300s，而不是 `5 * 2^9 = 2560s`。
    #[tokio::test]
    async fn the_retry_backoff_is_capped() {
        let (pool, _dir) = test_pool().await;
        seed_wake_row(&pool, "d1", "t1").await;

        arrange_claimed(&pool, "d1", 10, Some("-299 seconds")).await;
        assert_eq!(
            claim(&pool, "d1", short_backoff()).await.unwrap(),
            0,
            "未到 300s 仍在退避"
        );

        arrange_claimed(&pool, "d1", 10, Some("-301 seconds")).await;
        assert_eq!(
            claim(&pool, "d1", short_backoff()).await.unwrap(),
            1,
            "超过上限的时间就够了"
        );
    }

    /// 认领按主键定位：同一个 task 在两行里时，只该改动被传进来的那一行。
    #[tokio::test]
    async fn claim_updates_only_the_row_named_by_its_primary_key() {
        let (pool, _dir) = test_pool().await;
        seed_wake_row(&pool, "d_a", "t_same").await;
        insert(
            &pool,
            &NewDispatch {
                id: "d_b",
                task_id: "t_same",
                target_ws_id: "ws_b",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "completed",
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(claim(&pool, "d_a", short_backoff()).await.unwrap(), 1);
        let claimed: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM d_dispatches WHERE wake_state = 'delivering' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(claimed, vec!["d_a".to_string()], "另一行必须原样不动");
    }

    /// 同一 task 在两个目标空间各一行 —— `UNIQUE(target_ws_id, task_id)` 允许这种
    /// 形状，故唤醒侧的落账语句必须只认主键。
    async fn seed_pair_in_two_spaces(pool: &SqlitePool, wake_state: &str) {
        seed_wake_row(pool, "d_a", "t_same").await;
        insert(
            pool,
            &NewDispatch {
                id: "d_b",
                task_id: "t_same",
                target_ws_id: "ws_b",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "completed",
                wake_state: WakeState::Pending,
                wake_skip_reason: None,
            },
        )
        .await
        .unwrap();
        for id in ["d_a", "d_b"] {
            sqlx::query("UPDATE d_dispatches SET wake_state = ? WHERE id = ?")
                .bind(wake_state)
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    async fn wake_state_of(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar("SELECT wake_state FROM d_dispatches WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn attempts_of(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT wake_attempts FROM d_dispatches WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// 送达按主键落账：另一个空间那一行的唤醒还没发出去，不能被连带记成已送达。
    #[tokio::test]
    async fn mark_delivered_touches_only_the_row_named_by_its_primary_key() {
        let (pool, _dir) = test_pool().await;
        seed_pair_in_two_spaces(&pool, "delivering").await;

        assert_eq!(
            mark_delivered(&pool, "d_a").await.unwrap(),
            1,
            "只该改动被点名的那一行"
        );
        assert_eq!(wake_state_of(&pool, "d_a").await, "delivered");
        assert_eq!(
            wake_state_of(&pool, "d_b").await,
            "delivering",
            "另一行的唤醒还没发出去，不得被连带落账"
        );
    }

    /// 失败同样按主键落账：连带标失败会把另一行的尝试预算一起烧掉。
    #[tokio::test]
    async fn mark_failed_touches_only_the_row_named_by_its_primary_key() {
        let (pool, _dir) = test_pool().await;
        seed_pair_in_two_spaces(&pool, "delivering").await;

        assert_eq!(
            mark_failed(&pool, "d_a").await.unwrap(),
            1,
            "只该改动被点名的那一行"
        );
        assert_eq!(wake_state_of(&pool, "d_a").await, "failed");
        assert_eq!(
            wake_state_of(&pool, "d_b").await,
            "delivering",
            "另一行没尝试过投递，不该被标失败"
        );
    }

    /// 回滚只退回本次认领的那一行：连带回滚会把另一行的尝试次数也退掉。
    #[tokio::test]
    async fn release_claim_rolls_back_only_the_row_named_by_its_primary_key() {
        let (pool, _dir) = test_pool().await;
        seed_pair_in_two_spaces(&pool, "delivering").await;
        sqlx::query("UPDATE d_dispatches SET wake_attempts = 2")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            release_claim(&pool, "d_a").await.unwrap(),
            1,
            "只该改动被点名的那一行"
        );
        assert_eq!(wake_state_of(&pool, "d_a").await, "pending");
        assert_eq!(attempts_of(&pool, "d_a").await, 1, "本次尝试要退回去");
        assert_eq!(
            wake_state_of(&pool, "d_b").await,
            "delivering",
            "另一行仍在认领中"
        );
        assert_eq!(
            attempts_of(&pool, "d_b").await,
            2,
            "另一行的尝试次数不该被退"
        );
    }

    /// 看板三格与关闭门槛看的是同一批状态：等待中的两态必须仍在在飞清单里，
    /// 否则一个等输入的任务在关闭空间时会被算成「没有在飞」。
    #[test]
    fn waiting_states_stay_inside_active_states() {
        for state in TASK_WAITING_STATES {
            assert!(
                TASK_ACTIVE_STATES.contains(state),
                "{state} 必须同时在 TASK_ACTIVE_STATES 里"
            );
        }
    }

    /// 终态清单必须等于「已完成」+ 失败侧，否则看板某一格会漏状态。
    #[test]
    fn terminal_states_are_completed_plus_failed() {
        let mut expected = vec![TASK_COMPLETED_STATE];
        expected.extend(TASK_FAILED_STATES.iter().copied());
        assert_eq!(expected, TASK_TERMINAL_STATES);
    }

    /// 四格互斥的前提：终态与在飞没有交集。
    #[test]
    fn terminal_and_active_states_do_not_overlap() {
        for state in TASK_TERMINAL_STATES {
            assert!(
                !TASK_ACTIVE_STATES.contains(state),
                "{state} 既是终态又在飞，三格会重复计数"
            );
        }
    }

    /// 一行的 `(task_state, wake_state, wake_skip_reason)`，三列必须一起看：
    /// 「跳过了」与「为什么跳过」要能对上。
    async fn ledger_of(pool: &SqlitePool, id: &str) -> (String, String, Option<String>) {
        sqlx::query_as(
            "SELECT task_state, wake_state, wake_skip_reason FROM d_dispatches WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// 播种一行「已跳过、原因已知」的台账，用来验证原因不被误改或不被清除。
    async fn seed_skipped(pool: &SqlitePool, id: &str, reason: WakeSkipReason) {
        insert(
            pool,
            &NewDispatch {
                id,
                task_id: id,
                target_ws_id: "ws_a",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "working",
                wake_state: WakeState::Skipped,
                wake_skip_reason: Some(reason),
            },
        )
        .await
        .unwrap();
    }

    /// 五个原因都得能落库：CHECK 约束与 [`WakeSkipReason`] 各写一份清单，任何
    /// 一边漏一个值，写入就会在运行期报 constraint failed。
    #[tokio::test]
    async fn every_skip_reason_passes_the_column_constraint() {
        let (pool, _dir) = test_pool().await;
        for (seq, reason) in WakeSkipReason::ALL.into_iter().enumerate() {
            let id = format!("d_{seq}");
            seed_skipped(&pool, &id, reason).await;
            assert_eq!(
                ledger_of(&pool, &id).await.2,
                Some(reason.as_str().to_string())
            );
        }

        // 反证：清单外的值必须被 CHECK 挡住 —— 否则约束形同虚设，前端会拿到
        // 一个没有文案的原因。
        let rejected =
            sqlx::query("UPDATE d_dispatches SET wake_skip_reason = 'made_up' WHERE id = 'd_0'")
                .execute(&pool)
                .await;
        assert!(rejected.is_err(), "CHECK 必须拒绝清单外的原因");
    }

    /// 运行期失联补写 `orphaned`，但保留更早的成因：一行先因来源不符被跳过、
    /// 之后才失联时，「本来就不该唤醒」才是要回答的问题。
    #[tokio::test]
    async fn runtime_orphaning_records_the_reason_only_when_none_is_recorded() {
        let (pool, _dir) = test_pool().await;
        seed_skipped(&pool, "d_notified", WakeSkipReason::NotPecoOrigin).await;
        insert(
            &pool,
            &NewDispatch {
                id: "d_quiet",
                task_id: "d_quiet",
                target_ws_id: "ws_a",
                target_work_dir: "/tmp/work",
                target_team_id: "research",
                origin_ws_id: "ws_peco",
                agent_name: "@master",
                origin_kind: "run_machine_task",
                task_state: "working",
                wake_state: WakeState::Delivered,
                wake_skip_reason: None,
            },
        )
        .await
        .unwrap();

        orphan_by_task(&pool, "d_notified").await.unwrap();
        orphan_by_task(&pool, "d_quiet").await.unwrap();

        assert_eq!(
            ledger_of(&pool, "d_notified").await,
            (
                TASK_ORPHAN_STATE.to_string(),
                "skipped".to_string(),
                Some("not_peco_origin".to_string())
            ),
            "更早的成因必须留着"
        );
        assert_eq!(
            ledger_of(&pool, "d_quiet").await,
            (
                TASK_ORPHAN_STATE.to_string(),
                "skipped".to_string(),
                Some("orphaned".to_string())
            ),
            "没记过原因的跳过行要补上 orphaned"
        );
    }

    /// 空间关闭只把「还没投出去」的行改成跳过并记 `space_closed`；已送达/在途的
    /// 行保持唤醒态，原因留空 —— reason 非空当且仅当 skipped。
    ///
    /// 这条同时锁住 UPDATE 的多列求值语义：原因与唤醒态的 CASE 都读旧行值，
    /// 若哪一列先落地再被另一列读到，`pending` 行会得到一个空原因。
    #[tokio::test]
    async fn closing_a_space_skips_only_the_rows_that_were_not_delivered() {
        let (pool, _dir) = test_pool().await;
        for (id, wake_state) in [
            ("d_pending", WakeState::Pending),
            ("d_failed", WakeState::Failed),
            ("d_delivering", WakeState::Delivering),
            ("d_delivered", WakeState::Delivered),
        ] {
            insert(
                &pool,
                &NewDispatch {
                    id,
                    task_id: id,
                    target_ws_id: "ws_a",
                    target_work_dir: "/tmp/work",
                    target_team_id: "research",
                    origin_ws_id: "ws_peco",
                    agent_name: "@master",
                    origin_kind: "run_machine_task",
                    task_state: "working",
                    wake_state,
                    wake_skip_reason: None,
                },
            )
            .await
            .unwrap();
        }

        assert_eq!(mark_orphan(&pool, "ws_a").await.unwrap(), 4);
        let task_orphan = TASK_ORPHAN_STATE.to_string();

        assert_eq!(
            ledger_of(&pool, "d_pending").await,
            (
                task_orphan.clone(),
                "skipped".to_string(),
                Some("space_closed".to_string())
            )
        );
        assert_eq!(
            ledger_of(&pool, "d_failed").await,
            (
                task_orphan.clone(),
                "skipped".to_string(),
                Some("space_closed".to_string())
            )
        );
        assert_eq!(
            ledger_of(&pool, "d_delivering").await,
            (task_orphan.clone(), "delivering".to_string(), None),
            "在途唤醒已有消息发出，不得倒回 skipped"
        );
        assert_eq!(
            ledger_of(&pool, "d_delivered").await,
            (task_orphan, "delivered".to_string(), None)
        );
    }

    /// 尝试次数用尽落 `skipped` 并记 `attempts_exhausted`。
    #[tokio::test]
    async fn exhausting_the_attempt_budget_records_the_reason() {
        let (pool, _dir) = test_pool().await;
        seed_wake_row(&pool, "d_1", "t_1").await;
        arrange_claimed(&pool, "d_1", 3, Some("-600 seconds")).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'failed' WHERE id = 'd_1'")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(skip_exhausted(&pool, 3).await.unwrap(), 1);

        assert_eq!(
            ledger_of(&pool, "d_1").await,
            (
                "completed".to_string(),
                "skipped".to_string(),
                Some("attempts_exhausted".to_string())
            )
        );
    }

    /// 推进或回退唤醒态的每一条路径都清空原因：`wake_skip_reason` 非空意味着
    /// 「这一行正停在 skipped」，留下旧原因会让 UI 把一条在队行显示成已跳过。
    #[tokio::test]
    async fn every_wake_state_transition_clears_the_stale_reason() {
        let (pool, _dir) = test_pool().await;

        // mark_delivered
        seed_skipped(&pool, "d_deliver", WakeSkipReason::SpaceClosed).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'delivering' WHERE id = 'd_deliver'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(mark_delivered(&pool, "d_deliver").await.unwrap(), 1);
        assert_eq!(
            ledger_of(&pool, "d_deliver").await.2,
            None,
            "mark_delivered 要清原因"
        );

        // mark_failed
        seed_skipped(&pool, "d_fail", WakeSkipReason::SpaceClosed).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'delivering' WHERE id = 'd_fail'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(mark_failed(&pool, "d_fail").await.unwrap(), 1);
        assert_eq!(
            ledger_of(&pool, "d_fail").await.2,
            None,
            "mark_failed 要清原因"
        );

        // release_claim
        seed_skipped(&pool, "d_release", WakeSkipReason::SpaceClosed).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'delivering' WHERE id = 'd_release'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(release_claim(&pool, "d_release").await.unwrap(), 1);
        assert_eq!(
            ledger_of(&pool, "d_release").await.2,
            None,
            "release_claim 要清原因"
        );

        // reschedule_failed
        seed_skipped(&pool, "d_reschedule", WakeSkipReason::SpaceClosed).await;
        sqlx::query(
            "UPDATE d_dispatches SET wake_state = 'failed', wake_attempts = 1 WHERE id = 'd_reschedule'",
        )
        .execute(&pool)
        .await
        .unwrap();
        // 上一个用例留下的 `d_fail` 也停在 failed，同样是这条语句的回收对象
        assert_eq!(reschedule_failed(&pool, 10).await.unwrap(), 2);
        assert_eq!(
            ledger_of(&pool, "d_reschedule").await.2,
            None,
            "reschedule_failed 要清原因"
        );

        // requeue_all_delivering
        seed_skipped(&pool, "d_requeue", WakeSkipReason::SpaceClosed).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'delivering' WHERE id = 'd_requeue'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(requeue_all_delivering(&pool).await.unwrap(), 1);
        assert_eq!(
            ledger_of(&pool, "d_requeue").await.2,
            None,
            "requeue_all_delivering 要清原因"
        );

        // requeue_stale_delivering
        seed_skipped(&pool, "d_stale", WakeSkipReason::SpaceClosed).await;
        sqlx::query(
            "UPDATE d_dispatches SET wake_state = 'delivering', \
             wake_claimed_at = datetime('now', '-600 seconds') WHERE id = 'd_stale'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            requeue_stale_delivering(&pool, Duration::from_secs(60))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            ledger_of(&pool, "d_stale").await.2,
            None,
            "requeue_stale_delivering 要清原因"
        );

        // claim 放最后：它把行留在 delivering，后面的批量回收断言行数会被它带偏。
        seed_skipped(&pool, "d_claim", WakeSkipReason::SpaceClosed).await;
        sqlx::query("UPDATE d_dispatches SET wake_state = 'pending' WHERE id = 'd_claim'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(claim(&pool, "d_claim", short_backoff()).await.unwrap(), 1);
        assert_eq!(ledger_of(&pool, "d_claim").await.2, None, "claim 要清原因");
    }

    /// 重启对账不清原因：它保留 `wake_state='pending'`，而 reason 只属于 skipped。
    #[tokio::test]
    async fn restart_orphaning_leaves_the_reason_alone() {
        let (pool, _dir) = test_pool().await;
        seed_wake_row(&pool, "d_restart", "t_restart").await;

        orphan_by_task_on_restart(&pool, "t_restart").await.unwrap();

        assert_eq!(
            ledger_of(&pool, "d_restart").await,
            ("orphan".to_string(), "pending".to_string(), None)
        );
    }
}
