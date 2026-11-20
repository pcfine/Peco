//! Web 访问身份：一枚 Bearer 令牌（运维 / 自动化）或一枚会话 cookie（浏览器）。
//!
//! 账号与密码落在 `users`（单用户 daemon 只有 `local` 一行），会话落在
//! `auth_sessions`；本模块只放与传输无关的原语与进程内状态，HTTP 形状（闸门与
//! 五个端点）在 [`crate::http::auth`]。

pub mod password;
pub mod throttle;
pub mod token;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use throttle::LoginThrottle;

/// 浏览器会话 cookie 的名字。
pub const SESSION_COOKIE: &str = "peco_session";

/// 30 天，与 `auth_sessions` 的滑动续期同一量级。
const SESSION_MAX_AGE: u64 = 30 * 24 * 60 * 60;

/// 进程内的认证状态。
pub struct WebAuth {
    token: Arc<str>,
    /// 只有 HTTPS 才给 cookie 加 `Secure`：HTTP 下加了浏览器根本不回传。
    cookie_secure: bool,
    throttle: Mutex<LoginThrottle>,
}

impl WebAuth {
    pub fn new(token: String, cookie_secure: bool) -> Self {
        Self {
            token: Arc::from(token),
            cookie_secure,
            throttle: Mutex::new(LoginThrottle::new()),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// setup token 与 Bearer 令牌是同一份东西：拿到它的进程能注册账号，也能免密调接口。
    pub fn token_matches(&self, candidate: &str) -> bool {
        token::constant_time_eq(&self.token, candidate)
    }

    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    pub fn session_cookie(&self, ticket: &str) -> String {
        let mut cookie = format!(
            "{SESSION_COOKIE}={ticket}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_MAX_AGE}"
        );
        if self.cookie_secure {
            cookie.push_str("; Secure");
        }
        cookie
    }

    /// 退出登录：同名空白 cookie 立即过期。
    pub fn cleared_cookie(&self) -> String {
        let mut cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
        if self.cookie_secure {
            cookie.push_str("; Secure");
        }
        cookie
    }

    pub fn login_retry_after(&self, client: &str) -> Option<Duration> {
        self.throttle().retry_after(client, Instant::now())
    }

    pub fn note_login_failure(&self, client: &str) {
        self.throttle().note_failure(client, Instant::now());
    }

    pub fn note_login_success(&self, client: &str) {
        self.throttle().note_success(client);
    }

    fn throttle(&self) -> MutexGuard<'_, LoginThrottle> {
        // 锁里只有失败计数：毒化了也不值得让登录整体 500。
        self.throttle.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Secure` 只在 HTTPS 下加：HTTP 下带了它，浏览器会把 cookie 丢掉，
    /// 表现为「登录成功但下一个请求还是 401」。
    ///
    /// 变异方法：无条件 push `; Secure` ⇒ 明文部署那条断言变红。
    #[test]
    fn the_session_cookie_is_secure_only_over_tls() {
        let plain = WebAuth::new("0".repeat(64), false);
        let tls = WebAuth::new("0".repeat(64), true);

        let cookie = plain.session_cookie("abc");
        assert_eq!(
            cookie,
            "peco_session=abc; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000"
        );
        assert!(!cookie.contains("Secure"));

        let cookie = tls.session_cookie("abc");
        assert!(cookie.ends_with("; Secure"), "{cookie}");
        assert!(cookie.starts_with("peco_session=abc; HttpOnly; SameSite=Lax; Path=/"));
        assert!(tls.cleared_cookie().ends_with("; Secure"));
    }

    /// 退出登录用 Max-Age=0 让浏览器删掉，不留一枚还能用的同名 cookie。
    #[test]
    fn the_clearing_cookie_expires_immediately() {
        let auth = WebAuth::new("0".repeat(64), false);
        let cookie = auth.cleared_cookie();
        assert!(cookie.starts_with("peco_session=;"), "{cookie}");
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
    }

    /// 令牌比较不接受前缀、大小写变体或空白。
    #[test]
    fn only_the_exact_token_passes() {
        let token = "a".repeat(64);
        let auth = WebAuth::new(token.clone(), false);
        assert!(auth.token_matches(&token));
        assert!(!auth.token_matches(&token[..63]));
        assert!(!auth.token_matches(&token.to_uppercase()));
        assert!(!auth.token_matches(&format!("{token} ")));
        assert!(!auth.token_matches(""));
    }

    /// 限流状态经 `WebAuth` 进出：同一客户端连续失败会被拦。
    #[test]
    fn repeated_failures_are_throttled_per_client() {
        let auth = WebAuth::new("b".repeat(64), false);
        for _ in 0..5 {
            auth.note_login_failure("192.0.2.9");
        }
        assert!(auth.login_retry_after("192.0.2.9").is_some());
        assert!(auth.login_retry_after("192.0.2.10").is_none());
        auth.note_login_success("192.0.2.9");
        assert!(auth.login_retry_after("192.0.2.9").is_none());
    }
}
