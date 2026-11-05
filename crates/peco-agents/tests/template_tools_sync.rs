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

/// 模板 agent 的完整清单（相对 `templates/`）：增删模板必须同时改这里，
/// 否则下面的清单比对变红 —— 清单本身就是规格。
const EXPECTED_TEMPLATE_AGENTS: &[&str] = &[
    "peco/agents/@master/agent.md",
    "peco/agents/@memory/agent.md",
    "team_default/agents/@master/agent.md",
];

/// 模板 tools 中的每个名字必须逐字存在于 BUILTIN_TOOL_NAMES（无拼写漂移）。
#[test]
fn template_tools_exist_in_builtin_registry() {
    let builtin = builtin_tool_names();
    assert!(
        builtin.len() >= 34,
        "BUILTIN_TOOL_NAMES parse looks wrong: only {} entries",
        builtin.len()
    );

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
    }
}

/// 模板集合必须与声明清单逐项相等：比「数量恰好 N」多一层身份校验，
/// 少了模板、多了模板、目录改名都能定位到具体路径。
#[test]
fn template_agents_match_declared_inventory() {
    let templates = repo_root().join("crates/peco-agents/templates");
    let mut actual: Vec<String> = all_template_agents()
        .iter()
        .map(|p| {
            p.strip_prefix(&templates)
                .unwrap_or_else(|_| panic!("{} is not under {}", p.display(), templates.display()))
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    actual.sort();

    let mut expected: Vec<String> = EXPECTED_TEMPLATE_AGENTS
        .iter()
        .map(|p| p.to_string())
        .collect();
    expected.sort();

    let missing: Vec<&String> = expected.iter().filter(|p| !actual.contains(p)).collect();
    let extra: Vec<&String> = actual.iter().filter(|p| !expected.contains(p)).collect();
    assert_eq!(
        actual, expected,
        "模板 agent 清单漂移：缺失 {missing:?}，多余 {extra:?}"
    );
}

/// G1 锁定：@memory agent 必须声明三个图删除工具。
#[test]
fn memory_template_declares_graph_delete_tools() {
    let tools = frontmatter_tools(
        &repo_root().join("crates/peco-agents/templates/peco/agents/@memory/agent.md"),
    );
    for required in GRAPH_DELETE_TOOLS {
        assert!(
            tools.iter().any(|t| t == required),
            "peco/@memory must declare '{required}', got {tools:?}"
        );
    }
}

/// 解析 agent.md frontmatter 的 `llm:` 块中的 `max_tokens:` 值。
fn frontmatter_llm_max_tokens(path: &Path) -> Option<u64> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut lines = content.lines();
    assert_eq!(
        lines.next().map(str::trim_end),
        Some("---"),
        "{} must open with frontmatter ---",
        path.display()
    );

    let mut in_llm = false;
    for line in lines {
        if line.trim_end() == "---" {
            break; // frontmatter 结束
        }
        if line.starts_with("llm:") {
            in_llm = true;
            continue;
        }
        if !in_llm {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ") {
            // 仍在 llm 块内（两空格缩进）
            if let Some(v) = rest.trim().strip_prefix("max_tokens:") {
                return v.trim().parse::<u64>().ok();
            }
        } else if !line.trim().is_empty() {
            in_llm = false; // 顶格 → 离开 llm 块
        }
    }
    None
}

/// 防漂移：所有模板 agent.md 必须显式声明 `llm.max_tokens`——
/// 落到服务端默认（约 4096）时，推理 token 与可见输出共用预算必然截断。
#[test]
fn template_agents_declare_explicit_max_tokens() {
    const FLOOR: u64 = 16_384;
    for md in all_template_agents() {
        match frontmatter_llm_max_tokens(&md) {
            None => panic!(
                "{} does not declare llm.max_tokens — requests fall back to the server default (~4096)",
                md.display()
            ),
            Some(v) => assert!(
                v >= FLOOR,
                "{} declares llm.max_tokens = {v}, below the {FLOOR} floor",
                md.display()
            ),
        }
    }
}

/// 读取 frontmatter 顶层的 `template_version`。
fn frontmatter_template_version(path: &Path) -> u32 {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut lines = content.lines();
    assert_eq!(
        lines.next().map(str::trim_end),
        Some("---"),
        "{} must open with frontmatter ---",
        path.display()
    );

    for line in lines {
        if line.trim_end() == "---" {
            break; // frontmatter 结束
        }
        if let Some(value) = line.strip_prefix("template_version:") {
            return value.trim().parse().unwrap_or_else(|e| {
                panic!("{} 的 template_version 不是整数: {e}", path.display())
            });
        }
    }
    panic!(
        "{} 缺少 template_version —— 无版本号的文件在模板更新时永远不会被覆盖",
        path.display()
    );
}

/// Peco 独占的机器与跨空间工具 —— 也是「Peco 模板」与「团队模板」的能力分界线。
const PECO_ONLY_TOOLS: [&str; 6] = [
    "run_machine_task",
    "inspect_machine_task",
    "create_workspace",
    "open_workspace",
    "dispatch_to_workspace",
    "inspect_workspace_task",
];

/// Peco 模板给 @master 的机器与跨空间工具必须同时满足「版本已 bump + 工具已声明」。
///
/// 版本闸门是老目录的唯一升级入口：`template_version` 不涨，`init_from_template`
/// 会 skip 整个文件，工具只装进全新工作空间。工具按 `tools:` 块校验而非全文包含：
/// prompt 正文也提到这些工具名，全文匹配在被摘出工具清单时仍然通过。
#[test]
fn peco_master_ships_machine_and_cross_space_tools() {
    const MIN_VERSION: u32 = 12;
    let md = repo_root().join("crates/peco-agents/templates/peco/agents/@master/agent.md");

    let version = frontmatter_template_version(&md);
    assert!(
        version >= MIN_VERSION,
        "{} 的 template_version = {version} < {MIN_VERSION}：模板版本未提交或工具集漂移，老工作空间装不上新工具",
        md.display()
    );

    let tools = frontmatter_tools(&md);
    for required in PECO_ONLY_TOOLS {
        assert!(
            tools.iter().any(|t| t == required),
            "{} 的 tools 缺少 '{required}'：模板版本未提交或工具集漂移，实际清单 {tools:?}",
            md.display()
        );
    }
}

/// 团队模板必须留在团队边界内：不含机器与跨空间工具，连正文提及也不行 ——
/// 提示词里出现工具名会诱导模型调用它没有的能力。全文匹配故意从严。
#[test]
fn team_default_master_stays_inside_team_boundary() {
    const MIN_VERSION: u32 = 1;
    let md = repo_root().join("crates/peco-agents/templates/team_default/agents/@master/agent.md");

    let version = frontmatter_template_version(&md);
    assert!(
        version >= MIN_VERSION,
        "{} 的 template_version = {version} < {MIN_VERSION}：模板版本未提交或工具集漂移，老工作空间装不上新内容",
        md.display()
    );

    let text = std::fs::read_to_string(&md)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", md.display()));
    for tool in PECO_ONLY_TOOLS {
        assert!(
            !text.contains(tool),
            "{} 出现了 '{tool}'：模板版本未提交或工具集漂移，团队模板越界到机器/跨空间能力",
            md.display()
        );
    }
}
