// ============================================================================
// Session 标识 — 每用户永久会话
// ============================================================================

/// 会话 ID 后缀。
pub const SESSION_SUFFIX: &str = "private-session";

/// 会话标题（用于前端显示）。
pub const SESSION_TITLE: &str = "个人助理";

/// 构建 per-user 的私有会话 ID。
///
/// 每个用户仅有一个个人助理会话，会话 ID 格式：
/// `{user_id}-private-session`
///
/// 与 `assistant` 模块的 `PERSONAL_ASSISTANT_ID` ("personal_assistant") 不同，
/// 避免两套系统在 `session_snapshots` 表中冲突。
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
