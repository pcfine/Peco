//! DeepSeek Agent example using `peco-core`.
//!
//! Demonstrates the full Agent lifecycle with the DeepSeek provider:
//! 1. Creating a minimal `providers.toml` and `agent.md` at runtime
//! 2. Building an Agent via [`Agent::from_file`] with dependency injection
//! 3. Running the agent through [`AgentLooper`] — a complete ReAct loop
//!
//! ## Running
//!
//! ```sh
//! export DEEPSEEK_API_KEY="sk-..."
//! cargo run -p peco-core --example deepseek_agent
//! ```
//!
//! Or with logging:
//!
//! ```sh
//! RUST_LOG=debug cargo run -p peco-core --example deepseek_agent
//! ```

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use peco_core::agent::{Agent, AgentError, AgentLooper, LooperConfig, LooperEvent, UserMsg};
use peco_core::config::{SystemConfig, UserConfig};
use peco_core::knowledge::KnowledgeManager;
use peco_core::skills::SkillRegister;
use peco_core::utils::intercom::make_async_intercom_pair;
use peco_core::workspace::{AgentLoader, KnowledgeAccess, SkillProvider, ToolDependencies};

// ── Noop trait implementations for the example (agent has tools: []) ─────

struct NoopAgentLoader;
impl AgentLoader for NoopAgentLoader {
    fn load_agent(&self, _name: &str) -> Result<Arc<Agent>, AgentError> {
        Err(AgentError::Config("noop agent loader".into()))
    }
    fn list_agent_names(&self) -> Vec<String> {
        vec![]
    }
}

struct NoopSkillProvider {
    registry: Arc<std::sync::RwLock<SkillRegister>>,
}
impl SkillProvider for NoopSkillProvider {
    fn skill_registry(&self) -> &Arc<std::sync::RwLock<SkillRegister>> {
        &self.registry
    }
}

struct NoopKnowledgeAccess;
impl KnowledgeAccess for NoopKnowledgeAccess {
    fn user_id(&self) -> &str {
        "example-user"
    }
    fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
        // Leaked for simplicity in example — ok for a short-lived example process
        static KM: std::sync::LazyLock<Arc<KnowledgeManager>> = std::sync::LazyLock::new(|| {
            Arc::new(KnowledgeManager::new(
                std::env::temp_dir().join("peco-example-kb"),
            ))
        });
        &KM
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing so we can see what the agent is doing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── 0. Resolve API key ────────────────────────────────────────────────
    if std::env::var("DEEPSEEK_API_KEY").is_err() {
        eprintln!(
            "ERROR: DEEPSEEK_API_KEY environment variable not set.\n\
             Set it with: export DEEPSEEK_API_KEY=\"sk-...\""
        );
        std::process::exit(1);
    }

    // ── 1. Create a temp directory for config files ───────────────────────
    let dir = std::env::temp_dir().join(format!("peco-example-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    println!("Config dir: {}", dir.display());

    // ── 2. Write providers.toml ───────────────────────────────────────────
    let providers_toml = dir.join("providers.toml");
    let providers_content = r#"# Peco provider configuration
default_provider = "deepseek"

[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"

[providers.deepseek.default]
model = "deepseek-v4-flash"
temperature = 0.7
max_tokens = 4096
"#;
    std::fs::write(&providers_toml, providers_content)?;
    println!("Wrote: {}", providers_toml.display());

    // CRITICAL: set env var BEFORE SystemConfig is first accessed
    unsafe {
        std::env::set_var("PECO_PROVIDERS_CONFIG", &providers_toml);
    }

    // ── 3. Write agent.md ─────────────────────────────────────────────────
    let agent_md = dir.join("agent.md");
    let agent_content = r#"---
agent:
  name: "example-agent"
  description: "A simple example agent"
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
  temperature: 0.7
  max_tokens: 4096
tools: []
mcp: []
skills: []
max_turns: 5
---

## Role

You are a helpful AI assistant. Answer questions concisely and accurately.

## Instructions

- Be direct and to the point
- Use Chinese if the user writes in Chinese
- Keep responses concise
"#;
    std::fs::write(&agent_md, agent_content)?;
    println!("Wrote: {}", agent_md.display());

    // ── 4. Build the Agent (new API: dependency injection) ──────────────────
    println!("\n=== Building Agent ===");

    let system_config = SystemConfig::load();
    let user_config = UserConfig::load(&system_config, &dir)?;

    let skill_registry = Arc::new(std::sync::RwLock::new(SkillRegister::new(
        dir.join("skills"),
    )));

    let tool_deps = ToolDependencies {
        agent_loader: Arc::new(NoopAgentLoader),
        skill_provider: Arc::new(NoopSkillProvider {
            registry: skill_registry.clone(),
        }),
        knowledge_access: Arc::new(NoopKnowledgeAccess),
    };

    let agent = Arc::new(Agent::from_file(
        &agent_md,
        &user_config,
        &skill_registry,
        &tool_deps,
    )?);
    println!(
        "Agent built successfully: name={}, provider={}",
        agent.config().agent.name,
        agent.provider().name(),
    );

    // ── 5. Set up AgentLooper ─────────────────────────────────────────────
    use peco_core::session::Session;

    let session = Box::new(Session::new(
        uuid::Uuid::new_v4().to_string(),
        "example session".to_string(),
    ));

    // ── 5. Set up AgentLooper ─────────────────────────────────────────────
    // Create a bidirectional channel pair using AsyncIntercom
    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(256);

    // Split each side into its send/receive halves
    let (event_speaker, user_listener) = looper_side.split();
    let (user_speaker, mut event_listener) = caller_side.split();

    // Cancellation flag
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let pause_flag = Arc::new(AtomicBool::new(false));
    let config = LooperConfig::default();

    let persister = Arc::new(
        peco_core::FileSessionPersister::new(std::env::temp_dir().join("peco-deepseek-example"))
            .await
            .unwrap(),
    );
    let mut looper = AgentLooper::new(
        agent,
        session,
        event_speaker,
        cancel_flag.clone(),
        pause_flag,
        config,
        persister,
    );

    // ── 6. Run the Agent in a background task ─────────────────────────────
    let prompt = "用一句话介绍 Rust 编程语言的核心优势".to_string();
    println!("\n=== Running Agent ===");
    println!("Prompt: {prompt}\n");

    // Send the user query
    user_speaker.send(UserMsg::Query(prompt)).await.ok();
    // Drop speaker so looper.run() exits after processing all messages
    drop(user_speaker);

    // ── 6. Run the Agent and print streaming events ───────────────────────
    println!("=== Response ===");

    let run_fut = looper.run(user_listener);

    // Pin the run future so we can poll it alongside the event stream
    tokio::pin!(run_fut);

    loop {
        tokio::select! {
            result = &mut run_fut => {
                match result {
                    Ok(response) => {
                        println!("\n=== Final Response ===");
                        println!("Turns: {}", response.turns);
                        println!(
                            "Total tokens: {} in, {} out, {} total",
                            response.usage.input_tokens,
                            response.usage.output_tokens,
                            response.usage.total_tokens
                        );
                        // Text output is delivered via LooperEvent::TurnComplete.text field
                    }
                    Err(e) => eprintln!("Agent error: {e}"),
                }
                break;
            }
            maybe_event = event_listener.recv() => {
                match maybe_event {
                    Some(event) => match event {
                        LooperEvent::TextDelta { delta } => {
                            print!("{delta}");
                        }
                        LooperEvent::ReasoningDelta { delta } => {
                            print!("\n[think] {delta}");
                        }
                        LooperEvent::ToolCallStart { name, arguments, .. } => {
                            println!("\n[🔧 calling tool: {name}({arguments})]");
                        }
                        LooperEvent::ToolResult { name, result, .. } => {
                            println!("[📋 tool result from {name}: {result}]");
                        }
                        LooperEvent::ModelUsage { call_index, usage } => {
                            println!(
                                "\n[📊 turn {call_index}: {} in / {} out / {} total tokens]",
                                usage.input_tokens, usage.output_tokens, usage.total_tokens
                            );
                        }
                        _ => {}
                    },
                    None => break,
                }
            }
        }
    }
    println!();

    // ── 7. Cleanup ────────────────────────────────────────────────────────
    let _ = std::fs::remove_dir_all(&dir);

    Ok(())
}
