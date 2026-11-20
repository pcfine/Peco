//! d_dispatches 表 DAO —— 跨空间派发与同空间异步任务的台账。
//!
//! 台账生命周期长于空间行：`target_ws_id` 故意没有外键，删空间不得带走审计。

use std::time::Duration;

use serde::Serialize;
use sqlx::{QueryBuilder, SqlitePool};

pub use crate::db::dispatch_state::{WakeSkipReason, WakeState};

/// [`DispatchRow`] 的列清单：四处查询共用一份，加列时不会漏掉某条路径。
///
/// `output` 只取「有没有」，正文由 [`crate::space::ledger`] 按 id 单查。
const ROW_COLUMNS: &str = "id, task_id, target_ws_id, target_work_dir, target_team_id, \
     origin_ws_id, agent_name, origin_kind, task_state, wake_state, wake_skip_reason, \
     result_digest, prompt_digest, dispatch_id, created_at, terminal_at, \
     (output IS NOT NULL) AS has_output";

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
    /// 终态正文是否留存；正文本身按需单查，列表接口不拖上最多 100 万字符的 `output`。
    pub has_output: bool,
    /// 派发入参里那段任务描述的前 512 字；旧行是 `None`（当时没有记录）。
    pub prompt_digest: Option<String>,
    /// 派发回执里的 dispatch_id；旧行是 `None`（当时没有记录）。
    pub dispatch_id: Option<String>,
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
    /// 派发入参里的任务描述摘要；解析不到入参时为空。
    pub prompt_digest: Option<&'a str>,
    /// 派发回执里的 dispatch_id；观察不到回执时为空。
    pub dispatch_id: Option<&'a str>,
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
          agent_name, origin_kind, task_state, wake_state, wake_skip_reason, prompt_digest, \
          dispatch_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
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
    .bind(dispatch.prompt_digest)
    .bind(dispatch.dispatch_id)
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
    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM d_dispatches WHERE task_id = ? \
         ORDER BY created_at DESC LIMIT 1"
    );
    sqlx::query_as::<_, DispatchRow>(&sql)
        .bind(task_id)
        .fetch_optional(pool)
        .await
}

/// 报告要用到的那些台账事实。
///
/// 不复用 [`DispatchRow`]：后者故意只取「有没有正文」，列表接口不该拖上最多
/// 100 万字符的 `output`。报告是唯一确实需要全文的读者，所以它单查一列。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReportSource {
    /// 台账留存的正文；旧行与「任务没产出」都是 `None`。
    pub output: Option<String>,
    /// 台账正文本身是否已被上限截断：报告只是它的副本，这个标志必须继承。
    pub output_truncated: bool,
    pub result_digest: Option<String>,
    pub created_at: String,
    pub prompt_digest: Option<String>,
}

/// 终态之后回读台账，报告的全部正文与身份事实只从这一行取。
///
/// 不能沿用调用方手里的快照：`finish` 用 `COALESCE(?, output)` 保留旧正文，重复
/// 终态时第二次快照可能已经没有 `output`，据快照写就会把台账里还在的全文抹成
/// 「无输出」—— 交付副本于是和唯一事实源互相矛盾。`created_at` 与
/// `prompt_digest` 同理：运行期的观察根本拿不到它们。
///
/// 一次只查一个任务：正文可能上百万字符，只有终态落库成功的那一次调用需要它。
pub async fn find_report_source(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<ReportSource>, sqlx::Error> {
    sqlx::query_as::<_, ReportSource>(
        "SELECT output, output_truncated, result_digest, created_at, prompt_digest \
         FROM d_dispatches WHERE task_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
}

/// 某个目标空间的在飞台账行。
///
/// force 关闭要在 orphan 化之后逐行补一份失联说明，所以这里返回行本身而不是
/// 计数；状态清单与 [`mark_orphan`] 同源，读到的就是要被改写的那批行。
pub async fn list_active_by_target(
    pool: &SqlitePool,
    target_ws_id: &str,
) -> Result<Vec<DispatchRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM d_dispatches WHERE target_ws_id = ? AND task_state IN ({}) \
         ORDER BY created_at DESC, id DESC",
        placeholders(TASK_ACTIVE_STATES)
    );
    let mut query = sqlx::query_as::<_, DispatchRow>(&sql).bind(target_ws_id);
    for state in TASK_ACTIVE_STATES {
        query = query.bind(*state);
    }
    query.fetch_all(pool).await
}

/// 启动对账要逐行点查的台账：只有非终态行需要判断「任务是否还活着」。
///
/// 与 [`mark_orphan`] 共用同一份状态清单 —— 两处各写一份会让「关闭时算在飞、
/// 重启时不算」这类漂移无从收敛。
pub async fn list_active(pool: &SqlitePool) -> Result<Vec<DispatchRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM d_dispatches WHERE task_state IN ({}) \
         ORDER BY created_at DESC, id DESC",
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
    let mut builder = QueryBuilder::new(format!("SELECT {ROW_COLUMNS} FROM d_dispatches"));
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

/// 「等终态的行该入队了」的赋值片段：`awaiting` 转 `pending`，其余唤醒态原样返回。
///
/// 三条写入路径（终态落库、运行期失联、重启对账）共用这一处 —— 各写一遍的话，
/// 改判据时漏一处就会让那些任务永远停在 `awaiting`，而没有任何路径会再碰到它们。
fn queue_awaiting_assignment() -> String {
    format!(
        "wake_state = CASE WHEN wake_state = '{awaiting}' THEN '{pending}' ELSE wake_state END",
        awaiting = WakeState::Awaiting.as_str(),
        pending = WakeState::Pending.as_str(),
    )
}

/// 终态落库：状态、终态时间、结果摘要，以及「该入队了」。
///
/// 入队与终态必须同一条语句：拆成两步做，中间崩溃会留下「行已终态、唤醒永远
/// 停在 awaiting」的任务，而没有任何路径会再碰到它。
///
/// `awaiting` 是本条语句唯一改写的唤醒态 —— 它只存在于终态之前，终态一落就
/// 该投；其余唤醒态（`delivering` / `delivered` / `skipped`）描述的是一次已经
/// 发生的投递，倒回队列会把同一条消息发第二遍。
pub async fn finish(
    pool: &SqlitePool,
    task_id: &str,
    state: &str,
    result_digest: Option<&str>,
    output: Option<&str>,
    output_truncated: bool,
) -> Result<u64, sqlx::Error> {
    let (output, output_truncated) = bounded_output(output, output_truncated);
    let sql = format!(
        "UPDATE d_dispatches SET task_state = ?, terminal_at = datetime('now'), \
         result_digest = COALESCE(?, result_digest), output = COALESCE(?, output), \
         output_truncated = MAX(output_truncated, ?), {} \
         WHERE task_id = ? AND task_state <> 'orphan'",
        queue_awaiting_assignment(),
    );
    let result = sqlx::query(&sql)
        .bind(state)
        .bind(result_digest)
        .bind(output)
        .bind(output_truncated)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 落库正文上限（字符）：台账要留得下全文，但不能再无界增长。
///
/// 按字符而不是字节：切在字节上会把一个汉字劈成两半，取回来的正文本身就坏了。
pub const MAX_PERSISTED_OUTPUT_CHARS: usize = 1_000_000;

/// 正文裁剪的唯一实现：返回要用的正文与「这份正文是否已被截断」。
///
/// 台账落库与报告正文都走这一处 —— 两边各写一份「按字符砍到上限」，哪天口径
/// 变了就会有一边悄悄多留或少留几个字，报告里那句「台账只保留了前 N 字符」
/// 也随之变成假话。
pub fn clamp_chars(text: &str) -> (&str, bool) {
    match text.char_indices().nth(MAX_PERSISTED_OUTPUT_CHARS) {
        Some((cut, _)) => (&text[..cut], true),
        None => (text, false),
    }
}

/// 落库前的裁剪。
///
/// `already_truncated` 是调用方自己的约定：上游交来的正文若已是一段前缀，标志
/// 必须留下来，不能被这次「没再截断」洗掉。
fn bounded_output(output: Option<&str>, already_truncated: bool) -> (Option<String>, bool) {
    let Some(text) = output else {
        return (None, already_truncated);
    };
    let (shown, clamped) = clamp_chars(text);
    (Some(shown.to_string()), clamped || already_truncated)
}

/// 「还没到终态」的谓词：只命中真正会迁移的行，故 `rows_affected() > 0` 才等于
/// 「本次发生了状态迁移」，调用方可以据此决定要不要覆盖交付副本。
///
/// 没有它，`UPDATE ... WHERE task_id = ?` 对终态行同样返回 1（SQLite 把同一组值
/// 写回去也算改动），一条迟到的失联通知就会把带全文的报告改写成「无结果可回捞」。
/// 状态清单取常量而不是字面量：新增终态时这里必须跟着走。
fn not_terminal_predicate() -> String {
    format!(
        "task_state NOT IN ({}, '{orphan}')",
        placeholders(TASK_TERMINAL_STATES),
        orphan = TASK_ORPHAN_STATE,
    )
}

/// 单行 orphan 化：任务已不可观察（空间关闭、Hub 消失、重订阅耗尽）。
///
/// `awaiting` 行转 `pending` 而不是 `skipped`：Peco 正是要知道「你派出去的那条
/// 断了」，而这条通知只能由一次投递送出去。已经在队列里或正在投的行保持原样，
/// 已投出去的行（`delivered`）也不回退 —— 消息发出过就是发出过。
///
/// 原因列一概不动：它会随 `skipped` 行留到下一次真正被跳过的时候。
pub async fn orphan_by_task(pool: &SqlitePool, task_id: &str) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET task_state = '{orphan}', terminal_at = datetime('now'), {} \
         WHERE task_id = ? AND {}",
        queue_awaiting_assignment(),
        not_terminal_predicate(),
        orphan = TASK_ORPHAN_STATE,
    );
    let mut query = sqlx::query(&sql).bind(task_id);
    for state in TASK_TERMINAL_STATES {
        query = query.bind(*state);
    }
    let result = query.execute(pool).await?;
    Ok(result.rows_affected())
}

/// 停机收尾：所有未终态行置 `orphan`，返回受影响行数。
///
/// 逐行复用 [`orphan_by_task`]：台账里「结果已不可回捞」只有这一种表达。
pub async fn orphan_all_active(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let rows = list_active(pool).await?;
    let mut total = 0;
    for row in &rows {
        total += orphan_by_task(pool, &row.task_id).await?;
    }
    Ok(total)
}

/// 在飞台账置 `orphan`，返回受影响行数。
///
/// 关闭空间会销毁唯一持有结果的 Hub，未落库的在飞任务将永远查不到结果；
/// 先标 `orphan` 是重启后对账的唯一依据，台账本身长于空间行。
///
/// 唤醒态分三类：
/// - `awaiting`（任务还没到终态）→ `pending`：这正是要通知 Peco「断了一条」的
///   时刻，跳过就没人在看板之外知道这件事。
/// - `pending` / `failed`（还没投出去）→ `skipped` + `space_closed`：空间都没了，
///   再唤醒 Peco 去查只会拿到「任务不存在」，投出去只是白跑一轮。
/// - `delivering` / `delivered`：一条唤醒已经发出或已送达 —— 倒回 `skipped`，
///   台账就变成「消息发出过，却记着没发」，重投与去重都失去依据。
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
         WHEN wake_state = '{awaiting}' THEN '{pending}' \
         ELSE wake_state END \
         WHERE target_ws_id = ? AND task_state IN ({active})",
        orphan = TASK_ORPHAN_STATE,
        pending = WakeState::Pending.as_str(),
        failed = WakeState::Failed.as_str(),
        space_closed = WakeSkipReason::SpaceClosed.as_str(),
        skipped = WakeState::Skipped.as_str(),
        awaiting = WakeState::Awaiting.as_str(),
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
// 唤醒队列：wake_state 的六态迁移
// ============================================================================
//
// `awaiting → pending → delivering → delivered / failed`，外加三条回收路径
// （启动回收、60s 超时、failed 重排）。全部是条件更新：并发投递与事件重放的幂等
// 都靠「条件不满足就是 0 行」，不在内存里另立一把锁。

/// 待唤醒的行，最老的在前；仍在退避窗口里的行不返回，留给后面的 tick。
///
/// 只认 `pending`：`awaiting` 是「任务还没到终态」，此刻投出去只能报一个中间态，
/// 而终态会自己把它转成 `pending`。
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
        "SELECT {ROW_COLUMNS} FROM d_dispatches WHERE wake_state = '{pending}' AND {} \
         ORDER BY created_at, id LIMIT ?",
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
/// 与 [`orphan_by_task`] 一样把 `awaiting` 转成 `pending`：一行可能是终态之前就被
/// 上个进程带走的，它从没被通知过，也就还没有一条能告诉 Peco「那条断了」的消息。
pub async fn orphan_by_task_on_restart(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE d_dispatches SET task_state = '{orphan}', terminal_at = datetime('now'), {} \
         WHERE task_id = ? AND {}",
        queue_awaiting_assignment(),
        not_terminal_predicate(),
        orphan = TASK_ORPHAN_STATE,
    );
    let mut query = sqlx::query(&sql).bind(task_id);
    for state in TASK_TERMINAL_STATES {
        query = query.bind(*state);
    }
    let result = query.execute(pool).await?;
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
                prompt_digest: None,
                dispatch_id: None,
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

    /// 重启对账同样把 `awaiting` 行转入待投递：上个进程可能在任务终态之前就没了，
    /// 那些行从没被通知过，「你派出去的那条断了」还没有任何一条消息说过。
    #[tokio::test]
    async fn restart_orphaning_queues_the_awaiting_rows() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_awaiting", "working", WakeState::Awaiting, None).await;

        orphan_by_task_on_restart(&pool, "d_awaiting")
            .await
            .unwrap();

        assert_eq!(
            ledger_of(&pool, "d_awaiting").await,
            (TASK_ORPHAN_STATE.to_string(), "pending".to_string(), None),
            "等终态的行失联后要转入待投递"
        );
    }

    /// 终态落库与入队是同一条 UPDATE：分开写的话，中间崩溃会留下「已终态、唤醒
    /// 永远停在 awaiting」的行，而没有任何路径会再碰到它。
    ///
    /// 反证同时在场：`skipped` 行跑完之后仍是 `skipped` —— 终态不是重新决定
    /// 「要不要唤醒」的理由，否则一条被明确跳过的任务会因为跑完而被叫醒。
    #[tokio::test]
    async fn finishing_a_task_queues_the_awaiting_row_and_leaves_skipped_alone() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_live", "working", WakeState::Awaiting, None).await;
        seed_skipped(&pool, "d_skipped", WakeSkipReason::NotPecoOrigin).await;

        assert_eq!(
            finish(
                &pool,
                "d_live",
                "completed",
                Some("12 个测试通过"),
                Some("全文正文，长度远超 512 字的摘要"),
                false
            )
            .await
            .unwrap(),
            1
        );
        finish(&pool, "d_skipped", "completed", None, None, false)
            .await
            .unwrap();

        assert_eq!(
            ledger_of(&pool, "d_live").await,
            ("completed".to_string(), "pending".to_string(), None),
            "awaiting 必须随终态一起入队"
        );
        let (terminal_at, digest) = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT terminal_at, result_digest FROM d_dispatches WHERE id = 'd_live'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(terminal_at.is_some(), "终态时间要落");
        assert_eq!(digest.as_deref(), Some("12 个测试通过"));

        assert_eq!(
            ledger_of(&pool, "d_skipped").await,
            (
                "completed".to_string(),
                "skipped".to_string(),
                Some("not_peco_origin".to_string())
            ),
            "已跳过的行不因终态被叫醒"
        );
    }

    /// 正文超上限时保留头部并置标志：字数不再是原文总长，读者必须看得到这一点。
    ///
    /// 变异点：`bounded_output` 不截断 ⇒ 字数断言红；截断但不置标志 ⇒
    /// `output_truncated` 断言红。
    #[tokio::test]
    async fn an_over_long_body_is_truncated_at_the_persistence_cap() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_big", "working", WakeState::Awaiting, None).await;
        let body = "正".repeat(MAX_PERSISTED_OUTPUT_CHARS + 10);

        assert_eq!(
            finish(&pool, "d_big", "completed", None, Some(&body), false)
                .await
                .unwrap(),
            1
        );

        let (output, truncated) = sqlx::query_as::<_, (String, i64)>(
            "SELECT output, output_truncated FROM d_dispatches WHERE id = 'd_big'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            output.chars().count(),
            MAX_PERSISTED_OUTPUT_CHARS,
            "按字符截断，保留头部"
        );
        assert!(
            body.starts_with(&output),
            "留下的必须是原文前缀，不能是重新拼的内容"
        );
        assert_eq!(truncated, 1, "超上限必须留下截断标志");
    }

    /// 上限本身不算截断：恰好 1_000_000 字符原样落库、标志为 0；多一个字符才截。
    ///
    /// 变异点：边界写成 `>=` 或 `nth(MAX - 1)` ⇒ 「恰好到上限」那条的正文与标志都红。
    #[tokio::test]
    async fn the_persistence_cap_is_inclusive_of_its_last_character() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_exact", "working", WakeState::Awaiting, None).await;
        seed_row(&pool, "d_over", "working", WakeState::Awaiting, None).await;
        let exact = "正".repeat(MAX_PERSISTED_OUTPUT_CHARS);

        assert_eq!(
            finish(&pool, "d_exact", "completed", None, Some(&exact), false)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            finish(
                &pool,
                "d_over",
                "completed",
                None,
                Some(&format!("{exact}多")),
                false
            )
            .await
            .unwrap(),
            1
        );

        let (output, truncated) = sqlx::query_as::<_, (String, i64)>(
            "SELECT output, output_truncated FROM d_dispatches WHERE id = 'd_exact'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(output, exact, "恰好到上限必须原样落库");
        assert_eq!(truncated, 0, "没超过上限，不得置截断标志");

        let (output, truncated) = sqlx::query_as::<_, (String, i64)>(
            "SELECT output, output_truncated FROM d_dispatches WHERE id = 'd_over'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(output, exact, "多一个字符也只留到上限为止");
        assert_eq!(truncated, 1, "截断过就必须留标志");
    }

    /// `awaiting` 不进待投递队列：任务还没终态，此刻投出去只能报一个中间态，而
    /// 终态会自己把它转成 `pending`。
    #[tokio::test]
    async fn awaiting_rows_are_not_listed_as_pending() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_awaiting", "working", WakeState::Awaiting, None).await;
        seed_row(&pool, "d_ready", "completed", WakeState::Pending, None).await;

        let listed = list_pending(&pool, 10, short_backoff()).await.unwrap();

        assert_eq!(
            listed.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["d_ready"],
            "只有 pending 进队列"
        );
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
                prompt_digest: None,
                dispatch_id: None,
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

    /// 把一行直接摆到指定唤醒态。
    async fn set_wake_state(pool: &SqlitePool, id: &str, state: WakeState) {
        sqlx::query("UPDATE d_dispatches SET wake_state = ? WHERE id = ?")
            .bind(state.as_str())
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// 播种一行任意状态的台账；`task_id` 取 `id`，便于按行点名。
    async fn seed_row(
        pool: &SqlitePool,
        id: &str,
        task_state: &str,
        wake_state: WakeState,
        reason: Option<WakeSkipReason>,
    ) {
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
                task_state,
                wake_state,
                wake_skip_reason: reason,
                prompt_digest: None,
                dispatch_id: None,
            },
        )
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
                prompt_digest: None,
                dispatch_id: None,
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
                prompt_digest: None,
                dispatch_id: None,
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
                prompt_digest: None,
                dispatch_id: None,
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

    /// 截断口径只有一份：正好等于上限不算截断，多一个字符才算。边界错一个字，
    /// 报告里那句「台账只保留了前 N 字符」就与事实不符。
    #[test]
    fn clamping_keeps_the_limit_itself_and_flags_only_what_it_drops() {
        let exact = "正".repeat(MAX_PERSISTED_OUTPUT_CHARS);

        assert_eq!(clamp_chars(&exact), (exact.as_str(), false));

        let over = format!("{exact}多");
        let (shown, truncated) = clamp_chars(&over);
        assert_eq!(shown.chars().count(), MAX_PERSISTED_OUTPUT_CHARS);
        assert!(truncated, "多出来的那一个字必须被标成截断");
        assert!(!shown.contains('多'));
    }

    /// 六态的字符串都得能落库：CHECK 与 [`WakeState::ALL`] 各写一份清单，任一边
    /// 漏一个值，写入就会在运行期报 constraint failed。
    #[tokio::test]
    async fn every_wake_state_passes_the_column_constraint() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_state", "completed", WakeState::Pending, None).await;

        for state in WakeState::ALL {
            set_wake_state(&pool, "d_state", state).await;
            assert_eq!(wake_state_of(&pool, "d_state").await, state.as_str());
        }

        // 反证：清单外的值必须被 CHECK 挡住 —— 否则上面的循环对一张没有约束的
        // 表同样绿。
        let rejected =
            sqlx::query("UPDATE d_dispatches SET wake_state = 'sent' WHERE id = 'd_state'")
                .execute(&pool)
                .await;
        assert!(rejected.is_err(), "CHECK 必须拒绝清单外的唤醒态");
    }

    /// 运行期失联不改写已跳过行的原因：一行先因来源不符被跳过、之后才失联时，
    /// 「本来就不该唤醒」才是要回答的问题。已送达的行同样不动 —— 一条唤醒已经
    /// 发出去过，把台账改成 `skipped` 会让去重与重投都失去依据。
    #[tokio::test]
    async fn runtime_orphaning_keeps_rows_that_were_already_skipped_or_delivered() {
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
                prompt_digest: None,
                dispatch_id: None,
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
            (TASK_ORPHAN_STATE.to_string(), "delivered".to_string(), None),
            "已送达的唤醒不得倒回 skipped，也不该凭空多出一条原因"
        );
    }

    /// 运行期失联的待唤醒行要进队列，不是被跳过：Peco 得知道「你派出去的那条
    /// 断了」，而这条通知只能由一次投递送出去。
    ///
    /// `skipped` 行同时在场：它本来就决定过不唤醒，失联不该把它拖回队列。
    #[tokio::test]
    async fn runtime_orphaning_queues_the_rows_that_were_still_waiting() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_awaiting", "working", WakeState::Awaiting, None).await;
        seed_skipped(&pool, "d_skipped", WakeSkipReason::NotPecoOrigin).await;

        orphan_by_task(&pool, "d_awaiting").await.unwrap();
        orphan_by_task(&pool, "d_skipped").await.unwrap();

        assert_eq!(
            ledger_of(&pool, "d_awaiting").await,
            (TASK_ORPHAN_STATE.to_string(), "pending".to_string(), None),
            "等终态的行失联后要转入待投递"
        );
        assert_eq!(
            ledger_of(&pool, "d_skipped").await,
            (
                TASK_ORPHAN_STATE.to_string(),
                "skipped".to_string(),
                Some("not_peco_origin".to_string())
            ),
            "已跳过的行保持 skipped，原因也不动"
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
                    prompt_digest: None,
                    dispatch_id: None,
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

    /// 空间关闭时 `awaiting` 行转待投递：断掉的任务正是要通知 Peco 的那件事，
    /// 跳过它等于让「派出去的东西断了」只在日志里发生过。已跳过的行保持跳过、
    /// 原因也保持原样。
    #[tokio::test]
    async fn closing_a_space_queues_the_awaiting_rows_and_leaves_skipped_alone() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_awaiting", "working", WakeState::Awaiting, None).await;
        seed_skipped(&pool, "d_skipped", WakeSkipReason::NotPecoOrigin).await;

        assert_eq!(mark_orphan(&pool, "ws_a").await.unwrap(), 2);

        assert_eq!(
            ledger_of(&pool, "d_awaiting").await,
            (TASK_ORPHAN_STATE.to_string(), "pending".to_string(), None),
            "等终态的行失联后要转入待投递，且 pending 不带原因"
        );
        assert_eq!(
            ledger_of(&pool, "d_skipped").await,
            (
                TASK_ORPHAN_STATE.to_string(),
                "skipped".to_string(),
                Some("not_peco_origin".to_string())
            ),
            "已跳过的行保持 skipped，更早的成因不被 space_closed 顶掉"
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

    /// 已经离开在飞状态的行不再被失联通知改写 —— 两条 orphan 语句都必须带状态
    /// 谓词，`rows_affected() > 0` 才真的等于「本次发生了迁移」。
    ///
    /// 否则 `WHERE task_id = ?` 对终态行同样返回 1，一条迟到的失联会把带全文的
    /// 报告改写成「无结果可回捞」，而台账里的正文还在。
    #[tokio::test]
    async fn orphaning_never_rewrites_a_row_that_left_the_active_states() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_done", "completed", WakeState::Delivered, None).await;
        seed_row(
            &pool,
            "d_orphan",
            TASK_ORPHAN_STATE,
            WakeState::Skipped,
            None,
        )
        .await;
        sqlx::query(
            "UPDATE d_dispatches SET output = 'FULL-BODY', result_digest = 'digest' \
             WHERE id = 'd_done'",
        )
        .execute(&pool)
        .await
        .unwrap();

        for task_id in ["d_done", "d_orphan"] {
            assert_eq!(
                orphan_by_task(&pool, task_id).await.unwrap(),
                0,
                "{task_id} 已不在飞，运行期失联不得算作一次迁移"
            );
            assert_eq!(
                orphan_by_task_on_restart(&pool, task_id).await.unwrap(),
                0,
                "{task_id} 已不在飞，重启对账同样不得算作一次迁移"
            );
        }

        let (state, terminal_at, output): (String, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT task_state, terminal_at, output FROM d_dispatches WHERE id = 'd_done'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "completed", "终态不得被改写");
        assert_eq!(terminal_at, None, "不得给一条早已终态的行补失联时刻");
        assert_eq!(
            output.as_deref(),
            Some("FULL-BODY"),
            "正文是唯一事实源，谁都不许动"
        );
    }

    /// 重启对账不清原因：它保留 `wake_state='pending'`，而 reason 只属于 skipped。
    #[tokio::test]
    async fn restart_orphaning_leaves_the_reason_alone() {
        let (pool, _dir) = test_pool().await;
        seed_row(&pool, "d_restart", "working", WakeState::Pending, None).await;

        orphan_by_task_on_restart(&pool, "d_restart").await.unwrap();

        assert_eq!(
            ledger_of(&pool, "d_restart").await,
            ("orphan".to_string(), "pending".to_string(), None)
        );
    }
}
