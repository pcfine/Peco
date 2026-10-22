// ============================================================================
// Session 标识 — 每用户永久会话
// ============================================================================

/// 会话 ID 后缀。
pub const SESSION_SUFFIX: &str = "private-session";

/// 会话标题（用于前端显示）。
pub const SESSION_TITLE: &str = "个人助理";

/// 私有会话行的 `agent_name` 兜底值（仅供展示）。
///
/// 「不是 agent 会话」的判据是 `conversations.scope = 'peco'`，不是这个名字 ——
/// 列表 / 计数 / 归档查询都靠 scope 排除，故这里无需躲避与真实 agent 撞名。
pub const SESSION_AGENT_NAME: &str = "peco";

/// 构建 per-user 的私有会话 ID。
///
/// 每个用户仅有一个 Peco 永续会话，会话 ID 格式：
/// `{user_id}-private-session`
pub fn private_session_id(user_id: &str) -> String {
    format!("{user_id}-{SESSION_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_private_session_id() {
        assert_eq!(
            private_session_id("user-abc-123"),
            "user-abc-123-private-session"
        );
    }

    #[test]
    fn test_session_id_contains_user_id() {
        let id = private_session_id("alice");
        assert!(id.starts_with("alice-"));
        assert!(id.ends_with("-private-session"));
    }
}
