//! 登录失败限流：按客户端地址计，15 分钟窗口，第 5 次失败起指数退避。
//!
//! 内存表而非库表：重启后重新计数可以接受（攻击者重启不了我们的进程），换来的是
//! 登录路径零写入 —— 登录恰恰是最需要抗洪的入口。

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 失败计数的存活窗口。
const WINDOW: Duration = Duration::from_secs(15 * 60);
/// 前几次失败只计数，不拖慢正常用户。
const FREE_ATTEMPTS: u32 = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
pub struct LoginThrottle {
    buckets: HashMap<String, Bucket>,
}

#[derive(Debug)]
struct Bucket {
    failures: u32,
    last_failure: Instant,
}

impl LoginThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// 现在能不能再试；不能就给出还要等多久。
    pub fn retry_after(&mut self, client: &str, now: Instant) -> Option<Duration> {
        let bucket = self.buckets.get(client)?;
        if now.saturating_duration_since(bucket.last_failure) >= WINDOW {
            self.buckets.remove(client);
            return None;
        }
        let backoff = backoff(bucket.failures);
        let waited = now.saturating_duration_since(bucket.last_failure);
        (waited < backoff).then(|| backoff - waited)
    }

    pub fn note_failure(&mut self, client: &str, now: Instant) {
        self.prune(now);
        let bucket = self.buckets.entry(client.to_string()).or_insert(Bucket {
            failures: 0,
            last_failure: now,
        });
        bucket.failures = bucket.failures.saturating_add(1);
        bucket.last_failure = now;
    }

    pub fn note_success(&mut self, client: &str) {
        self.buckets.remove(client);
    }

    /// 过期的桶必须清掉：键是客户端地址，爆破换源地址就能把这张表撑大。
    fn prune(&mut self, now: Instant) {
        self.buckets
            .retain(|_, bucket| now.saturating_duration_since(bucket.last_failure) < WINDOW);
    }
}

/// 1、2、4…… 直至 300s。
fn backoff(failures: u32) -> Duration {
    if failures < FREE_ATTEMPTS {
        return Duration::ZERO;
    }
    let exponent = failures - FREE_ATTEMPTS;
    let secs = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 变异方法：把 `backoff` 里的指数挪成 `2^failures` ⇒ 第 5 次的等待变成 32s，
    /// 「第 5 次只等 1s」的断言变红。
    #[test]
    fn the_first_five_failures_are_free_then_the_delay_doubles() {
        let mut throttle = LoginThrottle::new();
        let start = Instant::now();
        let client = "192.0.2.7";

        for attempt in 1..=4 {
            throttle.note_failure(client, start);
            assert_eq!(
                throttle.retry_after(client, start),
                None,
                "第 {attempt} 次失败不该拦下一次尝试"
            );
        }

        throttle.note_failure(client, start);
        assert_eq!(
            throttle.retry_after(client, start),
            Some(Duration::from_secs(1)),
            "第 5 次失败后等 1s"
        );
        throttle.note_failure(client, start);
        assert_eq!(
            throttle.retry_after(client, start),
            Some(Duration::from_secs(2))
        );
        throttle.note_failure(client, start);
        assert_eq!(
            throttle.retry_after(client, start),
            Some(Duration::from_secs(4))
        );
    }

    /// 等待是「从最后一次失败起算」，过了退避窗口就放行。
    #[test]
    fn the_delay_is_measured_from_the_last_failure() {
        let mut throttle = LoginThrottle::new();
        let start = Instant::now();
        for _ in 0..5 {
            throttle.note_failure("client", start);
        }
        assert_eq!(
            throttle.retry_after("client", start + Duration::from_millis(999)),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            throttle.retry_after("client", start + Duration::from_secs(1)),
            None
        );
    }

    /// 退避有上限：失败次数再多也不会把人锁到 300s 以上。
    #[test]
    fn the_delay_is_capped() {
        let mut throttle = LoginThrottle::new();
        let start = Instant::now();
        for _ in 0..80 {
            throttle.note_failure("client", start);
        }
        assert_eq!(
            throttle.retry_after("client", start),
            Some(MAX_BACKOFF),
            "80 次失败也只是 300s，不能溢出成天文数字"
        );
    }

    /// 窗口过期＝重新计数；成功一次＝这一桶清空（换客户端互不影响）。
    #[test]
    fn a_stale_window_or_a_success_resets_the_bucket() {
        let mut throttle = LoginThrottle::new();
        let start = Instant::now();
        for _ in 0..5 {
            throttle.note_failure("client", start);
        }

        let later = start + WINDOW;
        assert_eq!(throttle.retry_after("client", later), None);
        assert!(!throttle.buckets.contains_key("client"), "过期桶要被清掉");
        assert_eq!(throttle.retry_after("other", later), None, "按客户端分桶");

        for _ in 0..5 {
            throttle.note_failure("client", start);
        }
        throttle.note_success("client");
        assert_eq!(
            throttle.retry_after("client", start),
            None,
            "登录成功即解封"
        );
    }

    /// 过期的桶不会永久占内存。
    #[test]
    fn stale_buckets_do_not_pile_up() {
        let mut throttle = LoginThrottle::new();
        let start = Instant::now();
        for seq in 0..100 {
            throttle.note_failure(&format!("client-{seq}"), start);
        }
        assert_eq!(throttle.buckets.len(), 100);

        throttle.note_failure("fresh", start + WINDOW);
        assert_eq!(throttle.buckets.len(), 1, "上一窗口的桶必须被清掉");
    }
}
