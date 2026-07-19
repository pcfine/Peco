# peco-server

**Axum Web 后端 — AI Agent 平台的 RESTful API + SSE 流式对话服务**

`peco-server` 是 peco 项目的 Web 后端，基于 [Axum](https://crates.io/crates/axum) 框架构建，提供多用户 AI Agent 管理、SSE 流式对话、知识库（文档上传 + 混合检索）、定时任务（Cron 调度 Agent 执行）等完整功能。

## 目录

- [架构概览](#架构概览)
- [项目结构](#项目结构)
- [快速开始](#快速开始)
- [API 接口](#api-接口)
  - [认证](#认证)
  - [Agent 管理](#agent-管理)
  - [对话与流式聊天](#对话与流式聊天)
  - [知识库](#知识库)
  - [定时任务](#定时任务)
- [配置](#配置)
- [技术栈](#技术栈)
- [数据库设计](#数据库设计)
- [核心设计](#核心设计)
- [编译与运行](#编译与运行)
- [Docker 部署](#docker-部署)
- [测试](#测试)
- [项目依赖关系](#项目依赖关系)

---

## 架构概览

```
┌─────────────────────────────────────────────────────┐
│                    Web UI / Client                    │
├─────────────────────────────────────────────────────┤
│                   Axum HTTP Server                    │
│  ┌──────────┬──────────┬──────────┬──────────────┐  │
│  │  Auth    │  Agent   │  Chat    │  Knowledge   │  │
│  │  JWT     │  Handler │  SSE     │  Handler     │  │
│  │  BCrypt  │  agent.md│  Stream  │  User-Isolate│  │
│  ├──────────┴──────────┴──────────┴──────────────┤  │
│  │         Middleware: Rate Limit (GCRA)           │  │
│  ├────────────────────────────────────────────────┤  │
│  │              Task Scheduler (Cron)              │  │
│  └────────────────────┬───────────────────────────┘  │
│                       │                              │
│  ┌────────────────────┼───────────────────────────┐  │
│  │              peco-core                         │  │
│  │  AgentLooper │ Session │ Tools │ MCP │ Skills   │  │
│  └────────────────────┬───────────────────────────┘  │
│                       │                              │
│  ┌────────────────────┼───────────────────────────┐  │
│  │         model-provider (DeepSeek / OpenAI ...)   │  │
│  └─────────────────────────────────────────────────┘  │
│                                                       │
│  ┌─────────────────────────────────────────────────┐  │
│  │    SQLite (SQLx)  +  LanceDB (向量检索)          │  │
│  └─────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

## 项目结构

```
src/
├── main.rs                  # 入口：初始化 → 构建 Router → 启动 Server
├── lib.rs                   # build_router / build_router_with_limits
├── config.rs                # ServerConfig — 环境变量驱动的配置
├── state.rs                 # AppState — 全局共享状态
├── error.rs                 # ApiError — 统一错误类型 (IntoResponse)
├── openapi.rs               # utoipa OpenAPI / Swagger 文档定义
│
├── auth/
│   ├── mod.rs               # Auth 路由组 (/api/auth/*)
│   ├── handler.rs           # register / login / me
│   ├── jwt.rs               # JWT 签发（7天有效期）+ HS256 验证
│   └── middleware.rs         # AuthUser — FromRequestParts extractor
│
├── agent/
│   ├── mod.rs               # Agent 路由组 (/api/agents/*)
│   └── handler.rs           # CRUD + agent.md 文件生成与解析
│
├── chat/
│   ├── mod.rs               # 对话路由组 (/api/conversations/*)
│   ├── handler.rs           # 对话 CRUD + SSE 流式聊天 + PPA 组件构建
│   └── sse.rs               # ChatSseEvent 类型 + LooperEvent → SSE 映射
│
├── knowledge/
│   ├── mod.rs               # 知识库路由组 (/api/knowledge/*)
│   └── handler.rs           # CRUD + 文件上传 + 手动同步（调用 peco-core 工具）
│
├── personal_assistant/
│   ├── mod.rs               # PPA 个人记忆系统模块
│   ├── types.rs             # MemoryFact / UserProfile / TurnContext 数据模型
│   ├── config.rs            # PpaConfig 及子配置
│   ├── store.rs             # PersonalMemoryStore — 三层记忆 CRUD
│   ├── classifier.rs        # QueryClassifier — 关键词规则引擎
│   ├── analyzer.rs          # MemoryAnalyzer — Flash 模型驱动记忆提取
│   ├── dynamic_context.rs   # PpaDynamicContext — 读路径（Profile + 向量检索）
│   └── hook.rs              # PpaMemoryHook — 写路径（对话完成 → 记忆提取 → 写入）
│
├── task/
│   ├── mod.rs               # 任务路由组 (/api/tasks/*)
│   ├── handler.rs           # CRUD + toggle + 执行日志
│   ├── scheduler.rs         # CronScheduler — tokio-cron-scheduler 封装
│   └── executor.rs          # 任务执行逻辑
│
├── workspace/
│   ├── mod.rs               # Workspace 模块入口
│   └── manager.rs           # WorkspaceManager — LRU 缓存 Workspace 生命周期
│
├── db/
│   ├── mod.rs               # 连接池 + 迁移 + server_config 存取
│   ├── schema.sql           # DDL — 10 张表 + 索引
│   ├── agents.rs            # Agent CRUD
│   ├── conversations.rs     # 对话 CRUD
│   ├── messages.rs          # 消息 CRUD
│   ├── knowledge_bases.rs   # 知识库 CRUD
│   ├── documents.rs         # 文档 CRUD
│   ├── tasks.rs             # 定时任务 CRUD
│   └── task_logs.rs         # 任务执行日志
│
├── middleware/
│   ├── mod.rs
│   └── rate_limit.rs        # GCRA per-user 限流 (governor)
│
├── session_store/
│   └── mod.rs               # SqliteSessionPersister — Session 持久化
│
tests/
│   ├── common/mod.rs         # 测试辅助：TestServer / auth / DB helpers
│   ├── auth_test.rs          # 认证接口测试
│   ├── agent_test.rs         # Agent CRUD 测试
│   ├── chat_test.rs          # 对话接口测试
│   ├── knowledge_test.rs     # 知识库接口测试
│   └── task_test.rs          # 定时任务接口测试
```

---

## 快速开始

### 环境要求

- **Rust** 1.86+
- **SQLite** 3（通过 `sqlx` 自动管理）
- **DeepSeek API Key**（或其他兼容 OpenAI 的 model provider）

### 本地运行

```bash
# 1. 设置环境变量
export DEEPSEEK_API_KEY=sk-your-api-key
export PECO_JWT_SECRET=your-production-secret   # 可选，未设置时自动生成+持久化

# 2. 编译并启动
cd crates/peco-server
cargo run --release

# 服务默认监听 http://0.0.0.0:9227
# Swagger UI: http://localhost:9227/docs
# OpenAPI JSON: http://localhost:9227/api-docs/openapi.json
```

### 快速验证

```bash
# 注册用户
curl -X POST http://localhost:9227/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"alice","email":"alice@example.com","password":"secret123"}'

# 登录获取 token
TOKEN=$(curl -s -X POST http://localhost:9227/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"alice@example.com","password":"secret123"}' | jq -r '.token')

# 查看当前用户
curl http://localhost:9227/api/auth/me -H "Authorization: Bearer $TOKEN"

# 创建对话
curl -X POST http://localhost:9227/api/conversations \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"测试对话"}'

# SSE 流式对话（在终端中实时输出）
curl -N "http://localhost:9227/api/conversations/{conv_id}/stream?message=你好" \
  -H "Authorization: Bearer $TOKEN"
```

---

## API 接口

### 认证

所有接口（除认证本身）需要在请求头中携带 Bearer Token。

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/api/auth/register` | 注册新用户（username + email + password） |
| `POST` | `/api/auth/login` | 登录，返回 JWT token |
| `GET` | `/api/auth/me` | 获取当前用户信息（需要认证） |

**JWT 特性：**
- 有效期 **7 天**
- 算法 HS256
- 密钥三层降级：环境变量 `PECO_JWT_SECRET` → SQLite 持久化 → 随机 UUID
- 验证时会查询 `users` 表确保用户存在

### Agent 管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/agents` | 列出当前用户的 Agent |
| `POST` | `/api/agents` | 创建 Agent（name， description， system_prompt, model, tools…） |
| `GET` | `/api/agents/:id` | 获取 Agent 详情（含完整 system_prompt 和 config） |
| `PATCH` | `/api/agents/:id` | 更新 Agent 部分字段 |
| `DELETE` | `/api/agents/:id` | 删除 Agent 及关联文件 |

**Agent 配置字段（agent.md YAML frontmatter）：**

```yaml
agent:
  name: "代码审查员"
  description: "负责代码质量审查"
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
  temperature: 0.7
  max_tokens: 4096
tools:
  - shell_exec
  - fetch
mcp:
  - filesystem
skills:
  - code-review
max_turns: 30
```

创建的 Agent 在 `{data_dir}/agents/{user_id}/{agent_name}/agent.md` 生成对应的配置文件。DB 仅保存 name/description/icon/color 等索引字段。

### 对话与流式聊天

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/conversations` | 对话列表（支持 `?agent_id=` 筛选） |
| `POST` | `/api/conversations` | 创建新对话 |
| `DELETE` | `/api/conversations/:id` | 删除对话及消息、会话快照 |
| `GET` | `/api/conversations/:id/messages` | 消息历史（支持 `?offset=&limit=` 分页） |
| `GET` | `/api/conversations/:id/stream?message=` | **SSE 流式对话** |

**SSE 事件类型：**

| 事件 | 说明 |
|------|------|
| `text_delta` | 逐 token 输出的文本增量 |
| `reasoning_delta` | DeepSeek reasoning_content 推理过程 |
| `tool_call_start` | 工具调用开始（含 id / name / arguments） |
| `tool_result` | 工具执行结果 |
| `agent_call_start` | 子 Agent 调用开始（委托/并行编排） |
| `agent_call_end` | 子 Agent 调用结束 |
| `turn_complete` | 本轮对话完成 |
| `done` | 流结束（含 token 用量） |
| `error` | 错误信息 |

SSE 流每 15 秒发送 keep-alive 心跳。

**对话流程：**
1. 首次对话自动创建「全能助手」Agent（如果用户没有 Agent）
2. 每次请求加载/恢复 Session（从 `session_snapshots` 表）
3. `AgentLooper` 执行 ReAct 循环，事件通过 mpsc channel 转为 SSE 推送
4. 轮次完成后自动写入消息并更新对话标题

### 知识库

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/knowledge` | 知识库列表 |
| `POST` | `/api/knowledge` | 创建知识库（name， description， embedding_model, chunk_strategy） |
| `GET` | `/api/knowledge/:id` | 知识库详情 |
| `DELETE` | `/api/knowledge/:id` | 删除知识库 |
| `GET` | `/api/knowledge/:id/documents` | 文档列表（支持分页+状态过滤） |
| `POST` | `/api/knowledge/:id/upload` | 上传文件（异步解析管道） |
| `POST` | `/api/knowledge/:id/sync` | 手动触发增量同步 |
| `DELETE` | `/api/knowledge/:id/documents/:doc_id` | 删除文档 |

**支持的文件类型：**
PDF、DOCX、HTML、Markdown、纯文本、Python/Rust/Go/JS/TS 源码。

**知识检索能力：**
- 后端使用 **LanceDB** 向量数据库 + **fastembed** (bge-small-zh-v1.5) 嵌入模型
- 支持混合检索（BM25 + 向量相似度，RRF 融合）
- 支持三种分块策略：重叠窗口、固定大小、基于句子

**知识工具（Agent 可用，由 peco-core 提供）：**
- `search_knowledge` — 搜索知识库
- `list_knowledge_bases` — 列出可用知识库
- `add_to_knowledge_base` — 添加文本内容
- `sync_knowledge_base` — 增量同步
- `get_knowledge_base_docs` — 查看文档列表

Server 层通过 `ToolDependencies` trait 注入 `user_id`，确保用户间数据隔离。所有工具实现位于 `peco-core::tools`。

### 定时任务

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/tasks` | 任务列表 |
| `POST` | `/api/tasks` | 创建任务（agent_id, cron_expr, prompt） |
| `PATCH` | `/api/tasks/:id` | 更新任务配置 |
| `DELETE` | `/api/tasks/:id` | 删除任务 |
| `POST` | `/api/tasks/:id/toggle` | 启用/禁用 |
| `GET` | `/api/tasks/:id/logs` | 执行日志（支持分页） |

**调度器特性：**
- 基于 `tokio-cron-scheduler`，支持标准 Cron 表达式（通过 `croner` 校验）
- 服务启动时自动从 DB 加载已启用的任务
- 支持运行时动态增减、重新调度
- 每次执行写入 `task_logs` 表，记录状态（running/success/error）、输出、耗时
- 优雅关闭时停止所有定时任务

执行日志可查询每次任务运行的输入/输出 token 量、成功/失败状态。

---

## 配置

### 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PECO_SERVER_HOST` | `0.0.0.0` | 绑定地址 |
| `PECO_SERVER_PORT` | `9227` | 监听端口 |
| `PECO_DATABASE_URL` | `sqlite:~/.peco/server.db?mode=rwc` | SQLite 连接串 |
| `PECO_JWT_SECRET` | 自动生成+持久化 | JWT 签名密钥（三层降级） |
| `PECO_DATA_DIR` | `~/.peco/` | 数据存储根目录 |
| `DEEPSEEK_API_KEY` | — | DeepSeek API 密钥（必填） |
| `RUST_LOG` | `peco_server=info,tower_http=info` | 日志级别 |

### JWT 密钥三层降级策略

1. **环境变量 `PECO_JWT_SECRET`** — 生产环境推荐方式
2. **SQLite `server_config` 表** — 自动持久化，重启不失效
3. **随机 UUID** — 无 DB 连接时兜底（重启后所有 token 失效）

---

## 技术栈

| 领域 | 技术 | 说明 |
|------|------|------|
| **Web 框架** | Axum 0.8+ | 基于 Tokio 的异步 Web 框架 |
| **数据库** | SQLite + SQLx | 连接池、WAL 模式、外键约束 |
| **认证** | JWT (jsonwebtoken) + BCrypt | HS256 签名，密码 cost factor 12 |
| **向量存储** | LanceDB + fastembed | bge-small-zh-v1.5 (512 维) |
| **LLM** | DeepSeek v4 (via model-provider) | 可扩展为 OpenAI 兼容接口 |
| **作业调度** | tokio-cron-scheduler + croner | Cron 表达式 + 异步作业执行 |
| **限流** | governor (GCRA) | Per-user 速率限制 |
| **缓存** | LRU (lru crate) | Agent 实例缓存（默认 128） |
| **API 文档** | utoipa + Swagger UI | OpenAPI 3.0 自动生成 |
| **序列化** | serde + serde_json | JSON 请求/响应 |
| **部署** | Docker + docker-compose | 多阶段构建 |

---

## 数据库设计

SQLite 数据库，10 张表，完整外键约束 + CASCADE 删除。Agent 完整配置（model、provider、system_prompt、tools、MCP、skills）存储为 `agents/{name}/agent.md` 文件，DB 仅保留轻量索引和 UI 元数据：

| 表 | 说明 | 关键字段 |
|-----|------|---------|
| `users` | 用户 | id, username (UNIQUE), email (UNIQUE), password_hash |
| `agents` | Agent 轻量索引 | user_id FK, name (UNIQUE per user), description, icon, color — 完整配置存于 `agent.md` |
| `conversations` | 对话 | user_id FK, agent_id FK, title |
| `messages` | 消息概要 | conversation_id FK, role, content |
| `knowledge_bases` | 知识库 | user_id FK, name, description |
| `documents` | 文档元数据 | kb_id FK, filename, status, error_msg |
| `tasks` | 定时任务 | user_id FK, agent_id FK, cron_expr, prompt, enabled |
| `task_logs` | 任务日志 | task_id FK, status, output, error, started_at |
| `session_snapshots` | 会话快照 | conversation_id PK, snapshot_json (JSON 格式) |
| `server_config` | 服务配置 | key PK, value (键值对存储) |

自动创建的索引覆盖所有高频外键查询字段。

---

## 核心设计

### Agent 配置模型

Agent 以 `agent.md`（YAML frontmatter + Markdown body）为唯一真相源：

- **agent.md** 包含完整配置：`llm`（provider/model/temperature/max_tokens）、`tools`、`mcp`、`skills`、`max_turns`，以及 Markdown 格式的 system prompt
- **DB agents 表**仅保存轻量索引（name, description, icon, color）和 UI 元数据，不含 system_prompt/model/provider
- `assemble_agent_md()` / `parse_agent_md()` 负责序列化与反序列化
- 迁移 `002_slim_agents.sql` 安全地将旧宽表迁移到新轻量表

### Workspace 隔离

`WorkspaceManager` 封装 LRU 缓存（默认容量 128），管理用户级 Workspace 生命周期：

- 每个 Workspace 持有该用户的 `ToolRegister`、`PersonalMemoryStore`、知识库访问等资源
- `Workspace` 下移到 `peco-core`，提供 `ToolDependencies` 窄 trait 接口实现依赖注入
- Server 层通过 `WorkspaceManager` 管理缓存、构建和销毁

### Personal Memory（PPA）

PPA（Personal PA）在对话中自动学习用户偏好和事实，采用三层记忆模型（Profile → Semantic → Episodic）：

- **写路径**：`PpaMemoryHook`（`on_turn_complete`）→ 阈值过滤 → `MemoryAnalyzer`（独立 Flash 模型）→ 写入 `PersonalMemoryStore`
- **读路径**：`PpaDynamicContext` 在每次请求时检索 UserProfile + 向量相似记忆，注入 system prompt
- **记忆工具**：Agent 可通过 `remember` / `recall` / `forget` 主动管理记忆
- **查询分类**：`QueryClassifier` 关键词规则引擎，4 类查询分类（记忆操作 / 偏好查询 / 事实查询 / 通用）

### 用户隔离

Web 层所有资源均按 `user_id` 隔离：

- **Agent 工具**：子 Agent 按 `user_id` 从 DB 查找（而非文件系统路径），通过 `peco-core::workspace::AgentLoader` trait 注入
- **知识工具**：5 个知识工具注入 `user_id`，路由到用户专属目录 `{data_dir}/knowledge/{user_id}/`
- **API 层**：所有 handler 通过 `AuthUser` extractor 提取 `user_id`，查询 DB 时附加 `user_id` 过滤

### SSE 流式对话架构

```
Client                     Axum Handler                  Background Task
  |                             |                              |
  |── GET /stream?message= ────>|                              |
  |                             |── spawn AgentLooper ────────>|
  |                             |   (mpsc channel 256)         |
  |<── SSE: text_delta ────────|<─── LooperEvent::TextDelta ───|
  |<── SSE: tool_call_start ───|<─── LooperEvent::ToolCall ────|
  |<── SSE: tool_result ───────|<─── LooperEvent::ToolResult ──|
  |<── SSE: turn_complete ─────|<─── LooperEvent::TurnComplete |
  |<── SSE: done ──────────────|<─── LooperEvent::Shutdown ────|
  |                             |                              |
```

### 定时任务执行

`CronScheduler` 封装 `tokio-cron-scheduler`，内部用 `tokio::sync::Mutex` 保护（因 `JobScheduler::shutdown` 需要 `&mut self`）。

每个 job 的 closure 在触发时 clone 所有捕获状态（task_id、agent_id、prompt、pool、state），然后委托给 `executor::execute_task`，后者：

1. 写入 running 日志
2. 通过 `WorkspaceManager` 获取 Agent
3. `AgentLooper` 执行 ReAct 循环
4. 收集结果，更新日志状态
5. 更新任务 `last_run_at`

---

## 编译与运行

### 本地编译

```bash
# 在 workspace 根目录
cargo build --release -p peco-server

# 或进入 crate 目录
cd crates/peco-server
cargo build --release
```

### 运行

```bash
# 确保已设置 DEEPSEEK_API_KEY
export DEEPSEEK_API_KEY=sk-your-key

# 直接运行
cargo run --release -p peco-server

# 或运行编译产物
./target/release/peco-server
```

### 代码检查

```bash
cargo fmt --check
cargo clippy -- -D warnings
```

---

## Docker 部署

### 使用 docker-compose（推荐）

```bash
# 构建并启动
PECO_JWT_SECRET=your-production-secret \
DEEPSEEK_API_KEY=sk-your-key \
  docker-compose up -d

# 查看日志
docker-compose logs -f

# 停止
docker-compose down
```

### 使用 Docker

```bash
# 构建
docker build -t peco-server .

# 运行
docker run -d \
  --name peco-server \
  -p 9227:9227 \
  -e DEEPSEEK_API_KEY=sk-your-key \
  -e PECO_JWT_SECRET=your-secret \
  -v peco_data:/root/.peco \
  peco-server
```

### Docker 架构

- **阶段 1 (builder)**：`rust:1.86-slim-bookworm` — 利用 Docker 缓存层先预编译依赖，再编译源码
- **阶段 2 (runtime)**：`debian:bookworm-slim` — 最小化镜像，仅包含 ca-certificates
- **数据卷**：`/root/.peco` — 持久化 SQLite DB、会话文件、知识库向量数据

---

## 测试

```bash
# 运行所有集成测试
cargo test -p peco-server --test '*' -- --test-threads=1

# 运行单个测试文件
cargo test -p peco-server --test auth_test
cargo test -p peco-server --test agent_test
cargo test -p peco-server --test chat_test
cargo test -p peco-server --test knowledge_test
cargo test -p peco-server --test task_test
```

测试使用 `sqlite::memory:` 数据库，独立于生产数据。

### API 限流

- 普通 API：每秒 **20** 次，突发 **100** 次（per user，基于 JWT user_id）
- 未认证请求共享 `anonymous` 限流池
- 429 响应包含 `Retry-After` header

---

## 项目依赖关系

```
peco-server
├── peco-core          # Agent/Session/Workspace/PPA/Tools/MCP/Skills 核心抽象
├── model-provider       # LLM Provider 抽象层 (DeepSeek / OpenAI)
├── knowledge-base       # 本地文档向量化、混合检索、PDF 解析
├── knowledge-helixdb    # HelixDB 知识图谱后端（可选扩展）
├── Axum + Tokio         # Web 框架与异步运行时
├── SQLx (SQLite)         # 数据库
└── utoipa               # OpenAPI 文档自动生成
```

---

## License

本项目为内部开发项目。
