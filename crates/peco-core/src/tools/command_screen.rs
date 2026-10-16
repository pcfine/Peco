// ============================================================================
// command_screen — 以 / 为根的递归命令减速带
// ============================================================================
//
// 定位是减速带而非沙箱：宁可漏、不误伤。历史事故 `grep -rln 任职于 /` 超时后
// 孙进程脱离为孤儿持续烧 CPU —— 这类「以 / 为根的递归扫描」在这里先拦一道。

/// 返回 `Some(拒绝理由)` 表示拦截；`None` 表示放行。
pub fn screen_command(cmd: &str) -> Option<String> {
    let tokens = tokenize(cmd);
    if !tokens.iter().any(|t| t == "/" || t == "//") {
        return None;
    }

    tokens.iter().find_map(|token| {
        is_recursive_signal(token).then(|| {
            format!(
                "refused: recursive operation rooted at `/` (matched `{token}`); \
                 递归操作以 / 为根被拒绝，请改指具体子目录"
            )
        })
    })
}

/// 粗切分：按空白与 shell 元字符断词，并剥掉引号。
fn tokenize(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in cmd.chars() {
        if ch == '\'' || ch == '"' {
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '|' | ';' | '&' | '(' | ')') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// 该 token 是否构成「递归」信号。
fn is_recursive_signal(token: &str) -> bool {
    if matches!(token, "find" | "fd" | "du" | "tree" | "rg") {
        return true;
    }
    if matches!(token, "--recursive" | "--dereference-recursive") {
        return true;
    }
    // 单个 `-` 的短选项簇，去掉首个 `-` 后含 r/R（如 -r、-R、-rln）。
    match token.strip_prefix('-') {
        Some(rest) if !token.starts_with("--") => rest.contains('r') || rest.contains('R'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_recursive_rooted_at_slash() {
        let cases = [
            "grep -rln 任职于 /",
            "rm -rf /",
            "find / -name a",
            "chmod -R 777 /",
            "du /",
            "grep -r x \"/\"",
            "rg foo /",
            "ls -R /",
            "grep --recursive x /",
        ];
        for cmd in cases {
            assert!(
                screen_command(cmd).is_some(),
                "expected refuse: {cmd:?} => {:?}",
                screen_command(cmd)
            );
        }
    }

    #[test]
    fn allows_non_recursive_or_subdir() {
        let cases = [
            "ls /",
            "df -h /",
            "grep -rn x ./src",
            "rm -rf /tmp/x",
            "cat /etc/hosts",
            "find /home -name a",
            "echo hello",
            "grep -rln x /var/log",
        ];
        for cmd in cases {
            assert_eq!(screen_command(cmd), None, "expected pass: {cmd:?}");
        }
    }

    #[test]
    fn reason_mentions_matched_token() {
        let reason = screen_command("grep -rln 任职于 /").unwrap();
        assert!(
            reason.contains("-rln"),
            "reason should name the token: {reason}"
        );
        assert!(reason.contains("请改指具体子目录"), "reason: {reason}");
    }
}
