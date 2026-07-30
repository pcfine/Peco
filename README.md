# Peco — AI Agent 平台

基于 **Rust + React** 的全栈 AI Agent 平台。提供 Agent 定义与编排、MCP 协议集成、多模态 RAG 知识库、可视化对话、定时任务调度等完整能力。

## 架构总览

```
┌─────────────────────────────────────────────────────────────┐
│                    Web UI (React 19 + TypeScript)            │
│      SSE 流式对话 · Agent 管理 · 知识库 · 定时任务 · 认证     │
├─────────────────────────────────────────────────────────────┤
│                  peco-server (Axum + Tokio)                  │
│      REST API · SSE 流 · JWT 认证 · 限流 · OpenAPI/Swagger   │
├─────────────────────────────────────────────────────────────┤
│                  peco-core (Agent 引擎)                       │
│  Agent · Session · ReAct Loop · Workflow · WorkSpace · PPA · MCP · Skills · Tools · KB │
├────────────────────────┼────────────────────────────────────┤
│     model-provider      │       knowledge-base               │
│   (LLM 统一抽象层)       │   (RAG: 向量+BM25+知识图谱)          │
├────────────────────────┴────────────────────────────────────┤
│    SQLite · LanceDB · FastEmbed · DeepSeek API · MCP Server  │
└─────────────────────────────────────────────────────────────┘
```

## 核心特性

### Agent 引擎
- **声明式定义**：通过 `agent.md`（YAML frontmatter + Markdown）定义 Agent 的模型、工具、MCP、Skills 和 KB 访问白名单
- **ReAct 执行循环**：Think → Act → Observe，自动处理多轮工具调用
- **Workflow 编排**：声明式 DAG 工作流（`workflow.md`），拓扑分层并行执行，支持条件门控、模板变量传递（minijinja）、失败策略（Continue/Abort/Pause/Retry）和人工审批暂停/恢复
- **子 Agent 编排**：支持串行委派 (`delegate_sub_agent`) 和并行执行 (`run_parallel_sub_agents`)，前端可视化追踪
- **Session 管理**：状态机驱动（Idle → Active → Commit/Rollback/Cancel），支持 turn 回滚、中断队列、自动持久化
- **模板初始化**：内置 3 套 Workspace 模板（personal / minimal / developer），`--init-template` 一键初始化

### 工具系统
- **14 个内置工具**：`shell`、`fetch`、`read_skill`、`execute_workflow`、子 Agent 委派/并行、知识库 CRUD 与搜索、PPA 记忆（`remember` / `recall` / `forget`）
- **MCP 协议**：完整实现 Model Context Protocol，支持 stdio 和 HTTP Streamable 传输，工具自动发现与同步
- **可扩展**：`#[peco_tool]` 宏自动生成工具定义，`Tool`/`ToolDyn` 双 trait 设计

### Personal Memory（PPA）
- **自动记忆提取**：对话完成后独立 Flash 模型分析，自动识别并存储用户偏好、决策、事实
- **三层记忆模型**：Profile（用户身份/偏好）→ Semantic（离散事实/知识）→ Episodic（对话摘要/上下文）
- **智能检索**：写路径（阈值过滤 + LLM 分析）和读路径（Profile + 向量检索）分离
- **记忆工具**：Agent 可通过 `remember` / `recall` / `forget` 主动管理记忆

### 知识库（RAG）
- **多格式解析**：PDF、DOCX、HTML、Markdown、代码、纯文本
- **智能分块**：滑动窗口（句子边界对齐）、固定大小、按句子
- **混合检索**：向量搜索 + BM25 全文搜索 + 知识图谱，自适应 RRF 融合
- **本地嵌入**：FastEmbed ONNX 推理，默认 `bge-small-zh-v1.5`（中文优化）
- **增量同步**：基于哈希 manifest 的文档变更检测

### Skill 系统
- **渐进式加载**：3 级 token 预算控制，按需激活
- **`SKILL.md` 格式**：与 agent.md 同源，YAML frontmatter + Markdown body
- **目录发现**：`<skill-name>/SKILL.md` 结构，自动扫描注册

### Web 前端
- **SSE 流式对话**：9 种事件类型（text_delta / reasoning_delta / tool_call / agent_call / turn_complete / done / error）
- **Agent 管理**：创建、编辑、配置工具/MCP/Skills、选择模型参数
- **知识库管理**：上传文档、增量同步、搜索预览
- **定时任务**：Cron 表达式配置，自动执行 Agent 对话，日志查看
- **认证系统**：JWT（7 天有效期）、登录/注册、路由守卫、自动 token 刷新

### 运维
- **一键部署**：`scripts/deploy.sh` — 编译 → systemd → nginx 全自动
- **API 文档**：Swagger UI (`/docs`) + OpenAPI 规范
- **限流保护**：per-user API 速率限制
- **优雅关闭**：SIGTERM 信号处理，调度器安全停止

## 快速开始

### 前提条件

- **Rust** 1.85+（edition 2024）
- **Node.js** 22+ / npm 10+
- **DeepSeek API Key**（[获取地址](https://platform.deepseek.com/)）

### 1. 克隆并配置

```bash
git clone <repo-url> peco
cd peco

# 配置 API Key
echo "DEEPSEEK_API_KEY=sk-your-key-here" > .env
```

### 2. 开发模式（一键启动）

```bash
bash scripts/dev.sh
```

后端启动于 `http://localhost:9227`，前端启动于 `http://localhost:9233`。

### 3. 生产部署

```bash
# 设置 API Key（若未在 .env 中配置）
export DEEPSEEK_API_KEY=sk-your-key-here

# 一键部署（需要 root 权限）
sudo -E bash scripts/deploy.sh
```

部署后访问 `http://localhost`。

### 4. CLI 模式

```bash
# 从内置模板初始化 Workspace
cargo run -p peco-cli -- -t personal     # 个人助手
cargo run -p peco-cli -- -t developer    # 开发辅助
cargo run -p peco-cli -- -t minimal      # 最轻量对话

# 启动交互式对话
cargo run -p peco-cli -- --agent <agent-name>

# 运行 Workflow 演示
cargo run -p peco-core --example workflow_demo
```

## 项目结构

```
peco/
├── crates/
│   ├── peco-core/              # Agent 引擎：ReAct Loop、Session、Workflow、WorkSpace、PPA、MCP、Skills、Tools
│   ├── peco-server/            # Web 服务：Axum REST API、SSE、JWT、Cron 调度
│   ├── peco-cli/               # 命令行 AI 助手
│   ├── model-provider/         # LLM 统一抽象层（当前仅实现 DeepSeek）
│   ├── knowledge-base/         # RAG 引擎：解析→分块→嵌入→混合检索
│   ├── peco-agents/            # 内置 Workspace 模板（编译时嵌入）
│   └── peco-derive/            # 过程宏（#[peco_tool]）
├── webui/                      # React 19 前端
│   ├── src/
│   │   ├── api/                # API 层（axios + SSE 流解析器）
│   │   ├── pages/              # 页面（chat/agents/knowledge/tasks/auth）
│   │   ├── components/         # UI 组件（shadcn/ui）
│   │   ├── stores/             # Zustand 状态管理
│   │   └── types/              # TypeScript 类型定义
│   └── package.json
├── scripts/
│   ├── dev.sh                  # 开发环境一键启动
│   └── deploy.sh               # 生产部署脚本
├── docs/
│   └── workflow-design.md       # Workflow 模块技术方案
├── Cargo.toml                  # Rust workspace
└── .env                        # 环境变量（API Key 等）
```

## Agent 定义示例

创建 `agent.md` 文件定义 Agent：

```yaml
---
agent:
  name: "代码审查员"
  description: "负责代码质量审查和技术方案评审"
llm:
  provider: "deepseek"
  model: "deepseek-v4-pro"
  temperature: 0.3
tools:
  - shell
  - fetch
  - search_knowledge
mcp:
  - helixdb-docs
skills:
  - code-review
knowledge_bases:
  - @project_docs
max_turns: 30
---

# System Prompt

你是一位资深代码审查专家，拥有 10 年以上的软件工程经验。

## 你的职责
1. 审查代码的正确性和安全性
2. 识别性能瓶颈和架构问题
3. 提供可操作的具体改进建议

## 工作流程
- 首先理解代码的业务意图
- 检查边界条件和错误处理
- 评估代码可维护性和可测试性
```

### Workflow 定义示例

创建 `workflow.md` 文件定义声明式 DAG 工作流：

```yaml
---
workflow:
  name: "ci-pipeline"
  description: "CI 流水线：Lint → Test → Build"
  version: "1.0"
  timeout_seconds: 300
steps:
  - id: "lint"
    name: "Lint"
    type: shell
    config:
      command: "cargo clippy -- -D warnings 2>&1"
    on_failure: "continue"

  - id: "test"
    name: "Test"
    type: shell
    config:
      command: "cargo test --workspace 2>&1"
    depends_on: ["lint"]
    condition: "{{ steps.lint.success }}"
    on_failure: "abort"

  - id: "build"
    name: "Build"
    type: shell
    config:
      command: "cargo build --release 2>&1"
    depends_on: ["test"]
    on_failure: "pause"
---
```

Workflow 支持 Shell、Agent 两种步骤类型，通过 `depends_on` 定义 DAG 拓扑，`condition` 控制条件执行，`{{ steps.X.output }}` 在步骤间传递数据。

## 配置

### 环境变量

| 变量 | 必需 | 说明 |
|------|------|------|
| `DEEPSEEK_API_KEY` | ✓ | DeepSeek API 密钥 |
| `PECO_SERVER_HOST` | - | 服务监听地址（默认 `127.0.0.1`） |
| `PECO_SERVER_PORT` | - | 服务端口（默认 `9227`） |
| `PECO_JWT_SECRET` | - | JWT 签名密钥（自动生成） |
| `PECO_DATA_DIR` | - | 数据目录（默认 `/var/lib/peco`） |
| `PECO_DATABASE_URL` | - | SQLite 数据库路径 |

### API 文档

启动后端后访问 `http://localhost:9227/docs` 查看 Swagger UI。

## 技术栈

| 层 | 技术 |
|----|------|
| Agent 引擎 | Rust (Tokio async) |
| Web 后端 | Axum + SQLite (sqlx) + JWT (jsonwebtoken) |
| LLM 抽象 | async-trait 动态分发 |
| RAG 引擎 | LanceDB + FastEmbed (ONNX) + BM25 |
| MCP 协议 | rmcp (stdio + HTTP Streamable) |
| Web 前端 | React 19 + TypeScript + Vite 8 |
| UI 框架 | Tailwind CSS v4 + shadcn/ui (Radix) |
| 状态管理 | Zustand |
| 部署 | systemd + nginx |

## 开发

```bash
# 运行测试
cargo test --workspace

# 运行特定 crate 测试
cargo test -p peco-core
cargo test -p peco-core -- workflow  # 仅 Workflow 模块测试
cargo test -p peco-server
cargo test -p knowledge-base

# 前端测试
cd webui && npx vitest run

# TypeScript 类型检查
cd webui && npx tsc --noEmit

# 代码格式化
cargo fmt --all
cd webui && npx prettier --write src/
```

## 扩展计划

- [x] 声明式 Workflow 编排引擎（DAG 拓扑 + 模板变量 + 条件门控 + 失败策略）— Phase 1–2 ✅
- [ ] Workflow peco-server 集成（REST API + SSE 流式 + SQLite 持久化 + Cron 触发）— Phase 3
- [ ] Workflow 高级特性（Llm/Tool 步骤类型、重试、HumanApproval、子 Workflow 嵌套）— Phase 4
- [ ] OpenAI / Anthropic / 本地模型 Provider 支持
- [ ] 结构化输出（Structured Output Executor）
- [ ] 对话分支（Branching）与 A/B 对比
- [ ] 更丰富的 MCP 传输（WebSocket）

## License

MIT OR Apache-2.0

---

**[English Version](#english-version)** | **[中文版](#peco--ai-agent-平台)**

---

<h2 id="english-version">Peco — AI Agent Platform</h2>
<h3><a href="#peco--ai-agent-平台">← 中文版</a></h3>

A full-stack AI Agent platform built on **Rust + React**. Provides Agent definition & orchestration, MCP protocol integration, multimodal RAG knowledge base, visual chat interface, and cron-based task scheduling.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Web UI (React 19 + TypeScript)            │
│     SSE Streaming · Agent Mgmt · Knowledge · Tasks · Auth    │
├─────────────────────────────────────────────────────────────┤
│                  peco-server (Axum + Tokio)                  │
│      REST API · SSE · JWT Auth · Rate Limit · OpenAPI        │
├─────────────────────────────────────────────────────────────┤
│                  peco-core (Agent Engine)                     │
│ Agent · Session · ReAct Loop · Workflow · WorkSpace · PPA · MCP · Skills · Tools · KB │
├────────────────────────┬────────────────────────────────────┤
│     model-provider      │       knowledge-base               │
│     (LLM Abstraction)   │   (RAG: Vector+BM25+Graph)         │
├────────────────────────┴────────────────────────────────────┤
│    SQLite · LanceDB · FastEmbed · DeepSeek API · MCP Server  │
└─────────────────────────────────────────────────────────────┘
```

### Key Features

- **Agent Engine**: Declarative `agent.md` definitions, ReAct execution loop, DAG workflow orchestration (`workflow.md` with topology-level parallelism, condition gating, template variables, failure policies), sub-agent orchestration (serial delegation / parallel execution), state-machine-based Session management with rollback & interrupt queue, built-in workspace templates (`--init-template`)
- **Tool System**: 14 built-in tools, full MCP protocol support (stdio + HTTP Streamable), auto tool discovery & sync, extensible `Tool`/`ToolDyn` trait design
- **Personal Memory (PPA)**: Auto memory extraction via Flash model, three-tier memory (Profile → Semantic → Episodic), `remember`/`recall`/`forget` tools
- **RAG Knowledge Base**: Multi-format parsing (PDF/DOCX/HTML/MD/Code/TXT), intelligent chunking, hybrid search (vector + BM25 + knowledge graph), adaptive RRF fusion, local ONNX embeddings with Chinese-optimized `bge-small-zh-v1.5`
- **Skill System**: 3-tier progressive loading, `SKILL.md` format, automatic directory discovery
- **Web UI**: SSE streaming chat with 9 event types, Agent CRUD, knowledge base management, cron task scheduling, JWT authentication
- **Operations**: One-command deploy via `deploy.sh`, Swagger API docs, rate limiting, graceful shutdown

### Quick Start

**Prerequisites**: Rust 1.85+, Node.js 22+, DeepSeek API Key

```bash
# Clone and configure
git clone <repo-url> peco && cd peco
echo "DEEPSEEK_API_KEY=sk-your-key-here" > .env

# Development (both backend + frontend)
bash scripts/dev.sh
# Backend: http://localhost:9227  |  Frontend: http://localhost:9233
# API Docs: http://localhost:9227/docs

# Production deployment
sudo -E bash scripts/deploy.sh
# Visit http://localhost

# CLI mode
cargo run -p peco-cli -- -t personal          # init workspace from template
cargo run -p peco-cli -- --agent <agent-name> # interactive chat
cargo run -p peco-core --example workflow_demo # workflow demo
```

### Tech Stack

| Layer | Technology |
|-------|-----------|
| Agent Engine | Rust (Tokio async) |
| Web Backend | Axum + SQLite (sqlx) + JWT (jsonwebtoken) |
| LLM Abstraction | async-trait dynamic dispatch |
| RAG Engine | LanceDB + FastEmbed (ONNX) + BM25 |
| MCP Protocol | rmcp (stdio + HTTP Streamable) |
| Frontend | React 19 + TypeScript + Vite 8 |
| UI | Tailwind CSS v4 + shadcn/ui (Radix) |
| State | Zustand |
| Deploy | systemd + nginx |

### Development

```bash
cargo test --workspace          # All tests
cd webui && npx vitest run       # Frontend tests
cargo fmt --all                  # Format Rust
```

### License

MIT OR Apache-2.0
