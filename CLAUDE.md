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
cargo test -p peco-server
cargo test -p knowledge-base

# 运行单个测试
cargo test -p peco-core <test_name>
cargo test -p peco-core -- --nocapture          # 显示测试输出

# 格式化 Rust 代码（提交前必须通过）
cargo fmt --all

# Lint（CI 中 warning 视为 error — unused_crate_dependencies 默认为 warn）
cargo clippy --workspace -- -D warnings

# CLI 模式 — 交互式运行 Agent
cargo run -p peco-cli -- --agent <agent-name>

# 开发模式 — 同时启动后端 (9227) + 前端 (9233)
bash scripts/dev.sh

# 前端
cd webui && npm install
cd webui && npx vitest run                     # 运行前端测试
cd webui && npx tsc --noEmit                   # TypeScript 类型检查
cd webui && npx prettier --write src/          # 格式化前端代码
```

## 架构总览

Peco 是一个全栈 AI Agent 平台：**Rust 后端**（Axum + Tokio）+ **React 19 前端**（TypeScript, Vite, Zustand, shadcn/ui）。

### Crate 依赖图（自上而下）

```
peco-server (Axum Web 服务, REST/SSE, JWT 认证, Cron 调度器, Peco 记忆管理)
  ├── peco      (Peco 永续对话 — /api/peco)
  ├── chat      (Agent 对话管理 — /api/chat)
  ├── provider  (Provider 配置管理 — /api/providers)
  ├── skill     (Skill 管理 — /api/skills)
  ├── mcp_config(MCP 配置管理 — /api/mcp)
  ├── usage     (Token 用量统计 — /api/usage)
  ├── peco-core (Agent 引擎: Agent, Session, ReAct 循环, Workflow, WorkSpace, MCP, Skills, Tools)
  │     ├── model-provider (LLM 抽象层: ModelProvider trait, DeepSeek 实现)
  │     ├── knowledge-base (RAG: LanceDB + FastEmbed + BM25 + 知识图谱)
  │     └── peco-derive (#[peco_tool] 过程宏)
  ├── peco-cli (终端对话 — 独立使用 peco-core)
  └── peco-agents (编译时嵌入的 Workspace 模板 — 无 peco-core 依赖)
```

**核心原则**：`peco-core` 是引擎 — 不感知 HTTP、数据库连接或 Web。`peco-server` 通过 `AppState` 和 `WorkspaceManager` 将其接入 Web。`peco-cli` 是围绕 `peco-core` 的轻量 TUI 外壳。`peco-agents` 提供编译时嵌入的模板数据，不依赖 `peco-core`。`Workflow` 模块遵循相同的 DI 模式 — 引擎通过 `WorkflowEngine::spawn()` 在 tokio 任务中运行，事件通过 mpsc channel 流出，不绑定特定传输层。

### peco-agents：Workspace 模板

- `BuiltinTemplate` 结构体：编译时通过 `include_bytes!` 嵌入的模板文件集合。
- 三套内置模板：`personal`（个人助手 + 记忆管理）、`minimal`（最轻量对话）、`developer`（编码助手 + 项目记忆）。
- `materialize()` 将模板解压到临时目录。
- `WorkSpace::init_from_template()` 执行幂等安装：已存在的 Agent 和 KB 不会被覆盖，错误收集到 `TemplateInitReport` 中。
- CLI 入口：`cargo run -p peco-cli -- --init-template personal` 或 `-t personal`。

### peco-core：Agent 引擎

**Agent**（[crates/peco-core/src/agent/](crates/peco-core/src/agent/)）：
- 由 `agent.md` 文件定义：YAML frontmatter（模型、工具、MCP 服务器、Skills、max_iterations）+ Markdown 正文（系统提示词）。
- `Agent::from_file(path)` 解析文件，从 `providers.toml` 解析 provider 配置，创建 `ModelProvider`（目前始终为 DeepSeek），并注册工具 + MCP 工具。
- `MessageFilter` trait：上下文组装后的钩子，可在消息列表发送给 LLM 之前对其进行转换（如脱敏、注入）。

**AgentLooper**（[crates/peco-core/src/agent/agent_looper.rs](crates/peco-core/src/agent/agent_looper.rs)）：
- 双层状态机驱动 ReAct 循环：
  - **外层**：`Idle ↔ Paused` / `RunningInnerLoop`
  - **内层**：`PreparingRequest → [batch] AwaitingModel → ResolvingResponse` 或 `[stream] Streaming → ExecutingTools →（循环回）→ Done / Failed`
- **主循环四步**：`run()` 每轮只做四件事 —— ① 取消息（`has_pending_work()` 为真时只非阻塞排空，否则阻塞 `recv()` 等一条再补排空同批）→ ② 收尾（`is_cancelled()` 走 `finalize_cancel`，否则 `react_state == Failed` 走 `finalize_failure`，互斥）→ ③ 启动 pending 轮（**唯一的「pending → turn」入口**）→ ④ 推进一步 `react_step()`（仅 `RunningInnerLoop`）。
- **不空转不变量**：每一轮迭代要么在 ① 阻塞、要么严格推进一个状态。`has_pending_work` 的四个非阻塞触发点（`RunningInnerLoop` / `cancel_flag` / `Failed` / `armed && has_pending`）各自有明确消解者，**新增触发点必须同时给出消解者**，否则热自旋。
- **取消 = 中止当前 ReAct 轮，looper 不退出**（`Cancel` 只置 `cancel_flag`，收尾在 ② 完成）：`finalize_cancel` 按内层状态回收在途资源（`Streaming` 丢 `active_stream`、`ExecutingTools` 走 `abort_inflight_tools`）→ 冻结 + **强制落盘**（`force_persist`，取消不丢 pending）→ 归位到干净 Idle。`cancel_flag` 是**一次性**的，收尾后必须复位。`Cancel` / `Shutdown` 都返回 `ReadDirective::StopReading`（丢弃同批后续消息），但只有 `Shutdown` 置 `shutdown_requested` 退出循环。
- **`pending_armed`** 区分「有待处理输入」与「这些输入现在该不该跑」：`Resume` / 新 `Query` 置位，`Cancel` 收尾与启动轮后清零。取消后不自动开新轮，pending 保留并随快照落盘。
- **步进上界 `STEP_POLL`（200ms）**：流式等 chunk、工具 poll、退避切片三处共用的唯一数字，同时是**取消的最坏生效延迟**。**batch 路径（`stream: false`）没有步进边界** —— 该轮取消延迟 = 整段生成时间。looper 无整轮看门狗，唯一的中止入口是取消。
- 工具执行分两阶段：**spawn 阶段**（将所有工具调用启动到 `JoinSet` 中），然后 **poll 阶段**（以 `STEP_POLL` 超时贪婪排空结果，每完成一个即发出事件）。
- **pending → 一条消息**：pending 不是消息，全项目只有 `Session::dequeue_and_start_turn` 一处把它变成消息（`merge_contents` 合并，产出恰好一条 user message）。合并来源**按批大小定**：单条走 `MessageSource::UserInput`（逐字节未改），多条才走 `MergedPending` —— 展示层只对后者调 `strip_merge_markers`，标错会让用户自己写的一行 `---` 在渲染时消失。
- **通道关闭**：无在途轮直接退出；有在途轮则继续步进把它跑完、回到 Idle 时才退。暂停中通道关闭走**取消语义**（没人能再解除暂停了）。
- ⚠ 已知偏差：web 前端 `abortStream()` 发出取消后立即关闭 SSE 连接，导致 run 被 runner 回收、「取消后不退出 looper」在 web 主路径未兑现（只对 CLI / 长连接有效）。
- **流式路径**：使用 `StreamAssembler` 将 `StreamEvent` 块中的增量文本/推理/工具调用增量累积为完整的 assistant 消息。
- 动态上下文组装：系统提示词每轮重新注入，工具结果追加其后。`DynamicContext` trait 支持在每次新用户查询时注入 RAG 增强内容；同一轮的 ReAct 迭代复用缓存上下文。
- **上下文策略**：`FullHistory`（默认）、`SlidingWindow { max_turns }`、`TokenBudget { max_tokens, summarize_overflow }` 或 `Custom(Arc<dyn ContextFilter>)`。通过 `LooperConfig` 为每个 looper 选择。
- `LooperEvent` 枚举（19 个变体）通过异步 intercom 通道（`Speaker`/`Listener` 对）流动，覆盖文本增量、推理增量、工具调用生命周期、状态转换、轮次边界和关闭。
- **模型错误分类与统一重发**：模型故障统一经 `model_failure_reason`（按 `ProviderError::classify()` 映射）转成类型化 `TurnFailureReason`（`RateLimited`/`ModelUnavailable`/`AuthError`/`QuotaExhausted`/`ContextOverflow`/`ContentFiltered`，均携带 provider 原始 msg）。故障出口 `fail_or_retry` 收敛三处 Err 站点：**只 classify 一次**，瞬时类进重发，否则写入 `failure_reason`。
  - **单一重发入口 `begin_retry(RetryCause)`**：截断（`Truncated { output_tokens }`，触发条件 `Incomplete + MaxTokens`）与瞬时（`Transient { trigger }`，限流/网络/5xx，或流被掐断 `Incomplete + finish_reason=None`）共用一条路径 —— 守卫 `can_retry` → 回退 staging 到请求前锚点 → 计数 → 设定退避 deadline → 通知。两者**语义差异只是入参**：截断额外抬输出预算，其余（退避、通知、回退）完全一致。
  - **单计数单上限**：`retry_limit` 默认 3（旧「截断 1 + 瞬时 2」的合计天花板），`PECO_RETRY_LIMIT` 可调，`0` = 关闭。**截断不需要独立限额** —— `budget_raised` 粘性到轮末，抬到 `retry_output_budget` 后 headroom 判据恒假，第二次截断重试被 `can_retry` 拦死。
  - **`budget_raised` 必须是独立布尔**而非 `retries_used > 0`：瞬时重发递增同一计数但绝不能抬预算（抬了可能超出模型真实上限 → 网关 400，且违背「原样重发」语义）。随 `reset_turn_counters` 清零，粘性不出轮。
  - **抬升目标 = 抬升上限**：`retry_output_budget` 默认 32_768，同时是 headroom 判据与粘性覆盖的唯一参数（旧 `max(配置值, min_budget)` 形式已化简删除）。**模型真实输出上限低于该值时截断重试会被网关 400 拒** —— 调低该值到模型上限之内，或 `PECO_RETRY_LIMIT=0` 关闭重试。
  - **统一退避**：`base * 2^(n-1)` 截断到上限，`retry_base_delay_ms` 默认 500 / `retry_max_delay_ms` 默认 5000，截断重发同样走退避。退避在 `prepare_and_send_request` 入口以可取消切片 sleep 等待（只读 deadline 不 `take()`，见 `wait_retry_backoff`），不阻塞取消。
  - **残片只丢弃不归还**：被回退的截断文本不进历史（`rollback_attempt` 的 `discarded_text` 是发给前端/落库的**丢弃指令**，随 `TruncationRetry` 事件即发即弃）。但 `plan_failure` 保留了**轮次存活桩** —— 重试后失败且新尝试零产出时补一条 `[response discarded after retry]` 的 assistant 消息，否则 `interrupt_turn` 见 staging 为空会退化成 `rollback_turn`，用户提问整轮从历史消失。
  - 四个 env 的读取**下沉到 `LooperConfig::from_env()`**（`PECO_RETRY_LIMIT` / `PECO_RETRY_OUTPUT_BUDGET` / `PECO_RETRY_BASE_DELAY_MS` / `PECO_RETRY_MAX_DELAY_MS`），`PecoConfig`、`chat/handler.rs`、`peco-cli` 三个构造点统一委托 —— 旧版本里后两者无视 env 是既有 bug。
- `LooperHook` trait：8 个拦截点（`on_before_request`、`on_after_response`、`on_text_delta`、`on_before_tool`、`on_after_tool`、`on_turn_complete`、`on_react_state_change`、`on_outer_state_change`）。内置钩子：`ToolAllowlistHook`、`TokenBudgetHook`。

**SimpleAgentLooper**（[crates/peco-core/src/agent/simple_looper.rs](crates/peco-core/src/agent/simple_looper.rs)）：
- 最小化的纯 batch 变体，由 `DelegateSubAgent` 和 `RunParallelSubAgents` 使用。
- 无流式、无钩子、无事件、无会话持久化。仅：`用户消息 →（模型 → 工具）* → 最终文本`。

**Session**（[crates/peco-core/src/session/](crates/peco-core/src/session/)）：
- 状态机：`Idle → Active → Commit/Rollback/Cancel`。还有 `Cancelling` 和 `Interrupted` 中间状态。
- 双层消息缓冲区：`CommittedBuffer`（已持久化的轮次，`Vec<Vec<AnnotatedMessage>>`）+ `StagingBuffer`（当前轮次的在途消息）。
- `TurnBoundaryToken` — 一个零大小证明令牌，仅由 `commit_turn()` 和 `rollback_turn()` 返回。`snapshot()` 需要此令牌，提供编译期保证：快照仅在轮次边界发生，永不在轮次中间。
- `PendingInput` 队列处理活跃轮次期间的并发用户输入：轮结束时整个队列一次排空，经 `merge_contents()` 合并为一条 user 消息启动新轮（单条原样、多条每条前置 `---` 标记行、图片部件保留；纯文本与含图两分支产出的 wire 文本一致），交由大模型自行判断如何处理。合并消息标记 `MessageSource::MergedPending`，展示层（session_dto / markdown 导出）据此经 `strip_merge_markers()` 剥离 `---` 标记后再渲染，模型侧原样。
- 消息以 `Arc<Message>` 包裹在 `AnnotatedMessage` 中（含 id、turn_index、timestamp、source、estimated_tokens），实现零拷贝上下文构建。
- 持久化是外部的：`SessionPersister` trait（基于文件的 `FileSessionPersister` 或 `NullSessionPersister`）。Looper 在轮次边界调用 `persister.save()`。在 peco-server 中，Session 快照也通过 `SqliteSessionPersister` 持久化到 SQLite。

**Tools**（[crates/peco-core/src/tools/](crates/peco-core/src/tools/)）：
- 双 trait 设计：`Tool`（静态、泛型、类型化）和 `ToolDyn`（对象安全，`Pin<Box<dyn Future>>`）。
- blanket impl `impl<T: Tool> ToolDyn for T` 桥接二者。
- `ToolExecutor` trait：运行时接口 — `execute(name, args) -> Result<String, String>` + `definitions() -> Vec<ToolDefinition>`。
- **DI 契约**（`deps.rs`）：定义 6 个窄 trait — `AgentAccess`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`、`McpAccess`、`MemoryAuditAccess` — 以及聚合结构体 `ToolDependencies`。工具只依赖这些 trait，不直接依赖 `WorkSpace`。
- **工具组装**（`tool_register.rs`）：`ToolRegister::build()` 根据 tool_names 和 `ToolDependencies` 一次性构建包含所有工具的 `ToolExecutor`。权威工具名清单为 `BUILTIN_TOOL_NAMES` 常量（由防漂移测试保障与 match arms 一致）；可选依赖（`workflow_access`/`mcp_access`）缺失时对应工具 warn + skip，不 panic。
- 内置工具（31 个）：`shell`、`fetch`、`web_search`、`show_workspace`、`list_tools`、`read_skill`、`list_skills`、`save_skill`、`delete_skill`、`delegate_sub_agent`、`run_parallel_sub_agents`、`save_agent`、`read_agent`、`delete_agent`、`execute_workflow`、`list_workflows`、`save_workflow`、`delete_workflow`、`list_mcp_servers`、`save_mcp_server`、`delete_mcp_server`、`test_mcp_connection`、`search_knowledge`、`list_knowledge_bases`、`add_to_knowledge_base`、`sync_knowledge_base`、`get_knowledge_base_docs`、`add_facts_to_knowledge_base`、`query_entity_facts`、`delete_kb_document`、`delete_kb_documents`。
- KB 工具通过 `check_kb_access()` 执行 Agent 级别访问控制（基于 agent.md `knowledge_bases` 白名单）。
- `#[peco_tool]` 宏（来自 `peco-derive`）：标注一个 async fn，生成实现 `Tool` 的零大小结构体、带有 `#[derive(Deserialize, JsonSchema)]` 的类型化 `Parameters` 结构体，以及 `static TOOL_NAME` 常量。
- `DefaultToolsExecutor` 是标准实现：持有 `HashMap<String, Box<dyn ToolDyn>>` 并按名称分发。

**WorkSpace**（[crates/peco-core/src/workspace/](crates/peco-core/src/workspace/)）：
- 按用户隔离的边界。每个 `WorkSpace` 持有 `Config`（用户级别）、`SkillRegister`、`KnowledgeManager`、`AgentManager`、`WorkflowManager`。实现 `tools` 模块中定义的 DI trait（`AgentLoader`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`），通过 `build_tool_executor()` 委托给 `ToolRegister::build()` 完成工具组装。

**Workflow**（[crates/peco-core/src/workflow/](crates/peco-core/src/workflow/)）：
- 声明式 DAG 工作流编排引擎。与 Agent 对话驱动的 ReAct 循环互补 — Workflow 提供确定性的步骤编排。
- **定义格式**：`workflow.md`（YAML frontmatter + 可选 Markdown body），与 `agent.md`/`SKILL.md` 风格一致。
- **引擎模型**：`WorkflowEngine::spawn()` 在 tokio 任务中运行，通过 `tokio::sync::mpsc` 通道发射 `WorkflowEvent`（Started → StepStarted → StepCompleted/StepFailed/StepSkipped → Completed/Failed/Cancelled）。外部通过 `WorkflowHandle` 消费事件、发送审批决策、取消或等待完成。
- **DAG 拓扑执行**：Kahn 算法拓扑排序 + BFS 分层，层级间串行，层级内步骤通过 `tokio::spawn` 并行执行。
- **步骤类型**：`shell`（`tokio::process::Command`）、`agent`（复用 `SimpleAgentLooper`，Agent 自带 `agent.md` 中定义的工具）；`llm`（纯推理）、`tool`（调用 `ToolExecutor`）已定义类型但尚未实现。
- **模板变量**：基于 minijinja，支持 `{{ steps.X.output }}`、`{{ inputs.xxx }}`、`{% if %}` 条件、`truncate`/`length`/`replace` 过滤器。
- **失败策略**：`Continue`（记录失败继续）| `Abort`（默认，中止并取消同级未完成步骤）| `Pause`（暂停等待审批，通过独立 mpsc channel）| `Retry`（已定义，尚未实现）。
- **条件门控**：`condition` 字段通过 minijinja 求值控制步骤是否执行，正交于 `depends_on` 拓扑依赖。
- **持久化**：`WorkflowPersister` trait（与 `SessionPersister` 同模式）。引擎在 Pause、每层完成、Completed/Failed 时自动保存快照。CLI/测试使用 `NullWorkflowPersister`，peco-server 提供 `SqliteWorkflowPersister`。
- **工具集成**：`execute_workflow` 是一个 `ToolDyn` 工具，Agent 可在 ReAct 循环中调用（同步阻塞语义，适合短 workflow）。`OutputSchema` 功能通过在 prompt 中追加 JSON schema 指令实现，尚未做真正的结构化输出。
- **DI 契约**：`WorkflowAccess` trait（窄接口，load/list/reload）由 `WorkSpace` 实现，注入 `ToolDependencies`。

**MCP**（[crates/peco-core/src/mcp/](crates/peco-core/src/mcp/)）：
- `McpManager`：每个 Agent 的 MCP 连接编排器。接收已解析的 `(name, McpServerConfig)` 对，创建传输层（通过 `rmcp` 的 Stdio / SSE / StreamableHTTP），连接、发现工具，并将其注册为 `McpTool` 包装器。
- `McpClientHandler`：实现 `rmcp::ClientHandler`，自动同步工具列表变更（list_changed → 移除所有受管工具 → 重新列出 → 重新注册）。
- MCP 配置存储在 `~/.peco/mcp_config.json`（或 `$PECO_CONFIG_DIR/mcp_config.json`），通过 `McpConfig::load()` 加载。

**Peco 永续会话：滚动压缩 + 记忆双路径** ：

- **滚动压缩**（[crates/peco-core/src/agent/compaction.rs](crates/peco-core/src/agent/compaction.rs)）：`CompactionPolicy::maybe_compact()` 在 turn 边界（looper Done 分支）估算 pinned + committed token，超过 `compaction_trigger_tokens` 时用 Flash 模型递归合并旧摘要与被驱逐轮次为结构化摘要（四段固定模板），`Session::compact()` 物理驱逐最旧轮次并重编号 turn_index，摘要作为 `pinned_summary`（System 消息）钉在上下文最前。失败非致命，仅记日志。
- **单一截断点**：全部裁剪决策在 `PecoContextFilter`（[crates/peco-server/src/peco/filter.rs](crates/peco-server/src/peco/filter.rs)）一处完成 — pinned 层（System/摘要）→ Verbatim 层（按 token 预算从最新往回整轮选择）→ 当前轮（完整保留）。`ContextStrategy` 保持 `FullHistory` 直通。
- **token 估算**：`estimate_str_tokens()`（[crates/peco-core/src/agent/context.rs](crates/peco-core/src/agent/context.rs)）CJK 0.6 token/字、其他 0.3 token/char 的校准估算，全项目唯一实现。
- **记忆双路径**（[crates/peco-server/src/peco/memory/](crates/peco-server/src/peco/memory/)）：存储载体是 personal 模板幂等安装的 per-user `@private_memory` KB。
  - **写路径** `MemoryExtractionHook`（LooperHook）：每轮成功完成后守卫检查（失败轮跳过、`analyze_min_chars` 过滤），`tokio::spawn` 后台检索既有记忆抑制重复 → Flash 模型（`ModelTurnAnalyzer`，严格 JSON 输出）提取事实 → 逐条写入 KB（source 标签 `ppa_{profile|semantic|episodic}`）。turn 边界零阻塞，所有失败点 warn 后 return。
  - **读路径** `MemoryRecallContext`（DynamicContext）：每次新用户 query 前，闲聊门控（问候/感谢关键词，零成本跳过；不按长度门控）→ 混合检索 → 按类别格式化注入 instructions 尾部，`injection_token_cap` 逐行截断。
  - **分工**：compaction 解决"会话内上下文放不下"，记忆解决"跨会话/超长期的知识"，二者正交。记忆的更新/删除由 `@assistant → @memory` 子 Agent 的显式 KB 工具路径负责（自动路径只做 add）：删除走 `delete_kb_document(s)`（outbox 审计：删除前写 pending、成功 done、失败 cancelled，偏好类 `ppa_profile` 硬拒绝），审计落 SQLite `memory_audit` 表（含完整原文，不在任何检索面），`POST /api/peco/memory/audit/:id/restore` 按审计行重放回滚；审计注入在 `WorkspaceManager::open_workspace` 统一完成，审计存储缺失时删除工具运行时拒绝（fail-closed）。召回统计 / 整理水位表（`memory_recall_stats` / `memory_consolidation_state`）已建，供后续巩固流水线使用。

**Skills**（[crates/peco-core/src/skills/](crates/peco-core/src/skills/)）：
- 三级渐进式加载：Tier 1（启动时加载名称+描述）、Tier 2（激活时加载完整正文）、Tier 3（按需加载 scripts/references/assets）。
- `SKILL.md` 格式：YAML frontmatter（`name`、`description`、`allowed-tools`）+ Markdown 正文。
- 目录结构：`<skill-name>/SKILL.md`，可选 `scripts/`、`references/`、`assets/`。

**Persistence**（[crates/peco-core/src/persistence/](crates/peco-core/src/persistence/)）：
- `SessionPersister` trait，含 `save(SessionSnapshot)` 和 `load(session_id) -> SessionSnapshot`。
- `FileSessionPersister`：将 JSON 序列化的快照写入 `{data_dir}/sessions/{session_id}.json`。
- **重要**：持久化格式使用 `serde` 反序列化和 `Session::from_snapshot()` 重建。向 `AnnotatedMessage` 或 session 类型添加字段时，JSON 格式必须保持向后兼容或显式迁移。

### peco-server：Web 层

**启动流程**（参见 [main.rs](crates/peco-server/src/main.rs)）：
1. 初始化 tracing，加载 `.env`
2. 从环境变量加载初步 `ServerConfig`
3. 创建 SQLite 连接池，运行迁移
4. 重新加载完整配置（JWT 密钥：环境变量 → DB → 随机生成+持久化）
5. 创建 `CronScheduler`，从 DB 加载已启用的任务
6. 创建 `AppState`，构建路由，绑定并启动服务（优雅关闭）

**AppState**（[crates/peco-server/src/state.rs](crates/peco-server/src/state.rs)）：
- 持有：`SqlitePool`、`jwt_secret`、`data_dir`、`WorkspaceManager`（LRU 缓存，容量 128）、`CronScheduler`。

**Router**（[crates/peco-server/src/lib.rs](crates/peco-server/src/lib.rs)）：
- 公开路由：`/api/auth/*`（登录、注册）
- 受保护路由（JWT + 可选限流）：`/api/agents/*`、`/api/conversations/*`、`/api/knowledge/*`、`/api/tasks/*`
- Swagger UI 位于 `/docs`

**Auth**（[crates/peco-server/src/auth/](crates/peco-server/src/auth/)）：
- JWT（HS256），7 天有效期。`AuthUser` 提取器在每个受保护请求上验证 Bearer token。
- JWT 密钥三级解析：`PECO_JWT_SECRET` 环境变量 → DB `server_config` 表 → 随机 UUID（首次启动时生成并持久化到 DB）。
- 密码哈希使用 bcrypt（cost factor 12），通过 `spawn_blocking` 执行。

**Rate Limiting**（[crates/peco-server/src/middleware/rate_limit.rs](crates/peco-server/src/middleware/rate_limit.rs)）：
- GCRA 算法（通过 `governor` crate），按 JWT `sub` 声明分 key。
- 默认：20 req/s，burst 100。SSE 端点有单独的更严格限制（1 req/s，burst 3）。

**Chat/SSE**（[crates/peco-server/src/chat/](crates/peco-server/src/chat/)）：
- `GET /api/conversations/:id/stream` — SSE 端点。创建 `AgentLooper`，在 tokio 任务中启动它，并将 `LooperEvent` 桥接到 SSE 事件流。
- SSE 事件类型：`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_result`、`turn_complete`、`agent_call_start`、`agent_call_end`、`context_compacted`、`truncation_retry`、`usage`、`error`、`done`。（部分内部 `LooperEvent` 变体如 `ToolCallDelta`、`ModelUsage`、`ReactStateChange` 被过滤掉，不发送给客户端；`TruncationRetry` 的 `discarded_text` 字段同样不下发 —— 它只服务 `chat` 模块落库累加器的后缀剥离。）
- `truncation_retry` 是**纯通知**，不含撤销语义：重发回退的是 `Session`（staging）而非传输层，前端不删除已收到的增量，只在当前轮气泡**之前**插一条居中横幅解释残句；重载从快照恢复后残句与横幅一并消失。CLI 同路线（打一行提示，保留残句）。`reason` 字段区分两种重发：`truncated`（截断抬预算）与 `transient`（限流/网络/5xx 退避重发），缺省按截断展示以兼容旧后端。
- 失败轮的 `error` 事件文案由 `format_failure_message` 生成：类型化失败原因（限流/网络/鉴权/额度/上下文/过滤）输出「中文分类前缀 + provider 原始 msg」，分类给结论、原文保留诊断。
- 子 Agent 调用（`delegate_sub_agent` / `run_parallel_sub_agents`）通过按 tool_call_id 映射的 `SubAgentInfo` 注册表追踪，生成 `agent_call_start`/`agent_call_end` SSE 事件供前端可视化。
- `GET /api/conversations/:id/session` — 返回完整 `SessionSnapshot`，包含工具调用和推理内容。

**数据架构 — 双存储**：
- **SQLite**（通过 `sqlx`）：用户、对话、消息、Agent 索引、知识库元数据、任务定义、任务日志、Session 快照。`db/` 模块有每个表的 DAO 文件。
- **磁盘上的 agent.md 文件**：Agent 定义的**唯一真相源**。DB 仅存储轻量索引（name、path、user_id）。Agent 始终从 `.md` 文件通过 `WorkspaceManager::get_agent()` 加载。

**Peco 模块**（[crates/peco-server/src/peco/](crates/peco-server/src/peco/)）：
- `PecoManager`（[manager.rs](crates/peco-server/src/peco/manager.rs)）：每次流连接构造。确保 personal 模板幂等安装、加载 `@assistant` Agent，并组装 `PecoConfig` — 滚动压缩策略、环境上下文前缀、记忆双路径（hook + dynamic_context，`memory.enabled` 时）。
- `PecoConfig`（[config.rs](crates/peco-server/src/peco/config.rs)）：预算/压缩/记忆参数 + 管理器构造期填充的可选组件（compaction / environment / dynamic_context / hooks）。handler 无需改动即可增减注入组件。
- `PecoContextFilter`（[filter.rs](crates/peco-server/src/peco/filter.rs)）：单一截断点上下文组装器（见上文"滚动压缩"）。
- SSE 事件含 `context_compacted`（归档提示），前端以居中分隔条展示。

### model-provider：LLM 抽象层

- `ModelProvider` trait（async_trait）：`name()`、`generate_full(&GenerateRequest) -> GenerateResult`、`generate_stream(&GenerateRequest) -> GenerateStream`。
- `GenerateRequest`：model、instructions、input（`InputItem` 列表）、tools（作为 `ToolDefinition`）、tool_choice、temperature、top_p、max_output_tokens、reasoning（`ReasoningConfig`）、text（`TextFormat`）、additional_params。
- `StreamChunk` 枚举：`BlockStart`、`TextDelta`、`ReasoningDelta`、`ToolCallDelta`、`BlockEnd`、`Usage`、`Finish`。
- 目前实现了三个 provider：`DeepSeek`（`DEEPSEEK_API_KEY`，chat + Responses 双路径，`api` 默认 `"responses"`）、`Qwen`（`DASHSCOPE_API_KEY`，chat + Responses 双路径，`api` 默认 `"responses"`）和 `OpenAI`（`OPENAI_API_KEY`，chat + Responses 双路径，`api` 默认 `"chat"`；OpenAI 兼容网关经 `base_url` 覆盖接入）。Provider 类型定义支持 `anthropic`、`ollama`、`groq`（待实现）。
- **Responses 适配器**：`DeepSeekResponsesAdapter`（原生 `/responses`，端点剥离 `/v1`）与 `QwenResponsesAdapter`（百炼 OpenAI 兼容 `/responses`，端点**保留** `/v1`；reasoning 输出为 `summary` 摘要形态并按原形态回传；`function_call_output` 必须紧跟对应 `function_call` 的逐对排布；`store` 显式置 `false`）。`OpenAiResponsesAdapter`（OpenAI 原生 `/responses`，端点保留 `/v1`；`store` 显式置 `false`、顶层 `instructions` 直传、`Role::Developer` 原生直传、reasoning `{effort, summary:"auto"}` 与 chat 同名档位、历史 `Reasoning` 项不回传、非流式块序归一为 Reasoning→Text→ToolCall）。请求/响应经中立词汇表（`GenerateRequest`/`GenerateResult`/`StreamChunk`）直通映射，差异细节见 `docs/design/qwen-responses-design.md` 与 `docs/research/qwen-responses-research.md`。
- Provider 配置位于 `providers.toml`（相对于 agent.md 文件或从标准位置解析）。

**错误分类**（[crates/model-provider/src/error.rs](crates/model-provider/src/error.rs)）：
- `ProviderError::classify() -> ClassifiedError { kind, message, code }` — 惰性语义分类，12 处 `Api { status, body }` 构造点零改动。`ApiErrorKind`：`RateLimited`/`Network`/`Server`（瞬时，`is_transient()` 为 true）| `Auth`/`NotFound`/`ContextOverflow`/`QuotaExhausted`/`ContentFiltered`/`InvalidRequest`/`Unknown`（永久）。
- `classify_api_error(status, body)` 判定顺序**先 body 后 status** — 流内错误 payload 用伪造 status 500，真实语义只在 body 里：① 解析 OpenAI 兼容 `error.{code,type,message}` 关键词匹配；② status 兜底；③ 非 JSON body 文本关键词兜底。`ClassifiedError.message` 携带原始 body 摘要（截断 500 字符），分类不吞原文。
- `FinishReason::ContentFilter` 独立于 `Error` — 内容过滤与一般上游异常在 looper 侧必须分得开（前者类型化报错且明确不重试）。

**SSE 流式管道**（[crates/model-provider/src/streaming/pipeline.rs](crates/model-provider/src/streaming/pipeline.rs)）：
- 与 provider 无关的设计：`process_normalized_sse_stream_chunks()` 是一个共享状态机，消费原始 SSE 数据帧，经 `StreamingProfile` 将 provider 特定的块规范化为 `NormalizedChunk` 并生成 `StreamChunk`。
- 添加新 provider（如 groq）只需实现 `StreamingProfile` 和 `ModelProvider` — SSE 解析、重连和工具调用累积逻辑可复用。
- `StreamingEventSource<R>`（[sse.rs](crates/model-provider/src/streaming/sse.rs)）：一个 5 状态 SSE 流（`Connecting → Open → WaitingToRetry → Reconnecting → Closed`），带有 `Last-Event-Id` 追踪和可插拔的 `RetryPolicy`（默认指数退避：起始 300ms，2x 倍数，5s 上限，**最多 5 次**）。所有内置策略做**分类门控**（`error.is_transient()` 为 false 立即放弃）；非 200 校验失败（`ValidatingResponse`）同样咨询策略 — 429/5xx 退避重试、401/400 立即失败。**established-stream 中断不透明重连**（LLM SSE 不支持 `Last-Event-Id` 恢复，重连 = 全新生成 → 增量重复），直接上抛给 looper 做带回退的整轮重发。
- DeepSeek 思考/推理：`ChatRequest.reasoning_effort` 映射到 DeepSeek 的 `thinking` 字段（`"disabled"` / `{"type": "enabled", "effort": "<value>"}`）。未设置时默认：`{"type": "enabled", "effort": "high"}`。

### knowledge-base：RAG 引擎

- **摄入管道**（6 步）：解析（PDF/DOCX/HTML/MD/代码/纯文本）→ 分块（滑动窗口，句子边界对齐，默认 800 字符窗口，200 字符重叠）→ 批量嵌入（FastEmbed ONNX，默认 `bge-base-zh-v1.5`，768 维）→ 存储文档 → upsert 向量 → 全文索引 + 构建结构图谱边（`CONTAINS`、`NEXT_CHUNK`）。
- **Chunk ID 是确定性的**：`{doc_id}-{seq:04}-{sha256[0..8]}` — 幂等摄入。
- **基于 trait 的后端抽象**：5 个 trait（`DocumentStore`、`VectorIndex`、`FullTextIndex`、`GraphStore`、`CombinedSearch`）。三种后端：`InMemoryBackend`（测试用，暴力余弦 + CJK 感知分词器）、`LanceDbBackend`（生产用，基于 Arrow）、`HelixDbBackend`（feature-gated，HTTP 客户端，带 `CombinedSearch` 快速路径实现单次往返多路搜索）。
- **自适应 4 层检索**（`QueryAnalyzer` → `PathCalibration` → `CrossValidation` → `AdaptiveFusion`）：分类查询意图（FactLookup/Conceptual/Relational/Exploratory/ShortKeyword），校准每条路径的分数分布，跨路径交叉验证（StrongAgreement/WeakAgreement/SinglePath），并相应调整 RRF 融合权重和置信度。当 `QueryAnalyzer` 不存在时优雅降级。
- `KnowledgeBaseManager`：管理多个知识库，每个知识库有自己的 `KbConfig`（后端类型、分块策略、嵌入模型）。支持并发跨知识库 `search_all()`。
- **配置存储**：每个 KB 目录内自包含 `kb_config.json` 文件。`load()` 扫描 `knowledge/*/kb_config.json` 子目录发现 KB。旧的中心化 `kb_configs.json` 格式在 `load()` 时自动迁移并重命名为 `.bak`。
- **双重命名**：`KbConfig.name` 是对外名称（API、agent.md `knowledge_bases`），目录名是对内的 sanitize 形式（`sanitize_kb_name()` — 去除非 ASCII 字符）。HashMap key 始终使用 `config.name`，读写路径必须一致。
- **Agent 级别 KB 访问控制**：Agent profile 的 `knowledge_bases` 字段为每个 Agent 声明可访问的 KB 白名单。空列表 = 无权访问任何 KB。`ToolDependencies.allowed_kbs` 将白名单注入所有 KB 工具，通过 `check_kb_access()` 守卫执行。

### webui：React 前端

- **页面**：`chat/`、`agents/`、`knowledge/`、`tasks/`、`auth/`、`settings/`。
- **Stores**（Zustand）：`authStore.ts`（持久化到 localStorage，JWT + 用户信息）、`sidebarStore.ts`（仅在内存中，折叠状态）。
- **API 层**（[webui/src/api/](webui/src/api/)）：`client.ts`（axios 实例，带 JWT 拦截器 — 附加 Bearer token，处理 401 自动登出和 429 限流提示）、`stream.ts`（SSE 解析器使用 eventsource-stream），以及领域特定模块（`agents.ts`、`conversations.ts`、`knowledge.ts`、`tasks.ts`）。
- **SSE 流式**：使用原生 `fetch()` + `ReadableStream` reader（而非 EventSource）实现实时 token 流式传输，支持 `AbortController`。解析 12 种 SSE 事件类型（`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_result`、`turn_complete`、`agent_call_start`、`agent_call_end`、`context_compacted`、`truncation_retry`、`usage`、`done`、`error`），并响应式更新消息状态 — 追加文本增量、在可折叠的 `<details>` 中显示推理、将工具调用渲染为卡片、以嵌套消息气泡追踪子 Agent 委托。
- `reduceStreamEvent`（[ChatView.tsx](webui/src/components/chat/ChatView.tsx)）是 ChatView 与 peco store 共用的唯一 reducer。四个 delta 分支都以「末条 assistant」为目标，因此 `truncation_retry` 的横幅插在末条**之前**而非追加到末尾 —— 追加会让重试的增量全落进横幅里。
- [ChatDetailPage](webui/src/pages/chat/ChatDetailPage.tsx) 是最复杂的页面：挂载时加载 Session 快照，管理 SSE 流生命周期，处理工具调用和子 Agent 可视化。
- **组件**：shadcn/ui（Radix 原语）+ Tailwind CSS v4。表单使用 `react-hook-form` + `zod` 验证。Markdown 渲染通过 `react-markdown` + `remark-gfm` + `rehype-highlight`。
- **路由**：`react-router-dom` v7，带 `ProtectedRoute` 包装器，检查 JWT token 并在挂载时自动获取用户信息。

## 配置文件

### providers.toml（LLM provider 配置）
解析路径：agent.md 所在目录 → `~/.peco/providers.toml` → `$PECO_CONFIG_DIR/providers.toml`。
```toml
default_provider = "deepseek"
[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"
# api = "responses"（默认）| "chat" — 选择 Responses 端点或 chat completions
[providers.deepseek.default]
model = "deepseek-v4-flash"
temperature = 0.7
max_tokens = 4096
stream = true

[providers.qwen]
type = "qwen"
api_key = "${DASHSCOPE_API_KEY}"
# api = "responses"（默认）| "chat" — responses 走百炼 OpenAI 兼容 /responses 端点
[providers.qwen.default]
model = "qwen3.7-max"
temperature = 0.7
max_tokens = 4096
stream = true

[providers.openai]
type = "openai"
api_key = "${OPENAI_API_KEY}"
base_url = "https://api.openai.com/v1"   # 可省略；覆盖为兼容网关（vLLM/OpenRouter）地址
# api = "chat"（默认）| "responses" — responses 走 OpenAI 原生 /responses 端点
[providers.openai.default]
model = "gpt-5.2"
temperature = 0.7
max_tokens = 4096
stream = true

# 可选 — 内置 web_search 工具（未配置时该工具不注册）
[web_search]
provider = "searxng"   # "searxng" | "tavily" | "brave"

[web_search.searxng]
base_url = "http://localhost:8888"   # 实例需启用 JSON 输出格式
```

### agent.md（Agent 定义）
```yaml
---
agent:
  name: "agent-name"
  description: "Agent 的描述信息"
llm:
  provider: "deepseek"
  model: "deepseek-v4-pro"
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
    output_schema:
      type: object
      properties:
        issues:
          type: array

  - id: "auto-fix"
    name: "自动修复"
    type: agent
    config:
      agent: "@developer"
      prompt: "根据审查结果修复：{{ steps.review.output }}"
    depends_on: ["review"]
    condition: "{{ steps.review.success }}"
---
```

## 核心设计模式

1. **窄 trait 接口实现依赖注入**：`tools::deps` 定义 `AgentAccess`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`、`McpAccess` 五个窄 trait 及聚合结构体 `ToolDependencies` — 工具只依赖这些 trait，`WorkSpace` 实现它们，解耦工具与 workspace 的直接耦合。

2. **Session 独立于 Looper**：Looper 将消息推入 `Session`，并在轮次边界调用 `persister.save()`。Session 有自己的状态机，不知道 Looper 的存在。

3. **系统提示词是动态的**：每轮由 `DynamicContext` 从 agent 的 preamble + 活跃 skill body 重新组装 — 永不存储在 Session 历史中。

4. **MCP 工具自动发现**：`McpClientHandler` 在连接时调用 `list_all_tools()`，并在 `list_changed` 通知时重新同步。MCP 工具包装为 `McpTool`（实现 `ToolDyn`）。

5. **peco-server 中的 WorkspaceManager 是桥梁**：持有按用户 ID 索引的 `WorkSpace` 实例 LRU 缓存（128 条目）。每个 workspace 持有 `SkillRegister`、`KnowledgeManager`、`AgentManager`，并通过 `tools::ToolRegister::build()` 按需为 Agent 组装 `ToolExecutor`。

6. **错误处理**：`AgentError` 覆盖完整生命周期（IO、YAML 解析、缺失字段、环境变量、配置、工具执行、超过最大迭代次数、协议违规）。`?` 运算符可在各处使用，因为它为常见错误类型实现了 `From`。

## 命名约定：turn vs iteration

两个尺度都叫过 `turn`，是歧义的根源。**永远不要用 `turn` 表示 ReAct 迭代**：

- **`turn` / `turn_index` / `turns` = 对话轮次** —— 用户一次输入到最终答复，持久化单元。`Session::turn_index()`、`committed_turns`、`TurnBoundaryToken`、`LooperEvent::TurnComplete { turn_index }`、`ContextStrategy::SlidingWindow { max_turns }`（保留最近 N 个**对话轮**）。
- **`iteration` / `react_loop_iteration` / `max_iterations` = ReAct 循环迭代** —— 单个对话轮次内模型被调用的次数。`AgentProfile.max_iterations`（agent.md YAML / REST DTO / workflow step 同名字段）、`AgentError::MaxIterations`、`TurnFailureReason::MaxIterationsExceeded`、`reset_iteration_counters()`。

对齐 LangChain `AgentExecutor::max_iterations` / Semantic Kernel `max_iterations`；OpenAI Agents SDK 的 `max_turns` 是少数派用词，不跟。

日志字段：承载对话轮次写 `turn_index`，承载迭代次数写 `iteration`，预算值写 `max_iterations` —— 三者同时出现时应一眼可分。

## 日志约定

- **message 一律用简约英文**：`"SSE connect failed; scheduling retry"`、`"dropping incomplete tool call: missing id"`。日志是要被 grep 的运维数据，中英混排无法整体扫描。短句、分号分隔从句、不加句号。
- **注释仍用中文**，错误文案（`ProviderError` / `assert!` / `panic!` 的消息）也不改 —— 本规则只覆盖日志宏的 message 字面量。
- **宏用短名**：文件顶部 `use tracing::{info, warn};`，调用处写 `info!` / `warn!`，不写 `tracing::info!` 全路径。
- **打类型不打正文**：日志字段不落用户内容（对话原文、块正文），只记计数、类型、长度、id。
- **默认级别 debug**：项目自有 crate 全量 `debug`，见 [logging.rs](crates/peco-server/src/logging.rs) 的 `DEFAULT_FILTER` 与 [dev.sh](scripts/dev.sh)。`model_provider` / `knowledge_base` 不以 `peco` 开头，EnvFilter 对无匹配 directive 的目标取 `LevelFilter::OFF` —— 新增自有 crate 时漏写 directive 等于把该模块日志整体丢掉。

## Rust Edition 与工具链

- Rust edition **2024**（在 workspace `Cargo.toml` 中设置）
- 需要 Rust 1.85+
- WorkSpace resolver v3
- `unused_crate_dependencies = "warn"`（workspace 级别）
