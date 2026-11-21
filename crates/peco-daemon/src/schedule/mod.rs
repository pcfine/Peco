//! 定时任务：cron 时刻计算、到点触发、忙时重试、超时收尾与结果通知。
//!
//! 与台账（`space::tasks` + `wake`）的分工：台账记的是「别人派给这个空间的任务」，
//! 本模块记的是「用户要求这个空间到点自己做一件事」。两者的执行体是同一条
//! [`crate::chat::run::spawn_run`] 无客户端链路，差别只在触发者与落库的去处。
//!
//! 时间约定：库里一律 UTC 文本（与 `datetime('now')` 同格式），cron 计算用本机
//! 时区。两套时间只在 [`cron`] 的边界上换算一次，别处不再各自推导。

pub mod access;
pub mod cron;
pub mod fire;
pub mod notify;
pub mod scheduler;

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};

/// tick 间隔。它同时是「补跑窗口」的分辨率：错过一个槽位最多晚一个 tick 被发现。
pub const TICK: Duration = Duration::from_secs(15);

/// 补跑窗口：`now - scheduled_for` 超过它就算错过，记 `skipped(error='missed')`。
///
/// 取 10 分钟：daemon 重启、机器休眠都会让槽位过期，短于此的迟到仍值得补跑
/// （用户要的是「每天九点那次汇报」，不是「必须九点整」）；再晚就可能撞上下一
/// 个动作的语义（比如「每小时的整点巡检」补跑会与下一小时挤在一起）。
pub const GRACE: Duration = Duration::from_secs(10 * 60);

/// 会话忙时的重试间隔。
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// 会话忙时的尝试上限：配合 [`RETRY_INTERVAL`] 是 10 分钟，与 [`GRACE`] 同量级。
pub const RETRY_MAX_ATTEMPTS: i64 = 10;

/// 单轮硬上限。到点取消并记 `timeout`。
///
/// 这不是「保险起见加个超时」，而是**必需**的：watcher 持有帧接收端，`TurnFeed`
/// 因此恒有订阅者，`chat::run` 的空闲看门狗（只在没人看时生效）对这一轮永远
/// 不触发。没有这条上限，一个卡住的模型请求会让该会话的运行条目永久占住 ——
/// 之后的每次触发都判定为「会话忙」，重试预算烧完就再也不跑。
pub const MAX_RUN: Duration = Duration::from_secs(2 * 60 * 60);

/// 通知重试间隔与上限：合起来 10 分钟。
///
/// 通知的落点是 Peco 空间，用户可能正在那儿和 Peco 对话 —— 忙不是失败，等一会儿。
pub const NOTIFY_INTERVAL: Duration = Duration::from_secs(30);
pub const NOTIFY_MAX_ATTEMPTS: i64 = 20;

/// 一个 tick 最多处理几行（到期任务 / 重试 / 通知各自独立计数）。
pub const MAX_BATCH: i64 = 20;

/// 调度循环退出或 panic 后的重启退避 —— 与唤醒泵同一手法：一个每次启动就 panic
/// 的循环不能被无间隔地拉起。
pub const RESTART_BACKOFF: Duration = Duration::from_secs(1);
pub const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// 跳过成因（写进 `error` 列）。
///
/// 每种「这一轮没跑」的原因必须分得开：错过槽位是调度的问题，前一轮没结束是
/// 任务自己的问题，会话忙是用户正在用，任务停用是用户自己叫停 —— 执行历史里
/// 混成一句话就没法排查。
pub const SKIP_MISSED: &str = "missed";
pub const SKIP_PREVIOUS_RUNNING: &str = "previous_running";
pub const SKIP_CONVERSATION_BUSY: &str = "conversation_busy";
pub const SKIP_SCHEDULE_DISABLED: &str = "schedule_disabled";
/// 目标空间进了回收站：排期行还指着它，但它已经不接受运行。
pub const SKIP_SPACE_TRASHED: &str = "space_trashed";

/// run 的状态。与 `scheduled_runs.status` 的 CHECK 同集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// 等重试（会话忙），没有终态。
    Pending,
    /// 这一轮在跑。
    Running,
    Completed,
    Failed,
    /// 主动放弃：忙到阈值、错过槽位、前一轮未完成、任务停用。
    Skipped,
    /// 到 [`MAX_RUN`] 被取消。
    Timeout,
    /// 进程重启时仍在跑 —— 观察者已经没了。
    Interrupted,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Timeout => "timeout",
            Self::Interrupted => "interrupted",
        }
    }

    /// 终态不得回退 —— SQL 谓词与状态机共用这一份判据。
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    /// 从库里的文本读回状态；`CHECK` 之外的取值（手改过的库）返回 `None`。
    pub fn from_db(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str() == value)
    }

    pub const ALL: &'static [RunStatus] = &[
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Skipped,
        Self::Timeout,
        Self::Interrupted,
    ];
}

/// 通知的状态。与 `scheduled_runs.notify_state` 的 CHECK 同集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyState {
    /// 不需要通知（跳过的轮次、启动对账收掉的行）。
    None,
    Pending,
    Sent,
    /// 预算用尽：用户没收到，但也不该无限重试。
    Skipped,
    /// 投递链路报错：与预算用尽分开记，两者的处置不同。
    Failed,
}

impl NotifyState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub const ALL: &'static [NotifyState] = &[
        Self::None,
        Self::Pending,
        Self::Sent,
        Self::Skipped,
        Self::Failed,
    ];
}

/// 这次 run 是 cron 排到的，还是用户临时点的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Cron,
    Manual,
}

impl TriggerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cron => "cron",
            Self::Manual => "manual",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        [Self::Cron, Self::Manual]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

/// 落库用的 UTC 文本，与 `datetime('now')` 同格式（秒级、无时区后缀）。
pub fn utc_text(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 把库里的 UTC 文本读回时刻；格式不认识时返回 `None`（调用方自己决定怎么兜底）。
pub fn parse_utc_text(text: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

/// 这一槽位是否已经错过补跑窗口。
///
/// 边界取「严格大于」：正好 [`GRACE`] 的迟到仍算赶上了 —— 判据的意义是
/// 「还值不值得补」，卡在正中点的那一次没有理由放弃。
pub fn is_missed(scheduled_for: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(scheduled_for) > chrono::Duration::from_std(GRACE).unwrap_or_default()
}

/// 会话忙时是否该放弃这一轮。
///
/// 两个条件任一命中就放弃：尝试次数烧完，或者这一轮本身已经过期（再等下去补跑的
/// 就是一次早就没有意义的触发）。
pub fn busy_gives_up(
    attempts_after: i64,
    scheduled_for: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    attempts_after >= RETRY_MAX_ATTEMPTS || is_missed(scheduled_for, now)
}

/// 忙时下一次尝试的时刻。
pub fn retry_at(now: DateTime<Utc>) -> DateTime<Utc> {
    now + chrono::Duration::from_std(RETRY_INTERVAL).unwrap_or_default()
}

/// 通知下一次尝试的时刻。
pub fn notify_retry_at(now: DateTime<Utc>) -> DateTime<Utc> {
    now + chrono::Duration::from_std(NOTIFY_INTERVAL).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        parse_utc_text(text).expect("夹具时刻必须可解析")
    }

    #[test]
    fn utc_text_round_trips_through_the_db_format() {
        let text = utc_text(at("2026-02-03 04:05:06"));
        assert_eq!(text, "2026-02-03 04:05:06");
        assert_eq!(parse_utc_text(&text), Some(at("2026-02-03 04:05:06")));
        assert_eq!(parse_utc_text("2026-02-03T04:05:06Z"), None, "只认一种格式");
    }

    /// 补跑窗口的边界：9 分钟算赶上，11 分钟算错过。判据反过来会让重启后的
    /// 补跑要么全丢（窗口被读成 0）要么每次都补（窗口无限）。
    #[test]
    fn the_grace_window_ends_at_ten_minutes() {
        let scheduled_for = at("2026-02-03 09:00:00");
        assert!(!is_missed(scheduled_for, at("2026-02-03 09:09:00")));
        assert!(
            !is_missed(scheduled_for, at("2026-02-03 09:10:00")),
            "正好卡在窗口上仍值得补跑"
        );
        assert!(is_missed(scheduled_for, at("2026-02-03 09:11:00")));
        // 时钟早于槽位（机器时区回拨）不算错过：没到点而已。
        assert!(!is_missed(scheduled_for, at("2026-02-03 08:59:00")));
    }

    /// 忙时放弃的两个条件各自成立：尝试次数到 10，或这一轮已经过期。
    #[test]
    fn busy_gives_up_on_the_attempt_cap_or_an_expired_slot() {
        let scheduled_for = at("2026-02-03 09:00:00");
        let on_time = at("2026-02-03 09:00:30");
        assert!(!busy_gives_up(9, scheduled_for, on_time), "第 9 次还有机会");
        assert!(busy_gives_up(10, scheduled_for, on_time), "第 10 次到头");

        let late = at("2026-02-03 09:11:00");
        assert!(
            busy_gives_up(1, scheduled_for, late),
            "补跑窗口都过了，再等下去补的是一次没有意义的触发"
        );
    }

    #[test]
    fn only_outcome_states_are_terminal() {
        assert!(!RunStatus::Pending.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        for status in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Skipped,
            RunStatus::Timeout,
            RunStatus::Interrupted,
        ] {
            assert!(status.is_terminal(), "{} 是终态", status.as_str());
        }
    }

    /// 通知重试的时刻落在 30 秒后，run 重试落在 60 秒后 —— 两个重试各走各的列，
    /// 用同一列会让通知把「会话忙」的重试时刻覆盖掉。
    #[test]
    fn the_two_retry_clocks_are_independent() {
        let now = at("2026-02-03 09:00:00");
        assert_eq!(utc_text(retry_at(now)), "2026-02-03 09:01:00");
        assert_eq!(utc_text(notify_retry_at(now)), "2026-02-03 09:00:30");
    }
}
