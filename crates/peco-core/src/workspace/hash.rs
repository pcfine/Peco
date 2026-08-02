// ============================================================================
// 工作空间模块哈希 — 各模块文件系统状态的 SHA-256 摘要
// ============================================================================
//
// 为每个模块（agents / skills / workflows / mcp / providers）计算
// 确定性哈希值，供 peco-server 在启动时快速判断文件系统是否有变更。

use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::warn;

/// 空输入的 SHA-256 哈希（用于不存在的模块/文件）。
///
/// 64 字符 hex 字符串。
pub fn empty_hash() -> String {
    // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    let mut hasher = Sha256::new();
    hasher.update(b"");
    hex::encode(hasher.finalize())
}

/// 快速计算一段字节数据的 SHA-256 hex 摘要。
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ============================================================================
// Agents 模块哈希
// ============================================================================

/// 计算 agents 模块的哈希。
///
/// 扫描 `agents_dir` 下所有 `{name}/agent.md` 文件，
/// 按 name 字母序排序，将 `(name, content)` 顺序拼接后取 SHA-256。
///
/// 若目录不存在或无 agent.md 文件，返回 [`empty_hash`]。
pub fn compute_agents_hash(agents_dir: &Path) -> String {
    compute_module_hash(agents_dir, "agent.md")
}

// ============================================================================
// Skills 模块哈希
// ============================================================================

/// 计算 skills 模块的哈希。
///
/// 扫描 `skills_dir` 下所有 `{name}/SKILL.md` 文件，
/// 按 name 字母序排序，将 `(name, content)` 顺序拼接后取 SHA-256。
pub fn compute_skills_hash(skills_dir: &Path) -> String {
    compute_module_hash(skills_dir, "SKILL.md")
}

// ============================================================================
// Workflows 模块哈希
// ============================================================================

/// 计算 workflows 模块的哈希。
///
/// 扫描 `workflows_dir` 下所有 `{name}/workflow.md` 文件，
/// 按 name 字母序排序，将 `(name, content)` 顺序拼接后取 SHA-256。
pub fn compute_workflows_hash(workflows_dir: &Path) -> String {
    compute_module_hash(workflows_dir, "workflow.md")
}

// ============================================================================
// 单文件模块哈希（MCP / Providers）
// ============================================================================

/// 计算 MCP 配置文件的哈希。
///
/// 读取 `{workspace_root}/mcpconfig.json`，计算内容 SHA-256。
/// 若文件不存在，返回 [`empty_hash`]。
pub fn compute_mcp_hash(workspace_root: &Path) -> String {
    compute_single_file_hash(workspace_root, "mcpconfig.json")
}

/// 计算 providers 配置文件的哈希。
///
/// 读取 `{workspace_root}/providers.toml`，计算内容 SHA-256。
/// 若文件不存在，返回 [`empty_hash`]。
pub fn compute_providers_hash(workspace_root: &Path) -> String {
    compute_single_file_hash(workspace_root, "providers.toml")
}

// ============================================================================
// 内部辅助函数
// ============================================================================

/// 通用「目录 + 子文件」模式哈希计算。
///
/// 扫描 `dir`，找到所有 `{name}/{filename}` 路径，按 name 排序，
/// 拼接 `name\n{content}` 后用 SHA-256 摘要。
fn compute_module_hash(dir: &Path, filename: &str) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return empty_hash();
    };

    // 收集 (name, content) 对
    let mut items: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let file_path = entry.path().join(filename);
        if !file_path.exists() {
            continue;
        }
        match std::fs::read_to_string(&file_path) {
            Ok(content) => items.push((name, content)),
            Err(e) => {
                warn!(path = %file_path.display(), error = %e, "Failed to read file for hash computation");
                // 读取失败视为空文件（保持确定性）
                items.push((name, String::new()));
            }
        }
    }

    if items.is_empty() {
        return empty_hash();
    }

    // 按 name 排序，确保确定性
    items.sort_by(|a, b| a.0.cmp(&b.0));

    // 拼接: name1\ncontent1name2\ncontent2...
    let mut hasher = Sha256::new();
    for (name, content) in &items {
        hasher.update(name.as_bytes());
        hasher.update(b"\n");
        hasher.update(content.as_bytes());
    }

    hex::encode(hasher.finalize())
}

/// 单个文件的哈希计算（不存在则返回空哈希）。
fn compute_single_file_hash(dir: &Path, filename: &str) -> String {
    let path = dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(content) => sha256_hex(content.as_bytes()),
        Err(_) => empty_hash(),
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 确定性 ──────────────────────────────────────────────────────────

    #[test]
    fn agents_hash_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("alpha");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.md"), "prompt A").unwrap();

        let b = tmp.path().join("beta");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("agent.md"), "prompt B").unwrap();

        let h1 = compute_agents_hash(tmp.path());
        let h2 = compute_agents_hash(tmp.path());
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn agents_hash_changes_on_content_change() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("test-agent");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.md"), "original").unwrap();

        let h1 = compute_agents_hash(tmp.path());

        std::fs::write(a.join("agent.md"), "modified").unwrap();
        let h2 = compute_agents_hash(tmp.path());

        assert_ne!(h1, h2);
    }

    #[test]
    fn agents_hash_changes_on_name_change() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("old-name");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.md"), "content").unwrap();

        let h1 = compute_agents_hash(tmp.path());

        // 重命名目录
        std::fs::rename(a, tmp.path().join("new-name")).unwrap();
        let h2 = compute_agents_hash(tmp.path());

        assert_ne!(h1, h2);
    }

    // ── 空目录 / 缺失 ──────────────────────────────────────────────────

    #[test]
    fn empty_dir_returns_empty_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let hash = compute_agents_hash(tmp.path());
        assert_eq!(hash, empty_hash());
    }

    #[test]
    fn nonexistent_dir_returns_empty_hash() {
        let hash = compute_agents_hash(Path::new("/nonexistent/path/12345"));
        assert_eq!(hash, empty_hash());
    }

    #[test]
    fn dir_without_md_files_returns_empty_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("no-md");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("README.txt"), "not an agent").unwrap();

        let hash = compute_agents_hash(tmp.path());
        assert_eq!(hash, empty_hash());
    }

    // ── 单文件模块 ─────────────────────────────────────────────────────

    #[test]
    fn mcp_hash_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let hash = compute_mcp_hash(tmp.path());
        assert_eq!(hash, empty_hash());
    }

    #[test]
    fn mcp_hash_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("mcpconfig.json"), "{}").unwrap();
        let hash = compute_mcp_hash(tmp.path());
        assert_ne!(hash, empty_hash());
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn providers_hash_detects_change() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("providers.toml"), "v1").unwrap();
        let h1 = compute_providers_hash(tmp.path());

        std::fs::write(tmp.path().join("providers.toml"), "v2").unwrap();
        let h2 = compute_providers_hash(tmp.path());

        assert_ne!(h1, h2);
    }

    // ── Skills / Workflows ──────────────────────────────────────────────

    #[test]
    fn skills_hash_works() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("my-skill");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("SKILL.md"), "# My Skill").unwrap();

        let hash = compute_skills_hash(tmp.path());
        assert_ne!(hash, empty_hash());
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn workflows_hash_works() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path().join("build");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(w.join("workflow.md"), "# Build").unwrap();

        let hash = compute_workflows_hash(tmp.path());
        assert_ne!(hash, empty_hash());
        assert_eq!(hash.len(), 64);
    }
}
