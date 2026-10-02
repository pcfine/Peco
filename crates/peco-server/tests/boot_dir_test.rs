// ============================================================================
// boot_dir_test — 启动目录（<data_dir>/boot）的磁盘行为
// ============================================================================
//
// 覆盖 boot 模块碰磁盘的分支；用户名匹配等纯逻辑在 boot.rs 的单元测试里。

use std::time::{Duration, Instant};

use peco_server::peco::boot::{
    BOOT_FILE_EXT, BootDir, MAX_MESSAGE_CHARS, PRE_START_SCRIPT, ScriptOutcome, consume,
    read_message, run_pre_start_script, scan_boot_files,
};

fn temp_boot() -> (tempfile::TempDir, BootDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = BootDir::new(tmp.path());
    std::fs::create_dir_all(&dir.root).expect("create boot dir");
    (tmp, dir)
}

/// 扫描结果的纯文件名。
fn names(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

// ── 扫描 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn scan_empty_dir_returns_nothing() {
    let (_tmp, dir) = temp_boot();
    assert!(scan_boot_files(&dir).await.is_empty());
}

#[tokio::test]
async fn scan_missing_dir_returns_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = BootDir::new(tmp.path()); // boot/ 从未创建
    assert!(scan_boot_files(&dir).await.is_empty());
}

#[tokio::test]
async fn scan_sorts_md_files_by_name() {
    let (_tmp, dir) = temp_boot();
    for name in ["carol.md", "alice.md", "bob.md"] {
        std::fs::write(dir.root.join(name), "x").unwrap();
    }
    assert_eq!(
        names(&scan_boot_files(&dir).await),
        vec!["alice.md", "bob.md", "carol.md"]
    );
}

#[tokio::test]
async fn scan_ignores_non_md_and_dirs() {
    let (_tmp, dir) = temp_boot();
    std::fs::write(dir.root.join("alice.md"), "x").unwrap();
    // 非 md：钩子脚本、txt、无后缀
    std::fs::write(dir.root.join(PRE_START_SCRIPT), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.root.join("notes.txt"), "x").unwrap();
    std::fs::write(dir.root.join("noext"), "x").unwrap();
    // 目录：即使叫 .md 也不是普通文件
    std::fs::create_dir(dir.root.join("sub.md")).unwrap();
    std::fs::create_dir(dir.root.join("subdir")).unwrap();

    assert_eq!(names(&scan_boot_files(&dir).await), vec!["alice.md"]);
}

#[tokio::test]
async fn boot_file_ext_is_md() {
    assert_eq!(BOOT_FILE_EXT, "md");
}

// ── 启动消息读取 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn read_message_missing_returns_none_and_creates_nothing() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    assert!(read_message(&path).await.is_none());
    assert!(!path.exists());
}

#[tokio::test]
async fn read_message_blank_is_ignored() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    std::fs::write(&path, "  \n\n\t  \n").unwrap();
    assert!(read_message(&path).await.is_none());
}

#[tokio::test]
async fn read_message_trims_and_keeps_content() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    std::fs::write(&path, "\n\n  修好 A2/A3 的遗留问题  \n\n").unwrap();
    assert_eq!(
        read_message(&path).await.as_deref(),
        Some("修好 A2/A3 的遗留问题")
    );
}

#[tokio::test]
async fn read_message_directory_is_ignored() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    std::fs::create_dir(&path).unwrap();
    assert!(read_message(&path).await.is_none());
}

#[tokio::test]
async fn read_message_truncates_at_character_limit() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    // 用多字节字符，顺带验证截断不切坏 UTF-8 边界。
    let long = "中".repeat(MAX_MESSAGE_CHARS + 10);
    std::fs::write(&path, &long).unwrap();

    let got = read_message(&path).await.expect("should read");
    assert_eq!(got.chars().count(), MAX_MESSAGE_CHARS);
    assert!(long.starts_with(&got));
}

// ── 消费即删 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn consume_removes_the_file() {
    let (_tmp, dir) = temp_boot();
    let path = dir.root.join("alice.md");
    std::fs::write(&path, "pending").unwrap();

    consume(&path).await;

    assert!(!path.exists(), "消费后文件应被删除");
}

#[tokio::test]
async fn consume_is_idempotent_on_missing_file() {
    let (_tmp, dir) = temp_boot();
    // 不存在也不该 panic（重复消费、并发删除）。
    consume(&dir.root.join("gone.md")).await;
}

// ── 钩子脚本 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn pre_start_script_missing_is_reported() {
    let (_tmp, dir) = temp_boot();
    assert!(matches!(
        run_pre_start_script(&dir, Some(Duration::from_secs(5))).await,
        ScriptOutcome::Missing
    ));
}

#[tokio::test]
async fn pre_start_script_runs_with_data_dir_as_cwd_and_captures_output() {
    let (_tmp, dir) = temp_boot();
    // 非可执行位 + 相对路径输出 —— 同时验证 `sh <path>` 回退与 cwd = data_dir。
    std::fs::write(
        dir.pre_start_script(),
        "echo hello-from-hook\npwd > hook_cwd.txt\nexit 0\n",
    )
    .unwrap();

    let outcome = run_pre_start_script(&dir, Some(Duration::from_secs(10))).await;
    assert!(
        matches!(outcome, ScriptOutcome::Exited(Some(0))),
        "unexpected outcome: {outcome:?}"
    );

    let cwd = std::fs::read_to_string(dir.data_dir.join("hook_cwd.txt")).unwrap();
    assert_eq!(
        cwd.trim(),
        dir.data_dir.to_string_lossy(),
        "钩子工作目录应为 data_dir"
    );

    let log = std::fs::read_to_string(dir.script_log()).unwrap();
    assert!(log.contains("hello-from-hook"), "日志应捕获 stdout: {log}");
    assert!(log.contains("start sequence @"), "日志应有分隔标题: {log}");
}

#[tokio::test]
async fn pre_start_script_nonzero_exit_is_surfaced() {
    let (_tmp, dir) = temp_boot();
    std::fs::write(dir.pre_start_script(), "echo boom >&2\nexit 3\n").unwrap();

    let outcome = run_pre_start_script(&dir, Some(Duration::from_secs(10))).await;
    assert!(
        matches!(outcome, ScriptOutcome::Exited(Some(3))),
        "unexpected outcome: {outcome:?}"
    );
    let log = std::fs::read_to_string(dir.script_log()).unwrap();
    assert!(log.contains("boom"), "stderr 应进日志: {log}");
}

#[tokio::test]
async fn pre_start_script_timeout_kills_and_returns() {
    let (_tmp, dir) = temp_boot();
    std::fs::write(dir.pre_start_script(), "sleep 30\n").unwrap();

    let started = Instant::now();
    let outcome = run_pre_start_script(&dir, Some(Duration::from_millis(500))).await;
    let elapsed = started.elapsed();

    assert!(
        matches!(outcome, ScriptOutcome::TimedOut(_)),
        "unexpected outcome: {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "超时应及时返回，实际 {elapsed:?}"
    );
}

#[tokio::test]
async fn pre_start_script_is_kept_for_the_next_start() {
    let (_tmp, dir) = temp_boot();
    let script = dir.pre_start_script();
    std::fs::write(&script, "exit 0\n").unwrap();

    assert!(matches!(
        run_pre_start_script(&dir, Some(Duration::from_secs(10))).await,
        ScriptOutcome::Exited(Some(0))
    ));

    // 钩子不消费：每次启动都要再跑一遍（与 <name>.md 的「消费即删」相反）。
    assert!(script.exists(), "钩子应保留，下次启动还要执行");
    assert_eq!(std::fs::read_to_string(&script).unwrap(), "exit 0\n");
}
