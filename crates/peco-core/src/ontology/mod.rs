//! 记忆图谱本体 —— 实体类型闭集（B 侧）与关系组映射（A 侧）的唯一来源。
//!
//! 纯数据 + 纯函数：无 IO、无 trait、无全局可变状态。prompt 渲染、写侧类型
//! 校验/推断、读侧关系分组都从这里派生 —— 词表在多处各写一份时，改一处会漏
//! 另一处。
//!
//! 关系**不是**闭集：谓词原文入库，这里只提供读侧分组用的映射表；扩表不改历史
//! 数据，重算分组即可。

use knowledge_base::Fact;

/// 实体类型闭集。顺序即 prompt 展示序（高频在前），`Other` 恒在末位。
///
/// 只有「具名、可复用、可被再次提及」的才是实体；一次性的值/状态/描述不是，
/// 它们留在文档面整句事实里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    Person,
    Organization,
    SoftwareProject,
    SoftwareComponent,
    Technology,
    CodeSymbol,
    Document,
    KnowledgeBase,
    Agent,
    Issue,
    Task,
    Commit,
    Concept,
    Other,
}

impl EntityType {
    /// 全部类型，顺序即 prompt 展示序。
    pub const ALL: &'static [EntityType] = &[
        EntityType::Person,
        EntityType::Organization,
        EntityType::SoftwareProject,
        EntityType::SoftwareComponent,
        EntityType::Technology,
        EntityType::CodeSymbol,
        EntityType::Document,
        EntityType::KnowledgeBase,
        EntityType::Agent,
        EntityType::Issue,
        EntityType::Task,
        EntityType::Commit,
        EntityType::Concept,
        EntityType::Other,
    ];

    /// canonical 名称 —— 写入节点属性、下发 API 的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            EntityType::Person => "Person",
            EntityType::Organization => "Organization",
            EntityType::SoftwareProject => "SoftwareProject",
            EntityType::SoftwareComponent => "SoftwareComponent",
            EntityType::Technology => "Technology",
            EntityType::CodeSymbol => "CodeSymbol",
            EntityType::Document => "Document",
            EntityType::KnowledgeBase => "KnowledgeBase",
            EntityType::Agent => "Agent",
            EntityType::Issue => "Issue",
            EntityType::Task => "Task",
            EntityType::Commit => "Commit",
            EntityType::Concept => "Concept",
            EntityType::Other => "Other",
        }
    }

    /// 一行说明，供 prompt 注入。
    fn description(self) -> &'static str {
        match self {
            EntityType::Person => "a real person (the user, a colleague, a contact)",
            EntityType::Organization => "a company / team / department / organization",
            EntityType::SoftwareProject => {
                "a software product or project delivered/deployed as a whole"
            }
            EntityType::SoftwareComponent => {
                "a deployable unit / subsystem / module / crate / service inside a project"
            }
            EntityType::Technology => {
                "a third-party technology: library / framework / tool / external service / protocol / file format"
            }
            EntityType::CodeSymbol => {
                "a code-level identifier: function / type / constant / config key / env var name"
            }
            EntityType::Document => "a file / directory / document / spec / config directory",
            EntityType::KnowledgeBase => "a knowledge base / dataset / memory store",
            EntityType::Agent => "an AI assistant / sub-agent",
            EntityType::Issue => "a defect / risk / fault / problem to fix",
            EntityType::Task => "a todo / ticket / fix plan / proposal / decision item",
            EntityType::Commit => "a code commit / changeset / version number",
            EntityType::Concept => "a named, reusable abstract topic / method / domain concept",
            EntityType::Other => "a named entity that fits none of the above",
        }
    }

    /// canonical 名称 → 枚举；闭集外名称返回 `None`。
    pub fn from_name(name: &str) -> Option<EntityType> {
        let name = name.trim();
        Self::ALL.iter().copied().find(|t| t.as_str() == name)
    }
}

/// 精确别名表 —— 形状规则覆盖不到的具名实体（中文人名、CamelCase 技术名、
/// 系统库名）。推断仅在模型没给类型或给了闭集外类型时启用。
const ENTITY_ALIASES: &[(&str, EntityType)] = &[
    ("用户", EntityType::Person),
    ("peco", EntityType::SoftwareProject),
    ("HelixDB", EntityType::Technology),
    ("LanceDB", EntityType::Technology),
    ("Docling", EntityType::Technology),
    ("docx", EntityType::Technology),
    ("知识图谱", EntityType::Concept),
    ("本体", EntityType::Concept),
    ("@memory", EntityType::Agent),
    ("@master", EntityType::Agent),
    ("@private_memory", EntityType::KnowledgeBase),
];

/// 名称形状 → 实体类型，顺序即优先级。
///
/// 只做确定性判定、可单测；命中不了就落 [`EntityType::Other`]，不拒绝、不 panic。
pub fn infer_entity_type(name: &str) -> EntityType {
    let name = name.trim();
    if name.is_empty() {
        return EntityType::Other;
    }
    if let Some((_, ty)) = ENTITY_ALIASES.iter().find(|(alias, _)| *alias == name) {
        return *ty;
    }
    // `@` 前缀是系统知识库的命名约定；已知的 agent 名已在别名表里。
    if name.starts_with('@') {
        return EntityType::KnowledgeBase;
    }
    if looks_like_env_or_url(name) {
        return EntityType::CodeSymbol;
    }
    if looks_like_snake_case(name) {
        return EntityType::CodeSymbol;
    }
    if has_document_suffix(name) {
        return EntityType::Document;
    }
    if looks_like_commit_hash(name) {
        return EntityType::Commit;
    }
    if ["缺陷", "风险", "故障"].iter().any(|k| name.contains(k)) {
        return EntityType::Issue;
    }
    if ["方案", "立项", "待办"].iter().any(|k| name.contains(k)) {
        return EntityType::Task;
    }
    EntityType::Other
}

fn looks_like_env_or_url(name: &str) -> bool {
    name.ends_with("_URL")
        || (name.contains('_')
            && name.chars().any(|c| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
}

fn looks_like_snake_case(name: &str) -> bool {
    name.contains('_')
        && name.chars().any(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn has_document_suffix(name: &str) -> bool {
    [".md", ".rs", ".toml"].iter().any(|s| name.ends_with(s))
}

fn looks_like_commit_hash(name: &str) -> bool {
    (7..=40).contains(&name.len()) && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// 关系组清单，顺序即图例展示序。
const RELATION_GROUPS: &[&str] = &[
    "使用依赖",
    "组成包含",
    "定义记录",
    "修复变更",
    "人员归属",
    "偏好",
    "缺陷问题",
    "名称标识",
    "集成配置",
];

/// 谓词原文 → 关系组。组外谓词返回 `None`（不是错误，前端原样显示原文）。
const RELATION_MAP: &[(&str, &str)] = &[
    ("使用", "使用依赖"),
    ("采用", "使用依赖"),
    ("用到", "使用依赖"),
    ("依赖", "使用依赖"),
    ("依赖于", "使用依赖"),
    ("需要", "使用依赖"),
    ("包含", "组成包含"),
    ("含", "组成包含"),
    ("组成", "组成包含"),
    ("由…组成", "组成包含"),
    ("定义", "定义记录"),
    ("定义于", "定义记录"),
    ("位于", "定义记录"),
    ("记录于", "定义记录"),
    ("记录在", "定义记录"),
    ("文档化于", "定义记录"),
    ("字段位置", "定义记录"),
    ("修复", "修复变更"),
    ("已修复", "修复变更"),
    ("修复于", "修复变更"),
    ("提交", "修复变更"),
    ("变更", "修复变更"),
    ("改动", "修复变更"),
    ("新增", "修复变更"),
    ("任职于", "人员归属"),
    ("就职于", "人员归属"),
    ("所属公司", "人员归属"),
    ("参与", "人员归属"),
    ("负责", "人员归属"),
    ("主管", "人员归属"),
    ("作者", "人员归属"),
    ("提交者", "人员归属"),
    ("偏好", "偏好"),
    ("喜欢", "偏好"),
    ("倾向", "偏好"),
    ("偏好于", "偏好"),
    ("存在缺陷", "缺陷问题"),
    ("有缺陷", "缺陷问题"),
    ("缺陷是", "缺陷问题"),
    ("原名", "名称标识"),
    ("别名", "名称标识"),
    ("全名", "名称标识"),
    ("简称", "名称标识"),
    ("集成", "集成配置"),
    ("接入", "集成配置"),
    ("对接", "集成配置"),
    ("外挂", "集成配置"),
    ("配置", "集成配置"),
    ("配置项", "集成配置"),
    ("环境变量", "集成配置"),
];

/// 纯函数：谓词原文 → 关系组名。无 IO、无状态、可单测。
///
/// 入参先经 [`knowledge_base::normalize_predicate`] —— 与边 label 的轻归一化
/// 同口径，否则「使用 」与「使用」会被分到不同的组。
pub fn group_of(predicate: &str) -> Option<&'static str> {
    let normalized = knowledge_base::normalize_predicate(predicate);
    RELATION_MAP
        .iter()
        .find(|(p, _)| *p == normalized)
        .map(|(_, group)| *group)
}

/// 供前端/查询用的稳定组清单，顺序即图例展示序。
pub fn relation_groups() -> &'static [&'static str] {
    RELATION_GROUPS
}

/// 供 prompt 注入的实体类型清单，形如 `- Person: ...`。
pub fn render_entity_type_lines() -> String {
    EntityType::ALL
        .iter()
        .map(|ty| format!("- {}: {}", ty.as_str(), ty.description()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 写入路径的兜底变换：只做类型校验/推断，落 canonical 类型名。
///
/// **不做**实体名归一化：`compute_entity_id` 按实体名原文算身份，若在此改写
/// 名称，写侧 id 会与读/删/幂等守卫的 id 错位，导致守卫恒 miss、平行边复发。
/// 同样**不改写谓词、不动 id** —— 谓词的轻归一化发生在 knowledge-base 的
/// 唯一 helper 里，读/写/删三处共用。
pub fn normalize_fact(fact: &mut Fact) {
    fact.subject_type = Some(
        resolve_type(fact.subject_type.take(), &fact.subject)
            .as_str()
            .into(),
    );
    fact.object_type = Some(
        resolve_type(fact.object_type.take(), &fact.object)
            .as_str()
            .into(),
    );
}

fn resolve_type(provided: Option<String>, name: &str) -> EntityType {
    provided
        .as_deref()
        .and_then(EntityType::from_name)
        .unwrap_or_else(|| infer_entity_type(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_table_hits_exact_names() {
        assert_eq!(infer_entity_type("用户"), EntityType::Person);
        assert_eq!(infer_entity_type("peco"), EntityType::SoftwareProject);
        assert_eq!(infer_entity_type("HelixDB"), EntityType::Technology);
        assert_eq!(infer_entity_type("docx"), EntityType::Technology);
        assert_eq!(infer_entity_type("知识图谱"), EntityType::Concept);
        assert_eq!(infer_entity_type("@memory"), EntityType::Agent);
        assert_eq!(
            infer_entity_type("@private_memory"),
            EntityType::KnowledgeBase
        );
    }

    #[test]
    fn shape_rules_map_each_form() {
        // 未知 `@` 前缀 → 系统知识库约定
        assert_eq!(infer_entity_type("@whatever"), EntityType::KnowledgeBase);
        // ALLCAPS_WITH_UNDERSCORE / *_URL → 代码符号
        assert_eq!(
            infer_entity_type("PECO_KB_DOCLING_URL"),
            EntityType::CodeSymbol
        );
        assert_eq!(infer_entity_type("MY_CONST"), EntityType::CodeSymbol);
        // snake_case → 代码符号
        assert_eq!(
            infer_entity_type("sanitize_kb_name"),
            EntityType::CodeSymbol
        );
        // 文档后缀 → 文档
        assert_eq!(infer_entity_type("agent.md"), EntityType::Document);
        assert_eq!(infer_entity_type("lib.rs"), EntityType::Document);
        assert_eq!(infer_entity_type("Cargo.toml"), EntityType::Document);
        // 十六进制 → 提交
        assert_eq!(infer_entity_type("7ee7c1e"), EntityType::Commit);
        // 关键词 → 缺陷 / 任务
        assert_eq!(infer_entity_type("已知风险"), EntityType::Issue);
        assert_eq!(infer_entity_type("并发故障"), EntityType::Issue);
        assert_eq!(infer_entity_type("D1修复方案"), EntityType::Task);
        assert_eq!(infer_entity_type("待办"), EntityType::Task);
        // 兜底
        assert_eq!(infer_entity_type("某不知名实体"), EntityType::Other);
        assert_eq!(infer_entity_type(""), EntityType::Other);
        assert_eq!(infer_entity_type("   "), EntityType::Other);
    }

    #[test]
    fn relation_map_every_row_is_reachable() {
        for (predicate, group) in RELATION_MAP {
            assert_eq!(
                group_of(predicate),
                Some(*group),
                "谓词 {predicate:?} 应归入 {group:?}"
            );
        }
        // 组名清单覆盖映射表里出现的每个组
        for (_, group) in RELATION_MAP {
            assert!(
                RELATION_GROUPS.contains(group),
                "组 {group:?} 未登记进 relation_groups"
            );
        }
    }

    #[test]
    fn relation_map_specific_rows() {
        assert_eq!(group_of("原名"), Some("名称标识"));
        assert_eq!(group_of("使用"), Some("使用依赖"));
        assert_eq!(group_of("任职于"), Some("人员归属"));
    }

    #[test]
    fn group_of_normalizes_before_lookup() {
        // 与 label 轻归一化同口径：尾随空白/全角空格不应影响命中
        assert_eq!(group_of("使用 "), Some("使用依赖"));
        assert_eq!(group_of(" 使用"), Some("使用依赖"));
        assert_eq!(group_of("使用　"), Some("使用依赖"));
    }

    #[test]
    fn group_of_unknown_predicate_is_none() {
        assert_eq!(group_of("摸鱼"), None);
        assert_eq!(group_of(""), None);
    }

    #[test]
    fn normalize_fact_sets_types_without_touching_identity() {
        let mut fact = Fact::new("用户", "喜欢", "Rust", 0.8);
        let id = fact.id.clone();

        normalize_fact(&mut fact);

        assert_eq!(fact.subject_type.as_deref(), Some("Person"));
        assert!(EntityType::from_name(fact.object_type.as_deref().unwrap()).is_some());
        // 实体名、谓词、id 逐字不变 —— 归一化不得改写它们
        assert_eq!(fact.subject, "用户");
        assert_eq!(fact.predicate, "喜欢");
        assert_eq!(fact.object, "Rust");
        assert_eq!(fact.id, id);
    }

    #[test]
    fn normalize_fact_is_idempotent() {
        let mut fact = Fact::new("用户", "喜欢", "Rust", 0.8);
        normalize_fact(&mut fact);
        let once = fact.clone();
        normalize_fact(&mut fact);
        assert_eq!(fact.subject_type, once.subject_type);
        assert_eq!(fact.object_type, once.object_type);
        assert_eq!(fact.id, once.id);
        assert_eq!(fact.predicate, once.predicate);
    }

    #[test]
    fn normalize_fact_keeps_valid_type_and_replaces_invalid() {
        let mut fact = Fact::new("x", "使用", "y", 1.0);
        fact.subject_type = Some("Person".into());
        fact.object_type = Some("Bogus".into());
        normalize_fact(&mut fact);
        assert_eq!(
            fact.subject_type.as_deref(),
            Some("Person"),
            "合法类型应保留"
        );
        // 闭集外 → 推断（"y" 无形状特征 → Other）
        assert_eq!(fact.object_type.as_deref(), Some("Other"));
    }

    #[test]
    fn render_lines_cover_closed_set() {
        let lines = render_entity_type_lines();
        for ty in EntityType::ALL {
            assert!(
                lines.contains(&format!("- {}: ", ty.as_str())),
                "渲染结果缺少类型 {}",
                ty.as_str()
            );
        }
        assert!(!lines.contains("attribute of one named entity"));
    }
}
