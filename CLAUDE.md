# CLAUDE.md

本文件为 Claude Code (claude.ai/code) 在此仓库中工作时提供指导。

## 构建/运行/测试命令

```bash
# 构建整个 workspace
cargo build --workspace

# 运行所有 Rust 测试
cargo test --workspace

# 运行特定 crate 的测试
cargo test -p peco-core
cargo test -p peco-daemon
cargo test -p knowledge-base

# 运行单个测试
cargo test -p peco-core <test_name>
cargo test -p peco-core -- --nocapture          # 显示测试输出

# 格式化 Rust 代码（提交前必须通过）
cargo fmt --all

# Lint（CI 中 warning 视为 error — unused_crate_dependencies 默认为 warn）
cargo clippy --workspace -- -D warnings

# 运行后端 daemon（默认 127.0.0.1:6971）
cargo run -p peco-daemon

# 开发模式（两个终端）：daemon + 前端 Vite (5174，代理 /api、/uploads 到 6971)
cargo run -p peco-daemon
cd peco-webui && npm install && npm run dev

# 生产部署（构建后端 + systemd --user unit，默认托管 peco-webui/dist）
bash scripts/deploy-daemon.sh install

# 前端
cd peco-webui && npm run test                 # vitest
cd peco-webui && npx tsc -b                   # TypeScript 类型检查（solution 式 tsconfig）
cd peco-webui && npm run build                 # tsc -b && vite build
```

## 架构总览

Peco 是一个**本地单用户、单进程、绑定回环地址**的多工作空间 AI Agent 平台：**Rust 后端**（Axum + Tokio）+ **React 19 前端**（TypeScript, Vite, Zustand, shadcn/ui）。资源按 **团队（team）→ 工作空间（space）** 三层组织，身份固定为单用户 `local`。

### Crate 依赖图（自上而下）

```
peco-daemon (唯一后端: Axum HTTP/SSE、本地单用户、多工作空间、任务台账、定时调度、记忆、静态托管)
  ├── peco-core (Agent 引擎: Agent, Session, ReAct 循环, Workflow, WorkSpace, Machine, MCP, Skills, Tools, 上下文/记忆基元)
  │     ├── model-provider (LLM 抽象层: ModelProvider trait; DeepSeek / Qwen / OpenAI 完整实现)
  │     ├── knowledge-base  (RAG: LanceDB + FastEmbed + BM25 + 知识图谱)
  │     └── peco-derive     (#[peco_tool] 过程宏)
  ├── model-provider        (daemon 直接消费 Content 等模型层类型)
  ├── knowledge-base        (daemon 的知识库 handler 直接构造 KbConfig；feature `helixdb`)
  └── peco-agents           (编译时嵌入的 Workspace 模板 — 无 peco-core 依赖)

peco-webui (React 19 前端，独立工程；产物由 daemon 同源托管，或 vite dev 代理到 daemon)
```

**核心原则**：`peco-core` 是引擎 — 不感知 HTTP、数据库连接或 Web。`peco-daemon` 是唯一后端，通过 `AppState`、`SpaceEnv`、`space::registry::SpaceRegistry` 与进程级 `peco_core::tools::WorkspaceHub` 把引擎接入本地 HTTP/SSE。`peco-agents` 提供编译时嵌入的模板数据，不依赖 `peco-core`。`Workflow` 模块遵循相同的 DI 模式 — 引擎通过 `WorkflowEngine::spawn()` 在 tokio 任务中运行，事件通过 mpsc channel 流出，不绑定特定传输层。

### peco-agents：Workspace 模板

- `BuiltinTemplate` 结构体：编译时通过 `include_bytes!` 嵌入的模板文件集合。
- 两套内置模板：`peco`（Peco 元 Agent：`@master` + `@memory`）与 `team_default`（团队默认 `@master`）。模板包含 `agents/`（可选 `knowledge/`）子目录。
- `materialize()` 将模板解压到临时目录。
- `WorkSpace::init_from_template()` 执行幂等安装：已存在的 Agent 和 KB 不会被覆盖，错误收集到 `TemplateInitReport` 中。

### peco-core：Agent 引擎

**Agent**（[crates/peco-core/src/agent/](crates/peco-core/src/agent/)）：
- 由 `agent.md` 文件定义：YAML frontmatter（模型、工具、MCP 服务器、Skills、max_iterations）+ Markdown 正文（系统提示词）。
- `Agent::from_file(path)` 解析文件，从 `providers.toml` 解析 provider 配置，创建对应 `ModelProvider`（DeepSeek / Qwen / OpenAI），并注册工具 + MCP 工具。
- `MessageFilter` trait：上下文组装后的钩子，可在消息列表发送给 LLM 之前对其进行转换（如脱敏、注入）。

**AgentLooper**（[crates/peco-core/src/agent/agent_looper.rs](crates/peco-core/src/agent/agent_looper.rs)）：
- **单一状态机** `LooperState`（9 变体，权威字段 `state` 单字段，写点收敛为 `set` / `set_quiet`）驱动 ReAct 循环：`Idle / PreparingRequest / ResolvingResponse / Streaming / ExecutingTools / Done / Failed / PausedDuringTools / PausedBeforeRequest`。`is_paused()` = 两个 `Paused*`，`is_active()` = 非 Idle 且非 `Paused*`。`set` 内「写状态 → 同步 `pause_mirror` → 发 `StateChange` 事件」三步同帧有序 —— 外部收到 `StateChange{to: 暂停态}` 时 `is_paused()` 已为 true。
- **挂起意图 `pause_pending`**（不是状态、不进 `has_pending_work`）：`Pause` 只在两个落地点消费 —— **落地点 A**（`react_step` 的 `PreparingRequest` 入口，覆盖首开发前 / 瞬时重发前 / 工具批完成后下一次请求前）落 `PausedBeforeRequest`；**落地点 B**（`ExecutingTools` 的 spawn 阶段前，`active_tool_tasks.is_none()`）落 `PausedDuringTools`。清零点：两个落地点消费、`finalize_cancel`、`finalize_failure`、`reset_iteration_counters`（轮起点，先于 `set(PreparingRequest)`）；流收敛无 tool_calls 时暂停被**轮次完成吸收**（意图不清、留到下一轮复位）。空闲态 `Pause` 是**无操作**；非暂停态的 `Resume` 则**撤回**尚未落地的暂停意图（清 `pause_pending`）。
- **主循环四步**：`run()` 每轮只做四件事 —— ① 取消息（`has_pending_work()` 为真时只非阻塞排空，否则阻塞 `recv()` 等一条再补排空同批；通道关且暂停中先走 `finalize_cancel`，然后 `Idle` 才 break，排队 pending 不补跑）→ ② 收尾（`is_cancelled()` 走 `finalize_cancel`，否则 `state == Failed` 走 `finalize_failure`，互斥）→ ③ 启动 pending 轮（**唯一的「pending → turn」入口**，仅放行 `Idle`）→ ④ 推进一步 `react_step()`（仅 `is_active()`）。
- **不空转不变量**：每一轮迭代要么在 ① 阻塞、要么严格推进一个状态。`has_pending_work` 的非阻塞触发点（运行位含 `Failed` → ④ 步进或 ② 收尾消解；`Idle` 的 `is_cancelled` → ② 消解；`Idle` 的 `armed && has_pending` → ③ 消解；两个 `Paused*` 恒为 `false`）各自有明确消解者，**新增触发点必须同时给出消解者**，否则热自旋。
- **取消 = 中止当前 ReAct 轮，looper 不退出**（`Cancel` 只置 `cancel_flag`，收尾在 ② 完成）：`finalize_cancel` 按状态回收在途资源（`Streaming` 丢 `active_stream`、`ExecutingTools` 走 `abort_inflight_tools`、两个 `Paused*` 无在途资源）→ 先丢弃该轮未消费的 pending（`Session::clear_pending`，用户点「停止」= 这一轮连同排队输入都不要）→ 冻结 + **强制落盘**（`force_persist`）→ 归位到干净 Idle。`cancel_flag` 是**一次性**的，收尾后必须复位。`Cancel` / `Shutdown` 都返回 `ReadDirective::StopReading`（丢弃同批后续消息），但只有 `Shutdown` 置 `shutdown_requested` 退出循环。
- **`pending_armed`** 区分「有待处理输入」与「这些输入现在该不该跑」：`Resume` / 新 `Query` 置位，`Cancel` 收尾与启动轮后清零。取消后不自动开新轮；在途轮未消费的 pending 随取消丢弃，**无在途轮**的 Cancel 才保留 pending 并随快照落盘。
- **步进上界 `STEP_POLL`（200ms）**：流式等 chunk、工具 poll、退避切片三处共用的唯一数字，同时是**取消的最坏生效延迟**。**batch 路径（`stream: false`）没有步进边界** —— 该轮取消延迟 = 整段生成时间。looper 无整轮看门狗，唯一的中止入口是取消。
- 工具执行分两阶段：**spawn 阶段**（将所有工具调用启动到 `JoinSet` 中），然后 **poll 阶段**（以 `STEP_POLL` 超时贪婪排空结果，每完成一个即发出事件）。
- **pending → 一条消息**：pending 不是消息，只有两处把它变成消息：轮次边界 `Session::dequeue_and_start_turn`（起**新轮**）与请求前 `Session::drain_pending_into_turn`（并入**当前轮**，steering，发 `UserSteer` 事件）。两者都用 `merge_contents` 合并、产出恰好一条 user message，合并来源**按批大小定**：单条走 `MessageSource::UserInput`（逐字节未改），多条才走 `MergedPending` —— 展示层只对后者调 `strip_merge_markers`，标错会让用户自己写的一行 `---` 在渲染时消失。steering 在 `prepare_and_send_request` 的 max_iterations 检查之后、构建上下文之前排空 pending，下一次模型请求即可见；若本轮先结束，pending 仍走轮边界兜底起新轮。被排空的 steering 已进 staging，之后取消/失败会随中断轮冻结，不再回 pending。
- **通道关闭**：无在途轮直接退出；有在途轮则继续步进把它跑完、回到 Idle 时才退。暂停中通道关闭走**取消语义**（没人能再解除暂停了）。
- ⚠ 已知偏差：web 前端 `abortStream()` 发出取消后立即关闭 SSE 连接，导致 run 被 runner 回收、「取消后不退出 looper」在 web 主路径未兑现（只对长连接有效）。daemon 的 `cancel` / `pause` / `resume` / `steer` 走**独立 HTTP 请求**（`ChatActiveRuns` 表把控制通道传进在途 looper），不依赖「取消即断流」。
- **流式路径**：使用 `StreamAssembler` 将 `StreamEvent` 块中的增量文本/推理/工具调用增量累积为完整的 assistant 消息。
- 动态上下文组装：系统提示词每轮重新注入，工具结果追加其后。`DynamicContext` trait 支持在每次新用户查询时注入 RAG 增强内容；同一轮的 ReAct 迭代复用缓存上下文。
- **上下文策略**：`FullHistory`（默认）、`SlidingWindow { max_turns }`、`TokenBudget { max_tokens, summarize_overflow }` 或 `Custom(Arc<dyn ContextFilter>)`。通过 `LooperConfig` 为每个 looper 选择。
- `LooperEvent` 枚举通过异步 intercom 通道（`Speaker`/`Listener` 对）流动，覆盖文本增量、推理增量、工具调用生命周期、状态转换、轮次边界和关闭。新增的 `ModelOutput` 在每次**未被重试作废**的模型调用后发一次（batch/stream 都发）：stream 下是已下发增量的权威汇总，batch 下是该次输出唯一的中途视图（`AgentMachine` 据此把 batch 文本写进任务产出窗口）。被 `begin_retry` 作废的尝试不发 —— 只在重试决策之后外发，避免只增不减的产出窗口残留残句。
- **模型错误分类与统一重发**：模型故障统一经 `model_failure_reason`（按 `ProviderError::classify()` 映射）转成类型化 `TurnFailureReason`（`RateLimited`/`ModelUnavailable`/`AuthError`/`QuotaExhausted`/`ContextOverflow`/`ContentFiltered`，均携带 provider 原始 msg）。故障出口 `fail_or_retry` 收敛三处 Err 站点：**只 classify 一次**，瞬时类进重发，否则写入 `failure_reason`。
  - **单一重发入口 `begin_retry(RetryCause)`**：截断（`Truncated { output_tokens }`，触发条件 `Incomplete + MaxTokens`）与瞬时（`Transient { trigger }`，限流/网络/5xx，或流被掐断 `Incomplete + finish_reason=None`）共用一条路径 —— 守卫 `can_retry` → 回退 staging 到请求前锚点 → 计数 → 设定退避 deadline → 通知。两者**语义差异只是入参**：截断额外抬输出预算，其余（退避、通知、回退）完全一致。
  - **单计数单上限**：`retry_limit` 默认 3，`PECO_RETRY_LIMIT` 可调，`0` = 关闭。**截断不需要独立限额** —— `budget_raised` 粘性到轮末，抬到 `retry_output_budget` 后 headroom 判据恒假，第二次截断重试被 `can_retry` 拦死。
  - **`budget_raised` 必须是独立布尔**而非 `retries_used > 0`：瞬时重发递增同一计数但绝不能抬预算。随 `reset_turn_counters` 清零，粘性不出轮。
  - **抬升目标 = 抬升上限**：`retry_output_budget` 默认 32_768，同时是 headroom 判据与粘性覆盖的唯一参数。**模型真实输出上限低于该值时截断重试会被网关 400 拒** —— 调低该值到模型上限之内，或 `PECO_RETRY_LIMIT=0` 关闭重试。
  - **统一退避**：`base * 2^(n-1)` 截断到上限，`retry_base_delay_ms` 默认 500 / `retry_max_delay_ms` 默认 5000。退避在 `prepare_and_send_request` 入口以可取消切片 sleep 等待（只读 deadline 不 `take()`，见 `wait_retry_backoff`），不阻塞取消。
  - **残片只丢弃不归还**：被回退的截断文本不进历史（`rollback_attempt` 的 `discarded_text` 是发给前端/落库的**丢弃指令**，随 `TruncationRetry` 事件即发即弃）。但 `plan_failure` 保留了**轮次存活桩** —— 重试后失败且新尝试零产出时补一条 `[response discarded after retry]` 的 assistant 消息，否则 `interrupt_turn` 见 staging 为空会退化成 `rollback_turn`，用户提问整轮从历史消失。
  - 四个 env 的读取**下沉到 `LooperConfig::from_env()`**（`PECO_RETRY_LIMIT` / `PECO_RETRY_OUTPUT_BUDGET` / `PECO_RETRY_BASE_DELAY_MS` / `PECO_RETRY_MAX_DELAY_MS`），daemon 的 `SpaceEnv::looper_config` 统一委托。
- `LooperHook` trait：9 个拦截点（`on_before_request`、`on_after_response`、`on_text_delta`、`on_before_tool`、`on_after_tool`、`on_turn_complete`、`on_context_compacted`、`on_boundary_reduction`、`on_state_change(from, to: LooperState)`）。内置钩子：`ToolAllowlistHook`、`TokenBudgetHook`、`DispatchWatchHook`（跨空间任务终态回写台账）。

**SimpleAgentLooper**（[crates/peco-core/src/agent/simple_looper.rs](crates/peco-core/src/agent/simple_looper.rs)）：
- 最小化的纯 batch 变体，由 `DelegateSubAgent` 与 `RunParallelSubAgents` 使用。
- 无流式、无钩子、无事件、无会话持久化。仅：`用户消息 →（模型 → 工具）* → 最终文本`。

**Session**（[crates/peco-core/src/session/](crates/peco-core/src/session/)）：
- 状态机：`Idle → Active → Commit/Rollback/Cancel`。还有 `Cancelling` 和 `Interrupted` 中间状态。
- 双层消息缓冲区：`CommittedBuffer`（已持久化的轮次，`Vec<Vec<AnnotatedMessage>>`）+ `StagingBuffer`（当前轮次的在途消息）。
- `TurnBoundaryToken` — 一个零大小证明令牌，仅由 `commit_turn()` 和 `rollback_turn()` 返回。`snapshot()` 需要此令牌，提供编译期保证：快照仅在轮次边界发生，永不在轮次中间。
- `PendingInput` 队列处理活跃轮次期间的并发用户输入：轮结束时整个队列一次排空，经 `merge_contents()` 合并为一条 user 消息启动新轮（单条原样、多条每条前置 `---` 标记行、图片部件保留；纯文本与含图两分支产出的 wire 文本一致）。合并消息标记 `MessageSource::MergedPending`，展示层（session_dto / markdown 导出）据此经 `strip_merge_markers()` 剥离 `---` 标记后再渲染，模型侧原样。
- 消息以 `Arc<Message>` 包裹在 `AnnotatedMessage` 中（含 id、turn_index、timestamp、source、estimated_tokens），实现零拷贝上下文构建。
- 持久化是外部的：`SessionPersister` trait（基于文件的 `FileSessionPersister` 或 `NullSessionPersister`）。Looper 在轮次边界调用 `persister.save()`。**daemon 使用 `SqliteSessionPersister`**（[crates/peco-daemon/src/session_store/](crates/peco-daemon/src/session_store/)）把 `SessionSnapshot` 存入 `session_snapshots` 表，并另承载在途轮检查点 `conversation_inflight_turns`（崩溃恢复）。快照不进文件系统，空间隔离由 `conversations.workspace_id` 把关。

**Tools**（[crates/peco-core/src/tools/](crates/peco-core/src/tools/)）：
- 双 trait 设计：`Tool`（静态、泛型、类型化）和 `ToolDyn`（对象安全，`Pin<Box<dyn Future>>`）；blanket impl 桥接二者。
- `ToolExecutor` trait：运行时接口 — `execute(name, args) -> Result<String, String>` + `definitions() -> Vec<ToolDefinition>`。
- **DI 契约**（`deps.rs`）：6 个窄 trait — `AgentAccess`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`、`McpAccess`、`MemoryAuditAccess` — 以及聚合结构体 `ToolDependencies`。工具只依赖这些 trait，不直接依赖 `WorkSpace`。
- **工具组装**（`tool_register.rs`）：`ToolRegister::build()` 根据 tool_names 和 `ToolDependencies` 一次性构建 `ToolExecutor`。权威工具名清单为 `BUILTIN_TOOL_NAMES`（由防漂移测试保障与 match arms 一致）；可选依赖缺失时对应工具 warn + skip，不 panic。
- 内置工具（41 个）：
  - **通用/检索**：`shell`、`fetch`、`web_search`、`show_workspace`、`list_tools`
  - **Agent/Skill/Workflow/MCP**：`save_agent`、`read_agent`、`delete_agent`、`read_skill`、`list_skills`、`save_skill`、`delete_skill`、`execute_workflow`、`list_workflows`、`save_workflow`、`delete_workflow`、`list_mcp_servers`、`save_mcp_server`、`delete_mcp_server`、`test_mcp_connection`
  - **子 Agent**：`delegate_sub_agent`、`run_parallel_sub_agents`
  - **知识库**：`search_knowledge`、`list_knowledge_bases`、`add_to_knowledge_base`、`sync_knowledge_base`、`get_knowledge_base_docs`、`add_facts_to_knowledge_base`、`query_entity_facts`、`delete_kb_document`、`delete_kb_documents`、`delete_entity_fact`、`delete_entity_facts`、`delete_entity`
  - **跨空间调度**：`create_workspace`、`open_workspace`、`dispatch_to_workspace`、`inspect_workspace_task`、`run_machine_task`、`inspect_machine_task`、`save_schedule`
- KB 工具通过 `check_kb_access()` 执行 Agent 级别访问控制（基于 agent.md `knowledge_bases` 白名单）。
- `#[peco_tool]` 宏（来自 `peco-derive`）：标注一个 async fn，生成实现 `Tool` 的零大小结构体、带 `#[derive(Deserialize, JsonSchema)]` 的 `Parameters` 结构体，以及 `static TOOL_NAME` 常量。
- `DefaultToolsExecutor` 是标准实现：持有 `HashMap<String, Box<dyn ToolDyn>>` 并按名称分发。

**WorkSpace**（[crates/peco-core/src/workspace/](crates/peco-core/src/workspace/)）：
- 按空间隔离的边界。每个 `WorkSpace` 持有 `Config`、`SkillRegister`、`KnowledgeManager`、`AgentManager`、`WorkflowManager`。实现 `tools` 模块中定义的 DI trait（`AgentLoader`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`），通过 `build_tool_executor()` 委托给 `ToolRegister::build()` 完成工具组装。

**Machine / 跨空间派发**（[crates/peco-core/src/machine/](crates/peco-core/src/machine/)）：
- `machine.rs` / `runtime.rs` / `hub.rs` / `state.rs` / `store.rs`：跨空间「机器任务」的运行体与生命周期。派发工具（`run_machine_task` / `dispatch_to_workspace`）在 `tools/machine_tool.rs`、`tools/workspace_tools.rs`。
- `agent/hooks/dispatch_watch.rs`：`DispatchWatchHook` 在任务终态把结果回写台账（daemon 侧由 SQLite sink 承接）。
- daemon 侧：`space/ledger.rs` + `db/dispatches.rs` 存台账，`wake/`（`pump` / `compose` / `deliver`）负责任务终态后唤醒 Peco（debounce tick 5s、批量上限、重试退避）。

**Workflow**（[crates/peco-core/src/workflow/](crates/peco-core/src/workflow/)）：
- 声明式 DAG 工作流编排引擎，与 ReAct 循环互补 — 提供确定性步骤编排。
- **定义格式**：`workflow.md`（YAML frontmatter + 可选 Markdown body），与 `agent.md`/`SKILL.md` 风格一致。
- **引擎模型**：`WorkflowEngine::spawn()` 在 tokio 任务中运行，通过 `tokio::sync::mpsc` 发射 `WorkflowEvent`（Started → StepStarted → StepCompleted/StepFailed/StepSkipped → Completed/Failed/Cancelled）。外部通过 `WorkflowHandle` 消费事件、发送审批、取消或等待完成。
- **DAG 拓扑执行**：Kahn 算法拓扑排序 + BFS 分层，层级间串行，层级内步骤通过 `tokio::spawn` 并行。
- **步骤类型**：`shell`、`agent`（复用 `SimpleAgentLooper`）；`llm`、`tool` 已定义类型但尚未实现。
- **模板变量**：基于 minijinja，支持 `{{ steps.X.output }}`、`{{ inputs.xxx }}`、`{% if %}`、`truncate`/`length`/`replace` 过滤器。
- **失败策略**：`Continue` | `Abort`（默认）| `Pause`（独立 mpsc channel 等审批）| `Retry`（已定义，尚未实现）。
- **条件门控**：`condition` 字段通过 minijinja 求值，正交于 `depends_on`。
- **持久化**：`WorkflowPersister` trait（与 `SessionPersister` 同模式）；当前内置实现为 `NullWorkflowPersister`（daemon 的 `workflow_executions` 表为执行记录预留）。
- **工具集成**：`execute_workflow` 是 `ToolDyn` 工具，Agent 可在 ReAct 循环中调用（同步阻塞语义，适合短 workflow）。`OutputSchema` 通过 prompt 追加 JSON schema 指令实现，尚未做真正的结构化输出。
- **DI 契约**：`WorkflowAccess` trait（load/list/reload）由 `WorkSpace` 实现，注入 `ToolDependencies`。

**MCP**（[crates/peco-core/src/mcp/](crates/peco-core/src/mcp/)）：
- `McpManager`：每个 Agent 的 MCP 连接编排器。接收已解析的 `(name, McpServerConfig)` 对，创建传输层（`rmcp` 的 Stdio / SSE / StreamableHTTP），连接、发现工具，注册为 `McpTool` 包装器。
- `McpClientHandler`：实现 `rmcp::ClientHandler`，自动同步工具列表变更（list_changed → 移除所有受管工具 → 重新列出 → 重新注册）。
- MCP 配置由 daemon 的 `mcp_config` 模块按空间管理（`/api/mcp`），底层仍由 core 的 `McpConfig::load()` 解析。

**Skills**（[crates/peco-core/src/skills/](crates/peco-core/src/skills/)）：
- 三级渐进式加载：Tier 1（启动时名称+描述）、Tier 2（激活时完整正文）、Tier 3（按需 scripts/references/assets）。
- `SKILL.md` 格式：YAML frontmatter（`name`、`description`、`allowed-tools`）+ Markdown 正文。
- 目录结构：`<skill-name>/SKILL.md`，可选 `scripts/`、`references/`、`assets/`。

**上下文压缩与截断**（core 原语 + daemon 参数）：
- `CompactionPolicy::maybe_compact()`（[crates/peco-core/src/agent/compaction.rs](crates/peco-core/src/agent/compaction.rs)）在 turn 边界（looper Done 分支）估算 pinned + committed token，超过触发阈值时用 Flash 模型递归合并旧摘要与被驱逐轮次为结构化摘要（固定模板），`Session::compact()` 物理驱逐最旧轮次并重编号 turn_index，摘要作为 `pinned_summary`（System 消息）钉在最前。失败非致命，仅记日志。
- **单一截断点**：`PecoContextFilter`（[crates/peco-core/src/agent/](crates/peco-core/src/agent/)）一处完成 — pinned 层（System/摘要）→ Verbatim 层（按 token 预算从最新往回整轮选择）→ 当前轮（完整保留）。`ContextStrategy` 保持 `FullHistory` 直通。`StripPolicy` 负责历史 reasoning / 工具参数 / 工具输出的可逆削减。
- **daemon 参数入口**（[crates/peco-daemon/src/space/compaction.rs](crates/peco-daemon/src/space/compaction.rs)）：`PECO_COMPACTION_TRIGGER_TOKENS`（默认 384k）、`PECO_COMPACTION_KEEP_RECENT_TOKENS`（128k）、`PECO_HISTORY_TOKEN_BUDGET`（128k）、`PECO_SUMMARIZER_MODEL`（`deepseek-flash`）、`PECO_COMPACTION_STRIP_*` 系列、`PECO_COMPACTION_CONTEXT_SHADOW_RETENTION_DAYS` 等。边界观测（`CompactionMetricsHook` / `ContextShadowHook`）在 `space/compaction_hooks.rs`。
- **token 估算**：`estimate_str_tokens()`（[crates/peco-core/src/agent/context.rs](crates/peco-core/src/agent/context.rs)）CJK 0.6 token/字、其他 0.3 token/char，全项目唯一实现。

### peco-daemon：本地多工作空间后端

**定位**：单进程、单用户（`LOCAL_USER_ID = "local"`）、默认绑定 `127.0.0.1:6971`（`config.rs: DEFAULT_BIND_ADDR`）。数据根 `$PECO_HOME`（默认 `~/.peco-daemon`，`HOME_DIR_NAME`），与旧 `.peco` 隔离。同一数据根只允许一个 daemon（`flock` 单实例锁，`single_instance.rs`）。

**启动流程**（[main.rs](crates/peco-daemon/src/main.rs)）：tracing（`PECO_LOG`）→ `DaemonConfig::load()` → 单用户 SQLite 连接 + 迁移 → 边界观测表按保留期回收 → 单实例检查 → 注入进程级 `SystemConfig` → 解析 Peco 身份 → 构建 `WakePump` / `TaskObserver` / `SpaceEnv` / `WorkspaceHub`（`DaemonOpener`）并 `install_workspace_hub` → `bootstrap_peco` 幂等装 Peco 团队与默认空间 → `db::workspaces::rescan` 普查团队/空间 → 构建 `SpaceRegistry` / `ChatActiveRuns` / `PecoDelivery` / `Scheduler` → `AppState` → `reconcile_ledger` 对账 → `wake.startup_reclaim()` / `wake.spawn()` → 中断上个进程残留的 running 排期 → `install(schedule store)` → `scheduler.spawn()` → `http::serve()` → 优雅收尾 `shutdown::drain`。

**AppState**（[crates/peco-daemon/src/state.rs](crates/peco-daemon/src/state.rs)）：`SqlitePool`、`Arc<WorkspaceHub>`（**唯一**已打开实例表）、`Arc<SpaceEnv>`、`Arc<DaemonConfig>`、`Arc<TaskObserver>`、`Arc<SpaceRegistry>`（ws_id → 目录）、`Arc<ChatActiveRuns>`（会话 → 控制通道）、`Arc<WakePump>`、`Arc<Scheduler>`、`Shutdown`、`Arc<WebAuth>`。

**空间与团队**（[crates/peco-daemon/src/space/](crates/peco-daemon/src/space/)）：
- 三层：Peco（全局主管，专属 team + 默认空间）/ team / space。每个 space 绑定且仅绑定一个 team，资源目录按空间隔离；`db/workspaces.work_dir` 是 ws_id → 目录的唯一映射。
- `.workspace.json` marker 承载空间身份；`space/open.rs` 打开决策表，`close.rs` 关闭门槛，`trash.rs` 回收站（移入/销毁两套确认），`registry.rs` 适配层（Hub 仍是唯一真源），`bootstrap.rs` 引导 Peco，`reconcile.rs` 重启对账，`fanout.rs` 文件变更扇出，`index.rs` 索引普查。
- 空间**惰性打开**：`POST /api/spaces/{ws_id}/open` 由前端进入时调用；`close` 卸载。
- 团队：`team/` + `db/teams`；团队能力根持有 `providers.toml`、agents/skills/knowledge/mcp。

**HTTP 路由**（[crates/peco-daemon/src/http/mod.rs](crates/peco-daemon/src/http/mod.rs)）：
- 认证：`/api/auth/{status,register,login,logout,password}`
- 空间：`/api/spaces`（list/create）、`/api/spaces/{ws_id}`（detail/update/delete）、`/api/spaces/trash`、`/api/spaces/{ws_id}/{restore,close,open}`、`/api/spaces/{ws_id}/tasks`、`/tasks/live`、`/tasks/{task_id}/stream`、`/tasks/{task_id}/cancel`、`/api/spaces/{ws_id}/{artifacts,files,overview}`、`/api/spaces/{ws_id}/conversation/{ensure,clear}`、`/api/spaces/{ws_id}/memory/documents[/{doc_id}]`
- 团队：`/api/teams`、`/api/teams/{team_id}`（detail/update/delete）
- Provider：系统层 `/api/providers[/{name}][/test]` + `/types`；团队层 `/api/teams/{team_id}/providers[/{name}][/test]`
- 定时：`/api/schedules[/preview][/{id}][/{id}/run][/{id}/runs]`
- 其它：`/api/peco`（Peco 详情）、`/api/brand`、`/api/events`（台账 SSE）、`/api/upload`（图片上传）
- 嵌套：`/api/agents`、`/api/skills`、`/api/mcp`、`/api/knowledge`、`/api/chat`、`/api/artifacts`；`/uploads` 静态目录
- 静态前端：`PECO_WEBUI_DIST` 设置时作最后兜底（SPA），`/api` 与 `/uploads` 未命中仍按各自 404 语义。

**认证**（[crates/peco-daemon/src/web_auth/](crates/peco-daemon/src/web_auth/) + [http/auth.rs](crates/peco-daemon/src/http/auth.rs)）：
- 单用户账号：`users` 表只有 `local` 一行，密码摘要用 bcrypt。**没有 JWT**。
- 两种凭据：运维/自动化用 **Bearer setup token**（`$PECO_HOME/token`，启动缺失即生成）；浏览器用 **会话 cookie**（`peco_session`，`auth_sessions` 滑动续期 30 天，仅 HTTPS 加 `Secure`）。
- `require_auth` 中间件按 `needs_credentials`（`/api` 且非公开路径）判定；登录有进程内 throttle。

**Chat/SSE**（[crates/peco-daemon/src/chat/](crates/peco-daemon/src/chat/)）：
- 路由（nested 在 `/api/chat`）：`/{agent_id}/conversations`、`/{id}`（update/delete）、`/{id}/messages`、`/{id}/session`、`/{id}/images`、`/{id}/artifacts`、`/{id}/stream`（新开一轮）、`/{id}/stream/live`（只订阅在途轮，204=无在途）、`/{id}/cancel`、`/{id}/steer`、`/{id}/pause`、`/{id}/resume`、`/{id}/export`。每个 handler 要求 `LocalUser` + 空间选择器（`X-Peco-Space` / `?ws=`）。
- SSE 事件类型：`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_result`、`turn_complete`、`agent_call_start`、`agent_call_end`、`context_compacted`、`truncation_retry`、`usage`、`error`、`done`、`looper_state`、`user_steer`。内部 `LooperEvent::ModelOutput` 仅在 `origin: Batch` 时翻译成一次 `text_delta`；`StateChange` 只在跨暂停转换时转成 `looper_state`；`TruncationRetry` 的 `discarded_text` 不下发。
- `truncation_retry` 是**纯通知**（无撤销语义）：前端不删除已收到增量，只在本轮气泡**之前**插一条居中横幅。`reason` 区分 `truncated` / `transient`。
- 子 Agent 调用（按 tool_call_id 映射 `SubAgentInfo`）生成 `agent_call_start`/`agent_call_end`。
- 会话运行 `chat/run.rs::spawn_run` 同时被定时任务复用（无客户端链路）。

**定时任务**（[crates/peco-daemon/src/schedule/](crates/peco-daemon/src/schedule/)）：
- `cron`（本机时区计算，落库一律 UTC 文本）、`scheduler`（tick 15s）、`fire`（到点触发，复用 `spawn_run`）、`access`（存取）、`notify`（结果通知，落点固定 Peco 空间 `@master`）。
- 补跑窗口 `GRACE = 10min`；会话忙时重试；running 行在进程重启时按 `interrupt_running` 处理。

**记忆**（[crates/peco-daemon/src/memory/](crates/peco-daemon/src/memory/)）：
- 物理载体是**团队共用**的 `@private_memory` KB；**按空间隔离靠文档 metadata 的 `WS_ID_TAG = "ws_id"`**：写路径必须打标、读路径必须按标过滤（两侧共用同一常量，漏一处就跨空间串记忆）。
- **写路径** `MemoryExtractionHook`：每轮成功完成后守卫检查（失败轮跳过、`analyze_min_chars` 过滤），`tokio::spawn` 后台检索既有记忆抑制重复 → Flash 模型（`ModelTurnAnalyzer`，严格 JSON）提取 → 写入 KB。turn 边界零阻塞，失败 warn 后 return。
- **读路径** `MemoryRecallContext`（DynamicContext）：新 query 前闲聊门控（问候/感谢关键词，零成本跳过）→ 混合检索 → 按类别格式化注入 instructions 尾部，逐行截断。
- `compose.rs`（`ComposedDynamicContext`）组合 Peco 空间清单（`dynamic.rs` 的 `SpaceCatalogContext`）与记忆召回；`migration.rs` / `cleanup.rs` 负责存量迁移与旧记忆清理；`layout.rs` 定位库；`config.rs` 读 `MemoryConfig`。
- 删除路径经 `space/memory_audit.rs` + `db::memory_audit`（outbox 审计），管理员删除受保护类别时拒绝。
- **精简面**：当前只做文档记忆，不含图事实、取代事务、巩固流水线与 embedding 去重重排。

**静态托管**（[crates/peco-daemon/src/http/static_files.rs](crates/peco-daemon/src/http/static_files.rs)）：`PECO_WEBUI_DIST` 指向前端产物时挂 SPA 兜底；未设置或 dist 不可用时不挂（行为与没有这一层一致）。

### model-provider：LLM 抽象层

- `ModelProvider` trait（async_trait）：`name()`、`generate_full(&GenerateRequest) -> GenerateResult`、`generate_stream(&GenerateRequest) -> GenerateStream`。
- `GenerateRequest`：model、instructions、input（`InputItem` 列表）、tools（`ToolDefinition`）、tool_choice、temperature、top_p、max_output_tokens、reasoning（`ReasoningConfig`）、text（`TextFormat`）、additional_params。
- `StreamChunk` 枚举：`BlockStart`、`TextDelta`、`ReasoningDelta`、`ToolCallDelta`、`BlockEnd`、`Usage`、`Finish`。
- 三个 provider 完整实现：`DeepSeek`（`DEEPSEEK_API_KEY`，chat + Responses 双路径，`api` 默认 `"responses"`）、`Qwen`（`DASHSCOPE_API_KEY`，chat + Responses 双路径，`api` 默认 `"responses"`）、`OpenAI`（`OPENAI_API_KEY`，chat + Responses 双路径，`api` 默认 `"chat"`；兼容网关经 `base_url` 覆盖）。类型定义另支持 `anthropic`、`ollama`、`groq`（待实现）。
- **Responses 适配器**：`DeepSeekResponsesAdapter`（原生 `/responses`，端点剥离 `/v1`）与 `QwenResponsesAdapter`（百炼 OpenAI 兼容 `/responses`，端点**保留** `/v1`；reasoning 为 `summary` 摘要形态；`function_call_output` 逐对紧跟对应 `function_call`；`store=false`）。`OpenAiResponsesAdapter`（原生 `/responses`，端点保留 `/v1`；`store=false`、顶层 `instructions` 直传、`Role::Developer` 原生直传、reasoning `{effort, summary:"auto"}`、历史 `Reasoning` 不回传）。差异细节见 `docs/design/qwen-responses-design.md` 与 `docs/research/qwen-responses-research.md`。
- Provider 配置：系统层 `$PECO_HOME/providers.toml`（或 `PECO_PROVIDERS_CONFIG`），团队层 `<team>/providers.toml`。

**错误分类**（[crates/model-provider/src/error.rs](crates/model-provider/src/error.rs)）：
- `ProviderError::classify() -> ClassifiedError { kind, message, code }` — 惰性语义分类。`ApiErrorKind`：`RateLimited`/`Network`/`Server`（瞬时，`is_transient()` 为 true）| `Auth`/`NotFound`/`ContextOverflow`/`QuotaExhausted`/`ContentFiltered`/`InvalidRequest`/`Unknown`（永久）。
- `classify_api_error(status, body)` 判定顺序**先 body 后 status**（流内错误 payload 用伪造 status 500，真实语义只在 body 里）。`ClassifiedError.message` 携带原始 body 摘要（截断 500 字符），分类不吞原文。
- `FinishReason::ContentFilter` 独立于 `Error` — 内容过滤与一般上游异常在 looper 侧必须分得开（前者类型化报错且明确不重试）。

**SSE 流式管道**（[crates/model-provider/src/streaming/pipeline.rs](crates/model-provider/src/streaming/pipeline.rs)）：
- 与 provider 无关的设计：`process_normalized_sse_stream_chunks()` 消费原始 SSE 帧，经 `StreamingProfile` 规范化为 `NormalizedChunk` 并生成 `StreamChunk`。新增 provider 只需实现 `StreamingProfile` 与 `ModelProvider`。
- `StreamingEventSource<R>`（[sse.rs](crates/model-provider/src/streaming/sse.rs)）：5 状态流（`Connecting → Open → WaitingToRetry → Reconnecting → Closed`），默认指数退避（起始 300ms，2x，5s 上限，最多 5 次），按 `error.is_transient()` 分类门控。**established-stream 中断不透明重连**（LLM SSE 不支持 `Last-Event-Id` 恢复，重连 = 全新生成 → 增量重复），上抛给 looper 做带回退的整轮重发。
- DeepSeek 思考/推理：`reasoning_effort` 映射到 `thinking` 字段（`"disabled"` / `{"type":"enabled","effort":"<value>"}`），未设置默认 `{"type":"enabled","effort":"high"}`。

### knowledge-base：RAG 引擎

- **摄入管道**（6 步）：解析（PDF/DOCX/HTML/MD/代码/纯文本）→ 分块（滑动窗口，句子边界对齐，默认 800 字符窗、200 字符重叠）→ 批量嵌入（FastEmbed ONNX，默认 `bge-base-zh-v1.5`，768 维）→ 存储文档 → upsert 向量 → 全文索引 + 结构图谱边（`CONTAINS`、`NEXT_CHUNK`）。
- **Chunk ID 确定性**：`{doc_id}-{seq:04}-{sha256[0..8]}` — 幂等摄入。
- **基于 trait 的后端抽象**：5 个 trait（`DocumentStore`、`VectorIndex`、`FullTextIndex`、`GraphStore`、`CombinedSearch`）。三种后端：`InMemoryBackend`（测试）、`LanceDbBackend`（生产，Arrow）、`HelixDbBackend`（feature-gated，HTTP 客户端，`CombinedSearch` 快速路径单次往返多路搜索）。daemon 启用 `helixdb` feature。
- **自适应 4 层检索**（`QueryAnalyzer` → `PathCalibration` → `CrossValidation` → `AdaptiveFusion`）：分类意图、校准分数分布、跨路径交叉验证（StrongAgreement/WeakAgreement/SinglePath）、调整 RRF 融合权重与置信度；无 `QueryAnalyzer` 时优雅降级。
- `KnowledgeBaseManager`：管理多个 KB，各自 `KbConfig`（后端、分块、嵌入），支持并发跨 KB `search_all()`。
- **配置存储**：每个 KB 目录内自包含 `kb_config.json`；`load()` 扫描 `knowledge/*/kb_config.json` 发现 KB，旧中心化格式自动迁移为 `.bak`。
- **双重命名**：`KbConfig.name` 对外（API、agent.md `knowledge_bases`），目录名是对内 sanitize（`sanitize_kb_name()`），HashMap key 始终用 `config.name`。
- **Agent 级别 KB 访问控制**：agent.md `knowledge_bases` 为白名单（空 = 无权访问），`ToolDependencies.allowed_kbs` 注入所有 KB 工具，经 `check_kb_access()` 守卫。

### peco-webui：React 前端

- 独立工程（[peco-webui/](peco-webui/)），`npm run dev` 起 Vite（5174），把 `/api`、`/uploads` 代理到 daemon（`process.env.PECO_DAEMON_URL`，默认 `http://127.0.0.1:6971`，`secure:false` 接受自签证书）；生产由 daemon 同源托管 dist。
- **页面**（[peco-webui/src/pages/](peco-webui/src/pages/)）：`PecoChatPage`（Peco 面板）、`SpaceListPage`（含回收站）、`SpaceDetailPage`、`SpaceStatusPage`、`NewSpacePage`、`TeamListPage`、`TeamDetailPage`、`SchedulesPage`、`SettingsPage`、`ResultsPage`、`auth/`。
- **Stores**（Zustand）：`authStore`、`boardStore`、`connectionStore`、`dockStore`、`openSpacesStore`、`spaceActivityStore`、`spaceStore`、`themeStore`、`uiStore`。
- **API 层**（[peco-webui/src/api/](peco-webui/src/api/)）：`agents`、`auth`、`brand`、`conversations`、`knowledge`、`mcp`、`peco`、`providers`、`schedules`、`skills`、`spaces`、`stream`（SSE 解析）、`tasks`、`teams`、`upload`。
- **SSE**：`fetch()` + `ReadableStream` reader（非 EventSource），支持 `AbortController`，消费上述 14 种事件，响应式更新消息（追加增量、可折叠推理、工具调用卡片、子 Agent 嵌套气泡）。Peco 页在途轮仍可 `steer` / `cancel` / `pause` / `resume`（独立 HTTP 请求）。
- **组件**：shadcn/ui（Radix）+ Tailwind CSS v4；表单 `react-hook-form` + `zod`；数据请求 `@tanstack/react-query`；Markdown 渲染 `react-markdown` + `remark-gfm` + `rehype-highlight`。
- **路由**：`react-router-dom` v7，登录态守卫。

## 配置文件

### providers.toml（LLM provider 配置）
系统层：`$PECO_HOME/providers.toml`（或 `PECO_PROVIDERS_CONFIG` 绝对路径）；团队层：`<team>/providers.toml`。
```toml
default_provider = "deepseek"
[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"
# api = "responses"（默认）| "chat"
[providers.deepseek.default]
model = "deepseek-flash"
temperature = 0.7
max_tokens = 4096
stream = true

[providers.qwen]
type = "qwen"
api_key = "${DASHSCOPE_API_KEY}"
# api = "responses"（默认）| "chat"
[providers.qwen.default]
model = "qwen3.7-max"

[providers.openai]
type = "openai"
api_key = "${OPENAI_API_KEY}"
base_url = "https://api.openai.com/v1"   # 可省略；覆盖为兼容网关地址
# api = "chat"（默认）| "responses"

# 可选 — 内置 web_search 工具（未配置时该工具不注册）
[web_search]
provider = "searxng"   # "searxng" | "tavily" | "brave"
[web_search.searxng]
base_url = "http://localhost:8888"   # 实例需启用 JSON 输出格式
```

### daemon.toml（可选，`$PECO_HOME/daemon.toml`）
可配监听地址、全局退出策略（`ask`/`keep`/`stop`）、TLS 证书/私钥路径等；未知字段一律忽略。对应环境变量（见下）优先级更高。

### 环境变量（daemon）
- `PECO_HOME` — 数据根（默认 `~/.peco-daemon`）
- `PECO_DAEMON_BIND_ADDR` — 监听地址（默认 `127.0.0.1:6971`，仅回环）
- `PECO_DAEMON_EXIT_POLICY` — 退出时空间默认策略（`ask`/`keep`/`stop`）
- `PECO_PROVIDERS_CONFIG` — 系统层 `providers.toml` 绝对路径
- `PECO_WEBUI_DIST` — 前端产物目录（设置后 daemon 同源托管 SPA）
- `PECO_LOG` — 日志过滤（`EnvFilter` 语法；默认 `peco_daemon=debug,warn`）
- `PECO_COMPACTION_*` / `PECO_RETRY_*` / `PECO_HISTORY_TOKEN_BUDGET` / `PECO_SUMMARIZER_MODEL` — 压缩与重试参数（见引擎章节）

### agent.md（Agent 定义）
```yaml
---
agent:
  name: "agent-name"
  description: "Agent 的描述信息"
llm:
  provider: "deepseek"
  model: "deepseek-flash"
  temperature: 0.3
tools: [shell, fetch, search_knowledge]
mcp: [helixdb-docs]
skills: [code-review]
knowledge_bases: [@project_docs]
max_iterations: 30
---
# 系统提示词
...
```

### workflow.md（Workflow 定义）
```yaml
---
workflow:
  name: "code-review-and-fix"
  description: "代码审查 → 自动修复 → 验证"
  version: "1.0"
  timeout_seconds: 600
steps:
  - id: "lint"
    name: "静态检查"
    type: shell
    config:
      command: "cargo clippy --workspace -- -D warnings 2>&1"
    on_failure: "continue"
  - id: "review"
    name: "AI 代码审查"
    type: agent
    config:
      agent: "@code-reviewer"
      prompt: "请审查代码改动"
    depends_on: ["lint"]
---
```

## 核心设计模式

1. **窄 trait 接口实现依赖注入**：`tools::deps` 定义 `AgentAccess`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`、`McpAccess`、`MemoryAuditAccess` 及聚合结构体 `ToolDependencies` — 工具只依赖这些 trait，`WorkSpace` 实现它们。

2. **Session 独立于 Looper**：Looper 将消息推入 `Session`，在轮次边界调用 `persister.save()`。Session 有自己的状态机，不知道 Looper 的存在。

3. **系统提示词是动态的**：每轮由 `DynamicContext` 从 agent 的 preamble + 活跃 skill body 重新组装 — 永不存储在 Session 历史中。

4. **MCP 工具自动发现**：`McpClientHandler` 连接时 `list_all_tools()`，`list_changed` 通知时重新同步，工具包装为 `McpTool`。

5. **daemon 的 `WorkspaceHub` 是唯一实例表**：进程级 `peco_core::tools::WorkspaceHub` 持有已打开 `WorkSpace`；`SpaceRegistry` 只做 ws_id → 目录映射与适配，不另存第二份缓存。`SpaceEnv` 打包打开/关闭空间所需的池、能力根与系统配置，避免各 handler 各说各话。

6. **错误处理**：`AgentError` 覆盖完整生命周期（IO、YAML 解析、缺失字段、环境变量、配置、工具执行、超过最大迭代次数、协议违规）；daemon 侧 `DaemonError` 统一映射为 `{code, message}` 响应体。

## 命名约定：turn vs iteration

两个尺度都叫过 `turn`，是歧义的根源。**永远不要用 `turn` 表示 ReAct 迭代**：

- **`turn` / `turn_index` / `turns` = 对话轮次** —— 用户一次输入到最终答复，持久化单元。`Session::turn_index()`、`committed_turns`、`TurnBoundaryToken`、`LooperEvent::TurnComplete { turn_index }`、`ContextStrategy::SlidingWindow { max_turns }`（最近 N 个**对话轮**）。
- **`iteration` / `react_loop_iteration` / `max_iterations` = ReAct 循环迭代** —— 单个轮次内模型被调用的次数。`AgentProfile.max_iterations`、`AgentError::MaxIterations`、`TurnFailureReason::MaxIterationsExceeded`、`reset_iteration_counters()`。

对齐 LangChain `AgentExecutor::max_iterations` / Semantic Kernel `max_iterations`；OpenAI Agents SDK 的 `max_turns` 是少数派用词，不跟。

日志字段：对话轮次写 `turn_index`，迭代次数写 `iteration`，预算值写 `max_iterations` —— 三者同时出现时应一眼可分。

## 日志约定

- **message 一律用简约英文**：`"SSE connect failed; scheduling retry"`、`"dropping incomplete tool call: missing id"`。日志是要被 grep 的运维数据，中英混排无法整体扫描。短句、分号分隔从句、不加句号。
- **注释仍用中文**，错误文案（`ProviderError` / `assert!` / `panic!` 的消息）也不改 —— 本规则只覆盖日志宏的 message 字面量。
- **宏用短名**：文件顶部 `use tracing::{info, warn};`，调用处写 `info!` / `warn!`，不写 `tracing::info!` 全路径。
- **打类型不打正文**：日志字段不落用户内容（对话原文、块正文），只记计数、类型、长度、id。
- **默认级别**：daemon 启动时 `EnvFilter::try_from_env("PECO_LOG")`，缺省 `EnvFilter::new("peco_daemon=debug,warn")`（见 [main.rs](crates/peco-daemon/src/main.rs)）。daemon 内的自有库日志走各自 crate 路径（`peco_core` / `model_provider` / `knowledge_base` / `peco_daemon`），**新增自有 crate 时确认它在 `PECO_LOG` 的 directive 覆盖范围内**，否则会被默认 `warn` 吞掉或整体不可见。

## Rust Edition 与工具链

- Rust edition **2024**（workspace `Cargo.toml`）
- 需要 Rust 1.85+
- Workspace resolver v3
- `default-members = ["crates/peco-daemon"]`
- `unused_crate_dependencies = "warn"`（workspace 级别）
