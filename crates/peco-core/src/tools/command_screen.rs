// ============================================================================
// command_screen — 以 / 为根的递归命令减速带
// ============================================================================
//
// 定位是减速带而非沙箱：宁可漏、不误伤。判定按「命令段 + 程序」，不看单个 token。

/// 返回 `Some(拒绝理由)` 表示拦截；`None` 表示放行。
pub fn screen_command(cmd: &str) -> Option<String> {
    let body = strip_heredocs(cmd);
    for seg in segments(&body) {
        let Some((prog, rest)) = leading_program(&seg) else {
            continue;
        };
        let Some(signal) = recursion_signal(prog, rest) else {
            continue;
        };
        if let Some(root) = rest.iter().find(|t| *t == "/" || *t == "//") {
            return Some(format!(
                "refused: recursive operation rooted at `{}` (matched `{signal}`); \
                 递归操作以 / 为根被拒绝，请改指具体子目录",
                root.as_str()
            ));
        }
    }
    None
}

/// 按空白与 shell 元字符断词，剥掉引号，元字符保留为分隔符。
fn tokenize(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in cmd.chars() {
        if ch == '\'' || ch == '"' {
            continue;
        }
        // 换行在 shell 里等同 ';'。
        if ch == '\n' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(";".to_string());
        } else if ch.is_whitespace() || matches!(ch, '|' | ';' | '&' | '(' | ')') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if !ch.is_whitespace() {
                tokens.push(ch.to_string());
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

const SEPARATORS: [&str; 5] = ["|", ";", "&", "(", ")"];

/// 按 shell 运算符切成命令段。
fn segments(cmd: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    for tok in tokenize(cmd) {
        if SEPARATORS.contains(&tok.as_str()) {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(tok);
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

/// 包装器的带值选项：其后一个 token 是取值。
fn wrapper_value_opts(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "sudo" => &[
            "-u",
            "-g",
            "-p",
            "-C",
            "-h",
            "-r",
            "-t",
            "-U",
            "-T",
            "-D",
            "-R",
            "--user",
            "--group",
            "--prompt",
            "--chdir",
            "--host",
            "--role",
            "--type",
            "--other-user",
            "--close-from",
            "--command-timeout",
        ],
        "doas" => &["-u", "-C"],
        "env" => &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"],
        "time" => &["-f", "--format", "-o", "--output"],
        "exec" => &["-a"],
        _ => &[],
    }
}

/// 跳过前缀包装与 `VAR=val` 赋值，返回 (程序名, 其余参数)。
/// 包装器的选项及其取值一并跳过，否则 `sudo -n rm -rf /` 会漏拦。
fn leading_program(seg: &[String]) -> Option<(&str, &[String])> {
    const WRAPPERS: [&str; 7] = ["sudo", "doas", "env", "nohup", "time", "command", "exec"];
    let mut i = 0;
    while i < seg.len() {
        let tok = seg[i].as_str();
        if tok == "--" {
            i += 1;
            continue;
        }
        if WRAPPERS.contains(&tok) {
            i += 1;
            let opts = wrapper_value_opts(tok);
            while i < seg.len() {
                let opt = seg[i].as_str();
                if opt == "--" {
                    i += 1;
                    break;
                }
                if !opt.starts_with('-') {
                    break;
                }
                i += if opt.contains('=') || !opts.contains(&opt) {
                    1
                } else {
                    2
                };
            }
            continue;
        }
        if tok.contains('=') {
            i += 1;
            continue;
        }
        break;
    }
    let prog = seg.get(i)?;
    Some((prog.rsplit('/').next().unwrap_or(prog), &seg[i + 1..]))
}

/// 本身即递归的程序。
fn recurses_by_default(prog: &str) -> bool {
    matches!(prog, "find" | "fd" | "fdfind" | "du" | "tree" | "rg" | "ag")
}

/// 该程序代表递归的短选项字母。
fn recursive_letters(prog: &str) -> Option<&'static str> {
    match prog {
        "rm" | "mv" | "grep" | "egrep" | "fgrep" | "rgrep" => Some("rR"),
        "cp" => Some("raR"),
        "chmod" | "chown" | "chgrp" | "ls" => Some("R"),
        _ => None,
    }
}

/// 命中的递归信号。
fn recursion_signal(prog: &str, rest: &[String]) -> Option<String> {
    if recurses_by_default(prog) {
        return Some(prog.to_string());
    }
    for tok in rest {
        if matches!(tok.as_str(), "--recursive" | "--dereference-recursive") {
            return Some(tok.clone());
        }
        // 只认该程序自己的递归字母。
        if let (Some(letters), false) = (recursive_letters(prog), tok.starts_with("--"))
            && let Some(cluster) = tok.strip_prefix('-')
            && !cluster.is_empty()
            && cluster.chars().any(|c| letters.contains(c))
        {
            return Some(tok.clone());
        }
    }
    None
}

/// 去掉 heredoc 正文，只留可解析为命令的部分。
fn strip_heredocs(cmd: &str) -> String {
    let lines: Vec<&str> = cmd.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(pos) = line.find("<<") {
            let marker = line[pos + 2..].trim_start_matches('-').trim();
            let delim = marker
                .trim_matches(|c| c == '\'' || c == '"')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !delim.is_empty() {
                kept.push(&line[..pos]);
                i += 1;
                while i < lines.len() && lines[i].trim() != delim {
                    i += 1;
                }
                i += 1;
                continue;
            }
        }
        kept.push(line);
        i += 1;
    }
    kept.join("\n")
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
            "sudo rm -rf /",
            "/bin/rm -rf /",
            "cp -a / /tmp/x",
            "ls / && rm -rf /",
            "sudo -n rm -rf /",
            "sudo -u root rm -rf /",
            "sudo --user=root rm -rf /",
            "env -i rm -rf /",
            "time -p rm -rf /",
            "sudo -n cp -a /",
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
            "sudo -n ls -rt /",
            "env -i ls /",
        ];
        for cmd in cases {
            assert_eq!(screen_command(cmd), None, "expected pass: {cmd:?}");
        }
    }

    #[test]
    fn allows_false_positive_shapes() {
        let cases = [
            "ls -rt /",
            "ls -r /",
            "grep -l x /",
            "chmod -r 644 /tmp/x",
            "sed -n 1p /etc/hosts",
            "cat > a.tsx <<'EOF'\n// note\nstyle=-webkit-background-clip:border-box;\n/\nEOF",
            "cat > a.js <<EOF\n// just a comment\nEOF",
        ];
        for cmd in cases {
            assert_eq!(screen_command(cmd), None, "expected pass: {cmd:?}");
        }
    }

    #[test]
    fn heredoc_body_does_not_hide_real_commands() {
        let cmd = "cat > a.sh <<'EOF'\nrm -rf /tmp/x\nEOF\nrm -rf /";
        assert!(
            screen_command(cmd).is_some(),
            "heredoc 之后的真实命令仍要拦"
        );
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
