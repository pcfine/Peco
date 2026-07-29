# Peco Workflow 技术方案

> 版本: 1.4 | 日期: 2026-07-29 | 状态: Phase 2 完成 — 持久化、WorkflowManager、execute_workflow 工具、WorkSpace 集成

---

## 1. 概述与目标

### 1.1 问题

当前 peco-core 是一个「对话驱动」的 Agent 引擎。Agent 通过 ReAct 循环（模型推理 → 工具调用 → 循环）处理用户请求。这在单步任务中运行良好，但面对多步、有依赖、可并行的复杂任务时存在根本局限：

- **LLM 不是可靠的编排器**：ReAct 循环让模型自行决定步骤顺序，可能跳过、重复或遗漏关键步骤
- **无法并行执行**：LLM 的推理是串行的，无法同时执行多个独立的子任务
- **无流程记忆**：每次对话从零开始，中间失败需要用户手动引导恢复
- **人机协同困难**：需要用户确认时，缺乏精确的暂停/恢复机制
- **不可复用**：相同的多步流程每次都需要完整描述

### 1.2 目标

在 `peco-core` 中构建一个**基础的、与运行时无关的** Workflow 模块，提供：

1. **声明式流程定义** — YAML/Markdown 格式，与 `agent.md` 风格一致
2. **DAG 编排引擎** — 拓扑排序执行，支持依赖、并行、条件分支
3. **步骤级可观测** — 每步独立的输入/输出/状态/耗时
4. **失败策略** — 继续/中止/重试/等待确认
5. **与现有体系无缝集成** — 复用 Tool/ToolExecutor/AgentLooper/DI trait 体系

### 1.3 非目标（Phase 1–2 不做）

- Cron 定时触发（由 peco-server 的 `CronScheduler` 负责，Phase 3）
- 前端可视化编辑器
- 分布式执行
- 事务性回滚（Saga 模式）
- 跨 Workflow 嵌套调用
- Llm/Tool 步骤类型（Phase 4）
- 重试策略 RetryPolicy 实际执行（Phase 4）
- 断点续执行（从 `WorkflowSnapshot` 恢复引擎，Phase 4）

---

## 2. 设计约束

### 2.1 peco-core 中无长时间运行的后台服务

peco-core 是库，不启动持久化后台线程或定时器。`CronScheduler` 在 peco-server 层。

**决策**：Workflow 引擎采用 **spawn 模型**（与 `AgentLooper::spawn()` 一致）：
- 调用 `WorkflowEngine::spawn()` 在 tokio 中启动一个**任务生命周期内**的后台任务
- 返回 `WorkflowHandle`，通过事件通道驱动进度
- tokio runtime 由调用方（peco-server / peco-cli）提供
- Workflow 执行完毕后台任务自动退出

这是与 `AgentLooper::spawn()` 完全相同的模式——后台任务随 Workflow 的生命周期存在，而非持久化服务。

### 2.2 窄 Trait 依赖注入

遵循 `tools::deps` 的既有模式：Workflow 需要的依赖通过窄 trait 定义，由 `WorkSpace`（或 `AgentManager`）实现，不直接耦合。

### 2.3 与现有工具/Agent 体系的关系

- Workflow 的 **Agent 类型步骤** 使用 `SimpleAgentLooper` 执行（与 `DelegateSubAgent` 一致）。`SimpleAgentLooper` 接收的 `Agent` 已附带 `agent.md` 中定义的 tools，无需额外注入。
    - **设计决策：为什么用 `SimpleAgentLooper`（batch 模式）而非 `AgentLooper`（流式模式）**：
        1. Workflow 步骤是后台批量任务，不是面向终端用户的交互式对话。用户通过 `WorkflowEvent`（`StepStarted`/`StepCompleted`/`StepFailed`）观察进度，不需要 Agent 推理的逐 token 流式输出。
        2. `SimpleAgentLooper` 的 API 更简单：`spawn(agent, prompt, max_turns) → handle.wait()`，返回值是最终文本。这与 Workflow 步骤的"输入→处理→输出"语义完全匹配。
        3. 避免过度设计：在 Workflow 步骤的上下文中，Agent 的工具调用和推理过程是"内部实现细节"，暴露这些细节会给 `WorkflowEvent` 增加不必要的复杂性。如果将来需要调试/审计，可通过 `StepResult` 扩展（如增加 `tool_calls_log` 字段），而非切换到流式引擎。
        4. 与 Anthropic Claude Agent SDK 的子 Agent 模式一致：父 Agent 调子 Agent 作为工具，获取最终结果，不感知其内部推理过程。
- Workflow 的 **Shell 类型步骤** 直接调用 `tokio::process::Command`（Phase 4 可通过 `ToolExecutor` 统一调度）
- Workflow 的 **LLM 类型步骤** 直接调用 `ModelProvider::chat()`（batch 模式，无工具）
- Workflow 的 **Tool 类型步骤** 通过 `ToolExecutor::execute()` 分派
- Workflow **本身**可被包装为一个工具（`execute_workflow`），让 Agent 调用。`execute_workflow` 的具体设计将在 WorkflowEngine 实现完成后重新评估，当前文档中的代码为占位草图。

### 2.4 Rust 类型层面的约束

- Workflow 定义需实现 `Serialize + Deserialize`（持久化支持）
- Workflow 引擎状态需可快照化（断点续执行支持）
- 事件类型需 `Clone + Send + 'static`（通过 intercom 通道传输）

---

## 3. 架构总览

```
┌─────────────────────────────────────────────────────────┐
│                    workflow.md                           │
│              (YAML frontmatter + 可选 Markdown body)      │
└──────────────────────┬──────────────────────────────────┘
                       │ 解析
┌──────────────────────▼──────────────────────────────────┐
│              WorkflowDefinition                          │
│   name, version, timeout, steps[],                      │
│   steps: [{id, type, config, depends_on, condition,     │
│            on_failure, retry_policy, output_schema}]     │
└──────────────────────┬──────────────────────────────────┘
                       │ 编译为 DAG
┌──────────────────────▼──────────────────────────────────┐
│              WorkflowEngine                              │
│   ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
│   │ DagGraph    │  │ StepExecutor │  │ TemplateCtx   │  │
│   │ (拓扑排序)   │  │ (分发执行)    │  │ (变量传递)    │  │
│   └─────────────┘  └──────────────┘  └───────────────┘  │
│                                                         │
│   状态机: Pending → Running → Paused → Completed/Failed  │
│                                                         │
│   事件通道: Speaker<WorkflowEvent> → Listener            │
└──────────────────────┬──────────────────────────────────┘
                       │ spawn / handle
┌──────────────────────▼──────────────────────────────────┐
│              WorkflowHandle                              │
│   .recv_event()  .pause()  .resume()  .cancel()         │
│   .approve(step_id)  .wait()  .run_id()                 │
└─────────────────────────────────────────────────────────┘
```

### 与现有 AgentLooper 的对照

| 概念 | AgentLooper | WorkflowEngine |
|------|------------|----------------|
| 定义来源 | `agent.md` | `workflow.md` |
| 状态机 | OuterState + ReActState（双层） | `WorkflowState`（单层，步骤级） |
| 控制句柄 | `LooperHandle` | `WorkflowHandle` |
| 事件通道 | `Speaker`/`Listener` → `LooperEvent` | `Speaker`/`Listener` → `WorkflowEvent` |
| 后台模式 | `AgentLooper::spawn()` → tokio task | `WorkflowEngine::spawn()` → tokio task |
| 步骤执行 | 模型决定工具调用 | 引擎按 DAG 拓扑执行 |
| 并行机制 | `JoinSet` 并发执行工具 | `JoinSet` 并发执行无依赖步骤 |

---

## 4. 核心模块设计

### 4.1 workflow.md 文件格式

遵循 `agent.md` / `SKILL.md` 的约定：YAML frontmatter + 可选 Markdown body。

```yaml
---
workflow:
  name: "code-review-and-fix"
  description: "代码审查 → 自动修复 → 验证"
  version: "1.0"
  timeout_seconds: 600          # 整个 workflow 超时 (秒)
inputs:                         # 外部输入参数（可选）
  target_branch:
    type: string
    description: "要审查的目标分支"
    required: true
  language:
    type: string
    description: "主要编程语言"
    default: "rust"
steps:
  - id: "lint"
    name: "静态检查"
    type: shell
    config:
      command: "cargo clippy --workspace -- -D warnings 2>&1"
    timeout_seconds: 120
    on_failure: "continue"      # continue | abort | retry | pause

  - id: "review"
    name: "AI 代码审查"
    type: agent
    config:
      agent: "@code-reviewer"
      prompt: "请审查 {{ inputs.target_branch }} 分支的代码改动，主要语言 {{ inputs.language }}。输出 JSON 格式结果。"
      max_turns: 20
    depends_on: ["lint"]
    output_schema:              # 可选：约束 Agent 的结构化输出
      type: object
      properties:
        issues:
          type: array
          items:
            type: object
            properties:
              severity: { type: string, enum: ["critical", "major", "minor"] }
              file: { type: string }
              description: { type: string }
    retry_policy:               # 可选：失败重试（Phase 4）
      max_attempts: 2
      backoff_seconds: 5

  - id: "auto-fix"
    name: "自动修复"
    type: agent
    config:
      agent: "@developer"
      prompt: "根据审查结果修复问题：{{ steps.review.output }}"
    depends_on: ["review"]
    # 条件表达式使用 minijinja 语法：仅在上一步成功时执行
    condition: "{{ steps.review.success }}"

  - id: "verify"
    name: "验证修复"
    type: shell
    config:
      command: "cargo test --workspace 2>&1"
    depends_on: ["auto-fix"]
    on_failure: "pause"         # 人工确认

  - id: "notify"
    name: "生成报告"
    type: llm                    # 纯 LLM 调用（无工具）
    config:
      prompt: |
        总结本次代码审查流程：
        - Lint: {{ steps.lint.output | truncate(200) }}
        - 审查问题数: {{ steps.review.output.issues | length }}
        - 验证: {{ steps.verify.output | truncate(200) }}
    depends_on: ["verify"]
---
# 可选的 Markdown 正文，作为 workflow 的文档说明
此 Workflow 在每次 PR 提交时自动运行，确保代码质量。
```

**StepType 枚举**：

| type | 说明 | 执行方式 | Phase |
|------|------|---------|-------|
| `shell` | 执行 Shell 命令 | 创建 `tokio::process::Command` 执行 | 1 |
| `agent` | 调用 Agent（ReAct 循环） | `SimpleAgentLooper::spawn()`（Agent 自带 agent.md 中定义的工具） | 1 |
| `llm` | 纯 LLM 推理（无工具） | 直接调用 `ModelProvider::chat()`（batch 模式） | 4 |
| `tool` | 调用指定工具 | 通过 `ToolExecutor::execute()` | 4 |

**关于 Agent 步骤的工具**：`SimpleAgentLooper::spawn(agent, prompt, max_turns)` 接收的 `Agent` 实例已包含从 `agent.md` 解析的完整工具列表（`tools: [shell, fetch, ...]`）。Workflow 引擎无需额外注入工具——Agent 步骤天然具备 `agent.md` 中声明的能力。

**Phase 2 扩展**：`parallel`（并行子步骤）、`human_approval`（人工审批）、`workflow`（子 Workflow 嵌套）。

### 4.2 WorkflowDefinition 解析

```rust
// crates/peco-core/src/workflow/definition.rs

use std::collections::HashMap;
use std::time::Duration;

/// 从 workflow.md 解析的完整工作流定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub description: String,
    pub version: String,
    pub timeout: Option<Duration>,
    /// 外部输入参数定义（可选）
    #[serde(default)]
    pub inputs: HashMap<String, WorkflowInput>,
    pub steps: Vec<WorkflowStep>,
    /// 可选：workflow.md 的 Markdown body（文档用途）
    pub body: Option<String>,
}

/// 单个输入参数的定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInput {
    /// 参数类型：string, number, boolean, array, object
    #[serde(rename = "type")]
    pub input_type: String,
    /// 参数描述
    pub description: Option<String>,
    /// 是否必填
    #[serde(default)]
    pub required: bool,
    /// 默认值（JSON）
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// 步骤唯一标识（在 workflow 内唯一）
    pub id: String,
    /// 人类可读名称
    pub name: String,
    /// 步骤类型
    #[serde(rename = "type")]
    pub step_type: StepType,
    /// 步骤配置
    pub config: StepConfig,
    /// DAG 依赖：此步骤需等待哪些步骤完成
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 条件表达式（minijinja 模板语法）：求值为真时执行，为空则总是执行
    #[serde(default)]
    pub condition: Option<String>,
    /// 步骤超时
    pub timeout_seconds: Option<u64>,
    /// 失败策略
    #[serde(default = "default_on_failure")]
    pub on_failure: OnFailure,
    /// 重试策略
    #[serde(default)]
    pub retry_policy: Option<RetryPolicy>,
    /// 输出 Schema（仅 agent 类型，用于结构化输出）
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// 步骤配置。
///
/// **反序列化策略**：使用内部 tag `_type` 区分变体。
/// YAML 编写时 `_type` 字段由解析器自动从 `WorkflowStep.step_type` 注入，
/// 用户无需手动编写——配置中的 type 在步骤级别已声明，config 内无需重复。
///
/// 不使用 `#[serde(untagged)]` 以避免：
/// - 单字段变体间的歧义（Llm { prompt } vs Shell { command }）
/// - 多余字段被静默忽略而非报错
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

/// 解析时从 `WorkflowStep.step_type` 自动向 config map 注入 `_type` 字段，
/// 使 serde 能正确分发到对应变体。
///
/// 实现方式：自定义 `Deserialize` for `WorkflowStep`，或解析时两步走：
///   1. 将 config 解析为 `serde_json::Value::Object`
///   2. 注入 `"_type": "<step_type>"` 键值对
///   3. 从修改后的 Value 反序列化 `StepConfig`
impl WorkflowStep {
    /// Phase 1 阶段在 validate() 中拒绝 Phase 4 的步骤类型，
    /// 避免运行时 `todo!()` 崩溃。
    pub fn validate_phase1(&self) -> Result<(), WorkflowError> {
        match &self.config {
            StepConfig::Llm { .. } | StepConfig::Tool { .. } => {
                Err(WorkflowError::Parse(format!(
                    "步骤 '{}': {:?} 类型在 Phase 4 中支持",
                    self.id, self.step_type
                )))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

fn default_on_failure() -> OnFailure {
    OnFailure::Abort
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
```

**步骤执行结果**（运行时类型）：

```rust
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
```

**WorkflowError 错误类型**：

```rust
// crates/peco-core/src/workflow/error.rs

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// 定义文件解析失败
    #[error("failed to parse workflow definition: {0}")]
    Parse(String),

    /// DAG 结构不合法（循环依赖、未知引用、自引用等）
    #[error("invalid DAG: {0}")]
    InvalidDag(String),

    /// 步骤执行失败
    #[error("step '{step_id}' failed: {message}")]
    StepExecution {
        step_id: String,
        message: String,
    },

    /// 模板渲染错误
    #[error("template error: {0}")]
    Template(String),

    /// 输入参数校验失败
    #[error("input validation failed: {0}")]
    InputValidation(String),

    /// Workflow 超时
    #[error("workflow timed out after {elapsed_seconds}s")]
    Timeout { elapsed_seconds: u64 },

    /// 被取消
    #[error("workflow cancelled")]
    Cancelled,

    /// 持久化错误
    #[error("persistence error: {0}")]
    Persist(String),

    /// IO 错误
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

**解析入口**：

```rust
impl WorkflowDefinition {
    /// 从 workflow.md 文件路径解析
    pub fn from_file(path: &Path) -> Result<Self, WorkflowError>;

    /// 从 YAML 字符串解析
    pub fn from_yaml(yaml: &str) -> Result<Self, WorkflowError>;

    /// 验证定义合法性（DAG 无环、depends_on 引用存在、步骤类型 Phase 支持等）
    pub fn validate(&self) -> Result<(), WorkflowError>;

    /// 验证外部输入参数是否满足 inputs schema。
    ///
    /// - 检查 required 参数是否存在
    /// - 为缺失的 optional 参数填充 default 值
    /// - 检查参数类型是否匹配声明的 type
    pub fn validate_inputs(
        &self,
        inputs: &HashMap<String, serde_json::Value>,
    ) -> Result<HashMap<String, serde_json::Value>, WorkflowError>;
}
```

解析流程与 `AgentProfile` 一致：`read_to_string → split_frontmatter → serde_yaml::from_str`。

### 4.3 WorkflowEngine 引擎

引擎是 Workflow 模块的核心。采用 **spawn 模型**（与 `AgentLooper::spawn()` 一致）。

```rust
// crates/peco-core/src/workflow/engine.rs

use std::sync::Arc;
use tokio::sync::Mutex;
use crate::utils::intercom::{Speaker, Listener};

/// WorkflowHandle 内部持有的 JoinHandle 类型别名。
pub type SharedWorkflowTask = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

/// Workflow 执行引擎。
///
/// # 生命周期
///
/// ```text
/// let handle = WorkflowEngine::spawn(definition, deps, config, inputs);
///
/// loop {
///     match handle.recv_event().await {
///         Some(WorkflowEvent::StepStarted { .. }) => { /* 转发 */ }
///         Some(WorkflowEvent::StepCompleted { .. }) => { /* 记录 */ }
///         Some(WorkflowEvent::Completed { .. }) => break,
///         Some(WorkflowEvent::Paused { .. }) => {
///             // 等待用户审批后:
///             handle.approve(ApprovalDecision::Proceed).await;
///         }
///         Some(WorkflowEvent::Failed { .. }) => break,
///         None => break,
///     }
/// }
/// ```
pub struct WorkflowEngine {
    /// 本次执行的唯一 ID
    run_id: String,
    definition: WorkflowDefinition,
    /// Agent 加载（用于 Agent 类型步骤）
    agent_access: Arc<dyn AgentAccess>,
    /// 执行快照持久化
    persister: Arc<dyn WorkflowPersister>,
    config: WorkflowConfig,
}

/// 引擎配置
pub struct WorkflowConfig {
    /// 事件通道 buffer 大小
    pub event_buffer: usize,
    /// Phase 2: 步骤间最小延迟（防止模型速率限制）
    pub step_delay: Option<Duration>,
}

/// 外部控制句柄。
///
/// `speaker` / `listener` 对用于事件流：engine 持有 speaker 发送事件，
/// handle 持有 listener 接收事件供外部消费。
///
/// 审批通过独立的 mpsc channel：engine 暂停时等待 `approval_rx`，
/// handle 通过 `approve()` 向 `approval_tx` 发送决策。
pub struct WorkflowHandle {
    run_id: String,
    cancel_flag: Arc<AtomicBool>,
    listener: Listener<WorkflowEvent>,
    /// 审批决策通道（handle → engine）
    approval_tx: tokio::sync::mpsc::Sender<ApprovalResponse>,
    join_handle: SharedWorkflowTask,
}
```

**引擎内部状态机**：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineState {
    /// 初始状态
    Pending,
    /// 正在执行步骤
    Running,
    /// 暂停中（等待审批或外部 resume）
    Paused,
    /// 已完成
    Completed,
    /// 已失败
    Failed,
    /// 已取消
    Cancelled,
}
```

**核心执行循环**（在 `tokio::spawn` 中运行）：

```rust
impl WorkflowEngine {
    pub fn spawn(
        definition: WorkflowDefinition,
        agent_access: Arc<dyn AgentAccess>,
        persister: Arc<dyn WorkflowPersister>,
        config: WorkflowConfig,
        /// 外部传入的运行时输入参数
        inputs: HashMap<String, serde_json::Value>,
    ) -> WorkflowHandle {
        let run_id = uuid::Uuid::new_v4().to_string();
        let (speaker, listener) = make_async_intercom_pair(config.event_buffer);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (approval_tx, approval_rx) = tokio::sync::mpsc::channel::<ApprovalResponse>(8);

        let engine = Self {
            run_id: run_id.clone(),
            definition,
            agent_access,
            persister,
            config,
        };

        let join_handle = tokio::spawn(async move {
            engine.run(speaker, approval_rx, cancel_flag.clone(), inputs).await
        });

        WorkflowHandle {
            run_id,
            cancel_flag,
            listener,
            approval_tx,
            join_handle: Arc::new(Mutex::new(Some(join_handle))),
        }
    }

    async fn run(
        mut self,
        speaker: Speaker<WorkflowEvent>,
        mut approval_rx: tokio::sync::mpsc::Receiver<ApprovalResponse>,
        cancel_flag: Arc<AtomicBool>,
        inputs: HashMap<String, serde_json::Value>,
    ) {
        let run_id = self.run_id.clone();

        // 0. 验证输入参数
        let validated_inputs = match self.definition.validate_inputs(&inputs) {
            Ok(v) => v,
            Err(e) => {
                let _ = speaker.send(WorkflowEvent::Failed {
                    run_id,
                    error: e.to_string(),
                    failed_at_step: None,
                    total_duration_ms: 0,
                });
                return;
            }
        };

        // 1. 构建 DAG + 拓扑排序
        let dag = match DagGraph::build(&self.definition.steps) {
            Ok(dag) => dag,
            Err(e) => {
                let _ = speaker.send(WorkflowEvent::Failed {
                    run_id,
                    error: e.to_string(),
                    failed_at_step: None,
                    total_duration_ms: 0,
                });
                return;
            }
        };

        // 2. 初始化模板上下文（注入外部输入参数）
        let mut tpl_ctx = TemplateContext::new(Some(&validated_inputs));

        // 3. 发送 Started 事件
        let _ = speaker.send(WorkflowEvent::Started {
            run_id: run_id.clone(),
            workflow_name: self.definition.name.clone(),
            total_steps: self.definition.steps.len(),
        });

        // 4. 按拓扑层级迭代执行
        let start = Instant::now();
        let mut step_results: HashMap<String, StepResult> = HashMap::new();
        let mut steps_completed = 0usize;
        let mut steps_failed = 0usize;
        let mut steps_skipped = 0usize;

        for level in dag.topological_levels() {
            // 4a. 层级开始前检查取消
            if cancel_flag.load(Ordering::Acquire) {
                let _ = speaker.send(WorkflowEvent::Cancelled { run_id: run_id.clone() });
                return;
            }

            // 4b. 发射 StepStarted 事件 + 启动同一层级的所有步骤（并行执行）
            let mut handles: Vec<(String, tokio::task::JoinHandle<StepResult>)> = Vec::new();

            for step in level {
                if !evaluate_condition(step, &step_results, &tpl_ctx) {
                    let reason = step.condition.clone()
                        .unwrap_or_else(|| "condition evaluated to false".into());
                    let _ = speaker.send(WorkflowEvent::StepSkipped {
                        step_id: step.id.clone(),
                        step_name: step.name.clone(),
                        reason: reason.clone(),
                    });
                    steps_skipped += 1;
                    step_results.insert(step.id.clone(), StepResult {
                        step: step.clone(),
                        outcome: StepOutcome::Skipped(reason),
                        output: None,
                        structured_output: None,
                        duration: Duration::ZERO,
                        attempt: 0,
                    });
                    continue;
                }

                // 发射 StepStarted
                let _ = speaker.send(WorkflowEvent::StepStarted {
                    step_id: step.id.clone(),
                    step_name: step.name.clone(),
                    step_type: format!("{:?}", step.step_type).to_lowercase(),
                });

                let step_id = step.id.clone();
                let step = step.clone();
                let prev = step_results.clone();
                let mut ctx = tpl_ctx.clone();
                let cancel = cancel_flag.clone();
                let agent_access = self.agent_access.clone();  // Arc clone
                let handle = tokio::spawn(async move {
                    WorkflowEngine::execute_step_static(
                        &step, &prev, &mut ctx, &cancel, &agent_access,
                    ).await
                });
                handles.push((step_id, handle));
            }

            // 4c. 收集结果，支持 Abort 时取消未完成的兄弟步骤
            let mut aborted = false;
            let mut remaining_handles = handles; // 所有权转移，确保所有 handle 都被处理

            while let Some((step_id, handle)) = remaining_handles.first() {
                let step_id = step_id.clone();

                // 如果已触发 abort，取消当前及剩余所有 handle
                if aborted || cancel_flag.load(Ordering::Acquire) {
                    handle.abort();
                    remaining_handles.remove(0);
                    // 继续循环，abort 所有剩余 handle
                    continue;
                }

                // 等待当前步骤完成
                let result = match handle.await {
                    Ok(r) => r,
                    Err(_join_err) => {
                        // tokio 任务 panic 或被取消
                        remaining_handles.remove(0);
                        continue;
                    }
                };
                remaining_handles.remove(0);

                // 发射结果事件
                let event = match &result.outcome {
                    StepOutcome::Success(_) => {
                        steps_completed += 1;
                        WorkflowEvent::StepCompleted {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            output: result.output.clone().unwrap_or_default(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                        }
                    }
                    StepOutcome::Skipped(reason) => {
                        steps_skipped += 1;
                        WorkflowEvent::StepSkipped {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            reason: reason.clone(),
                        }
                    }
                    StepOutcome::Failed(err) => {
                        steps_failed += 1;
                        WorkflowEvent::StepFailed {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            error: err.clone(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                            failure_policy: format!("{:?}", result.step.on_failure),
                        }
                    }
                };
                let _ = speaker.send(event);

                // 检查失败策略
                if result.outcome.is_failed() {
                    match result.step.on_failure {
                        OnFailure::Abort => {
                            let _ = speaker.send(WorkflowEvent::Failed {
                                run_id: run_id.clone(),
                                error: format!("Step '{}' failed: {}",
                                    result.step.id,
                                    result.output.unwrap_or_default()),
                                failed_at_step: Some(result.step.id.clone()),
                                total_duration_ms: start.elapsed().as_millis() as u64,
                            });
                            aborted = true;
                            // 不清空 remaining_handles — while 循环顶部会 abort 所有剩余项
                        }
                        OnFailure::Pause => {
                            let _ = speaker.send(WorkflowEvent::Paused {
                                run_id: run_id.clone(),
                                reason: format!("Step '{}' failed, waiting for approval",
                                    result.step.id),
                                paused_at_step: Some(result.step.id.clone()),
                            });
                            // TODO: persister.save(&snapshot) — 暂停时持久化
                            // 阻塞等待审批决策
                            match approval_rx.recv().await {
                                Some(ApprovalResponse { decision: ApprovalDecision::Abort, .. }) |
                                None => {
                                    let _ = speaker.send(WorkflowEvent::Failed {
                                        run_id,
                                        error: "User aborted after pause".into(),
                                        failed_at_step: Some(result.step.id.clone()),
                                        total_duration_ms: start.elapsed().as_millis() as u64,
                                    });
                                    // 取消剩余 handles
                                    for (_, h) in remaining_handles.drain(..) {
                                        h.abort();
                                    }
                                    return;
                                }
                                Some(ApprovalResponse { decision: ApprovalDecision::Proceed, .. }) => {
                                    let _ = speaker.send(WorkflowEvent::Resumed {
                                        run_id: run_id.clone(),
                                    });
                                    // 继续当前层级剩余步骤
                                }
                            }
                        }
                        OnFailure::Continue => {
                            // 继续执行，不中止
                        }
                        OnFailure::Retry => {
                            // Phase 4: 重试逻辑（此处 Phase 1 退化为 Continue）
                        }
                    }
                }

                // 更新模板上下文（即使在 Abort 路径中也要更新，供外部 snapshot 使用）
                tpl_ctx.set_step_result(&result.step.id, &result);
                step_results.insert(result.step.id.clone(), result);
            }

            // 4d. Abort 后不再进入下一层级
            if aborted {
                return;
            }

            // TODO: persister.save(&snapshot) — 每层完成后持久化
        }

        // 5. 完成
        let _ = speaker.send(WorkflowEvent::Completed {
            run_id,
            total_duration_ms: start.elapsed().as_millis() as u64,
            steps_completed,
            steps_failed,
            steps_skipped,
        });
        // TODO: persister.save(&snapshot) — 完成时持久化最终状态
    }
}
```

**关键设计决策**：

1. **事件通道使用 `tokio::sync::mpsc::channel`**（实际实现）：设计方案原使用 `Speaker`/`Listener` intercom 对，但实际实现改为单向 mpsc channel。原因是 engine 只生产事件（不消费），`Speaker`/`Listener` 的双向设计带来不必要的复杂度。事件通过同步 `try_send()` 发射（fire-and-forget），避免 async send future 被意外 drop。

2. **按拓扑层级执行，层级内并行**：DAG 拓扑排序后分组为层级（level），同一层级内的步骤无相互依赖，通过 `tokio::spawn` 并行执行。层级间串行（下一层级需等待当前层级全部完成）。

3. **失败即传播，Abort 时显式清理**：每个步骤完成后立即检查 `on_failure` 策略：
   - `Abort`：通过 `VecDeque` + `aborted` 标志位，**显式 abort 所有剩余兄弟步骤的 JoinHandle**，持久化 Failed 快照，然后立即退出。
   - `Continue`：记录失败，继续执行后续步骤
   - `Pause`：发送 Paused 事件，持久化快照，阻塞等待审批。
   - `Retry`：Phase 4 实现，Phase 1–2 退化为 Continue

4. **模板变量延迟求值**：`{{ steps.X.output }}` 在每个步骤执行前求值，而非 Workflow 启动时。

5. **审批使用独立 mpsc channel**：与事件流解耦 — Paused 事件走 event_tx/rx，审批决策走 approval_tx/rx。handle 调用 `approve()` 发送决策，engine 在 `approval_rx` 上阻塞等待。

6. **`run_id` 在 spawn 时生成**：UUID v4，写入每一个事件，贯穿整个生命周期。`WorkflowHandle::run_id()` 对外暴露，供调用方追踪和取消。

7. **持久化已集成（Phase 2）**：`WorkflowPersister` trait 在 Pause、每层完成、Completed、Failed 时自动保存。引擎通过 `build_snapshot()` 辅助方法构建 `WorkflowSnapshot`，调用 `persister.save()` 异步持久化（失败不影响执行）。CLI/测试场景使用 `NullWorkflowPersister`。

### 4.4 步骤执行器

```rust
// crates/peco-core/src/workflow/step_executor.rs

impl WorkflowEngine {
    /// 执行单个步骤（静态方法，供 tokio::spawn 使用），返回 StepResult。
    async fn execute_step_static(
        step: &WorkflowStep,
        prev_results: &HashMap<String, StepResult>,
        tpl_ctx: &TemplateContext,
        cancel_flag: &Arc<AtomicBool>,
        agent_access: &Arc<dyn AgentAccess>,
    ) -> StepResult {
        let start = Instant::now();

        let outcome = match &step.config {
            StepConfig::Shell { command } => {
                match tpl_ctx.render(command) {
                    Ok(rendered) => Self::execute_shell_step(&rendered, step.timeout_seconds).await,
                    Err(e) => StepOutcome::Failed(format!("template error: {e}")),
                }
            }
            StepConfig::Agent { agent, prompt, max_turns } => {
                match tpl_ctx.render(prompt) {
                    Ok(rendered_prompt) => {
                        let output_schema = step.output_schema.clone();
                        Self::execute_agent_step(
                            agent_access, agent, &rendered_prompt, *max_turns, output_schema, cancel_flag
                        ).await
                    }
                    Err(e) => StepOutcome::Failed(format!("template error: {e}")),
                }
            }
            StepConfig::Llm { prompt } => {
                // Phase 4: 需要 llm_provider，Phase 1 中 validate() 已拒绝此类型
                let _ = tpl_ctx.render(prompt);
                unreachable!("Llm step type should be rejected in Phase 1 validation")
            }
            StepConfig::Tool { tool_name: _, arguments: _ } => {
                // Phase 4: 需要 tool_executor，Phase 1 中 validate() 已拒绝此类型
                unreachable!("Tool step type should be rejected in Phase 1 validation")
            }
        };

        StepResult {
            step: step.clone(),
            output: match &outcome {
                StepOutcome::Success(s) => Some(s.clone()),
                _ => None,
            },
            outcome,
            duration: start.elapsed(),
            attempt: 1,
            structured_output: None,
        }
    }

    /// 执行 Agent 类型步骤（复用 SimpleAgentLooper）。
    ///
    /// `SimpleAgentLooper::spawn(agent, prompt, max_turns)` 接收的 `Agent` 实例
    /// 已包含从 `agent.md` 解析的完整工具集。无需额外向 Agent 注入 Shell/Tool 能力 —
    /// 步骤能使用的工具取决于该 Agent 自身 `agent.md` 的 `tools:` 声明。
    async fn execute_agent_step(
        agent_access: &Arc<dyn AgentAccess>,
        agent_name: &str,
        prompt: &str,
        max_turns: Option<usize>,
        output_schema: Option<serde_json::Value>,
        cancel_flag: &Arc<AtomicBool>,
    ) -> StepOutcome {
        // 1. 通过 DI 加载 Agent（返回 Arc<Agent>，已包含 agent.md 中定义的工具）
        let agent: Arc<Agent> = match agent_access.load_agent(agent_name) {
            Ok(a) => a,
            Err(e) => return StepOutcome::Failed(e.to_string()),
        };

        // 2. 根据是否有 output_schema 选择执行方式
        if let Some(schema) = output_schema {
            // Phase 4: StructuredOutputExecutor。Phase 1 回退：在 prompt 中追加 schema 指令
            let schema_hint = format!(
                "\n\n请以 JSON 格式输出，必须符合以下 schema:\n```json\n{}\n```",
                serde_json::to_string_pretty(&schema).unwrap_or_default()
            );
            let full_prompt = format!("{prompt}{schema_hint}");
            let handle = SimpleAgentLooper::spawn(agent, full_prompt, max_turns);
            match handle.wait().await {
                Ok(output) => StepOutcome::Success(output),
                Err(e) => StepOutcome::Failed(e.to_string()),
            }
        } else {
            // 标准 ReAct（batch 模式）
            let handle = SimpleAgentLooper::spawn(agent, prompt.to_string(), max_turns);
            match handle.wait().await {
                Ok(output) => StepOutcome::Success(output),
                Err(e) => StepOutcome::Failed(e.to_string()),
            }
        }
    }

    /// 执行 Shell 类型步骤
    async fn execute_shell_step(
        command: &str,
        timeout_seconds: Option<u64>,
    ) -> StepOutcome {
        let timeout = Duration::from_secs(timeout_seconds.unwrap_or(300));
        match tokio::time::timeout(timeout, tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
        ).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let combined = format!("{stdout}\n{stderr}").trim().to_string();
                if output.status.success() {
                    StepOutcome::Success(combined)
                } else {
                    StepOutcome::Failed(format!("exit code: {}\n{combined}",
                        output.status.code().unwrap_or(-1)))
                }
            }
            Ok(Err(e)) => StepOutcome::Failed(format!("command execution error: {e}")),
            Err(_) => StepOutcome::Failed("command timed out".to_string()),
        }
    }

    /// Phase 4: 执行 LLM 类型步骤（需在 WorkflowEngine 上新增 llm_provider 字段）
    /// Phase 4: 执行 Tool 类型步骤（需在 WorkflowEngine 上新增 tool_executor 字段）
    ///
    /// Phase 1 不支持这两个步骤类型，解析时会被 validate() 拒绝。
}
```

### 4.5 WorkflowEvent 事件

```rust
// crates/peco-core/src/workflow/events.rs

use std::time::Duration;

/// Workflow 执行期间产生的事件。
/// 通过 Speaker/Listener 通道传输，与 LooperEvent 模式一致。
///
/// 每个事件都携带 `run_id` 字段，用于追踪和审计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowEvent {
    /// Workflow 开始执行
    Started {
        run_id: String,
        workflow_name: String,
        total_steps: usize,
    },

    /// 步骤开始执行
    StepStarted {
        step_id: String,
        step_name: String,
        step_type: String,  // "shell" | "agent" | "llm" | "tool"
    },

    /// 步骤执行中的增量输出（Phase 2：仅 agent/llm 类型流式推送）
    StepDelta {
        step_id: String,
        text: String,
    },

    /// 步骤执行成功
    StepCompleted {
        step_id: String,
        step_name: String,
        output: String,
        duration_ms: u64,
        attempt: usize,   // 第几次尝试（含重试）
    },

    /// 步骤被跳过（条件不满足）
    StepSkipped {
        step_id: String,
        step_name: String,
        reason: String,
    },

    /// 步骤执行失败
    StepFailed {
        step_id: String,
        step_name: String,
        error: String,
        duration_ms: u64,
        attempt: usize,
        failure_policy: String,  // "continue" | "abort" | "retry" | "pause"
    },

    /// Phase 4: 等待重试
    StepRetrying {
        step_id: String,
        attempt: usize,
        max_attempts: usize,
        backoff_seconds: u64,
    },

    /// Workflow 暂停（等待审批或外部恢复）
    Paused {
        run_id: String,
        reason: String,
        paused_at_step: Option<String>,
    },

    /// Workflow 恢复执行
    Resumed {
        run_id: String,
    },

    /// Workflow 成功完成
    Completed {
        run_id: String,
        total_duration_ms: u64,
        steps_completed: usize,
        steps_failed: usize,
        steps_skipped: usize,
    },

    /// Workflow 执行失败
    Failed {
        run_id: String,
        error: String,
        failed_at_step: Option<String>,
        total_duration_ms: u64,
    },

    /// Workflow 被取消
    Cancelled {
        run_id: String,
    },
}

/// 审批决策枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// 继续执行（忽略失败）
    Proceed,
    /// 中止整个 workflow
    Abort,
}

/// 审批响应：外部通过 WorkflowHandle::approve() 发送给引擎。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
    /// 可选备注
    pub note: Option<String>,
}
```

### 4.6 模板变量引擎

Phase 1 直接集成 **minijinja**（纯 Rust，无 unsafe，与现有依赖风格一致，约 8K GitHub stars），无需从零构建模板引擎。

```toml
# crates/peco-core/Cargo.toml
minijinja = "2"
```

```rust
// crates/peco-core/src/workflow/template.rs

use std::collections::HashMap;
use minijinja::{Environment, value::Value};
use serde_json;

/// 模板上下文，封装 minijinja Environment。
///
/// 注入两类全局变量：
/// - `steps` — 各步骤的执行结果（动态更新）
/// - `inputs` — workflow 外部输入参数（构造时注入）
///
/// **Phase 1 即统一使用 minijinja**：condition 表达式和 prompt 模板
/// 共用同一套渲染引擎，Phase 2 无需迁移。
#[derive(Clone)]
pub struct TemplateContext {
    env: Environment<'static>,
    /// 累积的步骤结果（序列化为 JSON 后注入 minijinja）
    steps: serde_json::Map<String, serde_json::Value>,
}

impl TemplateContext {
    /// 创建模板上下文，注入外部输入参数。
    pub fn new(inputs: Option<&HashMap<String, serde_json::Value>>) -> Self {
        let mut env = Environment::new();
        // minijinja 默认启用 auto-escaping，对于纯文本模板场景关闭
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);

        // 注入外部输入
        if let Some(inputs) = inputs {
            env.add_global("inputs", Value::from_serialize(inputs));
        } else {
            env.add_global("inputs", Value::from_object(serde_json::Map::new()));
        }

        Self {
            env,
            steps: serde_json::Map::new(),
        }
    }

    /// 记录步骤结果到上下文。每步完成后调用。
    pub fn set_step_result(&mut self, step_id: &str, result: &StepResult) {
        let step_value = serde_json::json!({
            "output": result.output,
            "success": result.outcome.is_success(),
            "duration_ms": result.duration.as_millis(),
        });
        self.steps.insert(step_id.to_string(), step_value);

        // 将整个 steps map 重新注入 minijinja 环境
        self.env.add_global("steps", Value::from_serialize(&self.steps));
    }

    /// 渲染模板字符串。
    ///
    /// 支持的表达式（minijinja 原生语法）：
    /// - `{{ steps.lint.output }}` — 步骤输出文本
    /// - `{{ steps.lint.success }}` — 是否成功（true/false）
    /// - `{{ inputs.target_branch }}` — 外部输入参数
    /// - `{{ steps.review.output | truncate(200) }}` — 截断过滤器
    /// - `{{ steps.review.output.issues | length }}` — 数组/字符串长度
    /// - `{{ steps.review.output.issues[0].severity }}` — 嵌套 JSON 访问
    /// - `{% if steps.lint.success %}PASS{% else %}FAIL{% endif %}` — 条件
    ///
    /// # Errors
    ///
    /// 模板语法错误或变量未定义时返回 `WorkflowError::Template`。
    pub fn render(&self, template: &str) -> Result<String, WorkflowError> {
        self.env
            .render_str(template, ())
            .map_err(|e| WorkflowError::Template(format!("{e}")))
    }

    /// 渲染模板字符串并求值为布尔值（用于 condition 表达式）。
    ///
    /// condition 字段使用 minijinja 模板语法：
    /// - `"{{ steps.X.success }}"` → 渲染后为 "true" 或 "false"
    /// - `"{{ steps.X.output.severity == 'critical' }}"` → 渲染后为 "true" 或 "false"
    /// - 空字符串 → 总是 true
    ///
    /// 渲染结果按以下规则解析为 bool：
    /// - `"true"` / `"1"` → true
    /// - `"false"` / `"0"` / `""` → false
    /// - 其他 → 警告 + 按 false 处理
    pub fn render_bool(&self, template: &str) -> bool {
        if template.is_empty() {
            return true;
        }
        match self.render(template) {
            Ok(s) => {
                let trimmed = s.trim();
                matches!(trimmed, "true" | "1")
            }
            Err(e) => {
                tracing::warn!(%template, %e, "condition template render failed, defaulting to false");
                false
            }
        }
    }

    /// 渲染模板并将其解析为 JSON Value（用于 Tool 类型步骤的参数渲染）。
    pub fn render_json(
        &self,
        value: &serde_json::Value,
    ) -> Result<serde_json::Value, WorkflowError> {
        let rendered = self.render(&value.to_string())?;
        serde_json::from_str(&rendered)
            .map_err(|e| WorkflowError::Template(format!("json parse after render: {e}")))
    }
}
```

**为什么选择 minijinja vs 自研**：

| 考量 | 自研（正则替换） | minijinja |
|------|------------|-----------|
| 字段访问 `output.issues[0]` | 需手写 JSON Path 解析 | 原生支持 |
| 过滤器 `\| truncate` | 需手写管道解析 | 内置 + 可扩展 |
| 条件 `{% if %}` | 无法支持 | 原生支持 |
| 错误处理 | 静默产出空字符串 | 明确的错误信息 |
| Phase 1 估时 | 1d（严重低估） | 1.5d（集成 + 测试） |
| Phase 2 迁移成本 | 全部重写 | 零（直接使用高级特性） |

**Phase 1 使用的 minijinja 特性**：变量访问、点号路径、`truncate`/`length` 过滤器、`{% if %}` 条件。
**Phase 2 扩展**：自定义过滤器（`to_json`、`from_json`）、`{% for %}` 循环、`include` 模板继承。

### 4.7 Condition 条件表达式

条件表达式决定步骤是否执行。**`depends_on` 仅控制拓扑顺序（步骤 B 等待 A 完成），不关心 A 的成败**。若 A 失败但 B 仍需执行，通过 `condition` 控制。

**Phase 1 统一使用 minijinja 模板渲染 condition**，语法与 prompt 模板一致：

```rust
/// 求值步骤的 condition 表达式。
///
/// `condition` 和 `depends_on` 是正交的：
/// - `depends_on: [A]` → 等待 A 完成（无论成败）
/// - `condition: "{{ steps.A.success }}"` → 仅在 A 成功时执行
/// - 两者都设置 → 等待 A 完成，然后根据渲染结果决定是否执行
/// - 两者都不设置 → 无条件立即执行
///
/// condition 字段使用 minijinja 语法：
/// ```yaml
/// condition: "{{ steps.review.success }}"                      # 布尔求值
/// condition: "{{ steps.review.output.severity == 'critical' }}" # 比较
/// condition: ""                                                 # 总是执行
/// ```
///
/// 渲染结果经 `render_bool()` 转换为布尔值。
pub fn evaluate_condition(
    step: &WorkflowStep,
    results: &HashMap<String, StepResult>,
    tpl_ctx: &TemplateContext,  // ← Phase 1 即使用模板上下文
) -> bool {
    match &step.condition {
        None => true,
        Some(expr) if expr.is_empty() => true,
        Some(expr) => tpl_ctx.render_bool(expr),
    }
}
```

**Phase 2 高级条件**：`condition` 渲染结果支持更丰富的真值判断（数字非零、字符串非空、数组非空），`render_bool` 升级为 `render_truthy`。

### 4.8 DAG 构建与验证

```rust
// crates/peco-core/src/workflow/dag.rs

/// 步骤依赖的 DAG 图。
///
/// 构建时验证：
/// - 无环（拓扑排序成功）
/// - depends_on 引用的步骤 ID 存在
/// - 无自身依赖
pub struct DagGraph {
    steps: Vec<WorkflowStep>,
    /// 邻接表：step_id → [前置步骤 IDs]
    dependencies: HashMap<String, Vec<String>>,
    /// 拓扑排序后的层级分组
    levels: Vec<Vec<WorkflowStep>>,
}

impl DagGraph {
    /// 从步骤列表构建 DAG，验证合法性。
    pub fn build(steps: &[WorkflowStep]) -> Result<Self, WorkflowError> {
        // 1. 收集所有 step ID
        let ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();

        // 2. 验证 depends_on 引用
        for step in steps {
            for dep in &step.depends_on {
                if dep == &step.id {
                    return Err(WorkflowError::InvalidDag(
                        format!("步骤 '{}' 依赖自身", step.id)
                    ));
                }
                if !ids.contains(dep.as_str()) {
                    return Err(WorkflowError::InvalidDag(
                        format!("步骤 '{}' 依赖未知步骤 '{}'", step.id, dep)
                    ));
                }
            }
        }

        // 3. Kahn 算法拓扑排序 + 分层
        let levels = kahn_level_sort(steps)?;

        Ok(Self {
            steps: steps.to_vec(),
            dependencies: build_adjacency(steps),
            levels,
        })
    }

    /// 返回拓扑层级（每层内的步骤可并行执行）
    pub fn topological_levels(&self) -> &[Vec<WorkflowStep>] {
        &self.levels
    }
}

/// Kahn 算法 + BFS 分层
fn kahn_level_sort(steps: &[WorkflowStep]) -> Result<Vec<Vec<WorkflowStep>>, WorkflowError> {
    // 入度表
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for step in steps {
        in_degree.insert(&step.id, step.depends_on.len());
    }

    // 第 0 层：入度为 0 的步骤
    let mut current_level: Vec<&WorkflowStep> = steps
        .iter()
        .filter(|s| in_degree[s.id.as_str()] == 0)
        .collect();

    if current_level.is_empty() && !steps.is_empty() {
        return Err(WorkflowError::InvalidDag(
            "DAG 包含循环依赖，无法找到起始节点".into()
        ));
    }

    let mut levels: Vec<Vec<WorkflowStep>> = Vec::new();
    let step_map: HashMap<&str, &WorkflowStep> =
        steps.iter().map(|s| (s.id.as_str(), s)).collect();

    while !current_level.is_empty() {
        levels.push(current_level.iter().map(|s| (*s).clone()).collect());

        let mut next_level: Vec<&WorkflowStep> = Vec::new();

        for node in &current_level {
            // 找到所有依赖此节点的步骤，减少入度
            for step in steps {
                if step.depends_on.contains(&node.id.to_string()) {
                    let entry = in_degree.get_mut(step.id.as_str()).unwrap();
                    *entry -= 1;
                    if *entry == 0 {
                        next_level.push(step_map[step.id.as_str()]);
                    }
                }
            }
        }

        current_level = next_level;
    }

    // 检查是否有未处理的节点（循环依赖）
    let processed: usize = levels.iter().map(|l| l.len()).sum();
    if processed < steps.len() {
        return Err(WorkflowError::InvalidDag(
            "DAG 包含循环依赖".into()
        ));
    }

    Ok(levels)
}
```

### 4.9 持久化 Trait

```rust
// crates/peco-core/src/workflow/persistence.rs

use async_trait::async_trait;

/// Workflow 执行快照，支持断点续执行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    pub run_id: String,
    pub workflow_name: String,
    pub definition: WorkflowDefinition,
    pub state: WorkflowSnapshotState,
    pub step_results: HashMap<String, StepResult>,
    pub current_level: usize,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 快照状态。
///
/// 注意：不包含 `Cancelled` 变体 — 取消是终态，不可恢复。
/// Cancelled 后快照仅保留在日志/审计记录中，persister 的 save 不会对 Cancelled 状态写入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowSnapshotState {
    /// 执行中（每层完成后快照）
    Running,
    /// 暂停等待审批
    Paused,
    /// 已完成
    Completed,
    /// 已失败
    Failed,
}

/// Workflow 执行持久化接口。
///
/// 遵循 `SessionPersister` 的 trait 抽象模式。
/// peco-core 仅定义 trait，实现由 peco-server（SQLite）提供。
#[async_trait]
pub trait WorkflowPersister: Send + Sync {
    /// 保存 Workflow 执行快照。
    async fn save(&self, snapshot: &WorkflowSnapshot) -> Result<(), WorkflowError>;

    /// 加载 Workflow 执行快照。
    async fn load(&self, run_id: &str) -> Result<Option<WorkflowSnapshot>, WorkflowError>;

    /// 删除 Workflow 执行记录。
    async fn delete(&self, run_id: &str) -> Result<(), WorkflowError>;

    /// 列出用户的所有 Workflow 执行记录。
    async fn list(&self, user_id: &str) -> Result<Vec<WorkflowSnapshot>, WorkflowError>;
}

/// 空持久化实现（测试/CLI 场景使用）
pub struct NullWorkflowPersister;

#[async_trait]
impl WorkflowPersister for NullWorkflowPersister {
    async fn save(&self, _snapshot: &WorkflowSnapshot) -> Result<(), WorkflowError> {
        Ok(())
    }
    async fn load(&self, _run_id: &str) -> Result<Option<WorkflowSnapshot>, WorkflowError> {
        Ok(None)
    }
    async fn delete(&self, _run_id: &str) -> Result<(), WorkflowError> {
        Ok(())
    }
    async fn list(&self, _user_id: &str) -> Result<Vec<WorkflowSnapshot>, WorkflowError> {
        Ok(Vec::new())
    }
}
```

### 4.10 WorkflowAccess Trait

引擎通过 `WorkflowEngine::spawn()` 的签名直接声明依赖（窄接口，无额外封装）：

```rust
// 签名即契约 — 调用方需提供两个 Arc：
WorkflowEngine::spawn(
    definition,
    agent_access: Arc<dyn AgentAccess>,     // Agent 步骤加载
    persister: Arc<dyn WorkflowPersister>,  // 断点续执行
    config,
    inputs,
) -> WorkflowHandle
```

Phase 4 新增 `tool_executor` 和 `llm_provider` 时，直接在 `spawn()` 上加参数（`Option<Arc<...>>`）并在 `WorkflowEngine` 结构体上加对应字段。

`WorkSpace` 通过新增的 `WorkflowAccess` trait 提供 workflow 文件加载能力（与 `AgentAccess` 模式一致）：

```rust
// crates/peco-core/src/workflow/access.rs

/// Workflow 文件加载接口。
pub trait WorkflowAccess: Send + Sync {
    /// 按名称加载 WorkflowDefinition。
    fn load_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;
    /// 列出所有可用 Workflow 名称。
    fn list_workflow_names(&self) -> Vec<String>;
    /// 重新加载指定 Workflow（缓存失效时使用）。
    fn reload_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;
}
```

### 4.11 WorkflowManager

```rust
// crates/peco-core/src/workflow/manager.rs

/// Workflow 生命周期管理器。
///
/// 与 AgentManager 同级：管理 workflows/ 目录中的 workflow.md 文件，
/// 并提供执行管理能力。
///
/// **缓存策略**：Workflow 定义按需加载并缓存到内存中。若磁盘文件变更，
/// 调用方需显式调用 `reload(name)` 使缓存失效。Phase 2 可增加文件监听器
/// 自动更新缓存（与 `SkillRegister` 三级加载模式一致）。
pub struct WorkflowManager {
    workflows_dir: PathBuf,
    /// 缓存的 Workflow 定义（按需懒加载，通过 reload 失效）
    definitions: RwLock<HashMap<String, WorkflowDefinition>>,
    /// 活跃的执行句柄（run_id → WorkflowHandle）
    active_runs: RwLock<HashMap<String, WorkflowHandle>>,
    persister: Arc<dyn WorkflowPersister>,
}

impl WorkflowManager {
    pub fn new(workflows_dir: PathBuf, persister: Arc<dyn WorkflowPersister>) -> Self;

    /// 扫描目录，缓存 Tier-1 元数据（名称 + 描述）
    pub fn init(&self) -> Result<usize, WorkflowError>;

    /// 列出所有 Workflow 名称
    pub fn list_names(&self) -> Vec<String>;

    /// 加载指定 Workflow 定义（优先缓存，未命中则读文件）
    pub fn load(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;

    /// 强制从磁盘重新加载指定 Workflow（缓存失效）
    pub fn reload(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError>;

    /// 启动 Workflow 执行
    pub fn execute(
        &self,
        name: &str,
        agent_access: Arc<dyn AgentAccess>,
        config: WorkflowConfig,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Result<WorkflowHandle, WorkflowError>;

    /// 取消运行中的 Workflow
    pub fn cancel(&self, run_id: &str) -> Result<(), WorkflowError>;
}
```

---

## 5. 与现有系统的集成

### 5.1 WorkSpace 集成

```rust
// workspace/workspace.rs 新增

pub struct WorkSpace {
    // ... 现有字段 ...
    workflow_manager: Arc<WorkflowManager>,  // 新增
}

impl WorkSpace {
    pub fn workflow_manager(&self) -> &Arc<WorkflowManager> {
        &self.workflow_manager
    }
}

// 新增 trait 实现
impl WorkflowAccess for WorkSpace {
    fn load_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError> {
        self.workflow_manager.load(name)
    }
    fn list_workflow_names(&self) -> Vec<String> {
        self.workflow_manager.list_names()
    }
    fn reload_workflow(&self, name: &str) -> Result<WorkflowDefinition, WorkflowError> {
        self.workflow_manager.reload(name)
    }
}
```

### 5.2 工具集成 — execute_workflow（Phase 2 已实现）

> **实现状态**：已按以下设计实现，位于 `crates/peco-core/src/workflow/tools/execute_workflow.rs`。实现遵循 `SaveAgent`/`DelegateSubAgent` 的 `ToolDyn` 模式。Pause 场景自动 cancel workflow 并返回错误（Agent 无法在工具调用中途处理审批）。

```rust
// tools/workflow.rs

/// execute_workflow 工具 — 让 Agent 可以调用 Workflow。
///
/// 与 DelegateSubAgent 模式一致：Agent 调工具 → 工具启动 Workflow → 返回结果。
///
/// **阻塞语义**：从 Agent 视角，这是一个同步阻塞的工具调用 — Agent 的这轮
/// ReAct 迭代会等待 Workflow 执行完毕后继续。因此仅适用于短 Workflow（< 30s）。
/// 长时间运行的 Workflow 应通过 peco-server 的 SSE 接口由用户直接触发，
/// 而非 Agent 在 ReAct 循环中调用。
/// Phase 2 可通过 `StepDelta` 事件向 Agent 报告中间进度。
pub struct ExecuteWorkflow {
    workflow_access: Arc<dyn WorkflowAccess>,
    agent_access: Arc<dyn AgentAccess>,
    persister: Arc<dyn WorkflowPersister>,
}

impl ExecuteWorkflow {
    pub fn new(
        workflow_access: Arc<dyn WorkflowAccess>,
        agent_access: Arc<dyn AgentAccess>,
        persister: Arc<dyn WorkflowPersister>,
    ) -> Self {
        Self { workflow_access, agent_access, persister }
    }
}

impl ToolDyn for ExecuteWorkflow {
    fn name(&self) -> String {
        "execute_workflow".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute_workflow".to_string(),
            description: "执行一个预定义的 Workflow。Workflow 是包含多个步骤的自动化流程，\
                步骤间可以串行、并行和有条件地执行。注意：此工具同步等待 Workflow 完成，\
                仅适用于较短的 Workflow（预计 30 秒内完成）。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workflow_name": {
                        "type": "string",
                        "description": "要执行的 Workflow 名称"
                    },
                    "params": {
                        "type": "object",
                        "description": "传递给 Workflow 的外部输入参数（可选）"
                    }
                },
                "required": ["workflow_name"]
            }),
        }
    }

    async fn call(&self, args: serde_json::Value) -> Result<String, String> {
        // 1. 解析参数
        let workflow_name = args["workflow_name"].as_str()
            .ok_or("missing 'workflow_name'")?;
        let params = args.get("params")
            .and_then(|p| p.as_object())
            .cloned();

        // 2. 加载 Workflow 定义 + 验证输入
        let definition = self.workflow_access.load_workflow(workflow_name)
            .map_err(|e| format!("failed to load workflow '{workflow_name}': {e}"))?;

        // 3. 启动引擎（直接传依赖，无需中间结构体）
        let config = WorkflowConfig {
            event_buffer: 64,
            step_delay: None,
        };
        let inputs_map = params.unwrap_or_default();
        let handle = WorkflowEngine::spawn(
            definition,
            self.agent_access.clone(),
            self.persister.clone(),
            config,
            inputs_map,
        );

        // 4. 收集所有事件，等待完成/失败
        let mut outputs: Vec<String> = Vec::new();
        loop {
            match handle.recv_event().await {
                Some(WorkflowEvent::StepCompleted { output, .. }) => {
                    outputs.push(output);
                }
                Some(WorkflowEvent::Paused { reason, .. }) => {
                    // Phase 1: workflow 触发 pause 时，工具返回错误告知 Agent
                    // （Agent 无法在工具调用中途处理审批）
                    handle.cancel();
                    return Err(format!(
                        "Workflow paused and requires human approval: {reason}. \
                         Please run this workflow via the UI or CLI to handle approvals."
                    ));
                }
                Some(WorkflowEvent::Completed { steps_completed, steps_failed, .. }) => {
                    return Ok(format!(
                        "Workflow completed: {steps_completed} steps succeeded, \
                         {steps_failed} steps failed.\n\nOutputs:\n{}",
                        outputs.join("\n---\n")
                    ));
                }
                Some(WorkflowEvent::Failed { error, .. }) => {
                    return Err(format!("Workflow failed: {error}"));
                }
                Some(WorkflowEvent::Cancelled { .. }) => {
                    return Err("Workflow cancelled.".to_string());
                }
                None => {
                    return Err("Workflow ended unexpectedly.".to_string());
                }
                _ => {}  // StepStarted, StepSkipped 等 — 仅记录
            }
        }
    }
}
```

### 5.3 ToolRegister 集成

`ToolDependencies` 新增可选的 `workflow_access` 字段：

```rust
// tools/deps.rs 中 ToolDependencies 新增:

pub struct ToolDependencies {
    // ... 现有字段 ...
    /// Workflow 支持（可选，Phase 1 引入）
    pub workflow_access: Option<Arc<dyn WorkflowAccess>>,
}
```

`ToolRegister::build()` 中新增匹配（Phase 2 已实现）：

```rust
// tools/tool_register.rs 的 build() 中:

"execute_workflow" => {
    let wa = deps.workflow_access.clone()
        .expect("execute_workflow tool requires workflow_access in ToolDependencies");
    Some(Box::new(ExecuteWorkflow::new(
        wa,
        deps.agent_access.clone(),
        Arc::new(NullWorkflowPersister),  // Phase 2: CLI 用 Null；Phase 3: peco-server 注入 Sqlite
    )))
}
```

`WorkSpace::build_tool_executor()` 自动注入 `workflow_access`：

```rust
pub fn build_tool_executor(self: &Arc<Self>, tool_names: &[String]) -> Arc<dyn ToolExecutor> {
    let mut deps = self.agent_manager.build_deps();
    deps.workflow_access = Some(self.clone() as Arc<dyn WorkflowAccess>);
    ToolRegister::build(tool_names, &deps)
}
```

### 5.4 peco-server 集成（Phase 3）

- **REST API**：`POST /api/workflows/:name/execute`、`GET /api/workflows/executions/:id/stream`（SSE）
- **持久化**：`SqliteWorkflowPersister` 实现 `WorkflowPersister` trait，使用 SQLite 存储快照
- **Cron 触发**：`CronScheduler` 的任务类型增加 `Workflow` 变体

### 5.5 peco-cli 集成（Phase 3）

```bash
# 列出可用的 Workflow
peco workflow list

# 执行 Workflow
peco workflow run code-review-and-fix

# 查看执行历史
peco workflow history
```

---

## 6. 目录结构

```
crates/peco-core/src/workflow/
├── mod.rs                  # 公开 API，re-exports
├── error.rs                # WorkflowError 错误类型
├── definition.rs           # WorkflowDefinition, WorkflowStep, StepType, StepConfig, StepResult
├── events.rs               # WorkflowEvent 枚举 + ApprovalResponse + ApprovalDecision
├── dag.rs                  # DagGraph: Kahn 拓扑排序 + BFS 分层 + 验证
├── template.rs             # TemplateContext: minijinja 模板引擎
├── condition.rs            # evaluate_condition() — 条件表达式求值
├── step_executor.rs        # execute_step_static() — Shell + Agent 步骤执行
├── engine.rs               # WorkflowEngine: spawn 模型 + 核心执行循环 + 持久化集成
├── handle.rs               # WorkflowHandle: 外部控制句柄（cancel/approve/wait/recv_event）
├── access.rs               # WorkflowAccess trait（窄接口）
├── persistence.rs           # WorkflowPersister trait + NullWorkflowPersister + WorkflowSnapshot
├── manager.rs              # WorkflowManager: 两级缓存 + 生命周期管理
└── tools/
    ├── mod.rs              # workflow::tools 子模块
    └── execute_workflow.rs # ExecuteWorkflow 工具 (impl ToolDyn)
```

---

## 7. 实施计划

### Phase 1：核心引擎 ✅ 已完成（2026-07-29）

> **实际产出**：10 个源文件，~1400 行代码 + ~500 行测试，31 个测试通过。事件通道改用 `tokio::sync::mpsc` 替代 intercom 对。

| 任务 | 文件 | 估时 |
|------|------|------|
| `WorkflowError` 错误类型 | `error.rs` | 0.5d |
| `WorkflowDefinition` 解析 + 验证（含 `_type` 注入逻辑） | `definition.rs` | 2.5d |
| `StepResult` / `StepOutcome` 运行时类型 | `definition.rs` | 0.5d |
| `DagGraph` 拓扑排序 + 分层 | `dag.rs` | 1.5d |
| `WorkflowEvent` 事件枚举 + `ApprovalResponse` | `events.rs` | 0.5d |
| `TemplateContext` minijinja 集成（含 `render_bool`） | `template.rs` | 1.5d |
| `WorkflowEngine` 核心循环（含显式 Abort 清理 + StepStarted 发射） | `engine.rs` | 3d |
| `WorkflowHandle` 控制句柄 + `run_id` | `handle.rs` | 1d |
| `step_executor` Shell + Agent 类型 | `step_executor.rs` | 1.5d |
| `WorkflowConfig` + 条件求值（minijinja 统一渲染） | `condition.rs` | 0.5d |
| `WorkflowAccess` trait | `access.rs` | 0.5d |
| **小计** | | **13.5d** |

### Phase 2：集成与持久化 ✅ 已完成（2026-07-29）

> **实际产出**：4 个新文件，8 个文件修改，47 个测试通过（累计）。关键修复：`WorkflowStep` YAML 反序列化 `_type` tag 注入 bug。

| 任务 | 估时 |
|------|------|
| `WorkflowPersister` trait + `NullWorkflowPersister` | 0.5d |
| `WorkflowManager` 生命周期管理（含缓存 + reload） | 1d |
| `execute_workflow` 工具实现 | 1d |
| `ToolRegister` + `ToolDependencies` 扩展 | 0.5d |
| `WorkSpace` 集成（`WorkflowAccess` 实现） | 0.5d |
| 测试：单元测试 + 集成测试 | 2d |
| **小计** | **5.5d** |

### Phase 3：peco-server 集成（约 1.5 周）

| 任务 | 估时 |
|------|------|
| `SqliteWorkflowPersister` 实现 | 1d |
| REST API 端点（CRUD + Execute + Stream） | 2d |
| SSE 流式（`WorkflowEvent` → SSE 事件映射） | 1.5d |
| `CronScheduler` 集成（Workflow 任务类型） | 1d |
| peco-cli 命令（workflow list/run/history） | 1d |
| **小计** | **6.5d** |

### Phase 4：高级特性（约 2 周）

| 任务 | 估时 |
|------|------|
| `WorkflowEngine` 新增 `tool_executor` + `llm_provider` 字段 | 0.5d |
| `Llm` 步骤类型（通过 `llm_provider` 调用） | 1d |
| `Tool` 步骤类型（通过 `tool_executor` 执行） | 1d |
| `retry_policy` 重试逻辑（含指数退避） | 1d |
| Stream 步骤的增量输出（`StepDelta` 事件） | 1.5d |
| HumanApproval 步骤（暂停/恢复通道 — 需要 peco-server 支持） | 1.5d |
| Workflow 嵌套（子 Workflow 步骤类型） | 1d |
| 前端 DAG 可视化 | 5d |
| **小计** | **12.5d** |

---

## 8. 测试策略

### 8.1 单元测试

```rust
// dag.rs 测试
#[test]
fn test_kahn_sort_simple_chain()       // A → B → C
#[test]
fn test_kahn_sort_parallel_branches()  // A → (B, C) → D
#[test]
fn test_kahn_sort_diamond()            // A → (B, C) → D
#[test]
fn test_kahn_sort_cycle_detection()    // A → B → A
#[test]
fn test_depends_on_self_reference()    // A depends_on: [A]
#[test]
fn test_depends_on_unknown_step()      // A depends_on: [Z]

// definition.rs 测试
#[test]
fn test_parse_valid_workflow_yaml()
#[test]
fn test_parse_missing_required_fields()
#[test]
fn test_validate_empty_steps()
#[test]
fn test_validate_rejects_phase4_types()   // Llm/Tool 类型在 Phase 1 应被拒绝
#[test]
fn test_step_config_serde_roundtrip()     // StepConfig 序列化/反序列化正确性
#[test]
fn test_step_config_type_injection()      // _type 字段注入逻辑

// template.rs 测试
#[test]
fn test_render_step_output()
#[test]
fn test_render_nested_json_field()
#[test]
fn test_render_truncate_filter()
#[test]
fn test_render_length_filter()
#[test]
fn test_render_undefined_step()            // 预期：WorkflowError::Template
#[test]
fn test_render_malformed_template()        // 预期：WorkflowError::Template
#[test]
fn test_render_inputs_parameter()          // {{ inputs.xxx }}
#[test]
fn test_render_conditional_if()            // {% if steps.X.success %}...{% endif %}
#[test]
fn test_render_bool_true()                 // "true" / "1" → true
#[test]
fn test_render_bool_false()                // "false" / "0" / "" → false
#[test]
fn test_render_bool_comparison()           // "{{ x == 'critical' }}" → true/false

// engine.rs 测试
#[tokio::test]
async fn test_execute_simple_chain()       // shell → shell → shell
#[tokio::test]
async fn test_execute_parallel_steps()     // 验证并行执行时间 < 串行
#[tokio::test]
async fn test_execute_condition_skip()     // condition 为 false 时跳过
#[tokio::test]
async fn test_execute_failure_abort()      // on_failure: abort
#[tokio::test]
async fn test_execute_failure_abort_cancels_siblings()  // abort 时显式取消同级未完成步骤
#[tokio::test]
async fn test_execute_failure_continue()   // on_failure: continue
#[tokio::test]
async fn test_execute_failure_retry()      // on_failure: retry + max_attempts（Phase 4）
#[tokio::test]
async fn test_execute_failure_pause_approve()    // pause → approve → 继续
#[tokio::test]
async fn test_execute_failure_pause_abort()      // pause → abort → 退出
#[tokio::test]
async fn test_execute_cancel()             // 中途取消
#[tokio::test]
async fn test_execute_empty_steps()        // 0 个步骤 → 立即 Completed
#[tokio::test]
async fn test_template_variable_passing()  // 步骤间数据传递
#[tokio::test]
async fn test_template_undefined_step()    // 模板引用不存在的步骤
#[tokio::test]
async fn test_template_inputs_parameter()  // {{ inputs.xxx }} 外部参数
#[tokio::test]
async fn test_depends_on_failed_step()     // 依赖步骤失败但 condition 不检查 success
#[tokio::test]
async fn test_parallel_abort_cleans_up()   // Abort 后无资源泄漏（JoinHandle 全部 abort）
#[tokio::test]
async fn test_step_started_emitted()       // 验证 StepStarted 事件在步骤执行前发射
#[tokio::test]
async fn test_run_id_in_all_events()       // run_id 出现在所有事件中
#[tokio::test]
async fn test_condition_via_minijinja()    // condition 通过 minijinja 渲染求值
```

### 8.2 集成测试

- 在测试 workspace 中创建真实的 `workflow.md` 文件 → `WorkflowManager::load()` → `WorkflowEngine::spawn()` → 验证完整执行
- 测试 `execute_workflow` 工具在 Agent ReAct 循环中被调用的完整链路
- 测试 WorkflowManager 缓存 + `reload()` 失效

---

## 9. 未来扩展（Phase 3+）

### 9.1 高级步骤类型

- **HumanApproval**：暂停引擎 → 发送审批事件 → 外部通过 `WorkflowHandle::approve()` 提供决策 → 继续/中止
- **Parallel**（步骤内并行）：一个步骤定义包含多个子步骤，并发执行
- **Workflow**（子 Workflow）：嵌套调用另一个 Workflow，参数化传递

### 9.2 补偿事务（Saga 模式）

多步写操作的补偿回滚：
```yaml
- id: "create-resource"
  type: tool
  config: { tool_name: "create_db", arguments: {...} }
  compensation:          # 失败时的补偿操作
    type: tool
    config: { tool_name: "delete_db", arguments: {...} }
```

### 9.3 动态 Workflow 生成

LLM 根据自然语言描述自动生成 `WorkflowDefinition`：
```
用户: "帮我设置一个代码质量门禁流程"
  → LLM 生成 workflow.md YAML
  → 用户确认 / 修改
  → 保存到 workflows/ 目录
```

### 9.4 条件路由

DAG 在构建时静态分析，执行路径通过 **condition 表达式在 DAG 层面实现分枝**，无需引入专用的 `router` 步骤类型。每个可能被路由到的目标步骤设置各自的 `condition`，依赖同一个上游步骤：

```yaml
# 场景：审查完成后根据严重程度路由到不同后续步骤
- id: "review"
  type: agent
  config:
    agent: "@code-reviewer"
    prompt: "审查代码，输出 JSON 包含 severity 字段"

- id: "alert-critical"
  type: shell
  config:
    command: "./alert.sh '{{ steps.review.output }}'"
  depends_on: ["review"]
  condition: "{{ steps.review.output.severity == 'critical' }}"

- id: "auto-fix"
  type: agent
  config:
    agent: "@developer"
    prompt: "修复：{{ steps.review.output }}"
  depends_on: ["review"]
  condition: "{{ steps.review.output.severity == 'minor' }}"

- id: "summary"
  type: llm
  config:
    prompt: "生成审查摘要：{{ steps.review.output }}"
  depends_on: ["alert-critical", "auto-fix"]
  # summary 总是执行（等待所有可能路径完成）
```

**为什么不用 Router 步骤类型**：

> 深层原因见第 10 节「与业界 Agent Workflow 方案的对比」——保持 DAG 静态可分析性是架构级决策，涉及 Explicit-vs-Emergent 哲学选择（§10.3.2）和 LangGraph conditional edges 的对比（§10.2）。此处聚焦技术层面的对比：

| 考量 | Router 步骤 | Condition-based |
|------|-----------|----------------|
| DAG 模型 | 破坏静态分析（动态跳转） | 保持静态 DAG（condition 过滤） |
| 可测试性 | 执行路径不确定，难以编写确定性测试 | 每个 condition 的求值是确定性的 |
| 可观测性 | 跳过的路径不产生事件，排查困难 | 被跳过的步骤产生 `StepSkipped` 事件 |
| 实现复杂度 | 需引入控制流原语（if/else/switch） | 复用现有 condition + depends_on |
| 表达能力 | 等价 | 等价（都能表达条件分发） |

### 9.5 Workflow 模板市场

类似 `peco-agents` 的 `BuiltinTemplate`，提供编译时嵌入的预置 Workflow 模板（`code-review`、`release`、`daily-digest` 等）。

---

## 10. 与业界 Agent Workflow 方案的对比

### 10.1 生态光谱

当前业界 Agent 编排系统分布在一条从「完全声明式」到「完全对话驱动」的光谱上：

```
纯声明式  ←————————————————————————————————————————→  纯对话驱动
  Temporal    Peco       LangGraph   CrewAI    AutoGen   OpenAI Swarm
  (代码定义)  (YAML DAG)  (StateGraph) (角色扮演) (对话编排) (Handoff)
```

Peco Workflow 选择偏左的声明式定位，这是一个有意识的权衡：牺牲灵活性换取可预测性、可审计性和 LLM 生成友好性。

### 10.2 各系统详细对比

#### LangGraph (LangChain)

| 维度 | Peco Workflow | LangGraph |
|------|-------------|-----------|
| **编排模型** | 声明式 DAG（YAML）+ 拓扑分层 | 有状态图（Python/JS 代码定义）：State + Nodes + Edges |
| **并行** | 拓扑层级内自动并行（`tokio::spawn`） | 需显式使用 `Send()` API 向其他节点发送 |
| **条件分支** | 静态 condition 门控（`{{ steps.X.success }}`） | 动态 conditional edges（函数运行时决定下一节点） |
| **人机协同** | `OnFailure::Pause` + 独立审批 mpsc channel | `interrupt()` 内置 checkpoint，暂停时持久化状态 |
| **持久化** | Phase 2 — `WorkflowPersister` trait | 内置 `Checkpointer`（`MemorySaver` / `SqliteSaver`） |
| **循环** | ❌ 严格 DAG（无环） | ✅ 支持 cycles（适用于 Agentic 循环场景） |
| **学习曲线** | 低（YAML 声明式） | 高（需理解 State、Reducer、Pregel 模型） |

**场景分野**：LangGraph 适合需要动态路由的复杂 Agent 编排（多步推理、自适应研究）；Peco 适合可预测的工程流水线（代码审查门禁、部署验证、数据处理）。两者互补，不是替代关系。

#### Temporal.io

| 维度 | Peco Workflow | Temporal |
|------|-------------|----------|
| **定位** | Agent 编排框架 | 持久执行引擎（分布式） |
| **定义方式** | YAML 声明式 | 代码（Go/Java/Python/TS）：Workflow + Activity |
| **执行保证** | Phase 1: best-effort | 至少一次（event history 重放） |
| **故障恢复** | 进程内重试 | 跨节点、跨进程、跨重启 |
| **运行时长** | 分钟级 | 天/年级（zero-cost durable wait） |
| **Saga 补偿** | Phase 3+ 计划中 | 原生 `Saga` 支持 |
| **运维复杂度** | 零（嵌入 peco-core） | 需要独立 Temporal Server 集群 |

**关键区分**：Temporal 解决的是「执行可靠性」问题，Peco Workflow 解决的是「Agent 协调」问题。这是 Framework vs Engine 的本质差异——大多数生产系统将两者配对使用（Agent 框架负责编排逻辑，Temporal 负责执行保证）。Peco 的定位在 Framework 层，这是正确的——分布式持久执行是独立的产品维度，不应内嵌在 Agent 引擎中。

#### CrewAI

| 维度 | Peco Workflow | CrewAI |
|------|-------------|--------|
| **编排模型** | DAG 拓扑排序（声明式结构） | 顺序任务列表 + 层级委托 |
| **并行** | ✅ 拓扑层内自动并行 | ❌ 核心是串行（Sequential Process） |
| **Agent 定义** | 引用已有 Agent（`agent: "@code-reviewer"`） | 每个 Agent 有 Role/Goal/Backstory |
| **执行确定性** | 高（静态 DAG，condition 可测试） | 低（LLM 驱动的任务分解和分配） |
| **适用场景** | 可预测的工程流程 | 需要 emergent behavior 的开放式任务 |
| **语言** | Rust（YAML 定义跨语言） | Python only |

**理念差异**：CrewAI 模拟人类团队协作（角色扮演 + emergent delegation），Peco 模拟 CI/CD 流水线（声明式 DAG + 确定性的步骤执行）。两者面向不同的用户需求：CrewAI 适合「帮我研究一下竞争对手」，Peco 适合「每个 PR 自动运行 lint → review → fix → verify」。

#### AutoGen (Microsoft) + OpenAI Swarm/Agents SDK

| 维度 | Peco Workflow | AutoGen | OpenAI Agents SDK |
|------|-------------|---------|-------------------|
| **编排核心** | DAG + 拓扑排序 | 对话驱动（GroupChat） | Handoff（Agent 间控制权移交） |
| **并行** | ✅ 层级内并行 | ❌ 对话默认串行 | ❌ 无并行原语 |
| **控制流** | 显式声明式（YAML） | 模型决定（speaker selection） | 模型决定（handoff 决策） |
| **确定性** | 高 | 低 | 低 |
| **模型锁定** | 否（DeepSeek，可扩展） | 否（多 provider） | 是（仅 OpenAI） |
| **转型状态** | — | 2025.10 合并进 Microsoft Agent Framework (MAF) | 积极迭代中 |

**根本哲学差异**：AutoGen 和 Swarm 相信「模型应该决定协作方式」，Peco 相信「工程师应该定义协作结构」。对于代码审查、部署流水线等合规性和可重复性敏感的场景，显式结构优于涌现行为。

#### Anthropic Claude Agent SDK

这是与 Peco 架构最接近的系统，因为两者共享「Markdown/YAML 定义 + 工具调用子 Agent」的设计理念。但定位层次不同：

| 维度 | Peco Workflow | Claude Agent SDK |
|------|-------------|------------------|
| **Workflow 定义** | `workflow.md`（YAML DAG） | JavaScript 脚本（`agent()`/`parallel()`/`pipeline()`） |
| **编排粒度** | 粗粒度步骤（Agent、Shell、LLM、Tool） | 细粒度（每个 `agent()` 调用是一个子 Agent） |
| **条件分支** | `condition` + minijinja 模板 | JavaScript `if`/`while` 循环 |
| **并行** | 拓扑层级声明式并行 | `parallel()` 函数式屏障 |
| **可审计性** | 高（YAML 静态结构可读） | 中（需阅读 JS 脚本逻辑） |
| **LLM 生成友好** | ✅ 结构化 YAML 易于 LLM 输出 | ❌ 代码级定义难以可靠生成 |
| **图灵完备** | ❌ 严格 DAG | ✅ 完整 JS 运行时 |

**关键差异**：Claude Agent SDK 的 Workflow 是图灵完备的 JS 脚本（支持 `while` 循环、动态条件、变量作用域），Peco 选择的是声明式 YAML DAG。这不是优劣之分，而是**目标用户不同**：

- JS Workflow → 开发者手动编写复杂编排逻辑（如 adversarial verify、loop-until-dry）
- YAML DAG → 用户和 LLM 生成标准化流程（如代码审查、部署流水线）

两者互补——Peco 已有 JS Workflow 工具（用于 Claude Code 集成），YAML Workflow 是面向更高层的声明式抽象。Phase 3+ 可通过 `Workflow` 步骤类型桥接两者。

### 10.3 五大架构洞察

基于以上对比，总结五条影响 Peco 设计决策的架构洞察：

**1. Framework vs Engine 的区分**

这是整个生态中最根本的二分法。Agent 编排框架（LangGraph、CrewAI、Peco）解决「Agent 如何协调」，持久执行引擎（Temporal）解决「如何保证执行一定完成」。Peco 定位在 Framework 层是正确的——分布式持久执行引入了确定性约束、事件溯源和跨节点故障恢复等独立维度的复杂性，不应内嵌在 Agent 引擎中。文档第 1.3 节将「分布式执行」列为非目标，表明对此边界有清醒认识。

**2. Explicit-vs-Emergent 是根本性的哲学选择**

AutoGen/Swarm 的设计从对话出发，相信模型能通过 conversation 涌现出协调模式。Peco/LangGraph 从结构出发，相信显式的控制流声明产生更可预测的结果。对于 Peco 的目标场景（工程流水线、合规门禁），显式结构优于涌现行为。这也是第 9.4 节拒绝 Router 步骤类型的深层原因——保持 DAG 的静态可分析性是架构级决策，不是实现细节。

**3. Handoff vs Delegation 对应不同的使用场景**

- **Handoff**（OpenAI Swarm 模式）：Agent A 将控制权移交给 Agent B，A 退出。适合对话路由场景（"你是账单问题，转 billing agent"）。
- **Delegation**（Anthropic/Peco 模式）：主 Agent 调用子 Agent 作为工具，结果返回后主 Agent 继续。适合结构化任务分解（"审查代码 → 修复 → 验证"）。

Peco 的 Agent 步骤和 `execute_workflow` 工具都是 Delegation 模式。这是正确的——Workflow 中的步骤是管线阶段，不是对话路由。

**4. 上下文隔离是子 Agent 设计的关键**

Claude Agent SDK 的子 Agent 有严格的上下文隔离：不继承父 Agent 的对话历史、MCP 连接、skills、memory，只接收显式的 `prompt`。Peco 的 Agent 步骤通过 `SimpleAgentLooper` 实现了等效隔离——Agent 启动时附带自己的工具（来自 `agent.md`），只接收从模板渲染的 `prompt`。这种隔离防止了上下文污染和 token 膨胀，是 Delegation 模式的核心价值之一。

**5. 持久化是不可逆的架构决策**

Temporal 和 LangGraph 的经验表明，持久化（checkpoint/event sourcing）不是「后加的功能」——它会影响执行循环的结构。Temporal 的确定性执行约束和 LangGraph 的 reducer 语义都是在架构初期引入的。Peco 文档将 `WorkflowPersister` 标记为 Phase 2 是务实的（先验证核心引擎），但需要注意：如果 Phase 1 的执行循环需要为 checkpoint 重放能力做重构，返工成本可能高于预期。关键的防御措施是**保持引擎状态的序列化能力**——Phase 1 中 `WorkflowDefinition`、`StepResult`、`HashMap<String, StepResult>` 均已实现 `Serialize + Deserialize`，这降低了 Phase 2 的集成风险。

---

## 11. 参考资料

- 现有 AgentLooper 设计：[agent_looper.rs](../crates/peco-core/src/agent/agent_looper.rs)
- 现有 SimpleAgentLooper：[simple_looper.rs](../crates/peco-core/src/agent/simple_looper.rs)
- DI 模式参考：[deps.rs](../crates/peco-core/src/tools/deps.rs)
- 工具注册模式：[tool_register.rs](../crates/peco-core/src/tools/tool_register.rs)
- Session 持久化 trait：[persistence/traits.rs](../crates/peco-core/src/persistence/traits.rs)
- 业界对比参考：
  - LangGraph 文档：https://langchain-ai.github.io/langgraph/
  - Temporal 与 AI Agent 集成：https://temporal.io/blog
  - Anthropic Claude Agent SDK 子 Agent 文档：https://code.claude.com/docs/en/agent-sdk/subagents
  - AutoGen v0.4 架构：https://microsoft.github.io/autogen/
  - CrewAI Flows 文档：https://docs.crewai.com/concepts/flows
  - OpenAI Agents SDK：https://openai.github.io/openai-agents-python/
  - Framework vs Engine 辨析：https://langchain.com/resources/langgraph-vs-temporal
