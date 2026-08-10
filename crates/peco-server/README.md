# peco-server

**Axum Web 后端 — AI Agent 平台的 RESTful API + SSE 流式对话服务**

`peco-server` 是 peco 项目的 Web 后端，基于 [Axum](https://crates.io/crates/axum) 框架构建，提供多用户 AI Agent 管理、SSE 流式对话、声明式 Workflow 编排、RAG 知识库、PPA 个人记忆、定时任务调度等完整功能。

## 目录

- [架构概览](#架构概览)
- [项目结构](#项目结构)
- [快速开始](#快速开始)
- [API 接口](#api-接口)
  - [认证](#认证)
  - [Peco 永续对话](#peco-永续对话)
  - [Chat 对话管理](#chat-对话管理)
  - [Agent 管理](#agent-管理)
  - [Provider 管理](#provider-管理)
  - [Skill 管理](#skill-管理)
  - [MCP 配置](#mcp-配置)
  - [知识库](#知识库)
  - [Workflow 编排](#workflow-编排)
  - [定时任务](#定时任务)
  - [用量统计](#用量统计)
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
│  │   Peco / Provider / Skill / MCP / Workflow    │  │
│  ├───────────────────────────────────────────────┤  │
│  │     Personal Assistant (PPA) — 三层记忆       │  │
│  ├───────────────────────────────────────────────┤  │
│  │         Middleware: Rate Limit (GCRA)           │  │
│  ├────────────────────────────────────────────────┤  │
│  │              Task Scheduler (Cron)              │  │
│  └────────────────────┬───────────────────────────┘  │
│                       │                              │
│  ┌────────────────────┼───────────────────────────┐  │
│  │              peco-core                         │  │
│  │  AgentLooper │ Session │ Tools │ MCP │ Skills   │  │
│  │  Workflow Engine │ WorkSpace │ Knowledge       │  │
│  └────────────────────┬───────────────────────────┘  │
│                       │                              │
│  ┌────────────────────┼───────────────────────────┐  │
│  │         model-provider (DeepSeek)               │  │
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
├── upload.rs                # 文件上传处理
│
├── auth/
│   ├── mod.rs               # Auth 路由组 (/api/auth/*)
│   ├── handler.rs           # register / login / me
│   ├── jwt.rs               # JWT 签发（7天有效期）+ HS256 验证
│   └── middleware.rs         # AuthUser — FromRequestParts extractor
│
├── peco/
│   └── handler.rs           # Peco 永续对话路由 (/api/peco/*)
│
├── agent/
│   ├── mod.rs               # Agent 路由组 (/api/agents/*)
│   └── handler.rs           # CRUD + agent.md 文件生成与解析
│
├── provider/
│   └── handler.rs           # Provider 配置管理 (/api/providers/*)
│
├── skill/
│   └── handler.rs           # Skill 管理 (/api/skills/*)
│
├── mcp_config/
│   └── handler.rs           # MCP 配置管理 (/api/mcp/*)
│
├── chat/
│   ├── mod.rs               # 对话路由组 (/api/chat/*, /api/conversations/*)
│   ├── handler.rs           # 对话 CRUD + SSE 流式聊天 + PPA 组件构建
│   └── sse.rs               # ChatSseEvent 类型 + LooperEvent → SSE 映射
│
├── knowledge/
│   ├── mod.rs               # 知识库路由组 (/api/knowledge/*)
│   └── handler.rs           # CRUD + 文件上传 + 手动同步
│
├── workflow/
│   ├── mod.rs               # Workflow 路由组 (/api/workflows/*, /api/schedules/*)
│   ├── handler.rs           # CRUD + execute + statistics + cancel + approve
│   ├── stream_handler.rs    # SSE 流式执行追踪
│   ├── sse.rs               # WorkflowSseEvent 类型 + WorkflowEvent → SSE 映射
│   ├── active.rs            # ActiveExecutions 全局注册表 (broadcast 扇出)
│   ├── types.rs             # 请求/响应类型
│   └── helper.rs            # 共享辅助函数
│
├── usage/
│   └── handler.rs           # Token 用量统计 (/api/usage/*)
│
├── personal_assistant/
│   ├── mod.rs               # PPA 个人记忆系统模块
│   ├── types.rs             # MemoryFact / UserProfile / 数据模型
│   ├── config.rs            # PpaConfig 及子配置
│   ├── store.rs             # PersonalMemoryStore — 三层记忆 CRUD
│   ├── classifier.rs        # QueryClassifier — 关键词规则引擎
│   ├── analyzer.rs          # MemoryAnalyzer — Flash 模型驱动记忆提取
│   ├── dynamic_context.rs   # PpaDynamicContext — 读路径
│   └── hook.rs              # PpaMemoryHook — 写路径
│
├── personal_agent/
│   ├── mod.rs               # Personal Agent 管理器 (DEPRECATED)
│   ├── handler.rs           # REST 端点
│   ├── manager.rs           # Agent 模板安装与缓存
│   └── config.rs            # 配置
│
├── assistant/
│   └── manager.rs           # PersonalAssistantManager (DEPRECATED)
│
├── task/
│   ├── mod.rs               # 定时任务路由组 (/api/tasks/*)
│   ├── handler.rs           # CRUD + toggle + 执行日志
│   ├── scheduler.rs         # CronScheduler — tokio-cron-scheduler 封装
│   └── executor.rs          # 任务执行逻辑
│
├── session_store/
│   └── mod.rs               # SqliteSessionPersister — Session 持久化
│
├── workspace/
│   ├── mod.rs               # WorkSpace 模块入口
│   └── manager.rs           # WorkspaceManager — LRU 缓存 WorkSpace 生命周期
│
├── file_watcher/
│   └── ...                  # 文件变更监控 (notify)
│
├── db/
│   ├── mod.rs               # 连接池 + 迁移 + server_config 存取
│   ├── schema.sql           # DDL — 表 + 索引
│   ├── sync.rs              # 同步辅助
│   ├── agents.rs            # Agent 索引 CRUD
│   ├── conversations.rs     # 对话 CRUD
│   ├── messages.rs          # 消息 CRUD
│   ├── knowledge_bases.rs   # 知识库 CRUD
│   ├── documents.rs         # 文档 CRUD
│   ├── tasks.rs             # 定时任务 CRUD (task_logs 内联)
│   ├── workflow_executions.rs  # Workflow 执行记录
│   ├── workflow_schedules.rs   # Workflow 调度配置
│   └── workspace_hashes.rs     # WorkSpace 哈希索引
│
├── middleware/
│   ├── mod.rs
│   └── rate_limit.rs        # GCRA per-user 限流 (governor)
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

- **Rust** 1.85+（edition 2024）
- **SQLite** 3（通过 `sqlx` 自动管理）
- **DeepSeek API Key**

### 本地运行

```bash
# 1. 设置环境变量
export DEEPSEEK_API_KEY=sk-your-api-key
export PECO_JWT_SECRET=your-production-secret   # 可选，未设置时自动生成+持久化

# 2. 编译并启动（在 workspace 根目录）
cargo run --release -p peco-server

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

所有接口（除认证外）需要在请求头中携带 `Authorization: Bearer <token>`。

### 认证

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/api/auth/register` | 注册新用户（username + email + password） |
| `POST` | `/api/auth/login` | 登录，返回 JWT token |
| `GET` | `/api/auth/me` | 获取当前用户信息 |

**JWT 特性：**
- 有效期 **7 天**，算法 HS256
- 密钥三层降级：环境变量 `PECO_JWT_SECRET` → SQLite 持久化 → 随机 UUID
- 验证时查询 `users` 表确保用户存在

### Peco 永续对话

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/peco/stream?message=` | SSE 流式永续对话（per-user perpetual session） |
| `GET` | `/api/peco/session` | 获取当前 Session 快照 |

使用 Peco 内置 `@assistant` + `@memory` 子 Agent 协作模式，自动管理个人记忆。

### Chat 对话管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/conversations` | 对话列表（支持 `?agent_id=` 筛选） |
| `POST` | `/api/conversations` | 创建新对话 |
| `DELETE` | `/api/conversations/:id` | 删除对话及消息、会话快照 |
| `GET` | `/api/conversations/:id/messages` | 消息历史（支持 `?offset=&limit=` 分页） |
| `GET` | `/api/conversations/:id/stream?message=` | **SSE 流式对话** |
| `GET` | `/api/conversations/:id/session` | 获取完整 Session 快照 |

**SSE 事件类型（9 种）：**

| 事件 | 说明 |
|------|------|
| `text_delta` | 逐 token 输出的文本增量 |
| `reasoning_delta` | 推理过程（DeepSeek thinking） |
| `tool_call_start` | 工具调用开始（含 id / name / arguments） |
| `tool_result` | 工具执行结果 |
| `agent_call_start` | 子 Agent 调用开始（委托/并行编排） |
| `agent_call_end` | 子 Agent 调用结束 |
| `turn_complete` | 本轮对话完成（含 token 用量） |
| `done` | 流结束 |
| `error` | 错误信息 |

### Agent 管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/agents` | 列出当前用户的 Agent |
| `POST` | `/api/agents` | 创建 Agent |
| `GET` | `/api/agents/:id` | 获取 Agent 详情 |
| `PATCH` | `/api/agents/:id` | 更新 Agent 部分字段 |
| `DELETE` | `/api/agents/:id` | 删除 Agent 及关联文件 |

Agent 完整配置（model/provider/system_prompt/tools/MCP/skills）存储为 `agent.md` 文件，DB 仅保存轻量索引字段（name/description/icon/color）。

### Provider 管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/providers` | 列出所有 providers |
| `PUT` | `/api/providers` | 新增或更新 provider |
| `GET` | `/api/providers/:name` | 获取 provider 详情 |
| `DELETE` | `/api/providers/:name` | 删除 provider |

Provider 配置以 `providers.toml` 存储在 workspace 根目录，支持 `deepseek`/`openai`/`anthropic`/`ollama`/`groq` 类型。

### Skill 管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/skills` | 列出所有 skills |
| `POST` | `/api/skills` | 创建 skill（生成 SKILL.md） |
| `PATCH` | `/api/skills/:name` | 更新 skill |
| `DELETE` | `/api/skills/:name` | 删除 skill |

### MCP 配置

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/mcp` | 列出所有 MCP 服务器配置 |
| `POST` | `/api/mcp` | 添加 MCP 服务器 |
| `PATCH` | `/api/mcp/:name` | 更新 MCP 服务器 |
| `DELETE` | `/api/mcp/:name` | 删除 MCP 服务器 |

支持 3 种传输类型：Stdio / SSE / StreamableHTTP。

### 知识库

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/knowledge` | 知识库列表 |
| `POST` | `/api/knowledge` | 创建知识库 |
| `GET` | `/api/knowledge/:id` | 知识库详情 |
| `DELETE` | `/api/knowledge/:id` | 删除知识库 |
| `GET` | `/api/knowledge/:id/documents` | 文档列表（分页+状态过滤） |
| `POST` | `/api/knowledge/:id/upload` | 上传文件（异步解析管道） |
| `POST` | `/api/knowledge/:id/sync` | 手动触发增量同步 |
| `DELETE` | `/api/knowledge/:id/documents/:doc_id` | 删除文档 |

**支持的文件类型：** PDF、DOCX、HTML、Markdown、纯文本、Python/Rust/Go/JS/TS 源码。

**Agent 可用的 KB 工具（由 peco-core 提供）：**
- `search_knowledge` — 混合检索
- `list_knowledge_bases` — 列出可用知识库
- `add_to_knowledge_base` — 添加文本内容
- `sync_knowledge_base` — 增量同步
- `get_knowledge_base_docs` — 查看文档列表
- `add_facts_to_knowledge_base` — 批量添加事实到知识图谱
- `query_entity_facts` — 查询实体关联事实

### Workflow 编排

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/workflows` | Workflow 列表 |
| `POST` | `/api/workflows` | 创建 Workflow |
| `GET` | `/api/workflows/:name` | 获取 Workflow 详情 |
| `PUT` | `/api/workflows/:name` | 更新 Workflow |
| `DELETE` | `/api/workflows/:name` | 删除 Workflow |
| `POST` | `/api/workflows/:name/execute` | 执行 Workflow（返回 run_id） |
| `GET` | `/api/workflows/:name/statistics` | 执行统计（60s 缓存） |
| `GET` | `/api/workflows/executions` | 执行记录列表（分页+过滤） |
| `GET` | `/api/workflows/executions/:run_id` | 执行详情（含步骤结果） |
| `GET` | `/api/workflows/executions/:run_id/stream` | **SSE 实时执行追踪** |
| `POST` | `/api/workflows/executions/:run_id/cancel` | 取消执行 |
| `POST` | `/api/workflows/executions/:run_id/approve` | 审批（proceed/abort） |

**调度管理：**

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/schedules` | 调度列表 |
| `POST` | `/api/schedules` | 创建 Cron 调度 |
| `PUT` | `/api/schedules/:name` | 替换调度 |
| `PATCH` | `/api/schedules/:name` | 更新调度 |
| `DELETE` | `/api/schedules/:name` | 删除调度 |

Workflow 支持 Shell 和 Agent 两种步骤类型，DAG 拓扑分层并行执行，失败策略：Continue / Abort / Pause。

### 定时任务

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/tasks` | 任务列表 |
| `POST` | `/api/tasks` | 创建任务（agent_id, cron_expr, prompt） |
| `PATCH` | `/api/tasks/:id` | 更新任务配置 |
| `DELETE` | `/api/tasks/:id` | 删除任务 |
| `POST` | `/api/tasks/:id/toggle` | 启用/禁用 |
| `GET` | `/api/tasks/:id/logs` | 执行日志（分页） |

基于 `tokio-cron-scheduler`，服务启动时自动加载已启用任务，支持运行时动态增减。每次执行写入日志（状态、输出、耗时、token 用量）。

### 用量统计

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/usage` | Token 用量概览 |
| `GET` | `/api/usage/daily` | 按日统计 |

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
| **向量存储** | LanceDB + FastEmbed | bge-small-zh-v1.5 (512 维) |
| **LLM** | DeepSeek (via model-provider) | 类型定义支持 OpenAI/Anthropic/Ollama/Groq |
| **作业调度** | tokio-cron-scheduler + croner | Cron 表达式 + 异步作业执行 |
| **限流** | governor (GCRA) | Per-user 速率限制 (20 req/s, burst 100) |
| **缓存** | LRU (lru crate) | WorkSpace 实例缓存（默认 128） |
| **API 文档** | utoipa + Swagger UI | OpenAPI 3.0 自动生成 |
| **序列化** | serde + serde_json | JSON 请求/响应 |
| **模板引擎** | minijinja | Workflow 模板变量 |
| **部署** | Docker + systemd + nginx | 多阶段构建 |

---

## 数据库设计

SQLite 数据库，13 张表，完整外键约束 + CASCADE 删除。Agent 完整配置存储为 `agent.md` 文件，DB 仅保留轻量索引：

| 表 | 说明 | 关键字段 |
|-----|------|---------|
| `users` | 用户 | id, username (UNIQUE), email (UNIQUE), password_hash |
| `agents` | Agent 轻量索引 | user_id FK, name (UNIQUE per user), description, icon, color |
| `conversations` | 对话 | user_id FK, agent_id FK, title |
| `messages` | 消息概要 | conversation_id FK, role, content |
| `knowledge_bases` | 知识库 | user_id FK, name, description |
| `documents` | 文档元数据 | kb_id FK, filename, status, error_msg |
| `tasks` | 定时任务 | user_id FK, agent_id FK, cron_expr, prompt, enabled |
| `task_logs` | 任务日志 | task_id FK, status, output, error, started_at, tokens |
| `session_snapshots` | 会话快照 | conversation_id PK, session_id, snapshot_json |
| `server_config` | 服务配置 | key PK, value (键值对存储) |
| `workflow_executions` | Workflow 执行记录 | run_id, workflow_name, user_id, status, snapshot_json, step_results |
| `workflow_schedules` | Workflow Cron 调度 | workflow_name, user_id, cron_expr, enabled |
| `workspace_hashes` | WorkSpace 文件哈希 | path, hash (用于变更检测) |

自动创建的索引覆盖所有高频外键查询字段。

---

## 核心设计

### Agent 配置模型

Agent 以 `agent.md`（YAML frontmatter + Markdown body）为唯一真相源：

- **agent.md** 包含完整配置：`llm`（provider/model/temperature/max_tokens）、`tools`、`mcp`、`skills`、`max_turns`，以及 Markdown 格式的 system prompt
- **DB agents 表**仅保存轻量索引（name, description, icon, color）
- `assemble_agent_md()` / `parse_agent_md()` 负责序列化与反序列化

### WorkSpace 隔离

`WorkspaceManager` 封装 LRU 缓存（容量 128），管理用户级 WorkSpace 生命周期：

- 每个 WorkSpace 持有该用户的 SkillRegister、KnowledgeManager、AgentManager、WorkflowManager
- 通过 `ToolDependencies`（5 个窄 trait）实现依赖注入
- Server 层通过 `WorkspaceManager` 管理缓存、构建和销毁

### Personal Memory（PPA）

PPA 在对话中自动学习用户偏好和事实，采用三层记忆模型（Profile → Semantic → Episodic）：

- **写路径**：`PpaMemoryHook`（LooperHook，`on_turn_complete`）→ 阈值过滤 → `MemoryAnalyzer`（独立 Flash 模型）→ `PersonalMemoryStore`（KB 文档存储 + 图谱双写）
- **读路径**：`PpaDynamicContext`（DynamicContext）在每次请求时检索 UserProfile + 向量相似记忆，注入 system prompt
- **查询分类**：`QueryClassifier` 规则引擎，4 类查询分类（CasualChat / PersonalQuery / TechnicalQuery / GeneralQuery）
- **Agent 协作**：通过 `@assistant → @memory` 子 Agent 模式使用 KB 工具直接管理记忆

### 用户隔离

Web 层所有资源均按 `user_id` 隔离：

- **Agent 工具**：子 Agent 按 `user_id` 从 DB 查找，通过 `AgentAccess` trait 注入
- **知识工具**：KB 工具注入 `user_id` + `allowed_kbs` 白名单，路由到用户专属目录
- **API 层**：所有 handler 通过 `AuthUser` extractor 提取 `user_id`，查询 DB 时附加过滤

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

### Workflow SSE 流式架构

```
Client                     Axum Handler                  Background Task
  |                             |                              |
  |── GET /exec/{id}/stream ───>|                              |
  |                             |── subscribe_events() ───────>|
  |                             |   (broadcast channel 256)    |
  |<── SSE: started ───────────|<─── WorkflowEvent::Started ───|
  |<── SSE: step_started ──────|<─── StepStarted ─────────────|
  |<── SSE: step_completed ────|<─── StepCompleted ───────────|
  |<── SSE: completed ─────────|<─── Completed ───────────────|
```

Workflow 引擎通过 mpsc → broadcast 双层通道实现 1:N SSE 扇出。

---

## 编译与运行

```bash
# 在 workspace 根目录编译
cargo build --release -p peco-server

# 运行
export DEEPSEEK_API_KEY=sk-your-key
cargo run --release -p peco-server

# 代码检查
cargo fmt --check
cargo clippy -- -D warnings
```

---

## Docker 部署

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

- **阶段 1 (builder)**：`rust:1.85-slim-bookworm` — 预编译依赖 + 编译源码
- **阶段 2 (runtime)**：`debian:bookworm-slim` — 最小化镜像
- **数据卷**：`/root/.peco` — 持久化 SQLite DB、会话文件、知识库向量数据

---

## 测试

```bash
# 运行所有集成测试（单线程，因 SQLite 共享）
cargo test -p peco-server --test '*' -- --test-threads=1

# 运行单个测试文件
cargo test -p peco-server --test auth_test
cargo test -p peco-server --test agent_test
cargo test -p peco-server --test chat_test
```

测试使用 `sqlite::memory:` 数据库，独立于生产数据。

### API 限流

- 普通 API：每秒 **20** 次，突发 **100** 次（per user，基于 JWT sub）
- SSE 端点：每秒 **1** 次，突发 **3** 次（独立限流器）
- 429 响应包含 `Retry-After` header

---

## 项目依赖关系

```
peco-server
├── peco-core            # Agent/Session/Workflow/WorkSpace/Tools/MCP/Skills 核心引擎
├── model-provider       # LLM Provider 抽象层 (DeepSeek)
├── knowledge-base       # RAG 引擎：文档解析、向量检索、混合搜索
├── Axum + Tokio         # Web 框架与异步运行时
├── SQLx (SQLite)        # 数据库
└── utoipa               # OpenAPI 文档自动生成
```

---

## License

MIT OR Apache-2.0
