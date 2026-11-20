-- 030: 浏览器会话。
--
-- 客户端拿 64 位 hex 票据（cookie 值），库里只存它的 sha256：库文件泄漏不等于
-- 会话泄漏，摘要也换不回 cookie。user_id 恒为 'local'（单用户 daemon），保留这一
-- 列是为了会话行与账号行同源。
--
-- 时间列一律 UTC 文本（与 datetime('now') 同格式），滑动续期的算术全部交给 SQLite。
CREATE TABLE IF NOT EXISTS auth_sessions (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    user_agent   TEXT,
    ip           TEXT
);
-- 每次登录顺手回收过期行，扫的是这一列。
CREATE INDEX IF NOT EXISTS idx_auth_sessions_expires ON auth_sessions(expires_at);
