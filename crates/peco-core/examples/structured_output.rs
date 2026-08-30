//! StructuredOutputExecutor example — 基于工具注入的结构化输出。
//!
//! 演示如何使用 [`StructuredOutputExecutor`] 让 LLM 输出符合自定义
//! JSON Schema 的结构化数据。核心技术：将 `__submit_output__` 虚拟工具
//! 注入 ReAct 循环，模型必须调用该工具提交符合 schema 的结果。
//!
//! ## 运行
//!
//! ```sh
//! export DEEPSEEK_API_KEY="sk-..."
//! cargo run -p peco-core --example structured_output
//! ```
//!
//! ```sh
//! RUST_LOG=info cargo run -p peco-core --example structured_output
//! ```

use std::sync::Arc;

use peco_core::agent::{Agent, AgentError};
use peco_core::config::{SystemConfig, UserConfig};
use peco_core::executor::AgentExecutor;
use peco_core::executor::ExecutorInput;
use peco_core::executor::StructuredOutputExecutor;
use peco_core::knowledge::KnowledgeManager;
use peco_core::skills::SkillRegister;
use peco_core::tools::{AgentAccess, KnowledgeAccess, SkillProvider, ToolDependencies};

// ── Noop trait implementations（本示例无需真实子 agent / KB / skill）─────────

struct NoopAgentAccess;
impl AgentAccess for NoopAgentAccess {
    fn load_agent(&self, _name: &str) -> Result<Arc<Agent>, AgentError> {
        Err(AgentError::Config("noop agent loader".into()))
    }
    fn list_agent_names(&self) -> Vec<String> {
        vec![]
    }
    fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
        Err("noop agent writer".into())
    }
    fn read_agent(&self, _name: &str) -> Result<String, String> {
        Err("noop agent reader".into())
    }
    fn delete_agent(&self, _name: &str) -> Result<(), String> {
        Err("noop agent deleter".into())
    }
}

struct NoopSkillProvider {
    registry: Arc<SkillRegister>,
}
impl SkillProvider for NoopSkillProvider {
    fn skill_registry(&self) -> &Arc<SkillRegister> {
        &self.registry
    }
    fn save_skill(&self, _name: &str, _content: &str) -> Result<(), String> {
        Err("noop skill writer".into())
    }
    fn delete_skill(&self, _name: &str) -> Result<(), String> {
        Err("noop skill deleter".into())
    }
}

struct NoopKnowledgeAccess;
impl KnowledgeAccess for NoopKnowledgeAccess {
    fn user_id(&self) -> &str {
        "example-user"
    }
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        static KM: std::sync::LazyLock<Arc<KnowledgeManager>> = std::sync::LazyLock::new(|| {
            Arc::new(KnowledgeManager::new(
                std::env::temp_dir().join("peco-structured-output-example-kb"),
            ))
        });
        &KM
    }
}

/// 构建一个最简 Agent 实例（不注册任何工具 — 仅 __submit_output__ 注入）。
fn build_agent(agent_dir: &std::path::Path) -> Result<Arc<Agent>, Box<dyn std::error::Error>> {
    let system_config = SystemConfig::load();
    let user_config = UserConfig::load(&system_config, agent_dir)?;

    let skill_registry = Arc::new(SkillRegister::new(agent_dir.join("skills"))?);

    let tool_deps = ToolDependencies {
        agent_access: Arc::new(NoopAgentAccess),
        skill_provider: Arc::new(NoopSkillProvider {
            registry: skill_registry.clone(),
        }),
        knowledge_access: Arc::new(NoopKnowledgeAccess),
        allowed_kbs: Vec::new(),
        workflow_access: None,
        mcp_access: None,
        workflow_persister: None,
        workspace_root: None,
        web_search: None,
    };

    let agent_md = agent_dir.join("agent.md");
    let agent = Arc::new(Agent::from_file(&agent_md, &user_config, &tool_deps)?);

    println!(
        "Agent built: name={}, provider={}, model={}",
        agent.config().agent.name,
        agent.provider().name(),
        agent
            .model_config()
            .model_name
            .as_deref()
            .unwrap_or("(default)"),
    );

    Ok(agent)
}

// ============================================================================
// 示例 1：简单天气信息提取
// ============================================================================

async fn example_weather_info(agent: Arc<Agent>) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  示例 1：天气信息提取（Schema: temperature + condition）    ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // 1. 定义输出 Schema
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "temperature": {
                "type": "number",
                "description": "当前温度（摄氏度）"
            },
            "condition": {
                "type": "string",
                "enum": ["sunny", "cloudy", "rainy", "snowy", "stormy"],
                "description": "天气状况"
            },
            "humidity": {
                "type": "number",
                "description": "湿度百分比（0-100）"
            }
        },
        "required": ["temperature", "condition"]
    });

    // 2. 创建 StructuredOutputExecutor
    let executor = StructuredOutputExecutor::new(agent.clone())
        .with_max_retries(2)
        .with_max_turns(10);

    // 3. 执行
    let input =
        ExecutorInput::with_schema("北京今天天气怎么样？请根据你的知识给出合理估计。", schema);
    let output = executor.execute(input).await?;

    println!("── 最终文本 ──");
    println!("{}", output.content);
    println!("\n── 结构化数据 ──");
    if let Some(ref data) = output.structured_data {
        println!("{}", serde_json::to_string_pretty(data)?);
    }
    println!("success: {}", output.success);

    Ok(())
}

// ============================================================================
// 示例 2：代码分析结构化输出（提取函数签名）
// ============================================================================

async fn example_code_analysis(agent: Arc<Agent>) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  示例 2：代码分析（Schema: functions[] 数组）               ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // 1. 定义输出 Schema — 提取 Rust 函数信息
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "language": {
                "type": "string",
                "enum": ["Rust", "Python", "TypeScript", "Go", "Other"],
                "description": "编程语言"
            },
            "functions": {
                "type": "array",
                "description": "识别到的函数列表",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "函数名" },
                        "is_async": { "type": "boolean", "description": "是否异步" },
                        "is_public": { "type": "boolean", "description": "是否为 pub" },
                        "params_count": { "type": "number", "description": "参数个数" }
                    },
                    "required": ["name", "is_async", "is_public", "params_count"]
                }
            }
        },
        "required": ["language", "functions"]
    });

    let executor = StructuredOutputExecutor::new(agent.clone())
        .with_max_retries(2)
        .with_max_turns(10);

    let code_snippet = r#"
pub async fn fetch_user(id: u64) -> Result<User, Error> {
    let user = db.query("SELECT * FROM users WHERE id = ?", id).await?;
    Ok(user)
}

fn validate_email(email: &str) -> bool {
    email.contains('@')
}

pub fn create_user(name: String, email: String, age: u32) -> User {
    User { name, email, age }
}
"#;

    let prompt =
        format!("分析以下 Rust 代码，提取其中所有函数的信息：\n\n```rust\n{code_snippet}\n```");
    let input = ExecutorInput::with_schema(prompt, schema);
    let output = executor.execute(input).await?;

    println!("── 结构化数据 ──");
    if let Some(ref data) = output.structured_data {
        println!("{}", serde_json::to_string_pretty(data)?);
    }
    println!("success: {}", output.success);

    Ok(())
}

// ============================================================================
// 示例 3：模型未调用 __submit_output__ 时的自动重试
// ============================================================================

async fn example_retry_on_missing_submit(
    agent: Arc<Agent>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  示例 3：带重试的简单分类任务                                ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "sentiment": {
                "type": "string",
                "enum": ["positive", "negative", "neutral"],
                "description": "文本情感倾向"
            },
            "confidence": {
                "type": "number",
                "description": "置信度 0.0-1.0"
            },
            "keywords": {
                "type": "array",
                "items": { "type": "string" },
                "description": "关键情感词"
            }
        },
        "required": ["sentiment", "confidence"]
    });

    // 设置较多重试 — 展示重试机制的鲁棒性
    let executor = StructuredOutputExecutor::new(agent.clone())
        .with_max_retries(3)
        .with_max_turns(8);

    let input = ExecutorInput::with_schema(
        "分析这段文本的情感：\"这个产品的续航超出预期，但屏幕亮度在阳光下不太够用。\"",
        schema,
    );
    let output = executor.execute(input).await?;

    println!("── 结构化数据 ──");
    if let Some(ref data) = output.structured_data {
        println!("{}", serde_json::to_string_pretty(data)?);
    }
    println!("success: {}", output.success);

    Ok(())
}

// ============================================================================
// main
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── 检查 API Key ──────────────────────────────────────────────────────
    if std::env::var("DEEPSEEK_API_KEY").is_err() {
        eprintln!(
            "ERROR: DEEPSEEK_API_KEY environment variable not set.\n\
             Set it with: export DEEPSEEK_API_KEY=\"sk-...\""
        );
        std::process::exit(1);
    }

    // ── 准备临时配置文件 ───────────────────────────────────────────────────
    let dir = std::env::temp_dir().join(format!(
        "peco-structured-output-example-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    println!("Config dir: {}", dir.display());

    // providers.toml
    let providers_toml = dir.join("providers.toml");
    std::fs::write(
        &providers_toml,
        r#"# Peco provider configuration
default_provider = "deepseek"

[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"

[providers.deepseek.default]
model = "deepseek-v4-flash"
temperature = 0.3
max_tokens = 4096
"#,
    )?;

    // CRITICAL: 在 SystemConfig 首次访问之前设置环境变量
    unsafe {
        std::env::set_var("PECO_PROVIDERS_CONFIG", &providers_toml);
    }

    // agent.md — 不需要任何原生工具，工具注入由 executor 完成
    let agent_md = dir.join("agent.md");
    std::fs::write(
        &agent_md,
        r#"---
agent:
  name: "structured-output-example"
  description: "结构化输出示例 Agent"
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
  temperature: 0.3
  max_tokens: 4096
tools: []
mcp: []
skills: []
max_turns: 10
---

## Role

你是一个精确的数据提取助手。

## Instructions

- 根据用户请求提取结构化信息
- **必须调用 __submit_output__ 工具提交结果**
- 不要返回纯文本 — 始终通过 __submit_output__ 输出
- 如果信息不足，使用合理的推断并标明
"#,
    )?;

    // ── 构建 Agent ────────────────────────────────────────────────────────
    let agent = build_agent(&dir)?;

    // ── 运行示例 ───────────────────────────────────────────────────────────
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  StructuredOutputExecutor — 结构化输出示例集合               ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    example_weather_info(agent.clone()).await?;
    example_code_analysis(agent.clone()).await?;
    example_retry_on_missing_submit(agent.clone()).await?;

    println!("\n✅ 全部示例执行完毕！");

    // ── Cleanup ────────────────────────────────────────────────────────────
    let _ = std::fs::remove_dir_all(&dir);

    Ok(())
}
