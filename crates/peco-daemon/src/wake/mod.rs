//! 唤醒闭环的领域逻辑：谁该被唤醒、一次唤醒最多带多少、消息长什么样。
//!
//! 台账只记录「任务已经终态」；「Peco 要不要因此再跑一轮」是另一件事，判据与
//! 格式都属于唤醒侧知识，集中在这里，写入端（`space::tasks`）与查询端
//! （`db::dispatches`）各自不再持有第二份。

pub mod compose;
pub mod deliver;
pub mod pump;

use std::time::Duration;

use crate::db::dispatches::RetryBackoff;
use crate::space::peco::PecoIdentity;

/// 一次合成最多带几行台账：唤醒是「主动复核」，攒太多会让一轮的成本随积压线性增长。
pub const MAX_BATCH: i64 = 5;

/// 单项摘要的字符上限（按字符截断，不按字节）。
///
/// 与落库侧的 [`crate::space::tasks`] 摘要上限不是同一件事：落库要留全一点给
/// UI 详情，消息要省模型的 token。
pub const MAX_ITEM_CHARS: usize = 500;

/// tick 间隔 —— 同时也是合并的时间窗：同一个 tick 取到的行合成一条，跨 tick 各自成条。
pub const TICK: Duration = Duration::from_secs(5);

/// `delivering` 的存活上限：超过它仍没落地，说明认领方已死。
pub const DELIVERING_TIMEOUT: Duration = Duration::from_secs(60);

/// 一条台账的唤醒尝试上限；用尽即落 `skipped`，不再重排。
pub const MAX_ATTEMPTS: i64 = 10;

/// 重试退避的上限：第 n 次尝试之后等 `min(它, TICK * 2^(n-1))`。
///
/// 触顶点在第 7 次（`5s * 2^6 = 320s` 已越过 300s）。
///
/// 不设上限会把第 10 次尝试推到近一小时之后，等同于放弃；一条恒定的长间隔又让
/// 前几次失败白等。取 5 分钟：既压住「失败即重试」的抖动，也不至于让一条真能
/// 送达的行在进程寿命内等不到第二次机会。
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// 退避参数打包给台账层：数值归这里，`db::dispatches` 只负责把它翻成 SQL。
pub const RETRY_BACKOFF: RetryBackoff = RetryBackoff {
    tick: TICK,
    cap: MAX_BACKOFF,
};

/// 泵循环重启的初始间隔与上限 —— 与退避无关：它约束的是「重启本身」的密度，
/// 一个每次启动就 panic 的循环不能被无间隔地拉起。
pub const PUMP_RESTART_BACKOFF: Duration = Duration::from_secs(1);
pub const PUMP_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// 够得着唤醒的派发来源：Peco 亲自派出去的跨空间任务，与 Peco 自己空间里的异步任务。
///
/// 与 `d_dispatches.origin_kind` 的 CHECK 同集合；多一个值意味着唤醒侧要处理
/// 一种它没见过的任务来源。
const PECO_ORIGIN_KINDS: &[&str] = &["run_machine_task", "dispatch_to_workspace"];

/// 这条台账该不该进唤醒队列 —— 全仓唯一的判据。
///
/// 只看两件事：发起方是不是 Peco 自己的空间，以及来源是不是 Peco 会亲自动手
/// 的那两种。别的空间 `@master` 自用的异步任务，结论属于那个空间的语义，Peco
/// 既无权复核，也不该被叫醒。身份走 [`PecoIdentity::is_peco_space`]：`ws_id`
/// 是运行时从记录文件派生的，拿种子常量比较会在用户改过 `session_id` 之后判错。
pub fn qualifies(origin_ws_id: &str, origin_kind: &str, peco: &PecoIdentity) -> bool {
    peco.is_peco_space(origin_ws_id) && PECO_ORIGIN_KINDS.contains(&origin_kind)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::space::peco::PECO_WS_ID_SEED;

    fn identity(ws_id: &str) -> PecoIdentity {
        PecoIdentity {
            ws_id: ws_id.to_string(),
            work_dir: PathBuf::from("/tmp/peco/work"),
            team_id: "peco".to_string(),
            from_marker: true,
        }
    }

    #[test]
    fn only_pecos_own_dispatch_kinds_qualify() {
        let peco = identity("ws_recorded");

        assert!(qualifies("ws_recorded", "run_machine_task", &peco));
        assert!(qualifies("ws_recorded", "dispatch_to_workspace", &peco));
        assert!(
            !qualifies("ws_other", "run_machine_task", &peco),
            "别的空间的自有任务不是 Peco 的待办"
        );
        assert!(
            !qualifies("ws_recorded", "unknown_kind", &peco),
            "没见过的来源不猜"
        );
    }

    /// 身份来自记录文件时与种子不同值：判据一旦退回比种子，这条就会红。
    #[test]
    fn the_seed_is_never_a_stand_in_for_the_identity() {
        let peco = identity("ws_recorded");

        assert!(!qualifies(PECO_WS_ID_SEED, "run_machine_task", &peco));
    }
}
