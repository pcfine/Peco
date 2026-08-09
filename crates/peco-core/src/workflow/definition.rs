// ============================================================================
// WorkflowDefinition — workflow.md 解析与验证
// ============================================================================

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::error::WorkflowError;

// ============================================================================
// WorkflowDefinition
// ============================================================================

/// 从 workflow.md 解析的完整工作流定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub description: String,
    pub version: String,
    /// 整个 workflow 超时（秒）
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// 外部输入参数定义
    #[serde(default)]
    pub inputs: HashMap<String, WorkflowInput>,
    /// 步骤列表
    pub steps: Vec<WorkflowStep>,
    /// workflow.md 的 Markdown body（可选，文档用途）
    #[serde(skip)]
    pub body: Option<String>,
}

/// 从 workflow.md 内部解析用的中间结构（与 WorkflowDefinition 一致但 workflow 字段在外层）。
#[derive(Debug, Clone, Deserialize)]
struct WorkflowFile {
    workflow: WorkflowDefinition,
}

/// 单个输入参数的定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInput {
    /// 参数类型：string, number, boolean, array, object
    #[serde(rename = "type", default = "default_input_type")]
    pub input_type: String,
    /// 参数描述
    #[serde(default)]
    pub description: Option<String>,
    /// 是否必填
    #[serde(default)]
    pub required: bool,
    /// 默认值（JSON）
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

fn default_input_type() -> String {
    "string".to_string()
}

// ============================================================================
// WorkflowStep
// ============================================================================

/// 工作流中的单个步骤。
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowStep {
    /// 步骤唯一标识（在 workflow 内唯一）
    pub id: String,
    /// 人类可读名称
    pub name: String,
    /// 步骤类型
    #[serde(rename = "type")]
    pub step_type: StepType,
    /// 步骤配置（_type tag 由自定义 Deserialize 注入）
    pub config: StepConfig,
    /// DAG 依赖：此步骤需等待哪些步骤完成
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 条件表达式（minijinja 模板语法）：求值为真时执行，为空则总是执行
    #[serde(default)]
    pub condition: Option<String>,
    /// 步骤超时（秒）
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// 失败策略
    #[serde(default = "default_on_failure")]
    pub on_failure: OnFailure,
    /// 重试策略
    #[serde(default)]
    pub retry_policy: Option<RetryPolicy>,
    /// 输出 Schema（仅 agent 类型，用于结构化输出）
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
}

fn default_on_failure() -> OnFailure {
    OnFailure::Abort
}

/// 自定义反序列化：通过两步法注入 `_type` tag。
///
/// 使用 `WorkflowStepRaw` 中间结构体避免递归 —
/// 先将 config 解析为 `serde_json::Value`，从顶层读取 step_type，
/// 注入 `_type` 字段后反序列化为 `StepConfig`。
#[derive(Deserialize)]
struct WorkflowStepRaw {
    id: String,
    name: String,
    #[serde(rename = "type")]
    step_type: StepType,
    config: serde_json::Value,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default = "default_on_failure")]
    on_failure: OnFailure,
    #[serde(default)]
    retry_policy: Option<RetryPolicy>,
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
}

// Custom Deserialize for WorkflowStep that injects _type tag into config
impl<'de> Deserialize<'de> for WorkflowStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let raw = WorkflowStepRaw::deserialize(deserializer)?;

        // Inject _type tag into config
        let step_type_str = raw.step_type.to_string();
        let config = match raw.config {
            serde_json::Value::Object(mut map) => {
                map.insert(
                    "_type".to_string(),
                    serde_json::Value::String(step_type_str),
                );
                serde_json::from_value(serde_json::Value::Object(map)).map_err(|e| {
                    Error::custom(format!("step config deserialization failed: {e}"))
                })?
            }
            other => {
                return Err(Error::custom(format!(
                    "expected config to be a map, got: {other}"
                )));
            }
        };

        Ok(WorkflowStep {
            id: raw.id,
            name: raw.name,
            step_type: raw.step_type,
            config,
            depends_on: raw.depends_on,
            condition: raw.condition,
            timeout_seconds: raw.timeout_seconds,
            on_failure: raw.on_failure,
            retry_policy: raw.retry_policy,
            output_schema: raw.output_schema,
        })
    }
}

// ============================================================================
// StepType
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    Shell,
    Agent,
    /// Phase 4: 纯 LLM 调用（无工具）
    #[serde(rename = "llm")]
    Llm,
    /// Phase 4: 调用指定工具
    #[serde(rename = "tool")]
    Tool,
}

impl std::fmt::Display for StepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shell => write!(f, "shell"),
            Self::Agent => write!(f, "agent"),
            Self::Llm => write!(f, "llm"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

// ============================================================================
// StepConfig
// ============================================================================

/// 步骤配置。
///
/// 使用内部 tag `_type` 区分变体。YAML 编写时 type 在步骤级别声明，
/// 解析时自动注入到 config 中。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum StepConfig {
    Shell {
        command: String,
    },
    Agent {
        agent: String,
        prompt: String,
        #[serde(default)]
        max_turns: Option<usize>,
    },
    /// Phase 4: 纯 LLM 推理
    Llm {
        prompt: String,
    },
    /// Phase 4: 调用指定工具
    Tool {
        tool_name: String,
        arguments: serde_json::Value,
    },
}

// ============================================================================
// OnFailure / RetryPolicy
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// 标记失败但继续执行后续步骤
    Continue,
    /// 立即中止整个 workflow
    Abort,
    /// 按 retry_policy 重试
    Retry,
    /// 暂停等待人工处理
    Pause,
}

impl OnFailure {
    /// 返回 snake_case 的字符串表示，与 serde 序列化格式和 SSE 协议一致。
    pub fn as_str(&self) -> &'static str {
        match self {
            OnFailure::Continue => "continue",
            OnFailure::Abort => "abort",
            OnFailure::Retry => "retry",
            OnFailure::Pause => "pause",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub backoff_seconds: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_seconds: 5,
        }
    }
}

// ============================================================================
// StepResult / StepOutcome
// ============================================================================

/// 单个步骤的执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step: WorkflowStep,
    pub outcome: StepOutcome,
    /// 步骤输出文本（成功时）
    pub output: Option<String>,
    /// 结构化输出（仅 agent 类型 + output_schema 时填充）
    pub structured_output: Option<serde_json::Value>,
    /// 执行耗时
    pub duration: Duration,
    /// 第几次尝试（含重试，从 1 开始）
    pub attempt: usize,
}

/// 步骤执行结果枚举。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepOutcome {
    /// 执行成功，携带输出文本
    Success(String),
    /// 条件不满足，被跳过
    Skipped(String),
    /// 执行失败，携带错误信息
    Failed(String),
}

impl StepOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

// ============================================================================
// Pre-validation — 在 serde 严格解析前检查 YAML 结构，给出可操作的错误信息
// ============================================================================

/// 在严格解析之前预校验 workflow YAML 结构。
///
/// 解析为通用 `serde_json::Value`，逐步骤检查 config 是否包含
/// 该类型所需的必填字段。返回针对 LLM 自修正优化的可操作错误消息。
pub fn pre_validate_workflow_yaml(yaml: &str) -> Result<(), WorkflowError> {
    // 提取 frontmatter
    let frontmatter = if yaml.trim_start().starts_with("---") {
        match crate::agent::split_frontmatter(yaml) {
            Ok((fm, _)) => fm,
            Err(_) => yaml,
        }
    } else {
        yaml
    };

    // 解析为通用 JSON value（宽松模式，不要求所有字段类型匹配）
    let parsed: serde_json::Value = serde_yaml::from_str(frontmatter).map_err(|e| {
        WorkflowError::Parse(format!(
            "YAML syntax error: {e}\n\
             Expected format:\n\
             ---\n\
             workflow:\n\
               name: \"my-workflow\"\n\
               description: \"what it does\"\n\
               version: \"1.0\"\n\
               steps:\n\
                 - id: \"step1\"\n\
                   name: \"Step Name\"\n\
                   type: agent  # or shell\n\
                   config:\n\
                     agent: \"@agent-name\"\n\
                     prompt: \"instructions\"\n\
             ---"
        ))
    })?;

    let workflow = parsed.get("workflow").ok_or_else(|| {
        WorkflowError::Parse(
            "Missing top-level 'workflow:' key.\n\
             Expected format:\n\
             ---\n\
             workflow:\n\
               name: \"my-workflow\"\n\
               ...\n\
             ---"
            .into(),
        )
    })?;

    let steps = match workflow.get("steps") {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(_) => {
            return Err(WorkflowError::Parse(
                "'workflow.steps' must be an array".into(),
            ));
        }
        None => {
            return Err(WorkflowError::Parse(
                "Missing 'workflow.steps' field. A workflow must have at least one step.".into(),
            ));
        }
    };

    if steps.is_empty() {
        return Err(WorkflowError::Parse(
            "'workflow.steps' is empty. A workflow must have at least one step.".into(),
        ));
    }

    let mut errors: Vec<String> = Vec::new();

    for (i, step) in steps.iter().enumerate() {
        let step_num = i + 1;
        let step_id = step
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("(missing id)");
        let step_type = step
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("(missing type)");
        let config = step.get("config");

        match config {
            None => {
                errors.push(format!(
                    "Step {step_num} (id=\"{step_id}\", type=\"{step_type}\"): \
                     missing 'config' field. Every step must have a config block."
                ));
                continue;
            }
            Some(serde_json::Value::Object(config_map)) => match step_type {
                "agent" => {
                    let mut missing = Vec::new();
                    if !config_map.contains_key("agent") {
                        missing.push("agent (which agent to invoke, e.g. \"@assistant\")");
                    }
                    if !config_map.contains_key("prompt") {
                        missing.push("prompt (instructions for the agent)");
                    }
                    if !missing.is_empty() {
                        errors.push(format!(
                            "Step {step_num} (id=\"{step_id}\", type=\"agent\"): \
                                 config is missing required fields:\n  - {}\n\n\
                                 Expected format for agent steps:\n\
                                 config:\n  \
                                   agent: \"<agent-name>\"     # required\n  \
                                   prompt: \"<instructions>\"  # required\n  \
                                   max_turns: <number>        # optional\n\n\
                                 Received:\n  {}",
                            missing.join("\n  - "),
                            serde_yaml::to_string(&config_map)
                                .unwrap_or_else(|_| format!("{config_map:?}"))
                                .lines()
                                .collect::<Vec<_>>()
                                .join("\n  ")
                        ));
                    }
                }
                "shell" => {
                    if !config_map.contains_key("command") {
                        errors.push(format!(
                            "Step {step_num} (id=\"{step_id}\", type=\"shell\"): \
                                 config is missing required field 'command'.\n\n\
                                 Expected format for shell steps:\n\
                                 config:\n  \
                                   command: \"<shell command>\"  # required\n\n\
                                 Received:\n  {}",
                            serde_yaml::to_string(&config_map)
                                .unwrap_or_else(|_| format!("{config_map:?}"))
                                .lines()
                                .collect::<Vec<_>>()
                                .join("\n  ")
                        ));
                    }
                }
                "llm" | "tool" => {
                    errors.push(format!(
                        "Step {step_num} (id=\"{step_id}\"): \
                             step type \"{step_type}\" is not yet supported. \
                             Supported types: shell, agent"
                    ));
                }
                other => {
                    errors.push(format!(
                        "Step {step_num} (id=\"{step_id}\"): \
                             unknown step type \"{other}\". \
                             Supported types: shell, agent"
                    ));
                }
            },
            Some(_) => {
                errors.push(format!(
                    "Step {step_num} (id=\"{step_id}\"): \
                     'config' must be a mapping/dictionary, got a scalar value."
                ));
            }
        }
    }

    if !errors.is_empty() {
        let header = format!(
            "Workflow validation failed with {} error(s):\n\n",
            errors.len()
        );
        let body = errors
            .iter()
            .enumerate()
            .map(|(i, e)| format!("── Error {} ──\n{e}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        let footer = "\n\n──\nPlease fix the errors above and try again.";
        return Err(WorkflowError::Parse(format!("{header}{body}{footer}")));
    }

    Ok(())
}

// ============================================================================
// WorkflowDefinition — parsing methods
// ============================================================================

impl WorkflowDefinition {
    /// 从 workflow.md 文件路径解析。
    pub fn from_file(path: &Path) -> Result<Self, WorkflowError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_yaml(&raw)
    }

    /// 从 YAML 字符串解析（支持 `---` frontmatter 分隔的 workflow.md 格式）。
    pub fn from_yaml(yaml: &str) -> Result<Self, WorkflowError> {
        // 尝试 frontmatter 格式：`---\n...\n---\n...`
        let frontmatter = if yaml.trim_start().starts_with("---") {
            match crate::agent::split_frontmatter(yaml) {
                Ok((fm, body)) => {
                    let mut wf = Self::from_yaml_str(fm)?;
                    let body_trimmed = body.trim();
                    if !body_trimmed.is_empty() {
                        wf.body = Some(body_trimmed.to_string());
                    }
                    return wf.validate().map(|_| wf);
                }
                Err(_) => {
                    // 降级：作为纯 YAML 解析
                    yaml
                }
            }
        } else {
            yaml
        };

        let wf = Self::from_yaml_str(frontmatter)?;
        wf.validate()?;
        Ok(wf)
    }

    /// 从纯 YAML 字符串解析（内部方法，要求顶层 `workflow:` 包装格式）。
    fn from_yaml_str(yaml: &str) -> Result<Self, WorkflowError> {
        let wf: WorkflowFile = serde_yaml::from_str(yaml)
            .map_err(|e| WorkflowError::Parse(format!("YAML parse error: {e}")))?;
        Ok(wf.workflow)
    }

    /// 验证定义合法性。
    ///
    /// 检查：
    /// - 步骤 ID 唯一
    /// - depends_on 引用的步骤 ID 存在
    /// - 无自身依赖
    /// - 无循环依赖
    /// - Phase 1 步骤类型过滤（拒绝 Llm/Tool）
    pub fn validate(&self) -> Result<(), WorkflowError> {
        use std::collections::HashSet;

        // 收集所有 step ID，检查唯一性
        let mut ids = HashSet::new();
        for step in &self.steps {
            if !ids.insert(step.id.clone()) {
                return Err(WorkflowError::InvalidDag(format!(
                    "duplicate step id '{}'",
                    step.id
                )));
            }
        }

        // 验证 depends_on + Phase 1 类型检查
        for step in &self.steps {
            // Phase 1: 拒绝 Phase 4 的步骤类型
            match &step.config {
                StepConfig::Llm { .. } | StepConfig::Tool { .. } => {
                    return Err(WorkflowError::Parse(format!(
                        "step '{}': {:?} type is not supported in Phase 1 (planned for Phase 4)",
                        step.id, step.step_type
                    )));
                }
                _ => {}
            }

            // 验证 depends_on 引用
            for dep in &step.depends_on {
                if dep == &step.id {
                    return Err(WorkflowError::InvalidDag(format!(
                        "step '{}' depends on itself",
                        step.id
                    )));
                }
                if !ids.contains(dep) {
                    return Err(WorkflowError::InvalidDag(format!(
                        "step '{}' depends on unknown step '{}'",
                        step.id, dep
                    )));
                }
            }
        }

        // 循环依赖检测：Kahn 算法（复用 DagGraph::build 的验证逻辑，
        // 但此时 DagGraph 尚未构建，先做轻量检查）
        // 完整检测在 DagGraph::build() 中进行

        Ok(())
    }

    /// 验证外部输入参数是否满足 inputs schema。
    ///
    /// - 检查 required 参数是否存在
    /// - 为缺失的 optional 参数填充 default 值
    /// - 检查参数类型是否匹配声明的 type
    pub fn validate_inputs(
        &self,
        provided: &HashMap<String, serde_json::Value>,
    ) -> Result<HashMap<String, serde_json::Value>, WorkflowError> {
        let mut validated = provided.clone();

        for (name, input_def) in &self.inputs {
            match (provided.get(name), input_def.required) {
                (None, true) => {
                    return Err(WorkflowError::InputValidation(format!(
                        "required input '{}' is missing",
                        name
                    )));
                }
                (None, false) => {
                    // 填充默认值
                    if let Some(default) = &input_def.default {
                        validated.insert(name.clone(), default.clone());
                    }
                }
                (Some(value), _) => {
                    // 类型检查
                    if !Self::check_input_type(value, &input_def.input_type) {
                        return Err(WorkflowError::InputValidation(format!(
                            "input '{}' expected type '{}', got: {}",
                            name, input_def.input_type, value
                        )));
                    }
                }
            }
        }

        Ok(validated)
    }

    fn check_input_type(value: &serde_json::Value, expected: &str) -> bool {
        match expected {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => true, // 未知类型不做校验
        }
    }
}

#[cfg(test)]
mod pre_validate_tests {
    use super::*;

    fn wrap(wf_yaml: &str) -> String {
        format!(
            "---\nworkflow:\n  name: \"test\"\n  description: \"test\"\n  version: \"1.0\"\n{}\n---",
            wf_yaml
        )
    }

    #[test]
    fn test_valid_agent_step() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: agent\n      config:\n        agent: \"@test\"\n        prompt: \"do it\"",
        );
        assert!(pre_validate_workflow_yaml(&yaml).is_ok());
    }

    #[test]
    fn test_valid_shell_step() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: shell\n      config:\n        command: \"echo hi\"",
        );
        assert!(pre_validate_workflow_yaml(&yaml).is_ok());
    }

    #[test]
    fn test_agent_missing_agent_field() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: agent\n      config:\n        prompt: \"do it\"",
        );
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("agent"), "should mention 'agent': {msg}");
        assert!(
            msg.contains("missing required fields"),
            "should say missing: {msg}"
        );
        assert!(
            msg.contains("Expected format for agent steps"),
            "should show expected format: {msg}"
        );
        assert!(msg.contains("Received:"), "should show received: {msg}");
    }

    #[test]
    fn test_agent_missing_prompt_field() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: agent\n      config:\n        agent: \"@test\"",
        );
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("prompt"), "should mention 'prompt': {msg}");
    }

    #[test]
    fn test_shell_missing_command_field() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: shell\n      config:\n        something: \"else\"",
        );
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("command"), "should mention 'command': {msg}");
        assert!(
            msg.contains("Expected format for shell steps"),
            "should show expected format: {msg}"
        );
    }

    #[test]
    fn test_missing_steps() {
        let yaml =
            "---\nworkflow:\n  name: \"test\"\n  description: \"test\"\n  version: \"1.0\"\n---";
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("Missing 'workflow.steps'"));
    }

    #[test]
    fn test_empty_steps() {
        let yaml = wrap("  steps: []");
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn test_missing_workflow_key() {
        let yaml = "---\nname: \"test\"\n---";
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("Missing top-level 'workflow:'"));
    }

    #[test]
    fn test_multiple_errors_reported() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: agent\n      config:\n        prompt: \"do it\"\n    - id: \"s2\"\n      name: \"Step 2\"\n      type: shell\n      config:\n        something: \"else\"",
        );
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Error 1"), "should have Error 1: {msg}");
        assert!(msg.contains("Error 2"), "should have Error 2: {msg}");
        assert!(msg.contains("agent"), "should mention agent: {msg}");
        assert!(msg.contains("command"), "should mention command: {msg}");
    }

    #[test]
    fn test_unknown_step_type() {
        let yaml = wrap(
            "  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: unknown_type\n      config:\n        foo: bar",
        );
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("unknown step type"));
    }

    #[test]
    fn test_missing_config() {
        let yaml = wrap("  steps:\n    - id: \"s1\"\n      name: \"Step 1\"\n      type: agent");
        let err = pre_validate_workflow_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("missing 'config' field"));
    }
}
