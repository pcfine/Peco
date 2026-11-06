//! 唤醒投递的两组取值：投递态与「为什么没投」。
//!
//! 放在 `db` 下自成一叶：`db` 是被唤醒侧依赖的一层，反向依赖会让台账 DAO 必须
//! 知道唤醒策略。字符串与 DB 的 CHECK 约束一一对应，改动只在这一处落笔。

/// 唤醒投递的五态；与 `d_dispatches.wake_state` 的 CHECK 同集合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeState {
    Pending,
    Delivering,
    /// 已交给运行器，不代表这一轮已经处理完。
    Delivered,
    Skipped,
    Failed,
}

impl WakeState {
    pub const ALL: [WakeState; 5] = [
        WakeState::Pending,
        WakeState::Delivering,
        WakeState::Delivered,
        WakeState::Skipped,
        WakeState::Failed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            WakeState::Pending => "pending",
            WakeState::Delivering => "delivering",
            WakeState::Delivered => "delivered",
            WakeState::Skipped => "skipped",
            WakeState::Failed => "failed",
        }
    }

    /// 库里读出的字符串 → 枚举；未知值返回 `None`，由调用方决定如何展示。
    pub fn from_db(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// `wake_state='skipped'` 的成因；只在跳过时非空。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeSkipReason {
    /// 来源不是 Peco 该唤醒的派发。
    NotPecoOrigin,
    /// 观察者还没拿到 Peco 身份（装配异常），宁可不叫醒。
    PecoIdentityMissing,
    /// 运行期观察失联，结果再也查不到。
    Orphaned,
    /// 目标空间被关闭，唤醒只会拿到「任务不存在」。
    SpaceClosed,
    /// 唤醒重试次数用尽。
    AttemptsExhausted,
}

impl WakeSkipReason {
    pub const ALL: [WakeSkipReason; 5] = [
        WakeSkipReason::NotPecoOrigin,
        WakeSkipReason::PecoIdentityMissing,
        WakeSkipReason::Orphaned,
        WakeSkipReason::SpaceClosed,
        WakeSkipReason::AttemptsExhausted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            WakeSkipReason::NotPecoOrigin => "not_peco_origin",
            WakeSkipReason::PecoIdentityMissing => "peco_identity_missing",
            WakeSkipReason::Orphaned => "orphaned",
            WakeSkipReason::SpaceClosed => "space_closed",
            WakeSkipReason::AttemptsExhausted => "attempts_exhausted",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_db` 必须认得每一个 `as_str` 的产出：两边各写一份字符串时，只改一处
    /// 会让「写进去的值读不回来」。
    #[test]
    fn every_value_round_trips_through_the_database_representation() {
        for state in WakeState::ALL {
            assert_eq!(WakeState::from_db(state.as_str()), Some(state));
        }
        for reason in WakeSkipReason::ALL {
            assert_eq!(WakeSkipReason::from_db(reason.as_str()), Some(reason));
        }
    }

    /// 未知值不猜：猜成某一态会让 UI 把一条失败投递显示成正常的。
    #[test]
    fn unknown_values_are_not_mapped_to_a_variant() {
        assert_eq!(WakeState::from_db("sent"), None);
        assert_eq!(WakeState::from_db(""), None);
        assert_eq!(WakeSkipReason::from_db("skipped"), None);
        assert_eq!(WakeSkipReason::from_db(""), None);
    }
}
