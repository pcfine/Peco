use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use thiserror::Error;
use tracing::warn;

use crate::agent::{Agent, AgentError};

use super::agent::{AgentMachine, AgentMachineConfig};
use super::hub::{HubConfig, TaskHub};
use super::machine::{Machine, MachineError};
use super::store::InMemoryStore;

/// `kind` 的前缀：Hub 的注册表以 `kind` 为键，同名前缀 + agent 名即可让多个 agent
/// 共处一个 Hub 而不撞 `register` 的重复拒绝。
pub(crate) const AGENT_KIND_PREFIX: &str = "agent:";

/// 名字长度上界：`kind` 由名字派生，故这条同时约束路由键的长度。
pub(crate) const MAX_AGENT_NAME_BYTES: usize = 64;

/// 变体文案即对外错误描述：调用方直接用 `Display`，不再各自拼一遍（两处拼接必然漂移）。
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("machine runtime is unavailable: no tokio runtime in this context")]
    NoRuntime,
    #[error("invalid agent name '{agent_name}': {reason}")]
    InvalidAgentName { agent_name: String, reason: String },
    #[error("failed to load agent '{agent_name}': {reason}")]
    AgentLoad { agent_name: String, reason: String },
    #[error("failed to register machine kind '{kind}': {reason}")]
    Register { kind: String, reason: String },
}

/// 一个工作空间的 `TaskHub` 归属者：Hub 惰性建，machine 按 agent 名注册/替换。
///
/// 强引用只应由 `AgentManager` 持有；工具侧仅持 `Weak`（`ToolDependencies::machine_runtime`），
/// 否则 `Hub → AgentMachine → Agent → ToolExecutor → 工具 → runtime` 闭合，Hub 的后台
/// 扫描任务将随工作空间一起永不退出。
pub struct MachineRuntime {
    hub_config: HubConfig,
    /// 惰性建：`TaskHub::new` 要 tokio runtime，而本类型在 `AgentManager::new` 里同步构造
    /// （那里没有 runtime）。用 `Option` 而非 `OnceLock`：构造失败不得被缓存下来。
    hub: Mutex<Option<Arc<TaskHub>>>,
    /// agent 名（trim 后）→ 注册时那一份 `Arc<Agent>`：只服务 `Arc::ptr_eq` 判断，
    /// 决定要不要 `replace`。
    registered: Mutex<HashMap<String, Arc<Agent>>>,
}

impl Default for MachineRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineRuntime {
    pub fn new() -> Self {
        Self::with_hub_config(HubConfig::default())
    }

    pub fn with_hub_config(config: HubConfig) -> Self {
        Self {
            hub_config: config,
            hub: Mutex::new(None),
            registered: Mutex::new(HashMap::new()),
        }
    }

    pub fn hub(&self) -> Result<Arc<TaskHub>, RuntimeError> {
        let mut slot = lock(&self.hub, "hub");
        if let Some(hub) = slot.as_ref() {
            return Ok(Arc::clone(hub));
        }
        // `TaskHub::new` 内部 spawn 两个后台任务，没有 tokio runtime 时会死在半路。
        // 判定放在写入之前 ⇒ 失败不留缓存，下次进入仍可重试。
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(RuntimeError::NoRuntime);
        }
        let hub = TaskHub::new(self.hub_config.clone(), Arc::new(InMemoryStore::new()));
        *slot = Some(Arc::clone(&hub));
        Ok(hub)
    }

    /// 保证 `agent:<name>` 这个 kind 指向本次加载出来的 agent，返回该 kind。
    ///
    /// 三分支由 `Arc::ptr_eq` 划分：同一份 agent 已注册过就什么都不做（热路径），
    /// 换了一份就 `replace`，从未注册过就走 `register`（重复即拒绝可被观测）。
    pub fn ensure_machine<F>(&self, agent_name: &str, load: F) -> Result<String, RuntimeError>
    where
        F: FnOnce() -> Result<Arc<Agent>, AgentError>,
    {
        let name = validate_agent_name(agent_name)?;
        let kind = Self::kind_for(name);
        // Hub 先于加载：没有 runtime 时不该白读一次 agent.md 与工具装配。
        let hub = self.hub()?;
        // 锁外加载：`Agent::from_file` 是同步文件 IO + 工具装配，持锁会把同 runtime 上
        // 的所有提交排在它后面。
        let agent = load().map_err(|err| RuntimeError::AgentLoad {
            agent_name: name.to_string(),
            reason: err.to_string(),
        })?;

        let machine: Arc<dyn Machine> = Arc::new(AgentMachine::new(
            Arc::clone(&agent),
            AgentMachineConfig {
                kind: kind.clone(),
                ..Default::default()
            },
        ));

        let previous = {
            let mut registered = lock(&self.registered, "registered");
            match registered.get(name) {
                // 配置没变：重装 machine 只会让在飞任务失去取消转发。
                Some(known) if Arc::ptr_eq(known, &agent) => return Ok(kind),
                Some(_) => {
                    hub.replace(&kind, machine)
                        .map_err(|err| register_error(&kind, err))?;
                    registered.insert(name.to_string(), agent)
                }
                None => {
                    hub.register(machine)
                        .map_err(|err| register_error(&kind, err))?;
                    registered.insert(name.to_string(), agent)
                }
            }
        };
        // 旧 agent 带着 provider / 工具图，析构不在注册表锁内做。
        drop(previous);
        Ok(kind)
    }

    fn kind_for(name: &str) -> String {
        format!("{AGENT_KIND_PREFIX}{name}")
    }
}

fn register_error(kind: &str, err: MachineError) -> RuntimeError {
    RuntimeError::Register {
        kind: kind.to_string(),
        reason: err.to_string(),
    }
}

/// 返回 trim 后的名字：调用方必须用它拼 kind、也用它回显，否则 `" a "` 会拼出
/// `agent:a` 却回显 `" a "`，模型对不上自己提交的东西。
///
/// 字符集收紧到 `[A-Za-z0-9_-]` 后，结构上排除了 `/`、`\`、`.`、`..`、空格与 `:` ——
/// 唯一消费点是 `agents_dir.join(name).join("agent.md")`。
pub(crate) fn validate_agent_name(raw: &str) -> Result<&str, RuntimeError> {
    let name = raw.trim();
    let reason = if name.is_empty() {
        Some("must not be empty")
    } else if name.len() > MAX_AGENT_NAME_BYTES {
        Some("must be at most 64 bytes")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some("only ASCII letters, digits, '_' or '-'")
    } else {
        None
    };
    reason.map_or(Ok(name), |reason| {
        Err(RuntimeError::InvalidAgentName {
            agent_name: raw.to_string(),
            reason: reason.to_string(),
        })
    })
}

/// 中毒不致命：两张表里都是普通数据，沿用中毒前的状态比让整个 runtime 失效更接近真实。
fn lock<'a, T>(mutex: &'a Mutex<T>, what: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!(
            table = what,
            "machine runtime lock poisoned; continuing with recovered state"
        );
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::agent::test_support::{completed, scripted_agent};
    use crate::machine::model::{TaggedJson, Task, TaskContext, TaskId, TaskSource};

    fn test_config() -> HubConfig {
        HubConfig {
            emit_min_interval: Duration::ZERO,
            scan_interval: Duration::from_millis(10),
            terminal_grace: Duration::from_millis(50),
            cancel_grace: Duration::from_millis(50),
            ..HubConfig::default()
        }
    }

    fn prompt_task() -> Task {
        Task {
            // Hub 会覆盖 id 与 kind。
            id: TaskId(Uuid::nil()),
            kind: String::new(),
            input: TaggedJson {
                tag: "prompt".to_string(),
                payload: serde_json::Value::String("hi".to_string()),
            },
            context: TaskContext {
                scope: None,
                data: None,
            },
            source: TaskSource::External {
                principal: "tester".to_string(),
            },
            deadline: None,
            labels: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    /// 跑一单并取回终态文本：`ensure_machine` 的效果只有「路由到哪台 machine」可观测。
    async fn run_once(runtime: &MachineRuntime, kind: &str) -> String {
        let hub = runtime
            .hub()
            .expect("the hub is available inside a runtime");
        let id = hub.submit(kind, prompt_task()).await.expect("submit");
        let snapshot = hub
            .await_terminal(&id, Some(Duration::from_secs(2)))
            .await
            .expect("the task reaches a terminal state");
        snapshot
            .outcome
            .and_then(|outcome| outcome.output)
            .and_then(|output| output.payload.as_str().map(str::to_string))
            .expect("a successful turn carries a text output")
    }

    /// 没有 tokio runtime 时必须报错，而不是把 `tokio::spawn` 的 panic 交给调用方；
    /// 失败不得被缓存 —— 之后进入 runtime 仍要能建出来，且只建一次。
    #[test]
    fn hub_failure_is_reported_and_never_cached() {
        let runtime = MachineRuntime::new();
        assert!(
            matches!(runtime.hub(), Err(RuntimeError::NoRuntime)),
            "a hub cannot be built without a tokio runtime"
        );

        let tokio_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build a tokio runtime");
        let (first, second) = tokio_runtime.block_on(async { (runtime.hub(), runtime.hub()) });
        let first = first.expect("the hub is built once a runtime is available");
        let second = second.expect("the second call reuses the cached hub");
        assert!(
            Arc::ptr_eq(&first, &second),
            "a successful build must be cached, not repeated"
        );
    }

    /// 每次 `load()` 都给新的 `Arc<Agent>`（生产路径就是这样）：必须 `replace`，
    /// 否则 `save_agent` 之后的任务仍跑旧配置。
    #[tokio::test]
    async fn a_new_agent_instance_replaces_the_registered_machine() {
        let runtime = Arc::new(MachineRuntime::with_hub_config(test_config()));
        let (old_agent, _old_provider) = scripted_agent(vec![completed("old")]);
        let (new_agent, _new_provider) = scripted_agent(vec![completed("new")]);

        let loaded = Arc::clone(&old_agent);
        let kind = runtime
            .ensure_machine("worker", || Ok(Arc::clone(&loaded)))
            .expect("the first submission registers the machine");
        assert_eq!(kind, "agent:worker");
        assert_eq!(run_once(&runtime, &kind).await, "old");

        // 同一份 Arc 已注册：什么都不该重装。
        let loaded = Arc::clone(&old_agent);
        assert_eq!(
            runtime
                .ensure_machine("worker", || Ok(Arc::clone(&loaded)))
                .expect("re-registering the same agent is a no-op"),
            kind
        );

        let loaded = Arc::clone(&new_agent);
        assert_eq!(
            runtime
                .ensure_machine("worker", || Ok(Arc::clone(&loaded)))
                .expect("a new agent instance replaces the machine"),
            kind,
            "the kind is derived from the name, so it must not change on replace"
        );
        assert_eq!(
            run_once(&runtime, &kind).await,
            "new",
            "the submitted task must run on the freshly loaded agent"
        );
    }

    /// 非法名一律在加载 agent 之前就被拒：既不白读文件，也不留下 machine。
    #[test]
    fn names_outside_the_allowed_charset_are_rejected_before_loading() {
        let runtime = MachineRuntime::new();
        let mut rejected = vec![
            "",
            "  ",
            "..",
            ".hidden",
            "a/b",
            "a\\b",
            "a b",
            "a:b",
            "agent:name",
        ];
        let too_long = "x".repeat(MAX_AGENT_NAME_BYTES + 1);
        rejected.push(&too_long);

        for raw in rejected {
            let result =
                runtime.ensure_machine(raw, || panic!("no agent may be loaded for {raw:?}"));
            assert!(
                matches!(result, Err(RuntimeError::InvalidAgentName { .. })),
                "name {raw:?} must be rejected before the agent is loaded: {result:?}"
            );
        }
    }

    /// 返回值必须是 trim 后的名字：调用方要拿它拼 kind，也要拿它回显。
    #[test]
    fn agent_names_are_trimmed_and_byte_bounded() {
        assert_eq!(
            validate_agent_name(" ok_agent-1 ").expect("the name is valid"),
            "ok_agent-1"
        );
        let longest = "y".repeat(MAX_AGENT_NAME_BYTES);
        assert_eq!(
            validate_agent_name(&longest).expect("64 bytes is still valid"),
            longest
        );
        assert!(
            matches!(
                validate_agent_name(&format!("{longest}y")),
                Err(RuntimeError::InvalidAgentName { .. })
            ),
            "65 bytes exceeds the bound"
        );
    }
}
