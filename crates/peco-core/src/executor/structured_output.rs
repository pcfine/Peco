// ============================================================================
// StructuredOutputExecutor — 基于工具注入的结构化输出
// ============================================================================
//
// 使用装饰器模式：通过 OutputToolWrapper 在原 ToolExecutor 上追加一个虚拟的
// __submit_output__ 工具。当模型"调用"该工具时，参数被捕获为结构化数据。
//
// Agent 及其 ToolExecutor 零修改 — 仅 Arc::clone 读取。多个 executor 可
// 并发执行，互不干扰。
//
// 参考来源:
// - Anthropic: tool-use-based structured output
// - LangChain: with_structured_output(method="function_calling")
// - Instructor: Mode.TOOLS

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use model_provider::ToolDefinition;

use crate::agent::agent::Agent;
use crate::agent::error::AgentError;
use crate::agent::simple_looper::SimpleAgentLooper;
use crate::tools::ToolExecutor;

use super::{AgentExecutor, ExecutorError, ExecutorInput, ExecutorOutput, ExecutorType};

// ============================================================================
// OutputToolWrapper — ToolExecutor 透明装饰器
// ============================================================================

/// 透明装饰器：在原 ToolExecutor 基础上追加 `__submit_output__` 工具。
///
/// 每次执行创建一个实例，执行完即丢弃。Agent 内部的 ToolExecutor 完全不变。
struct OutputToolWrapper {
    /// Agent 原始 ToolExecutor（Arc::clone，只读）
    inner: Arc<dyn ToolExecutor>,
    /// 虚拟输出工具定义，由用户 output_schema 构建
    output_tool_def: ToolDefinition,
    /// 模型调用 __submit_output__ 时捕获的结构化数据
    captured_data: Mutex<Option<serde_json::Value>>,
}

impl OutputToolWrapper {
    /// 创建包装器，在原执行器基础上追加 `__submit_output__` 工具。
    fn new(inner: Arc<dyn ToolExecutor>, schema: &serde_json::Value) -> Self {
        let output_tool_def = ToolDefinition {
            name: "__submit_output__".to_string(),
            description: "提交最终结构化输出。你必须调用此工具，\
                         以要求的 JSON 格式提交结果，不要返回纯文本。"
                .to_string(),
            parameters: schema.clone(),
        };

        Self {
            inner,
            output_tool_def,
            captured_data: Mutex::new(None),
        }
    }

    /// 取出模型提交的结构化数据。
    fn take_data(&self) -> Option<serde_json::Value> {
        self.captured_data.lock().unwrap().take()
    }
}

#[async_trait]
impl ToolExecutor for OutputToolWrapper {
    async fn execute(&self, name: &str, args: &str) -> Result<String, String> {
        if name == "__submit_output__" {
            // ★ 拦截：不执行实际操作，仅捕获参数
            let parsed: serde_json::Value =
                serde_json::from_str(args).map_err(|e| format!("参数解析失败: {e}"))?;
            *self.captured_data.lock().unwrap() = Some(parsed);
            Ok("输出已提交。".to_string())
        } else {
            // 代理到原始执行器
            self.inner.execute(name, args).await
        }
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut defs = self.inner.definitions();
        defs.push(self.output_tool_def.clone());
        defs
    }
}

// ============================================================================
// StructuredOutputExecutor
// ============================================================================

/// 基于工具注入模式，保证输出符合用户指定 JSON Schema 的执行器。
///
/// # 工作流程
///
/// 1. 将用户 `output_schema` 包装为虚拟工具 `__submit_output__`
/// 2. 运行标准 ReAct 循环（模型可使用任意工具收集数据）
/// 3. 模型必须调用 `__submit_output__` 提交最终结果
/// 4. Executor 截获工具调用的参数即为结构化数据
/// 5. 失败时带具体错误反馈重试
///
/// # 示例
///
/// ```ignore
/// let schema = serde_json::json!({
///     "type": "object",
///     "properties": {
///         "temperature": { "type": "number", "description": "温度（摄氏度）" },
///         "condition": { "type": "string", "description": "天气状况" }
///     },
///     "required": ["temperature", "condition"]
/// });
///
/// let executor = StructuredOutputExecutor::new(agent.clone())
///     .with_max_retries(3);
/// let input = ExecutorInput::with_schema("今天北京天气怎么样？", schema);
/// let output = executor.execute(input).await?;
/// println!("{:?}", output.structured_data);
/// ```
pub struct StructuredOutputExecutor {
    agent: Arc<Agent>,
    /// 验证失败时的最大重试次数（默认 3）
    max_retries: usize,
    /// 每次尝试的最大 ReAct 轮数
    max_turns: Option<usize>,
    /// 每次尝试的超时时间
    timeout: Option<Duration>,
}

impl StructuredOutputExecutor {
    /// 创建执行器。`output_schema` 通过 [`ExecutorInput::output_schema`] 每次调用传入。
    pub fn new(agent: Arc<Agent>) -> Self {
        Self {
            agent,
            max_retries: 3,
            max_turns: None,
            timeout: None,
        }
    }

    /// 设置最大重试次数（默认 3）。
    pub fn with_max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// 设置每次尝试的最大 ReAct 轮数。
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// 设置每次尝试的超时时间。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    // ── 内部方法 ────────────────────────────────────────────────────────────────

    /// 构建带错误反馈的重试 prompt。
    fn build_retry_prompt(original_prompt: &str, error: &str, _attempt: usize) -> String {
        format!(
            "你的上一次回复存在以下问题：\n\n\
             {error}\n\n\
             请修正后重新调用 __submit_output__ 工具提交有效数据。\n\n\
             原始请求：{original_prompt}"
        )
    }

    /// 运行一次 ReAct 尝试，返回（最终文本，结构化数据）。
    async fn run_one_attempt(
        &self,
        prompt: String,
        schema: &serde_json::Value,
    ) -> Result<(String, Option<serde_json::Value>), AgentError> {
        let wrapper = Arc::new(OutputToolWrapper::new(
            self.agent.tool_executor().clone(),
            schema,
        ));

        let handle = SimpleAgentLooper::spawn_with_executor(
            self.agent.clone(),
            prompt,
            wrapper.clone(),
            self.max_turns,
        );

        let final_text = if let Some(timeout) = self.timeout {
            tokio::time::timeout(timeout, handle.wait())
                .await
                .map_err(|_| AgentError::AgentProtocol("timeout".into()))?
        } else {
            handle.wait().await
        }?;

        let data = wrapper.take_data();
        Ok((final_text, data))
    }

    /// 按 JSON Schema 验证输出的 JSON 值。
    ///
    /// 只做结构性检查：`type`、`required`、`properties` 类型、`enum` 约束。
    /// 不依赖外部 crate，覆盖 90% 的常见场景。
    fn validate_against_schema(
        value: &serde_json::Value,
        schema: &serde_json::Value,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // 检查根类型
        if let Some(expected_type) = schema.get("type").and_then(|t| t.as_str()) {
            let actual_type = match value {
                serde_json::Value::Object(_) => "object",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::String(_) => "string",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Bool(_) => "boolean",
                serde_json::Value::Null => "null",
            };
            if actual_type != expected_type {
                errors.push(format!(
                    "根类型应为 '{expected_type}'，实际为 '{actual_type}'"
                ));
                return Err(errors);
            }
        }

        // 检查必需字段 + 属性类型 + enum 约束
        if let serde_json::Value::Object(obj) = value {
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for field in required {
                    if let Some(field_name) = field.as_str()
                        && !obj.contains_key(field_name)
                    {
                        errors.push(format!("缺少必需字段 '{field_name}'"));
                    }
                }
            }

            if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
                for (prop_name, prop_schema) in properties {
                    if let Some(val) = obj.get(prop_name) {
                        if let Some(expected) = prop_schema.get("type").and_then(|t| t.as_str()) {
                            let matches = match expected {
                                "string" => val.is_string(),
                                "number" | "integer" => val.is_number(),
                                "boolean" => val.is_boolean(),
                                "object" => val.is_object(),
                                "array" => val.is_array(),
                                _ => true,
                            };
                            if !matches {
                                errors.push(format!(
                                    "字段 '{prop_name}' 应为 {expected} 类型，\
                                     实际为 {}",
                                    match val {
                                        serde_json::Value::String(_) => "string",
                                        serde_json::Value::Number(_) => "number",
                                        serde_json::Value::Bool(_) => "boolean",
                                        serde_json::Value::Object(_) => "object",
                                        serde_json::Value::Array(_) => "array",
                                        serde_json::Value::Null => "null",
                                    }
                                ));
                            }
                        }
                        if let Some(allowed) = prop_schema.get("enum").and_then(|e| e.as_array())
                            && !allowed.iter().any(|a| a == val)
                        {
                            let allowed_str: Vec<String> =
                                allowed.iter().map(|v| format!("{v}")).collect();
                            errors.push(format!(
                                "字段 '{prop_name}' 的值不在允许范围: [{}]",
                                allowed_str.join(", ")
                            ));
                        }
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[async_trait]
impl AgentExecutor for StructuredOutputExecutor {
    fn name(&self) -> &str {
        "structured_output"
    }

    fn executor_type(&self) -> ExecutorType {
        ExecutorType::StructuredOutput
    }

    async fn execute(&self, input: ExecutorInput) -> Result<ExecutorOutput, ExecutorError> {
        let schema = input
            .output_schema
            .as_ref()
            .ok_or_else(|| ExecutorError::Schema {
                retries: 0,
                message: "StructuredOutputExecutor 需要 output_schema".into(),
            })?;

        if !schema.is_object() || schema.get("type").is_none() {
            return Err(ExecutorError::Schema {
                retries: 0,
                message: "output_schema 必须是包含 'type' 字段的 JSON Schema 对象".into(),
            });
        }

        let mut last_error: Option<String> = None;

        for attempt in 0..=self.max_retries {
            let prompt = if attempt == 0 {
                input.prompt.clone()
            } else {
                Self::build_retry_prompt(&input.prompt, last_error.as_ref().unwrap(), attempt)
            };

            let (text, maybe_data) = self
                .run_one_attempt(prompt, schema)
                .await
                .map_err(ExecutorError::Agent)?;

            let data = match maybe_data {
                Some(d) => d,
                None => {
                    last_error = Some(
                        "未调用 __submit_output__ 工具。请在收集所需信息后，\
                         调用 __submit_output__ 提交结构化结果。"
                            .to_string(),
                    );
                    continue;
                }
            };

            match Self::validate_against_schema(&data, schema) {
                Ok(()) => {
                    return Ok(ExecutorOutput {
                        content: text,
                        usage: Default::default(),
                        structured_data: Some(data),
                        turns: 0,
                        success: true,
                    });
                }
                Err(errors) => {
                    last_error = Some(format!(
                        "输出验证失败，请修正以下问题：\n{}",
                        errors
                            .iter()
                            .enumerate()
                            .map(|(i, e)| format!("  {}. {e}", i + 1))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
            }
        }

        Err(ExecutorError::Schema {
            retries: self.max_retries,
            message: format!(
                "经过 {} 次重试仍无法产生有效的结构化输出。最后错误: {}",
                self.max_retries + 1,
                last_error.unwrap_or_default()
            ),
        })
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── OutputToolWrapper 测试 ──────────────────────────────────────────────────

    #[test]
    fn test_wrapper_definitions_includes_output_tool() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });

        let inner = Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let wrapper = OutputToolWrapper::new(inner, &schema);

        let defs = wrapper.definitions();
        let output_tool = defs.iter().find(|d| d.name == "__submit_output__");
        assert!(output_tool.is_some());
        assert_eq!(output_tool.unwrap().parameters, schema);
    }

    #[test]
    fn test_wrapper_captures_submit_data() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });

        let inner = Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let wrapper = OutputToolWrapper::new(inner, &schema);

        let result = rt.block_on(wrapper.execute("__submit_output__", r#"{"name": "Alice"}"#));
        assert!(result.is_ok());

        let data = wrapper.take_data();
        assert!(data.is_some());
        assert_eq!(data.unwrap()["name"], "Alice");
    }

    #[test]
    fn test_wrapper_delegates_other_tools() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let schema = serde_json::json!({ "type": "object" });
        let inner = Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let wrapper = OutputToolWrapper::new(inner, &schema);

        // __submit_output__ 被拦截
        let result = rt.block_on(wrapper.execute("__submit_output__", r#"{"x": 1}"#));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "输出已提交。");

        // 未知工具代理到原始执行器
        let result = rt.block_on(wrapper.execute("nonexistent_tool", "{}"));
        assert!(result.is_err());
    }

    // ── Schema 验证测试 ────────────────────────────────────────────────────────

    #[test]
    fn test_validate_type_mismatch() {
        let schema = serde_json::json!({ "type": "object", "properties": {} });
        let value = serde_json::json!("not an object");
        assert!(StructuredOutputExecutor::validate_against_schema(&value, &schema).is_err());
    }

    #[test]
    fn test_validate_missing_required() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" }, "age": { "type": "number" } },
            "required": ["name", "age"]
        });
        let value = serde_json::json!({ "name": "Alice" });
        let errs = StructuredOutputExecutor::validate_against_schema(&value, &schema).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("age")));
    }

    #[test]
    fn test_validate_property_type_mismatch() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "temperature": { "type": "number" } },
            "required": ["temperature"]
        });
        let value = serde_json::json!({ "temperature": "warm" });
        let errs = StructuredOutputExecutor::validate_against_schema(&value, &schema).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("temperature") && e.contains("number"))
        );
    }

    #[test]
    fn test_validate_enum_violation() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "condition": { "type": "string", "enum": ["sunny", "cloudy", "rainy"] } },
            "required": ["condition"]
        });
        let value = serde_json::json!({ "condition": "windy" });
        let errs = StructuredOutputExecutor::validate_against_schema(&value, &schema).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("condition")));
    }

    #[test]
    fn test_validate_success() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "temperature": { "type": "number" },
                "condition": { "type": "string", "enum": ["sunny", "cloudy", "rainy"] }
            },
            "required": ["temperature", "condition"]
        });
        let value = serde_json::json!({ "temperature": 25, "condition": "sunny" });
        assert!(StructuredOutputExecutor::validate_against_schema(&value, &schema).is_ok());
    }

    #[test]
    fn test_validate_extra_fields_ok() {
        // 额外字段不导致验证失败
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let value = serde_json::json!({ "name": "Alice", "extra": "field" });
        assert!(StructuredOutputExecutor::validate_against_schema(&value, &schema).is_ok());
    }

    // ── 重试 prompt 测试 ────────────────────────────────────────────────────────

    #[test]
    fn test_build_retry_prompt() {
        let prompt =
            StructuredOutputExecutor::build_retry_prompt("今天天气？", "缺少字段 'temperature'", 1);
        assert!(prompt.contains("今天天气？"));
        assert!(prompt.contains("缺少字段 'temperature'"));
        assert!(prompt.contains("__submit_output__"));
    }
}
