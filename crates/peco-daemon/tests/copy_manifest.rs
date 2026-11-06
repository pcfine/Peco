//! 拷贝账与模块剪裁的机器核验（不依赖网络与 daemon 运行）。

use std::path::{Path, PathBuf};

const SERVER_SRC: &str = "crates/peco-server/src/";
const DAEMON_SRC: &str = "crates/peco-daemon/src/";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate 位于 crates/peco-daemon")
        .to_path_buf()
}

/// P1-1：清单每行源存在、目标存在、目标不含旧鉴权提取器；
/// 标注不拷的条目在 daemon 侧确实不存在。
#[test]
fn copy_manifest_rows_are_all_verifiable() {
    let root = repo_root();
    let manifest = std::fs::read_to_string(root.join("crates/peco-daemon/COPY_MANIFEST.md"))
        .expect("COPY_MANIFEST.md 必须落盘");

    let mut checked = 0usize;
    for (index, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert!(
            fields.len() >= 3,
            "第 {} 行字段不足（需要 <源> <目标> <处置>）: {line}",
            index + 1
        );
        let (source, target, disposition) = (fields[0], fields[1], fields[2]);

        match disposition {
            "copy" | "copy+mod" | "new" => {
                if disposition != "new" {
                    assert!(root.join(source).exists(), "源不存在: {source}");
                }
                let target_path = root.join(target);
                assert!(target_path.exists(), "目标不存在: {target}");
                let body = std::fs::read_to_string(&target_path).unwrap();
                // daemon 自己的 `auth` 模块只提供 `LocalUser`；要挡的是 JWT 提取器
                // `AuthUser` 残留，它会把单用户请求拖回「查 users 表」的鉴权路径。
                assert!(
                    !body.contains("AuthUser"),
                    "目标仍是 JWT 鉴权提取器: {target}"
                );
            }
            "不copy" | "未copy" => {
                assert_eq!(target, "-", "不拷的条目目标字段应为 '-'");
                assert!(root.join(source).exists(), "源不存在: {source}");
                let would_be = source
                    .strip_prefix(SERVER_SRC)
                    .unwrap_or_else(|| panic!("源路径不在 {SERVER_SRC} 下: {source}"));
                assert!(
                    !root.join(DAEMON_SRC).join(would_be).exists(),
                    "标注为{disposition}却在 daemon 侧存在: {would_be}"
                );
            }
            other => panic!("第 {} 行处置未知: {other}", index + 1),
        }
        checked += 1;
    }

    assert!(
        checked >= 50,
        "只核验了 {checked} 行 —— 清单被掏空时本测试必须变红"
    );
}

/// P1-17：剪裁后的 db 模块不得再引用 `sync` / `workspace_hashes`。
#[test]
fn db_module_no_longer_references_removed_submodules() {
    let root = repo_root();
    let src = root.join(DAEMON_SRC);

    for needle in ["crate::db::sync", "crate::db::workspace_hashes"] {
        for file in rust_sources(&src) {
            let body = std::fs::read_to_string(&file).unwrap();
            assert!(!body.contains(needle), "{} 仍引用 {needle}", file.display());
        }
    }

    let db_mod = std::fs::read_to_string(src.join("db/mod.rs")).unwrap();
    assert!(!db_mod.contains("pub mod sync;"), "db/mod.rs 仍声明 sync");
    assert!(
        !db_mod.contains("pub mod workspace_hashes;"),
        "db/mod.rs 仍声明 workspace_hashes"
    );
    assert!(!src.join("db/sync.rs").exists());
    assert!(!src.join("db/workspace_hashes.rs").exists());
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files
}
