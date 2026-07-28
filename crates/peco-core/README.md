# peco-core

AI Agent 核心框架 — 提供 Agent 组装、会话管理、知识库、MCP 连接、Skill 系统和工具抽象的一站式运行时。

## 架构总览

```
WorkSpace（用户隔离核心）
├── SystemConfig          ← 系统级配置（providers.toml + mcpconfig.json）
├── UserConfig            ← 用户级配置（Merge 深递归合并）
├── ToolFactory           ← 内置工具注册表（13 个工具）
├── SkillRegister          ← Skill 发现与生命周期管理
├── KnowledgeManager      ← 知识库管理（增量同步 + 混合检索）
├── PersonalMemoryStore   ← PPA 个人记忆存储（三层记忆模型）
└── [create_agent()]      ← 从 agent.md 组装完整 Agent 实例
```

### 执行路径

```
外部代码 → AgentExecutor ─┬─→ AgentLooper （多轮，完整事件系统 + streaming + hook + session）
                          │     └─ 双层状态机：OuterState（用户交互）+ ReActState（推理-执行）
                          └─→ SimpleAgentLooper （单轮，batch-only，无 streaming/hook/session）
                                └─ 轻量 ReAct 循环，专用于子 Agent 一次性任务
```

核心原则：
- **AgentLooper / SimpleAgentLooper 是仅有的两个引擎** — AgentExecutor 不直接调用 `Agent.chat()`
- **Agent 内部接口不对外** — `Agent.chat()` / `Agent.stream_chat()` 为 `pub(crate)`
- **Executor 可注册为 Tool** — `AgentExecutorTool` 让一个 agent 把另一个 agent 当工具调用
- **WorkSpace 是用户隔离边界** — 所有用户级资源（工具、知识库、记忆）通过 WorkSpace 管理

## 模块地图

| 模块 | 职责 |
|------|------|
| [`agent`](src/agent/mod.rs) | Agent 组装（从 agent.md 配置 → LLM provider + 工具 + MCP + Skill），含 DynamicContext trait |
| [`executor`](src/executor/mod.rs) | AgentExecutor 外观层：SingleTurn / MultiTurn / agent-as-tool |
| [`config`](src/config/mod.rs) | 配置系统：SystemConfig + UserConfig + Merge 深递归合并，providers.toml + MCP 注册表 |
| [`workspace`](src/workspace/mod.rs) | 用户隔离核心：WorkSpace、WorkspaceError、模板初始化 |
| [`personal_memory`](src/personal_memory/mod.rs) | PPA 个人记忆：PersonalMemoryStore、MemoryFact/UserProfile/TurnContext 类型与配置 |
| [`knowledge`](src/knowledge/mod.rs) | 知识库管理：文件哈希追踪、增量同步（工具移至 `tools/knowledge.rs`） |
| [`mcp`](src/mcp/mod.rs) | MCP 客户端：连接管理、工具自动同步、热重载 |
| [`persistence`](src/persistence/mod.rs) | 会话持久化：`SessionPersister` trait + 文件持久化实现 |
| [`session`](src/session/mod.rs) | 多轮对话状态管理：Session、消息缓冲、Snapshot |
| [`skills`](src/skills/mod.rs) | Skill 系统：三层渐进式加载、YAML frontmatter 解析 |
| [`tools`](src/tools/mod.rs) | 工具抽象层：`Tool`/`ToolDyn` trait、`ToolFactory`、13 个内置工具（含 PPA 记忆工具） |

## 快速开始

### 1. 创建 agent.md

```markdown
---
agent:
  name: my-assistant
  description: 通用助手
  version: "1.0"
llm:
  provider: deepseek
  model: deepseek-chat
  temperature: 0.7
tools:
  - shell
  - fetch
  - search_knowledge
skills:
  - code-review
mcp:
  - filesystem
max_turns: 20
---

你是一个 AI 助手，可以使用工具和知识库来帮助用户。
```

### 2. 编写代码

```rust
use std::sync::Arc;
use peco_core::agent::Agent;
use peco_core::executor::{SingleTurnExecutor, ExecutorInput, AgentExecutor};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 从 agent.md 创建 Agent（内部自动加载配置、工具、Skill、MCP）
    let agent = Arc::new(Agent::from_file("agent.md").await?);

    // SingleTurnExecutor：单轮问答，自动 ReAct tool calling
    let executor = SingleTurnExecutor::new(agent);
    let output = executor.execute(
        ExecutorInput::new("帮我搜索知识库中关于 Rust 异步编程的内容"),
    ).await?;

    println!("{}", output.content);
    Ok(())
}
```

## 核心概念

### Agent 组装

[`Agent::from_file`](src/agent/agent.rs) 从 `agent.md` 配置文件自动组装完整 Agent：

```
agent.md
  ├── YAML frontmatter → AgentProfile（工具列表、MCP 服务器、Skill、LLM 参数）
  ├── Markdown body    → system prompt
  └── 自动组装 → Agent {
          model: Arc<dyn ModelProvider>,   // LLM provider（已注册工具）
          tool_executor: Arc<dyn ToolExecutor>,  // 工具分发器
          mcp_manager: Arc<McpManager>,    // MCP 连接管理
      }
```

配置优先级：**agent.md 显式设置 > providers.toml provider 默认值**

> **注意**：`Agent::from_file` 内部通过 `WorkSpace` 获取 MCP 配置、Skill 列表和工具依赖。
> 在测试中可通过 `AgentBuilder` 注入 mock 依赖。

### Agent 运行循环

本项目提供**两个** ReAct 引擎，适用于不同场景：

| 引擎 | 文件 | 适用场景 | Streaming | Hook | Session | 事件广播 |
|------|------|---------|-----------|------|---------|---------|
| `AgentLooper` | [agent_looper.rs](src/agent/agent_looper.rs) | 多轮对话、交互式 REPL | ✅ | ✅ | ✅ | ✅ |
| `SimpleAgentLooper` | [simple_looper.rs](src/agent/simple_looper.rs) | 子 Agent 一次性任务 | ❌ | ❌ | ❌ | ❌ |

[`AgentLooper`](src/agent/agent_looper.rs) 实现双层状态机驱动的 ReAct 循环（外层用户交互 + 内层推理-执行），是驱动多轮 Agent 的主引擎：

```
外层: Idle ──→ ProcessingUserInput ──→ RunningInnerLoop ──→ Paused
                                            │
内层: PreparingRequest ──→ [batch] AwaitingModel → ResolvingResponse
                      ──→ [stream] Streaming
                      ──→ ExecutingTools ──→ (循环回 PreparingRequest)
                      ──→ Done / Failed
```

外部不直接操作，而是通过 [`AgentExecutor`](src/executor/mod.rs) 外观层使用。

### AgentExecutor 执行器

[`AgentExecutor`](src/executor/mod.rs) 是引擎之上的薄封装，提供多种执行模式：

| 执行器 | 文件 | 模式 | 状态 |
|--------|------|------|------|
| `SingleTurnExecutor` | [single_turn.rs](src/executor/single_turn.rs) | 单轮问答（含 ReAct tool calling），内部使用 **SimpleAgentLooper** | ✅ 已实现 |
| `MultiTurnExecutor` | [multi_turn.rs](src/executor/multi_turn.rs) | 多轮对话，复用 Session + 事件归属匹配，内部使用 **AgentLooper** | ✅ 已实现 |
| `AgentExecutorTool` | [tool.rs](src/executor/tool.rs) | agent-as-tool：将 Agent 包装为 ToolDyn | ✅ 已实现 |
| `StructuredOutputExecutor` | — | 结构化输出（schema + 重试） | 🔜 Phase 2 |
| `ChainExecutor` | — | 链式编排 | 🔜 Phase 2 |
| `RouterExecutor` | — | 路由分发 | 🔜 Phase 2 |
| `ParallelExecutor` | — | 并行执行 | 🔜 Phase 2 |

### WorkSpace 用户隔离

[`WorkSpace`](src/workspace/workspace.rs) 是用户隔离的核心抽象，替代了旧的 `GlobalHandler` 全局单例：

- **WorkSpace** 持有用户级资源：`SkillRegister`、`KnowledgeManager`、`AgentManager`，实现 `tools::deps` 中的 DI trait（`AgentLoader`、`SkillProvider`、`KnowledgeAccess`），通过 `tools::ToolRegister::build()` 按需为 Agent 组装工具执行器
- **ToolDependencies** 在 `tools::deps` 中定义窄 trait 接口（`AgentLoader`、`SkillProvider`、`KnowledgeAccess`），实现依赖注入，工具只依赖 trait 不依赖 WorkSpace
- **ToolRegister** 在 `tools` 模块中作为工厂，基于 `ToolDependencies` 一次构建到位
- **SystemConfig + UserConfig** 分层配置，`merge.rs` 提供深递归合并策略

```rust
use peco_core::tools::ToolDependencies;

// 构建用户 WorkSpace
let workspace = WorkSpace::builder()
    .system_config(system_config)
    .user_config(user_config)
    .tool_deps(tool_deps)  // 注入 AgentLoader, KnowledgeAccess, MemoryStore 等
    .build()?;

// 从 agent.md 创建 Agent
let agent = workspace.create_agent("my-agent").await?;
```

### Personal Memory（PPA）

[`personal_memory`](src/personal_memory/mod.rs) 提供个人记忆存储与检索能力：

- **PersonalMemoryStore**：三层记忆 CRUD（`MemoryFact` → `UserProfile` → `TurnContext`）
- **MemoryFact**：原子事实，含 `category`（Profile/Semantic/Episodic 三层记忆）、`importance`（Low/Medium/High）、`content``
- **UserProfile**：聚合的用户偏好（语言、工具偏好、工作风格）
- **PpaConfig**：可配置的阈值、最大事实数、模型选择

```rust
use peco_core::personal_memory::{PersonalMemoryStore, MemoryFact, MemoryCategory, Importance};

let store = PersonalMemoryStore::new(knowledge_manager, "personal_memory".into(), config);
store.remember(MemoryFact::new(
    MemoryCategory::Semantic,
    Importance::High,
    "用户偏好 Rust 异步代码风格".into(),
)).await?;

let facts = store.recall("Rust 异步编程", 5).await?;
```

### 工具系统

三层架构：

```
#[peco_tool] proc macro          ← 编译期：生成零大小结构体 + Tool trait impl
    ↓
Tool trait (typed)                 ← 类型安全的工具定义（Args/Output/Error 关联类型）
    ↓
ToolDyn trait (object-safe)        ← Box<dyn ToolDyn> 异构存储
    ↓
ToolFactory (global registry)      ← 按名称创建工具实例
    ↓
DefaultToolsExecutor               ← 按名称分发执行
```

#### 内置工具

| 工具名 | 文件 | 功能 |
|--------|------|------|
| `shell` | [tools/shell.rs](src/tools/shell.rs) | 执行 shell 命令 |
| `fetch` | [tools/fetch.rs](src/tools/fetch.rs) | HTTP 请求 |
| `read_skill` | [tools/skill.rs](src/tools/skill.rs) | 读取 Skill 内容 |
| `search_knowledge` | [tools/knowledge.rs](src/tools/knowledge.rs) | 混合检索知识库 |
| `list_knowledge_bases` | [tools/knowledge.rs](src/tools/knowledge.rs) | 列出所有知识库 |
| `sync_knowledge_base` | [tools/knowledge.rs](src/tools/knowledge.rs) | 增量同步知识库 |
| `add_to_knowledge_base` | [tools/knowledge.rs](src/tools/knowledge.rs) | 手动添加文本到知识库 |
| `get_knowledge_base_docs` | [tools/knowledge.rs](src/tools/knowledge.rs) | 查看知识库文档列表 |
| `remember` | [tools/memory.rs](src/tools/memory.rs) | 主动记录记忆事实 |
| `recall` | [tools/memory.rs](src/tools/memory.rs) | 检索个人记忆 |
| `forget` | [tools/memory.rs](src/tools/memory.rs) | 删除指定记忆 |
| `delegate_sub_agent` | [tools/sub_agent.rs](src/tools/sub_agent.rs) | 委托子 Agent 执行任务 |
| `run_parallel_sub_agents` | [tools/sub_agent.rs](src/tools/sub_agent.rs) | 并行运行多个子 Agent |

#### 添加新工具

1. 创建工具函数并用 `#[peco_tool]` 注解
2. 在 `tools/mod.rs` 中声明模块并 `pub use`
3. 在 [`ToolFactory::init()`](src/tools/tool_factory.rs) 中注册
4. 如需外部依赖（知识库、Agent 加载），通过 `tools::deps` 中的 trait 注入

### Skill 系统

三层渐进式加载，按需消耗 token：

| 层级 | 加载时机 | 内容 | Token 成本 |
|------|---------|------|-----------|
| Tier 1 | 启动时 | `name` + `description` | ~100 tokens/Skill |
| Tier 2 | 激活时 | 完整 SKILL.md 正文 | ~3000 tokens/Skill |
| Tier 3 | 引用时 | 脚本 / 参考文档 / 资源 | 按需 |

Skill 目录结构：

```text
<skill-name>/
├── SKILL.md              # 必需：YAML frontmatter + Markdown 正文
├── scripts/              # 可选：可执行脚本
├── references/           # 可选：参考文档
└── assets/               # 可选：模板、图片等
```

### MCP 连接管理

[MCP (Model Context Protocol)](https://modelcontextprotocol.io/) 按 Agent 级别管理连接：

```text
mcpconfig.json（全局服务器注册表）
    ↓ 按名称查找
agent.md 中声明的 mcp: [server1, server2]
    ↓ 过滤 enabled + 匹配
McpManager::new(&servers, executor)
    ↓ 每个 server：
      创建 transport → McpClientHandler → serve() + list_all_tools()
    ↓
    executor.add_tool()  × N  →  工具可用于 LLM
```

支持 **工具热重载**：MCP 服务器推送 `tools/list_changed` → 自动同步到 `ToolExecutor`。

### 会话管理

[`Session`](src/session/session.rs) 是零锁单线程版本，管理多轮对话状态：

- **分层缓冲**（[`InMemorySessionBuffer`](src/session/buffer.rs)）：committed（已确认）/ staging（当前轮次）/ pending（排队中）
- **Snapshot 持久化**（[`SessionSnapshot`](src/session/snapshot.rs)）：通过 `TurnBoundaryToken` 保证一致性 — 仅 `commit_turn()` / `rollback_turn()` 可产生 token，编译期保证快照只在 turn 边界生成
- **消息标注**（[`AnnotatedMessage`](src/session/types.rs)）：每条消息携带来源、时间戳、token 用量

#### 并发模型

Session 不包含内部锁，由 `AgentLooper` 以 `Box<Session>` 独占所有权，所有可变操作需要 `&mut self`，编译期保证单线程安全。

涉及并发的组件及其保证：

| 组件 | 并发原语 | 保证 |
|------|---------|------|
| `WorkSpace` | `Arc` 共享 | 用户级资源隔离，Clone 语义 |
| `SkillRegister` | `RwLock` | 读多写少，Tier 1 加载后极少写入 |
| `KnowledgeManager` 底层 | `Mutex<Option<...>>` | 延迟初始化，初始化后不可变借用 |
| `LooperHandle` | `Arc` + `AtomicBool`（cancel/pause flag） | Clone 语义，多持有者共享控制 |
| `DefaultToolsExecutor` | `RwLock<HashMap>` | MCP 热重载时动态添加/移除工具 |
| `OwnedTask` | `Arc<Mutex<Option<JoinHandle>>>` | 仅最后一个 clone drop 时 abort 任务 |

### 持久化

[`persistence`](src/persistence/mod.rs) 模块提供独立的会话持久化层。Session 本身不感知持久化 — `AgentLooper` 在 turn 边界调用 `SessionPersister::save()` 触发落盘。

- **[`SessionPersister`](src/persistence/traits.rs)** trait — 定义 `save` / `load` / `delete` / `list` 接口
- **[`FileSessionPersister`](src/persistence/file.rs)** — JSON 文件持久化实现（`{base_dir}/{session_id}.json`）
- **[`NullSessionPersister`](src/persistence/traits.rs)** — 空实现，用于不需要持久化的场景

### 错误处理

错误类型分为三层，逐层向上传播：

```
工具层                          会话/配置层                       Agent/Executor 层
───────                         ───────────                      ─────────────────
ToolError                       SessionError                     AgentError
  ├─ ToolCallError (Box<dyn       ├─ InvalidStateTransition        ├─ Config (配置错误)
  │   Error + Send + Sync>)       ├─ TurnOutOfBounds               ├─ InvalidFrontmatter
  └─ JsonError                    ├─ EmptyStaging                  ├─ Provider (LLM 调用失败)
                                  └─ StagingInconsistent           ├─ Io (文件读写)
                                                                    ├─ AgentProtocol (协议错误)
KnowledgeModuleError            SkillError                       └─ Tool (来自工具层)
  ├─ Sync (同步失败)               ├─ Io
  ├─ Backend (后端错误)            ├─ Parse (YAML 解析失败)      ExecutorError
  └─ Config                       └─ SkillNotFound                 ├─ Agent (来自 AgentError)
                                                                    ├─ Schema (结构化输出解析失败)
McpError / McpClientError        WorkspaceError                    ├─ Timeout / Cancelled
  ├─ Connection                    ├─ Config (配置加载失败)         ├─ SessionRequired
  ├─ ToolList                      ├─ AgentNotFound                 ├─ LooperExited
  └─ ToolCall                      └─ ToolBuild (工具构建失败)      └─ ChainStep (Phase 2)
```

**错误传播规则**：
- 工具执行失败 → `ToolError::ToolCallError` → 错误消息返回给模型作为 tool result（让模型自纠正）
- Agent 层错误 → `AgentError` → 大部分映射为 `ExecutorError::Agent`
- `ExecutorError` 是外部调用方的唯一错误类型，内部错误通过 `From` trait 自动转换

**重试策略**：当前由模型自行决定是否重试 tool 调用（收到 tool error 后模型可选择修正参数重新调用）。结构化输出重试（`StructuredOutputExecutor`）计划在 Phase 2 实现。

### 知识库管理

[`KnowledgeManager`](src/knowledge/manager.rs) 提供面向用户的人性化知识库操作：

```
用户放入文件到 docs/ 目录
    ↓
sync_kb() 扫描 → SHA-256 对比 → 增量更新
    ↓
LanceDB / HelixDB 存储（向量 + BM25 + 知识图谱）
    ↓
search_kb() 混合检索（语义 + 关键词 + 图谱 + RRF 融合）
```

核心特性：
- **文件哈希追踪**：[`FileHashManifest`](src/knowledge/hash_manifest.rs) 记录 SHA-256，对比检测新增/更新/删除
- **增量同步**：只处理变更文件，错误累积不中断
- **多知识库并发搜索**：`search_all()` 使用 `futures::join_all` 并行检索
- **延迟加载**：`Mutex<Option<KnowledgeBaseManager>>` 确保与 WorkSpace 初始化兼容
- **多后端支持**：LanceDB（默认，本地）和 HelixDB（feature-gated：`helixdb` feature，需额外配置 URL 连接）
- **Agent 工具**：知识库工具位于 [`tools/knowledge.rs`](src/tools/knowledge.rs)，通过 `tools::deps::KnowledgeAccess` 注入用户隔离

## 测试策略

| 层级 | 范围 | 工具 | 状态 |
|------|------|------|------|
| 单元测试 | 纯数据结构、StreamAssembler、Session 状态机 | `cargo test -p peco-core --lib` | ✅ 已有覆盖 |
| 工具注册测试 | ToolFactory 注册完整性、tool 名称正确性 | `cargo test -p peco-core --lib` | ✅ 13 个工具全覆盖 |
| 集成测试 | 知识库同步/检索（需 fastembed 模型下载 ~100MB） | `cargo test -p peco-core --lib -- --include-ignored` | ✅ 已实现 |
| E2E 测试 | AgentLooper 完整 ReAct 循环（需 mock LLM） | — | 🔜 Phase 2 |
| 压力测试 | 并发 MCP 工具调用、大 Session 恢复 | — | 🔜 Phase 2 |

当前 agent_looper 的测试覆盖集中在 `StreamAssembler`、纯数据结构、状态枚举序列化等无副作用的单元。完整的 ReAct 循环集成测试需要 mock `ModelProvider`，计划在 Phase 2 完成。

## 安全性考量

- **Shell 工具**：`shell_exec` 通过 `sh -c` 执行命令。**不提供沙箱隔离**，调用方应自行评估风险。建议在生产环境中通过 MCP 服务器替代，以利用 MCP 的权限边界。
- **MCP 权限模型**：MCP 服务器在独立进程中运行（stdio transport），工具调用通过 stdin/stdout JSON-RPC 通信。权限由服务器端控制，Agent 侧仅做连接级别的 `enabled` 过滤。
- **Prompt Injection**：当前无内置注入防护。Session 中的用户输入直接拼接为 LLM 消息。未来计划在 Hook 层提供注入检测点（`on_before_request`）。
- **API Key 管理**：支持 `${ENV_VAR}` 语法在 `providers.toml` 中引用环境变量，避免明文密钥存入配置文件。

## API 稳定性

当前版本 **0.1.0** — 所有公开 API 均可能发生破坏性变更。

| 稳定性等级 | 范围 | 说明 |
|-----------|------|------|
| **相对稳定** | `Session`、`SessionPersister`、`Tool`/`ToolDyn` trait、`WorkSpace` | 核心抽象已稳定，预计不会有大的接口变更 |
| **可能变更** | `AgentExecutor` trait、`LooperHandle`、`LooperEvent`、`DynamicContext` | 正在根据实际使用反馈迭代 |
| **实验性** | `ExecutorType` 中标记 Phase 2 的 variant、HelixDB 后端、`PersonalMemoryStore` | 接口仅为占位，实现和 API 均可能大幅调整 |

## 配置

### 环境变量

| 变量 | 用途 | 默认值 |
|------|------|--------|
| `DEEPSEEK_API_KEY` | DeepSeek API 密钥 | — |
| `PECO_SKILLS_ROOT` | Skill 根目录 | `./skills/` |
| `PECO_KB_ROOT` | 知识库根目录 | `~/.peco/knowledge_bases/` |
| `PECO_CONVERSATIONS_DIR` | 会话持久化目录 | `~/.peco/conversations/` |
| `RUST_LOG` | 日志过滤级别 | `warn` |

### 配置文件

```text
~/.peco/
├── providers.toml            # LLM Provider 配置（API key、base URL、默认参数）
├── mcpconfig.json            # MCP 服务器注册表
└── knowledge_config.json     # 知识库模块配置（可选，有内置默认值）
```

#### providers.toml 示例

```toml
default_provider = "deepseek"

[providers.deepseek]
provider_type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"

[providers.deepseek.default]
model = "deepseek-chat"
temperature = 0.7
max_tokens = 4096
```

#### mcpconfig.json 示例

```json
{
  "servers": {
    "filesystem": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "enabled": true
    }
  }
}
```

## 目录结构

```text
src/
├── lib.rs                  # crate 根，公共 API 重导出
├── agent/
│   ├── agent.rs            # Agent 组装（from_file → ModelProvider + ToolExecutor + McpManager）
│   ├── agent_config.rs     # AgentProfile、ModelConfig、agent.md YAML + Markdown 解析/序列化
│   ├── agent_looper.rs     # 双层状态机 ReAct 循环（AgentLooper + LooperHandle + LooperEvent）
│   ├── context.rs          # 上下文构建策略（FullHistory / SlidingWindow）
│   ├── dynamic_context.rs  # DynamicContext trait — 根据 query 动态注入上下文
│   ├── hooks.rs            # LooperHook trait + 内置 hook（TokenBudgetHook、ToolAllowlistHook）
│   ├── simple_looper.rs    # SimpleAgentLooper（轻量 batch-only ReAct，用于子 Agent）
│   ├── stream.rs           # ModelStream / ModelStreamEvent
│   └── error.rs            # AgentError
├── executor/
│   ├── mod.rs              # AgentExecutor trait + ExecutorInput/Output/Error
│   ├── single_turn.rs      # SingleTurnExecutor（单轮问答 + 自动 ReAct，内部使用 SimpleAgentLooper）
│   ├── multi_turn.rs       # MultiTurnExecutor（多轮对话，复用 Session，内部使用 AgentLooper）
│   └── tool.rs             # AgentExecutorTool（impl ToolDyn，agent-as-tool）
├── config/
│   ├── mod.rs              # 配置模块入口
│   ├── system_config.rs    # SystemConfig（providers + MCP 注册表，系统级）
│   ├── user_config.rs      # UserConfig（用户级覆盖）
│   ├── merge.rs            # 深递归合并策略（system ← user）
│   ├── types.rs            # ProvidersConfig、ProviderEntry
│   ├── loader.rs           # providers.toml I/O
│   ├── mcp_config.rs       # McpConfig、McpServerConfig、TransportType
│   └── error.rs            # ConfigError
├── workspace/
│   ├── mod.rs              # WorkSpace 模块入口
│   ├── workspace.rs        # WorkSpace（用户隔离边界，实现 tools 中的 DI trait）
│   └── error.rs            # WorkspaceError
├── personal_memory/
│   ├── mod.rs              # PPA 记忆模块入口
│   ├── store.rs            # PersonalMemoryStore（三层记忆 CRUD）
│   ├── types.rs            # MemoryFact / UserProfile / TurnContext 数据模型
│   └── config.rs           # PpaConfig 及子配置
├── tools/
│   ├── mod.rs              # Tool/ToolDyn trait、ToolExecutor、ToolError
│   ├── deps.rs             # ToolDependencies 窄 trait（AgentLoader/SkillProvider/KnowledgeAccess）
│   ├── tool_register.rs    # ToolRegister（基于 ToolDependencies 一次构建工具执行器）
│   ├── tool_factory.rs     # ToolFactory 全局注册表 + DefaultToolsExecutor
│   ├── shell.rs            # shell 工具
│   ├── fetch.rs            # fetch 工具
│   ├── skill.rs            # read_skill 工具
│   ├── knowledge.rs        # 知识库工具（search/list/add/sync/getDocs/addFacts/queryEntityFacts）
│   ├── memory.rs           # 3 个 PPA 记忆工具（remember/recall/forget）
│   └── sub_agent.rs        # delegate_sub_agent + run_parallel_sub_agents 工具
├── knowledge/
│   ├── manager.rs          # KnowledgeManager 核心
│   ├── config.rs           # KnowledgeConfig
│   ├── hash_manifest.rs    # 文件哈希清单（SHA-256 追踪）
│   ├── sync.rs             # SyncReport + 增量同步逻辑
│   └── error.rs            # KnowledgeModuleError
├── skills/
│   ├── global_skill_list.rs # SkillRegister（三层渐进式加载）
│   ├── loader.rs            # SkillLoader（SKILL.md 解析）
│   ├── config.rs            # Skill、SkillFrontmatter、SkillMeta
│   └── error.rs             # SkillError
├── session/
│   ├── session.rs           # Session（零锁单线程版本，状态机管理）
│   ├── buffer.rs            # InMemorySessionBuffer（分层缓冲：committed/staging/pending）
│   ├── snapshot.rs          # SessionSnapshot + TurnBoundaryToken（持久化快照）
│   ├── types.rs             # AnnotatedMessage、MessageSource、SessionState
│   ├── metadata.rs          # SessionMeta
│   └── error.rs             # SessionError
├── persistence/
│   ├── traits.rs            # SessionPersister trait + NullSessionPersister
│   ├── file.rs              # FileSessionPersister（JSON 文件持久化）
│   └── format.rs            # SessionFile 序列化格式
├── mcp/
│   ├── mcp_manager.rs       # McpManager（连接编排）
│   ├── mcp_client_handler.rs # McpClientHandler（工具同步 + 热重载）
│   ├── tool.rs              # McpTool（MCP → ToolDyn 适配）
│   ├── connection.rs        # Transport 连接工厂
│   └── error.rs             # McpError、McpClientError
└── utils/
    ├── mod.rs               # 工具模块入口
    └── intercom.rs          # 内部通信工具（Speaker/Listener 模式）
```

## 构建与测试

```bash
# 编译
cargo build -p peco-core

# 运行测试（含工具注册测试）
cargo test -p peco-core --lib

# 运行被忽略的集成测试（需要 fastembed 模型下载 ~100MB）
cargo test -p peco-core --lib -- --include-ignored

# Lint
cargo clippy -p peco-core
```

## 依赖关系

```text
peco-core
├── model-provider      ← LLM provider 抽象（dyn-safe ModelProvider trait + DeepSeek 实现）
├── peco-derive       ← #[peco_tool] proc macro
├── knowledge-base      ← 底层知识库引擎（LanceDB + HelixDB(feature-gated) + 混合检索）
├── rmcp                ← MCP 协议 Rust 实现
├── fastembed            ← 本地 ONNX 嵌入推理
├── serde + serde_yaml   ← agent.md / SKILL.md YAML frontmatter 解析
├── tokio               ← 异步运行时
└── tracing             ← 结构化日志
```


## 性能基准

> ⚠️ 以下为设计目标，尚未系统化测量。

| 指标 | 目标 | 说明 |
|------|------|------|
| Agent 冷启动（from_file） | < 200ms | 不含 MCP 连接建立 |
| Agent 冷启动（含 MCP 连接） | < 2s | 取决于 MCP 服务器启动速度 |
| 单轮推理延迟（不含 LLM） | < 10ms | Session 操作 + 上下文构建 |
| Session 恢复（100 turns） | < 50ms | JSON 反序列化 + 内存重建 |
| Skill Tier 1 加载（50 skills） | < 100ms | 文件扫描 + YAML frontmatter 解析 |
| 知识库增量同步（1000 files, 无变更） | < 500ms | SHA-256 哈希清单对比 |
