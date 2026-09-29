// ============================================================================
// 防漂移：模板 agent.md 的 `tools:` 清单必须逐字存在于 BUILTIN_TOOL_NAMES
// ============================================================================
//
// 两侧各从单一权威源取值：
// - BUILTIN_TOOL_NAMES 是 peco-core 私有模块常量（`tools/mod.rs` 只再导出
//   ToolRegister），且 peco-agents 刻意不依赖 peco-core —— 从
//   `tool_register.rs` 源文本的 const 数组字面量逐字提取。
// - 模板清单从 `templates/*/agents/*/agent.md` 的 frontmatter `tools:` 块解析。
//
// 任一模板声明了不存在（或拼写漂移）的工具名 → 该 Agent 在运行时被
// warn + skip，能力静默缺失 —— 本测试把它变成编译期可见的失败。

use std::path::{Path, PathBuf};

/// 仓库根（CARGO_MANIFEST_DIR = crates/peco-agents）。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// 从 `tool_register.rs` 源文本提取 BUILTIN_TOOL_NAMES 数组内的全部字面量。
fn builtin_tool_names() -> Vec<String> {
    let src =
        std::fs::read_to_string(repo_root().join("crates/peco-core/src/tools/tool_register.rs"))
            .expect("failed to read tool_register.rs");
    let decl = src
        .find("BUILTIN_TOOL_NAMES")
        .expect("BUILTIN_TOOL_NAMES not found in tool_register.rs");
    let arr_start = src[decl..]
        .find("&[")
        .map(|i| decl + i + 2)
        .expect("BUILTIN_TOOL_NAMES initializer not found");
    let arr_end = src[arr_start..]
        .find("];")
        .map(|i| arr_start + i)
        .expect("BUILTIN_TOOL_NAMES array terminator not found");
    // 按双引号切分，取引号间的奇数段 = 数组元素字面量
    src[arr_start..arr_end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// 解析 agent.md frontmatter 的 `tools:` 块（`  - name` 列表项，允许行内注释）。
fn frontmatter_tools(path: &Path) -> Vec<String> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut lines = content.lines();
    assert_eq!(
        lines.next().map(str::trim_end),
        Some("---"),
        "{} must open with frontmatter ---",
        path.display()
    );

    let mut tools = Vec::new();
    let mut in_tools = false;
    for line in lines {
        if line.trim_end() == "---" {
            break; // frontmatter 结束
        }
        if line.starts_with("tools:") {
            in_tools = true;
            continue;
        }
        if !in_tools {
            continue;
        }
        if let Some(name) = line.strip_prefix("  - ") {
            tools.push(name.trim().to_string());
        } else if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue; // 块内注释 / 空行
        } else {
            in_tools = false; // 离开 tools 块
        }
    }
    tools
}

/// 枚举 `templates/<t>/agents/<agent>/agent.md` 的全部路径。
fn all_template_agents() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let templates = repo_root().join("crates/peco-agents/templates");
    for tpl in std::fs::read_dir(&templates).expect("read templates dir") {
        let tpl = tpl.unwrap().path();
        if !tpl.is_dir() {
            continue;
        }
        let agents = tpl.join("agents");
        if !agents.is_dir() {
            continue;
        }
        for agent in std::fs::read_dir(&agents).expect("read agents dir") {
            let md = agent.unwrap().path().join("agent.md");
            if md.is_file() {
                paths.push(md);
            }
        }
    }
    assert!(!paths.is_empty(), "no template agent.md files found");
    paths
}

/// G1：两个 @memory 模板必须声明的图删除三工具。
const GRAPH_DELETE_TOOLS: [&str; 3] =
    ["delete_entity_fact", "delete_entity_facts", "delete_entity"];

/// 模板 tools 中的每个名字必须逐字存在于 BUILTIN_TOOL_NAMES（无拼写漂移）。
#[test]
fn template_tools_exist_in_builtin_registry() {
    let builtin = builtin_tool_names();
    assert!(
        builtin.len() >= 34,
        "BUILTIN_TOOL_NAMES parse looks wrong: only {} entries",
        builtin.len()
    );

    let mut checked = 0;
    for md in all_template_agents() {
        let tools = frontmatter_tools(&md);
        assert!(
            !tools.is_empty(),
            "{} declares no tools — parser or template broken",
            md.display()
        );
        for tool in &tools {
            assert!(
                builtin.contains(tool),
                "{} declares tool '{}' which is not in BUILTIN_TOOL_NAMES",
                md.display(),
                tool
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected at least 5 template agents, got {checked}"
    );
}

/// 两套模板的 @memory 是镜像：tools 清单逐字一致（除 KB 名外不应分叉）。
#[test]
fn memory_tools_are_mirrored_across_templates() {
    let personal = frontmatter_tools(
        &repo_root().join("crates/peco-agents/templates/personal/agents/@memory/agent.md"),
    );
    let developer = frontmatter_tools(
        &repo_root().join("crates/peco-agents/templates/developer/agents/@memory/agent.md"),
    );
    assert_eq!(
        personal, developer,
        "@memory tools lists diverged between personal and developer templates"
    );
}

/// G1 锁定：两个 @memory 模板都必须声明三个图删除工具。
#[test]
fn memory_templates_declare_graph_delete_tools() {
    for tpl in ["personal", "developer"] {
        let tools = frontmatter_tools(&repo_root().join(format!(
            "crates/peco-agents/templates/{tpl}/agents/@memory/agent.md"
        )));
        for required in GRAPH_DELETE_TOOLS {
            assert!(
                tools.iter().any(|t| t == required),
                "{tpl}/@memory must declare '{required}', got {tools:?}"
            );
        }
    }
}
