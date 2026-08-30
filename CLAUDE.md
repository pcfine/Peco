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
- 由 `agent.md` 文件定义：YAML frontmatter（模型、工具、MCP 服务器、Skills、max_turns）+ Markdown 正文（系统提示词）。
- `Agent::from_file(path)` 解析文件，从 `providers.toml` 解析 provider 配置，创建 `ModelProvider`（目前始终为 DeepSeek），并注册工具 + MCP 工具。
- `MessageFilter` trait：上下文组装后的钩子，可在消息列表发送给 LLM 之前对其进行转换（如脱敏、注入）。

**AgentLooper**（[crates/peco-core/src/agent/agent_looper.rs](crates/peco-core/src/agent/agent_looper.rs)）：
- 双层状态机驱动 ReAct 循环：
  - **外层**：`Idle → ProcessingUserInput → RunningInnerLoop → Paused`
  - **内层**：`PreparingRequest → [batch] AwaitingModel → ResolvingResponse` 或 `[stream] Streaming → ExecutingTools →（循环回）→ Done / Failed`
- **协作式设计**：looper 在 `tokio::select!` 中运行，交替处理用户输入和 ReAct 进度。`react_step()` 每次调用仅推进内层状态机一步，因此循环永不会被模型响应或工具执行阻塞。
- 工具执行分两阶段：**spawn 阶段**（将所有工具调用启动到 `JoinSet` 中），然后 **poll 阶段**（以 200ms 超时贪婪排空结果，每完成一个即发出事件）。
- **流式路径**：使用 `StreamAssembler` 将 `StreamEvent` 块中的增量文本/推理/工具调用增量累积为完整的 assistant 消息。
- 动态上下文组装：系统提示词每轮重新注入，工具结果追加其后。`DynamicContext` trait 支持在每次新用户查询时注入 RAG 增强内容；同一轮的 ReAct 迭代复用缓存上下文。
- **上下文策略**：`FullHistory`（默认）、`SlidingWindow { max_turns }`、`TokenBudget { max_tokens, summarize_overflow }` 或 `Custom(Arc<dyn ContextFilter>)`。通过 `LooperConfig` 为每个 looper 选择。
- `LooperEvent` 枚举（19 个变体）通过异步 intercom 通道（`Speaker`/`Listener` 对）流动，覆盖文本增量、推理增量、工具调用生命周期、状态转换、轮次边界和关闭。
- `LooperHook` trait：8 个拦截点（`on_before_request`、`on_after_response`、`on_text_delta`、`on_before_tool`、`on_after_tool`、`on_turn_complete`、`on_react_state_change`、`on_outer_state_change`）。内置钩子：`ToolAllowlistHook`、`TokenBudgetHook`。

**SimpleAgentLooper**（[crates/peco-core/src/agent/simple_looper.rs](crates/peco-core/src/agent/simple_looper.rs)）：
- 最小化的纯 batch 变体，由 `DelegateSubAgent` 和 `RunParallelSubAgents` 使用。
- 无流式、无钩子、无事件、无会话持久化。仅：`用户消息 →（模型 → 工具）* → 最终文本`。

**Session**（[crates/peco-core/src/session/](crates/peco-core/src/session/)）：
- 状态机：`Idle → Active → Commit/Rollback/Cancel`。还有 `Cancelling` 和 `Interrupted` 中间状态。
- 双层消息缓冲区：`CommittedBuffer`（已持久化的轮次，`Vec<Vec<AnnotatedMessage>>`）+ `StagingBuffer`（当前轮次的在途消息）。
- `TurnBoundaryToken` — 一个零大小证明令牌，仅由 `commit_turn()` 和 `rollback_turn()` 返回。`snapshot()` 需要此令牌，提供编译期保证：快照仅在轮次边界发生，永不在轮次中间。
- `PendingInput` 队列处理活跃轮次期间的并发用户输入：`Interrupt` 优先级输入先于 `Normal` 优先级出队。
- 消息以 `Arc<Message>` 包裹在 `AnnotatedMessage` 中（含 id、turn_index、timestamp、source、estimated_tokens），实现零拷贝上下文构建。
- 持久化是外部的：`SessionPersister` trait（基于文件的 `FileSessionPersister` 或 `NullSessionPersister`）。Looper 在轮次边界调用 `persister.save()`。在 peco-server 中，Session 快照也通过 `SqliteSessionPersister` 持久化到 SQLite。

**Tools**（[crates/peco-core/src/tools/](crates/peco-core/src/tools/)）：
- 双 trait 设计：`Tool`（静态、泛型、类型化）和 `ToolDyn`（对象安全，`Pin<Box<dyn Future>>`）。
- blanket impl `impl<T: Tool> ToolDyn for T` 桥接二者。
- `ToolExecutor` trait：运行时接口 — `execute(name, args) -> Result<String, String>` + `definitions() -> Vec<ToolDefinition>`。
- **DI 契约**（`deps.rs`）：定义 5 个窄 trait — `AgentAccess`、`SkillProvider`、`KnowledgeAccess`、`WorkflowAccess`、`McpAccess` — 以及聚合结构体 `ToolDependencies`。工具只依赖这些 trait，不直接依赖 `WorkSpace`。
- **工具组装**（`tool_register.rs`）：`ToolRegister::build()` 根据 tool_names 和 `ToolDependencies` 一次性构建包含所有工具的 `ToolExecutor`。权威工具名清单为 `BUILTIN_TOOL_NAMES` 常量（由防漂移测试保障与 match arms 一致）；可选依赖（`workflow_access`/`mcp_access`）缺失时对应工具 warn + skip，不 panic。
- 内置工具（29 个）：`shell`、`fetch`、`web_search`、`show_workspace`、`list_tools`、`read_skill`、`list_skills`、`save_skill`、`delete_skill`、`delegate_sub_agent`、`run_parallel_sub_agents`、`save_agent`、`read_agent`、`delete_agent`、`execute_workflow`、`list_workflows`、`save_workflow`、`delete_workflow`、`list_mcp_servers`、`save_mcp_server`、`delete_mcp_server`、`test_mcp_connection`、`search_knowledge`、`list_knowledge_bases`、`add_to_knowledge_base`、`sync_knowledge_base`、`get_knowledge_base_docs`、`add_facts_to_knowledge_base`、`query_entity_facts`。
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
- **步骤类型**（Phase 1–2）：`shell`（`tokio::process::Command`）、`agent`（复用 `SimpleAgentLooper`，Agent 自带 `agent.md` 中定义的工具）。Phase 4 计划：`llm`（纯推理）、`tool`（调用 `ToolExecutor`）。
- **模板变量**：基于 minijinja，支持 `{{ steps.X.output }}`、`{{ inputs.xxx }}`、`{% if %}` 条件、`truncate`/`length`/`replace` 过滤器。
- **失败策略**：`Continue`（记录失败继续）| `Abort`（默认，中止并取消同级未完成步骤）| `Pause`（暂停等待审批，通过独立 mpsc channel）| `Retry`（Phase 4）。
- **条件门控**：`condition` 字段通过 minijinja 求值控制步骤是否执行，正交于 `depends_on` 拓扑依赖。
- **持久化**：`WorkflowPersister` trait（与 `SessionPersister` 同模式）。引擎在 Pause、每层完成、Completed/Failed 时自动保存快照。CLI/测试使用 `NullWorkflowPersister`，peco-server 提供 `SqliteWorkflowPersister`。Phase 3（REST API + SSE 流式 + Cron 触发）已完成。
- **工具集成**：`execute_workflow` 是一个 `ToolDyn` 工具，Agent 可在 ReAct 循环中调用（同步阻塞语义，适合短 workflow）。`OutputSchema` 功能通过在 prompt 中追加 JSON schema 指令实现，Phase 4 将使用 `StructuredOutputExecutor`。
- **DI 契约**：`WorkflowAccess` trait（窄接口，load/list/reload）由 `WorkSpace` 实现，注入 `ToolDependencies`。

**MCP**（[crates/peco-core/src/mcp/](crates/peco-core/src/mcp/)）：
- `McpManager`：每个 Agent 的 MCP 连接编排器。接收已解析的 `(name, McpServerConfig)` 对，创建传输层（通过 `rmcp` 的 Stdio / SSE / StreamableHTTP），连接、发现工具，并将其注册为 `McpTool` 包装器。
- `McpClientHandler`：实现 `rmcp::ClientHandler`，自动同步工具列表变更（list_changed → 移除所有受管工具 → 重新列出 → 重新注册）。
- MCP 配置存储在 `~/.peco/mcp_config.json`（或 `$PECO_CONFIG_DIR/mcp_config.json`），通过 `McpConfig::load()` 加载。

**Peco 永续会话：滚动压缩 + 记忆双路径**（详见 `docs/context-design.md`）：

- **滚动压缩**（[crates/peco-core/src/agent/compaction.rs](crates/peco-core/src/agent/compaction.rs)）：`CompactionPolicy::maybe_compact()` 在 turn 边界（looper Done 分支）估算 pinned + committed token，超过 `compaction_trigger_tokens` 时用 Flash 模型递归合并旧摘要与被驱逐轮次为结构化摘要（四段固定模板），`Session::compact()` 物理驱逐最旧轮次并重编号 turn_index，摘要作为 `pinned_summary`（System 消息）钉在上下文最前。失败非致命，仅记日志。
- **单一截断点**：全部裁剪决策在 `PecoContextFilter`（[crates/peco-server/src/peco/filter.rs](crates/peco-server/src/peco/filter.rs)）一处完成 — pinned 层（System/摘要）→ Verbatim 层（按 token 预算从最新往回整轮选择）→ 当前轮（完整保留）。`ContextStrategy` 保持 `FullHistory` 直通。
- **token 估算**：`estimate_str_tokens()`（[crates/peco-core/src/agent/context.rs](crates/peco-core/src/agent/context.rs)）CJK 0.6 token/字、其他 0.3 token/char 的校准估算，全项目唯一实现。
- **记忆双路径**（[crates/peco-server/src/peco/memory/](crates/peco-server/src/peco/memory/)）：存储载体是 personal 模板幂等安装的 per-user `@private_memory` KB。
  - **写路径** `MemoryExtractionHook`（LooperHook）：每轮成功完成后守卫检查（失败轮跳过、`analyze_min_chars` 过滤），`tokio::spawn` 后台检索既有记忆抑制重复 → Flash 模型（`ModelTurnAnalyzer`，严格 JSON 输出）提取事实 → 逐条写入 KB（source 标签 `ppa_{profile|semantic|episodic}`）。turn 边界零阻塞，所有失败点 warn 后 return。
  - **读路径** `MemoryRecallContext`（DynamicContext）：每次新用户 query 前，闲聊门控（问候/感谢关键词，零成本跳过；不按长度门控）→ 混合检索 → 按类别格式化注入 instructions 尾部，`injection_token_cap` 逐行截断。
  - **分工**：compaction 解决"会话内上下文放不下"，记忆解决"跨会话/超长期的知识"，二者正交。记忆的更新/删除由 `@assistant → @memory` 子 Agent 的显式 KB 工具路径负责（自动路径只做 add）。

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
- SSE 事件类型：`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_result`、`turn_complete`、`agent_call_start`、`agent_call_end`、`error`、`done`。（部分内部 `LooperEvent` 变体如 `ToolCallDelta`、`ModelUsage`、`ReactStateChange` 被过滤掉，不发送给客户端。）
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

- `ModelProvider` trait（async_trait）：`name()`、`chat(ChatRequest) -> ChatResponse`、`stream_chat(ChatRequest) -> ChatStream`。
- `ChatRequest`：model、messages、tools（作为 `ToolDefinition`）、temperature、max_tokens、reasoning_effort、additional_params。
- `StreamEvent` 枚举：`TextDelta`、`ReasoningDelta`、`ToolCallDelta`、`ToolCallComplete`、`End`。
- 目前实现了 `DeepSeek` provider（使用 `DEEPSEEK_API_KEY` 环境变量）。Provider 类型定义支持 `openai`、`anthropic`、`ollama`、`groq`（待实现）。
- Provider 配置位于 `providers.toml`（相对于 agent.md 文件或从标准位置解析）。

**SSE 流式管道**（[crates/model-provider/src/providers/streaming.rs](crates/model-provider/src/providers/streaming.rs)）：
- 与 provider 无关的设计：`process_normalized_sse_stream()` 是一个共享状态机，消费原始 SSE 事件并生成 `StreamEvent`。
- 添加新 provider（如 OpenAI）只需实现 `StreamingProfile`（将 provider 特定的块规范化为 `NormalizedChunk`）和 `ModelProvider` — SSE 解析、重连和工具调用累积逻辑可复用。
- `StreamingEventSource<R>`（[sse.rs](crates/model-provider/src/providers/sse.rs)）：一个 5 状态 SSE 流（`Connecting → Open → WaitingToRetry → Reconnecting → Closed`），带有 `Last-Event-Id` 追踪和可插拔的 `RetryPolicy`（默认指数退避：起始 300ms，2x 倍数，5s 上限）。
- DeepSeek 思考/推理：`ChatRequest.reasoning_effort` 映射到 DeepSeek 的 `thinking` 字段（`"disabled"` / `{"type": "enabled", "effort": "<value>"}`）。未设置时默认：`{"type": "enabled", "effort": "high"}`。

### knowledge-base：RAG 引擎

- **摄入管道**（6 步）：解析（PDF/DOCX/HTML/MD/代码/纯文本）→ 分块（滑动窗口，句子边界对齐，默认 800 字符窗口，200 字符重叠）→ 批量嵌入（FastEmbed ONNX，默认 `bge-small-zh-v1.5`，512 维）→ 存储文档 → upsert 向量 → 全文索引 + 构建结构图谱边（`CONTAINS`、`NEXT_CHUNK`）。
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
- **SSE 流式**：使用原生 `fetch()` + `ReadableStream` reader（而非 EventSource）实现实时 token 流式传输，支持 `AbortController`。解析 9 种 SSE 事件类型（`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_result`、`turn_complete`、`agent_call_start`、`agent_call_end`、`done`、`error`），并响应式更新消息状态 — 追加文本增量、在可折叠的 `<details>` 中显示推理、将工具调用渲染为卡片、以嵌套消息气泡追踪子 Agent 委托。
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
[providers.deepseek.default]
model = "deepseek-v4-flash"
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
max_turns: 30
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

6. **错误处理**：`AgentError` 覆盖完整生命周期（IO、YAML 解析、缺失字段、环境变量、配置、工具执行、超过最大轮次、协议违规）。`?` 运算符可在各处使用，因为它为常见错误类型实现了 `From`。

## Rust Edition 与工具链

- Rust edition **2024**（在 workspace `Cargo.toml` 中设置）
- 需要 Rust 1.85+
- WorkSpace resolver v3
- `unused_crate_dependencies = "warn"`（workspace 级别）
