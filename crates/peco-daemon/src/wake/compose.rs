//! 唤醒消息的合成：台账行 → 一条 user 文本。
//!
//! 无 IO、无 DB，格式只在这里出现一次 —— 投递侧只管把这段文本送进会话。

use crate::db::dispatches::{DispatchRow, TASK_ORPHAN_STATE};

use super::MAX_ITEM_CHARS;

/// 消息首行前缀：用户与模型都靠它认出「这一条是唤醒通知」。
const HEADER: &str = "[peco-wake] 后台任务结果";

/// orphan 行的摘要固定为这句：结果已经查不到了，台账里的旧摘要不再是事实。
const ORPHAN_DIGEST: &str = "任务在进程重启时中断，输出已丢失";

/// 把一批台账行合成一条消息：首行带项数，其后一项一行。
pub fn compose(rows: &[DispatchRow]) -> String {
    let mut text = format!("{HEADER}（{} 项）", rows.len());
    for row in rows {
        text.push('\n');
        text.push_str(&item_line(row));
    }
    text
}

/// 单项的格式：`task_id` 给完整值，Peco 才能凭它回台账复核这一条。
///
/// `dir` 是跨空间复核的必填入参（`inspect_workspace_task{dir, task_id, action}`）：
/// 只给 `target`（ws_id）时模型没有可填进 `dir` 的东西，只能凭摘要猜。`target`
/// 留着是为了让模型认出「这条属于哪个空间」，两者不是同一个值。
pub fn item_line(row: &DispatchRow) -> String {
    format!(
        "- task={} state={} target={} dir={} digest={}",
        row.task_id,
        row.task_state,
        row.target_ws_id,
        row.target_work_dir,
        digest(row)
    )
}

fn digest(row: &DispatchRow) -> String {
    if row.task_state == TASK_ORPHAN_STATE {
        return ORPHAN_DIGEST.to_string();
    }
    let text = row.result_digest.as_deref().unwrap_or_default();
    // 按字符截断：按字节截会把一个汉字劈成两半，投出去的文本就坏了。
    one_line(&text.chars().take(MAX_ITEM_CHARS).collect::<String>())
}

/// 一项一行是格式契约：摘要里的换行会把同一项拆成几行，看起来像多了几项。
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task_id: &str, task_state: &str, digest: Option<&str>) -> DispatchRow {
        DispatchRow {
            id: format!("d_{task_id}"),
            task_id: task_id.to_string(),
            target_ws_id: "ws_target".to_string(),
            target_work_dir: "/tmp/work".to_string(),
            target_team_id: "research".to_string(),
            origin_ws_id: "ws_peco".to_string(),
            agent_name: "@master".to_string(),
            origin_kind: "run_machine_task".to_string(),
            task_state: task_state.to_string(),
            wake_state: "pending".to_string(),
            wake_skip_reason: None,
            result_digest: digest.map(str::to_string),
            prompt_digest: None,
            dispatch_id: None,
            created_at: "2026-01-01 00:00:00".to_string(),
            terminal_at: Some("2026-01-01 00:00:01".to_string()),
        }
    }

    /// W13：首行固定、其后一项一行，且每行都带复核所需的字段。
    #[test]
    fn w13_header_carries_the_item_count_and_each_item_is_one_line() {
        let text = compose(&[
            row("t-1", "completed", Some("重构完成，12 个测试通过")),
            row("t-2", "failed", Some("编译错误")),
        ]);

        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("[peco-wake] 后台任务结果（2 项）"));
        for line in lines {
            assert!(line.starts_with("- task="), "每一项独占一行：{line}");
            assert!(line.contains(" state="), "缺状态：{line}");
            assert!(line.contains(" target="), "缺目标空间：{line}");
            assert!(
                line.contains(" dir=/tmp/work"),
                "缺跨空间复核要用的目录：{line}"
            );
            assert!(line.contains(" digest="), "缺摘要：{line}");
        }
    }

    /// W3 的纯函数侧：孤儿行不看台账里的旧摘要。
    #[test]
    fn orphan_items_ignore_the_stored_digest() {
        let line = item_line(&row("t-1", TASK_ORPHAN_STATE, Some("别的内容")));

        assert!(line.contains(ORPHAN_DIGEST), "孤儿行必须给固定文案：{line}");
        assert!(!line.contains("别的内容"), "旧摘要不再成立：{line}");
    }

    /// 截断按字符：600 个汉字取 500 个；按字节取会剩下 166 个半截字符。
    #[test]
    fn long_digests_are_truncated_by_chars() {
        let digest = "汉".repeat(600);
        let line = item_line(&row("t-1", "completed", Some(&digest)));
        let kept = line.matches('汉').count();

        assert_eq!(kept, MAX_ITEM_CHARS);
    }

    /// 摘要里的换行不得伪造出新的项。
    #[test]
    fn newlines_inside_a_digest_do_not_split_an_item() {
        let text = compose(&[row("t-1", "completed", Some("第一行\n第二行"))]);

        assert_eq!(text.lines().count(), 2, "首行 + 一项：{text}");
        assert!(text.contains("第一行 第二行"), "换行压成空格：{text}");
    }

    /// 没有摘要的行不能因此丢掉这一项。
    #[test]
    fn items_without_a_digest_still_appear() {
        let text = compose(&[row("t-1", "timed_out", None)]);

        assert!(text.contains("- task=t-1 state=timed_out"), "{text}");
        assert!(text.ends_with("digest="), "空摘要留字段：{text}");
    }
}
