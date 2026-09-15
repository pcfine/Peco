// ============================================================================
// cron — 自动整理的定时触发（两级门 + 空闲判定）
// ============================================================================
//
// 触发层角色：与 `consolidation.rs`（执行）分离，本模块只决定「何时触发」
// 与「这一轮整理谁」。单用户整轮的执行仍是 `ConsolidationWorker::run_once`，
// 与手动端点走同一条路径（装配见 `super::build_worker`）。
//
// 两级门（缺一不可，fail-closed）：
//   ① 服务器总开关 `memory.consolidation.enabled` —— 关闭时不注册 cron 任务
//      （`register` 直接返回，零开销），tick 入口再兜底判一次；
//   ② 用户级 opt-in —— `memory_consolidation_optin` 无行即未表态，
//      未表态的用户永不被自动整理（空表 = 不整理）。
//
// 空闲判定：只在用户离开后整理（`now - last_activity > idle_after_secs`）。
// 无活动记录 → 无从判断是否空闲 → 跳过（保守），用户下次访问后重新入列。
// 活动时间戳是进程内内存（见 `AppState::last_activity`），重启即空 ——
// 最坏情况是晚一轮被整理，方向保守。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio_cron_scheduler::JobSchedulerError;

use crate::db::memory_consolidation_optin::list_opted_in;
use crate::db::memory_consolidation_state::get_state;
use crate::error::ApiError;
use crate::state::AppState;
use crate::workflow::scheduler::JobHandler;

use super::config::{ConsolidationConfig, MemoryConfig};
use super::{RunStats, build_worker};

/// 自动整理 cron 任务的注册名。
pub const JOB_NAME: &str = "peco_memory_consolidation";

/// 注册自动整理 cron 任务。
///
/// 总开关关闭时直接返回 Ok（不注册、不占调度器）—— 灰度开启只需翻转
/// `memory.consolidation.enabled`。
pub async fn register(state: &Arc<AppState>) -> Result<(), JobSchedulerError> {
    if !state.consolidation_enabled {
        tracing::info!("Memory consolidation cron skipped (server switch off)");
        return Ok(());
    }

    let memory = memory_config();
    let cron_expr = memory.consolidation.cron_expr.clone();
    state
        .cron_scheduler
        .add_job(
            JOB_NAME.to_string(),
            cron_expr,
            job_handler(Arc::clone(state)),
        )
        .await?;
    Ok(())
}

/// 构造 cron 执行体：立即返回给调度器，整轮整理在后台任务里跑完。
///
/// 一轮整理是分钟级（batch 200 + ≤20 次 Flash 调用），不应占住调度器的
/// job 执行槽；失败路径全在 `run_tick` 内部收敛为日志，不 panic。
pub fn job_handler(state: Arc<AppState>) -> JobHandler {
    Arc::new(move || {
        let state = Arc::clone(&state);
        Box::pin(async move {
            tokio::spawn(async move { run_tick(&state).await });
        })
    })
}

/// 跑一轮：选人 → 串行逐用户整理。
///
/// 单用户失败只记日志并继续下一个用户 —— 一个用户的 KB 损坏不应让本轮
/// 其余用户饿死。
pub async fn run_tick(state: &Arc<AppState>) {
    let memory = memory_config();
    let users = plan_round(state, &memory.consolidation).await;
    if users.is_empty() {
        tracing::debug!("Memory consolidation round skipped: no eligible user");
        return;
    }

    tracing::info!(users = ?users, "Memory consolidation round started");
    // 串行：每用户的水位与统计各自落库（user_id 主键），互不覆盖
    for user_id in &users {
        match run_one(state, user_id, &memory).await {
            Ok(stats) => tracing::info!(
                user_id = %user_id,
                scanned = stats.scanned,
                merged = stats.merged,
                dedup_deleted = stats.dedup_deleted,
                ttl_deleted = stats.ttl_deleted,
                llm_calls = stats.llm_calls,
                "Consolidation round finished for user"
            ),
            Err(e) => tracing::error!(
                user_id = %user_id,
                error = %e,
                "Consolidation failed for user, continuing with the next"
            ),
        }
    }
}

/// 本轮待整理用户（两级门 → 空闲过滤 → 排序截断）。
///
/// 返回空列表 = 本轮不整理任何人。所有跳过分支都是保守方向。
pub async fn plan_round(state: &Arc<AppState>, config: &ConsolidationConfig) -> Vec<String> {
    // ① 服务器总开关（register 已判一次，此处兜底运行期变更）
    if !state.consolidation_enabled {
        tracing::debug!("Memory consolidation round skipped: server switch off");
        return Vec::new();
    }

    // ② 用户级 opt-in —— 读失败按「无人表态」处理，不整理（fail-closed）
    let opted_in = match list_opted_in(&state.db).await {
        Ok(users) => users,
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to list opted-in users, consolidation round skipped"
            );
            return Vec::new();
        }
    };
    if opted_in.is_empty() {
        return Vec::new();
    }

    let activity = state.activity_snapshot();
    let last_runs = load_last_runs(&state.db, &opted_in).await;

    select_round_users(
        &opted_in,
        &activity,
        &last_runs,
        Instant::now(),
        config.idle_after_secs,
        config.max_users_per_round,
    )
}

/// 候选选择（纯函数）：空闲过滤 + 最久未整理优先 + 上限截断。
///
/// - 不在 `opted_in` 中 / 无活动记录 / 空闲未超阈值 → 不入选；
/// - 排序按各用户上次整理时刻升序，无记录（含时间戳不可解析）视为最旧；
/// - 同刻用户保持 `opted_in` 的既有顺序（`list_opted_in` 按 user_id 排序）。
pub fn select_round_users(
    opted_in: &[String],
    activity: &HashMap<String, Instant>,
    last_runs: &HashMap<String, Option<String>>,
    now: Instant,
    idle_after_secs: u64,
    max_users: usize,
) -> Vec<String> {
    let idle_threshold = Duration::from_secs(idle_after_secs);
    let mut eligible: Vec<&String> = opted_in
        .iter()
        .filter(|user_id| match activity.get(*user_id) {
            // 严格大于阈值才算空闲：正好等于阈值时仍视为活跃（保守）
            Some(last) => now.saturating_duration_since(*last) > idle_threshold,
            None => false,
        })
        .collect();

    eligible
        .sort_by_key(|user_id| staleness_key(last_runs.get(*user_id).and_then(|v| v.as_deref())));

    eligible.into_iter().take(max_users).cloned().collect()
}

/// 排序键：上次整理时刻越早越靠前；无记录 / 不可解析视为 `UNIX_EPOCH`
/// （从未成功整理过 → 最优先）。
fn staleness_key(last_run_at: Option<&str>) -> DateTime<Utc> {
    last_run_at
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// 批量读各用户的上次整理时刻。读失败按「从未整理」处理（本轮优先跑它，
/// 最坏只是重复整理一轮，不损伤数据面）。
async fn load_last_runs(db: &SqlitePool, users: &[String]) -> HashMap<String, Option<String>> {
    let mut map = HashMap::with_capacity(users.len());
    for user_id in users {
        let last_run_at = match get_state(db, user_id).await {
            Ok(row) => row.and_then(|r| r.last_run_at),
            Err(e) => {
                tracing::warn!(
                    user_id = %user_id,
                    error = %e,
                    "Failed to read consolidation state, treating as never run"
                );
                None
            }
        };
        map.insert(user_id.clone(), last_run_at);
    }
    map
}

/// 单用户整轮整理（装配口径与手动端点一致）。
async fn run_one(
    state: &Arc<AppState>,
    user_id: &str,
    memory: &MemoryConfig,
) -> Result<RunStats, ApiError> {
    let worker = build_worker(state, user_id, memory).await?;
    worker
        .run_once(user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// 当前生效的记忆配置（与手动端点同源：`PecoConfig::default()`）。
fn memory_config() -> MemoryConfig {
    crate::peco::config::PecoConfig::default().memory
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::ServerConfig;
    use crate::workflow::scheduler::CronScheduler;

    /// 构造测试用 AppState（临时 DB + 临时数据目录）。
    async fn test_state(consolidation_enabled: bool) -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let config = ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            database_url: url,
            jwt_secret: "test-secret-key-do-not-use-in-production".to_string(),
            data_dir: dir.path().to_path_buf(),
        };
        let scheduler = Arc::new(CronScheduler::new().await.unwrap());
        let state = AppState::new(&config, pool, scheduler)
            .await
            .with_consolidation_enabled(consolidation_enabled);
        (Arc::new(state), dir)
    }

    /// `secs` 秒前的时刻。
    fn ago(now: Instant, secs: u64) -> Instant {
        now - Duration::from_secs(secs)
    }

    /// 全部用户都空闲的活动表。
    fn all_idle(now: Instant, users: &[&str]) -> HashMap<String, Instant> {
        users
            .iter()
            .map(|u| (u.to_string(), ago(now, 3600)))
            .collect()
    }

    /// 直接写入活动时间戳（绕过总开关门控，用于构造测试状态）。
    fn set_activity(state: &AppState, user_id: &str, at: Instant) {
        state
            .last_activity
            .lock()
            .unwrap()
            .insert(user_id.to_string(), at);
    }

    // ── 候选选择（纯函数）────────────────────────────────────────────────

    #[test]
    fn select_round_users_is_fail_closed_without_optin() {
        let now = Instant::now();
        let activity = all_idle(now, &["u1"]);

        // 无人 opt-in：活动与水位记录齐全也不选人
        assert!(
            select_round_users(&[], &activity, &HashMap::new(), now, 600, 3).is_empty(),
            "空 opt-in 表 = 不整理任何人"
        );

        // 未 opt-in 的用户不在入参列表里（哪怕它活跃记录齐全）
        let opted_in = vec!["u2".to_string()];
        assert!(
            select_round_users(&opted_in, &activity, &HashMap::new(), now, 600, 3).is_empty(),
            "未表态的用户不入选"
        );
    }

    #[test]
    fn select_round_users_skips_users_without_activity_record() {
        let now = Instant::now();
        let opted_in = vec!["u1".to_string(), "u2".to_string()];
        let activity = all_idle(now, &["u1"]);

        let users = select_round_users(&opted_in, &activity, &HashMap::new(), now, 600, 3);
        assert_eq!(
            users,
            vec!["u1".to_string()],
            "无活动记录 = 无从判断空闲，保守跳过"
        );
    }

    #[test]
    fn select_round_users_respects_idle_threshold() {
        let now = Instant::now();
        let opted_in = vec!["short".to_string(), "exact".to_string(), "long".to_string()];
        let activity = HashMap::from([
            ("short".to_string(), ago(now, 599)),
            ("exact".to_string(), ago(now, 600)),
            ("long".to_string(), ago(now, 601)),
        ]);

        let users = select_round_users(&opted_in, &activity, &HashMap::new(), now, 600, 3);
        assert_eq!(
            users,
            vec!["long".to_string()],
            "严格大于阈值才算空闲（活跃中不动）"
        );
    }

    #[test]
    fn select_round_users_orders_never_run_first_then_oldest() {
        let now = Instant::now();
        let opted_in = vec![
            "b".to_string(),
            "c".to_string(),
            "a".to_string(),
            "d".to_string(),
        ];
        let activity = all_idle(now, &["a", "b", "c", "d"]);
        let last_runs = HashMap::from([
            (
                "a".to_string(),
                Some("2026-09-10T00:00:00+00:00".to_string()),
            ),
            (
                "b".to_string(),
                Some("2026-09-12T00:00:00+00:00".to_string()),
            ),
            // 从未整理过
            ("c".to_string(), None),
            // 时间戳不可解析 → 同「从未整理」
            ("d".to_string(), Some("not-a-timestamp".to_string())),
        ]);

        let users = select_round_users(&opted_in, &activity, &last_runs, now, 600, 10);
        assert_eq!(
            users,
            vec!["c", "d", "a", "b"],
            "无记录最旧优先，其余按 last_run_at 升序"
        );
    }

    #[test]
    fn select_round_users_truncates_to_max_users() {
        let now = Instant::now();
        let opted_in: Vec<String> = (0..5).map(|i| format!("u{i}")).collect();
        let activity = all_idle(now, &["u0", "u1", "u2", "u3", "u4"]);
        let last_runs: HashMap<String, Option<String>> = (0..5)
            .map(|i| {
                (
                    format!("u{i}"),
                    Some(format!("2026-09-1{}T00:00:00+00:00", i + 1)),
                )
            })
            .collect();

        let users = select_round_users(&opted_in, &activity, &last_runs, now, 600, 3);
        assert_eq!(users.len(), 3);
        assert_eq!(users, vec!["u0", "u1", "u2"], "每轮上限取最久未整理的");

        // 上限为 0 → 本轮不整理（不 panic）
        assert!(select_round_users(&opted_in, &activity, &last_runs, now, 600, 0).is_empty());
    }

    // ── 两级门与串行水位 ────────────────────────────────────────────────

    #[tokio::test]
    async fn plan_round_requires_both_gates() {
        let config = ConsolidationConfig::default();

        // 服务器总开关关闭：用户已 opt-in 且长时间空闲，仍不入选
        let (state, _dir) = test_state(false).await;
        crate::db::memory_consolidation_optin::set_enabled(&state.db, "u1", true)
            .await
            .unwrap();
        set_activity(&state, "u1", ago(Instant::now(), 3600));
        assert!(
            plan_round(&state, &config).await.is_empty(),
            "总开关关闭 = 不整理（哪怕用户已 opt-in）"
        );

        // 总开关开启但无人 opt-in：fail-closed
        let (state, _dir) = test_state(true).await;
        set_activity(&state, "u1", ago(Instant::now(), 3600));
        assert!(
            plan_round(&state, &config).await.is_empty(),
            "空 opt-in 表 = 不整理"
        );
        // 总开关开启且已 opt-in 空闲用户 → 入选
        crate::db::memory_consolidation_optin::set_enabled(&state.db, "u1", true)
            .await
            .unwrap();
        assert_eq!(plan_round(&state, &config).await, vec!["u1".to_string()]);
    }

    #[tokio::test]
    async fn plan_round_skips_active_user_and_orders_by_own_watermark() {
        let (state, _dir) = test_state(true).await;
        let now = Instant::now();
        for user in ["u1", "u2"] {
            crate::db::memory_consolidation_optin::set_enabled(&state.db, user, true)
                .await
                .unwrap();
            set_activity(&state, user, ago(now, 3600));
        }
        let config = ConsolidationConfig::default();

        // 两个用户都从未整理过 → 同级，保持 list_opted_in 的 user_id 序
        assert_eq!(
            plan_round(&state, &config).await,
            vec!["u1".to_string(), "u2".to_string()]
        );

        // 模拟 u1 本轮已整理：u1 水位推进 → 下一轮 u2 优先
        crate::db::memory_consolidation_state::upsert_state(
            &state.db,
            "u1",
            None,
            Some("2026-09-15T00:00:00+00:00"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            plan_round(&state, &config).await,
            vec!["u2".to_string(), "u1".to_string()],
            "整理顺序随各用户自己的水位变化"
        );
        assert!(
            crate::db::memory_consolidation_state::get_state(&state.db, "u2")
                .await
                .unwrap()
                .is_none(),
            "u1 的水位写入不得触及 u2 的状态行"
        );

        // u1 转为活跃 → 只剩 u2（活跃中不动）
        set_activity(&state, "u1", now);
        assert_eq!(plan_round(&state, &config).await, vec!["u2".to_string()]);
    }

    #[tokio::test]
    async fn plan_round_respects_max_users_per_round() {
        let (state, _dir) = test_state(true).await;
        for user in ["u1", "u2", "u3", "u4"] {
            crate::db::memory_consolidation_optin::set_enabled(&state.db, user, true)
                .await
                .unwrap();
            set_activity(&state, user, ago(Instant::now(), 3600));
        }
        let config = ConsolidationConfig {
            max_users_per_round: 2,
            ..ConsolidationConfig::default()
        };

        assert_eq!(
            plan_round(&state, &config).await,
            vec!["u1".to_string(), "u2".to_string()],
            "每轮最多整理 max_users_per_round 个用户"
        );
    }
}
