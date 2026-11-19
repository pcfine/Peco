//! 结果通知的文本与摘要 —— 纯函数，不碰库也不看时钟。
//!
//! 通知落进 Peco 空间的 `@master` 会话（复用唤醒的投递端），所以格式要能独立
//! 读懂：用户看到的是对话里的一条消息，没有 ScheduleView 在边上解释字段。

use crate::db::schedules::{ScheduledRunRow, ScheduledTaskRow};

use super::{RunStatus, TriggerKind};

/// 结果摘要的字符上限（按字符截断，不按字节）。
///
/// 与台账的 `MAX_DIGEST_CHARS` 同一个量级：通知要能塞进一条消息里读，而模型
/// 的回复可能上万字。
pub const RESULT_DIGEST_CHARS: usize = 500;

/// 按字符截断出摘要。按字节截会劈开多字节字符 —— 那不是「短一点」，是坏字符串。
pub fn digest_of(text: &str) -> String {
    text.chars().take(RESULT_DIGEST_CHARS).collect()
}

/// 这个终态要不要通知用户。
///
/// `skipped` / `interrupted` 不通知：前者是「这一轮本来就没跑」（错过槽位、会话
/// 一直忙、前一轮未完成），后者是进程重启的产物 —— 两者都是系统自己的账，吵到
/// 用户只会让「定时任务」变成噪声源。它们是终态，因此在执行历史里看得到。
pub fn should_notify(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Timeout
    )
}

/// 合成一条通知。
pub fn compose_notification(task: &ScheduledTaskRow, run: &ScheduledRunRow) -> String {
    let outcome = run
        .result_digest
        .as_deref()
        .or(run.error.as_deref())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("（无内容）");
    let trigger = TriggerKind::from_db(&run.trigger_kind)
        .map(TriggerKind::as_str)
        .unwrap_or(run.trigger_kind.as_str());

    format!(
        "[peco-schedule] 定时任务{word} {name}（schedule={schedule} run={run}）\n\
         - 目标空间: {label}（{ws}）\n\
         - 计划: {cron}（原定 {scheduled_for}）\n\
         - 触发: {trigger}\n\
         - 结果: {outcome}",
        word = status_word(RunStatus::from_db(&run.status)),
        name = task.name,
        schedule = task.id,
        run = run.id,
        label = task.target_label,
        ws = task.target_ws_id,
        cron = task.cron_expr,
        scheduled_for = run.scheduled_for,
        outcome = outcome,
    )
}

/// 通知标题里的结果词。不在这三态里的行本不该进通知队列；真进来了也说实话，
/// 不替它编一个「已完成」。
fn status_word(status: Option<RunStatus>) -> String {
    match status {
        Some(RunStatus::Completed) => "已完成".to_string(),
        Some(RunStatus::Failed) => "失败".to_string(),
        Some(RunStatus::Timeout) => "超时".to_string(),
        Some(other) => format!("结束（{}）", other.as_str()),
        None => "结束".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> ScheduledTaskRow {
        ScheduledTaskRow {
            id: "s-1".into(),
            name: "每日站会汇报".into(),
            target_ws_id: "ws_a".into(),
            prompt: "汇报昨天的进展".into(),
            cron_expr: "0 9 * * *".into(),
            enabled: true,
            next_run_at: Some("2026-03-05 01:00:00".into()),
            last_run_at: None,
            last_status: None,
            created_at: "2026-03-01 00:00:00".into(),
            updated_at: "2026-03-01 00:00:00".into(),
            target_label: "研发".into(),
        }
    }

    fn run(status: RunStatus, digest: Option<&str>, error: Option<&str>) -> ScheduledRunRow {
        ScheduledRunRow {
            id: "r-1".into(),
            schedule_id: "s-1".into(),
            trigger_kind: "cron".into(),
            scheduled_for: "2026-03-05 01:00:00".into(),
            status: status.as_str().into(),
            attempts: 1,
            next_attempt_at: None,
            conversation_id: Some("conv-1".into()),
            started_at: "2026-03-05 01:00:01".into(),
            finished_at: Some("2026-03-05 01:00:09".into()),
            result_digest: digest.map(str::to_string),
            error: error.map(str::to_string),
            notify_state: "pending".into(),
            notify_attempts: 0,
            notify_attempt_at: Some("2026-03-05 01:00:09".into()),
            notified_at: None,
        }
    }

    /// 成功与失败两条文案逐字钉住：这段文字是用户在对话里唯一能看到的解释，
    /// 字段缺失或顺序变化都得先在测试里显形。
    #[test]
    fn the_success_notification_names_every_field() {
        let text = compose_notification(
            &task(),
            &run(RunStatus::Completed, Some("12 个测试通过"), None),
        );

        assert_eq!(
            text,
            "[peco-schedule] 定时任务已完成 每日站会汇报（schedule=s-1 run=r-1）\n\
             - 目标空间: 研发（ws_a）\n\
             - 计划: 0 9 * * *（原定 2026-03-05 01:00:00）\n\
             - 触发: cron\n\
             - 结果: 12 个测试通过"
        );
    }

    #[test]
    fn the_failure_notification_carries_the_reason() {
        let text = compose_notification(
            &task(),
            &run(RunStatus::Failed, None, Some("provider_error: 上游 500")),
        );
        assert!(
            text.starts_with("[peco-schedule] 定时任务失败 每日站会汇报"),
            "{text}"
        );
        assert!(text.contains("- 结果: provider_error: 上游 500"), "{text}");
    }

    #[test]
    fn the_timeout_notification_says_so() {
        let text = compose_notification(
            &task(),
            &run(RunStatus::Timeout, None, Some("超过单轮上限")),
        );
        assert!(text.contains("定时任务超时"), "{text}");
    }

    /// 摘要按字符截断：600 个汉字只留 500 个字符，不多不少也不劈开。
    /// 按字节截会把最后一个汉字截成半个，字符串直接非法。
    #[test]
    fn the_digest_truncates_by_characters_not_bytes() {
        let long = "汉".repeat(600);
        let digest = digest_of(&long);

        assert_eq!(digest.chars().count(), RESULT_DIGEST_CHARS);
        assert_eq!(digest.len(), RESULT_DIGEST_CHARS * 3, "每个汉字三个字节");
        assert_eq!(digest, "汉".repeat(RESULT_DIGEST_CHARS));

        let short = "短".repeat(10);
        assert_eq!(digest_of(&short), short, "没超上限的内容原样保留");
    }

    /// 只有三种终态通知用户：跳过与中断是系统自己的账。
    #[test]
    fn skipped_and_interrupted_runs_are_not_announced() {
        assert!(should_notify(RunStatus::Completed));
        assert!(should_notify(RunStatus::Failed));
        assert!(should_notify(RunStatus::Timeout));
        assert!(!should_notify(RunStatus::Skipped));
        assert!(!should_notify(RunStatus::Interrupted));
        assert!(!should_notify(RunStatus::Pending));
        assert!(!should_notify(RunStatus::Running));
    }

    /// 手动触发的通知要说是用户自己点的：同一个任务「每天九点」与「我刚才点了一次」
    /// 在对话里必须区分得开。
    #[test]
    fn a_manual_trigger_is_labelled_as_such() {
        let mut manual = run(RunStatus::Completed, Some("好了"), None);
        manual.trigger_kind = "manual".into();
        let text = compose_notification(&task(), &manual);
        assert!(text.contains("- 触发: manual"), "{text}");
    }

    /// 摘要与成因都空时不能留一个空白的「结果:」——那句话读起来像消息被截断了。
    #[test]
    fn an_empty_outcome_is_spelled_out() {
        let text = compose_notification(&task(), &run(RunStatus::Completed, Some("  "), None));
        assert!(text.ends_with("- 结果: （无内容）"), "{text}");
    }
}
