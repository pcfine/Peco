# Peco — 本地多工作空间 AI Agent 平台

基于 **Rust + React** 的本地 AI Agent 平台。单用户、单进程、绑定本机回环地址，以 **团队 → 工作空间** 组织多套 Agent 资源，提供 Agent 定义与编排、MCP 协议集成、多模态 RAG 知识库、可视化对话、任务派发与定时调度等完整能力。

## 架构总览

```
┌──────────────────────────────────────────────────────────────┐
│              peco-webui (React 19 + TypeScript)               │
│   Peco 面板 · 团队 · 空间 · 对话 · 日程 · 设置（SSE 实时流）    │
├──────────────────────────────────────────────────────────────┤
│                 peco-daemon (Axum + Tokio)                    │
│   本地单用户 · 多工作空间 · 任务台账 · 静态前端 · SSE 事件流    │
├──────────────────────────────────────────────────────────────┤
│                    peco-core (Agent 引擎)                      │
│ Agent · Session · ReAct · Workflow · WorkSpace · MCP · Skills · Tools · KB │
├────────────────────────┬─────────────────────────────────────┤
│     model-provider      │         knowledge-base               │
│     LLM 统一抽象层       │   RAG：向量 + BM25 + 知识图谱         │
├────────────────────────┴─────────────────────────────────────┤
│   SQLite · LanceDB · FastEmbed · DeepSeek/Qwen/OpenAI · MCP   │
└──────────────────────────────────────────────────────────────┘
```

`peco-core` 是与传输层无关的引擎；`peco-daemon` 是唯一后端，把引擎接入本地 HTTP/SSE，并负责工作空间生命周期、任务台账与单用户身份；`peco-webui` 是 React 前端，产物可由 daemon 直接托管（同源部署）。

## 核心特性

### 多工作空间组织
- **三层结构**：Peco（全局主管）／团队（team）／工作空间（space）。每个空间绑定且仅绑定一个团队，资源目录按空间隔离，身份固定为单用户 `local`。
- **空间生命周期**：进入时才惰性打开（`POST /api/spaces/{id}/open`），关闭即卸载；空间可移入回收站并恢复，或彻底销毁（两套确认）。
- **任务台账与唤醒**：跨空间的任务派发落 SQLite 台账；任务到达终态后自动唤醒 Peco，进程重启按台账对账回收。
- **单实例**：同一数据根只允许一个 daemon（`flock` 单实例锁）。

### Agent 引擎
- **声明式定义**：通过 `agent.md`（YAML frontmatter + Markdown）定义 Agent 的模型、工具、MCP、Skills 和 KB 访问白名单。
- **ReAct 执行循环**：单一状态机 `LooperState`（Idle / PreparingRequest / ResolvingResponse / Streaming / ExecutingTools / Done / Failed / PausedDuringTools / PausedBeforeRequest）驱动，流式 + batch 双路径；截断与瞬时故障走统一重试（退避 + 预算抬升）。
- **Workflow 编排**：声明式 DAG 工作流（`workflow.md`），Kahn 拓扑分层并行，支持条件门控、minijinja 模板变量、失败策略（Continue/Abort/Pause）。
- **子 Agent 编排**：串行委派（`delegate_sub_agent`）与并行执行（`run_parallel_sub_agents`），前端可视化追踪。
- **Session 管理**：状态机驱动（Idle → Active → Commit/Rollback/Cancel），支持轮次回滚与中断队列；双层消息缓冲 + 轮次边界快照。
- **模板初始化**：内置 Workspace 模板（`peco` / `team_default`），幂等安装。

### 工具系统（41 个内置工具）
- **通用**：`shell`、`fetch`、`web_search`、`show_workspace`、`list_tools`
- **Agent / Skill / Workflow / MCP CRUD**：全套增删改查与连接测试
- **知识库**：`search_knowledge`、`list_knowledge_bases`、`add_to_knowledge_base`、`sync_knowledge_base`、`get_knowledge_base_docs`、`add_facts_to_knowledge_base`、`query_entity_facts`，以及文档 / 实体事实删除工具
- **跨空间调度**：`create_workspace`、`open_workspace`、`dispatch_to_workspace`、`inspect_workspace_task`、`run_machine_task`、`inspect_machine_task`、`save_schedule`
- **MCP 协议**：完整 Model Context Protocol，支持 Stdio / SSE / StreamableHTTP 三种传输，工具自动发现与同步
- **可扩展**：`#[peco_tool]` 宏自动生成工具定义，`Tool` / `ToolDyn` 双 trait 设计

### 永续会话上下文与记忆
- **滚动压缩**：会话超过阈值时，在轮次边界用 Flash 模型把旧轮次递归合并为结构化摘要钉在上下文最前；被驱逐轮次从 Session 与快照物理删除。
- **单一截断点**：全部裁剪决策在一处完成 — pinned 摘要 → 按 token 预算整轮选择的逐字历史 → 完整保留的当前轮（CJK 校准 token 估算）。
- **记忆双路径**：写路径每轮后台提取用户偏好/事实/事项（Flash 模型，检索既有记忆抑制重复，轮次边界零阻塞）；读路径每次新 query 前检索相关记忆注入上下文（闲聊门控零成本跳过）。每空间独立记忆库。

### 知识库（RAG）
- **多格式解析**：PDF、DOCX、HTML、Markdown、代码、纯文本
- **智能分块**：滑动窗口（句子边界对齐）、固定大小、按句子；确定性 Chunk ID 实现幂等摄入
- **混合检索**：向量 + BM25 + 知识图谱，自适应 RRF 融合（QueryAnalyzer → PathCalibration → CrossValidation → AdaptiveFusion）
- **本地嵌入**：FastEmbed ONNX 推理，默认 `bge-base-zh-v1.5`（中文优化，768 维）
- **多后端**：InMemory（测试）、LanceDB（Arrow，生产）、HelixDB（HTTP 客户端）

### Skill 系统
- **渐进式加载**：3 级 token 预算控制（Tier 1 名称+描述 → Tier 2 完整正文 → Tier 3 scripts/references/assets），按需激活
- **`SKILL.md` 格式**：与 agent.md 同源，YAML frontmatter + Markdown body
- **目录发现**：`<skill-name>/SKILL.md` 结构，自动扫描注册

### 前端（peco-webui）
- **Peco 面板 / 团队 / 空间**：空间列表与回收站、团队详情、空间详情与状态看板
- **SSE 实时对话**：流式文本、推理过程折叠、工具调用卡片、子 Agent 委派追踪、Markdown 渲染与代码高亮
- **日程与设置**：Cron 表达式的定时任务、Provider 配置与系统设置
- **本地认证**：首次用 setup token 注册本地账号，密码登录、会话 Cookie

### 运维
- **一键部署**：`scripts/deploy-daemon.sh` — 构建 → systemd `--user` unit → 安装二进制，起停/状态/环境刷新/卸载全流程
- **同源托管**：daemon 通过 `PECO_WEBUI_DIST` 直接托管前端产物（`/api` 与 `/uploads` 之外的请求兜底到 SPA）
- **优雅关闭**：SIGINT / SIGTERM 统一收尾，停机会话与调度器安全停止

## 快速开始

### 前提条件

- **Rust** 1.85+（edition 2024）
- **Node.js** 22+ / npm 10+
- **LLM API Key**（如 [DeepSeek](https://platform.deepseek.com/)、阿里云百炼 DashScope）

### 1. 克隆并配置 Provider

```bash
git clone <repo-url> peco && cd peco

# 在 daemon 数据根写入 providers.toml（默认 $PECO_HOME，即 ~/.peco-daemon）
# 也可用 PECO_PROVIDERS_CONFIG 指向自定义路径
cat > "${PECO_HOME:-$HOME/.peco-daemon}/providers.toml" <<'EOF'
default_provider = "deepseek"
[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"
[providers.deepseek.default]
model = "deepseek-flash"
EOF

export DEEPSEEK_API_KEY=sk-your-key-here
```

### 2. 开发模式

后端（默认监听 `127.0.0.1:6971`）与前端（Vite，`5174`，自动把 `/api`、`/uploads` 代理到 daemon）分别启动：

```bash
# 终端 1 — daemon
cargo run -p peco-daemon

# 终端 2 — 前端
cd peco-webui && npm install && npm run dev
```

打开 `http://localhost:5174`。首次访问用 `$PECO_HOME/token`（默认 `~/.peco-daemon/token`）里的 setup token 注册本地账号。

### 3. 生产部署（systemd --user）

```bash
cd peco-webui && npm install && npm run build && cd ..

# 构建后端 + 安装 systemd --user unit（默认托管 peco-webui/dist）
bash scripts/deploy-daemon.sh install

bash scripts/deploy-daemon.sh status     # 查看状态
bash scripts/deploy-daemon.sh restart    # 重启
```

部署后 daemon 默认监听 `http://127.0.0.1:6971` 并同源托管前端。所有路径都可用环境变量覆盖（见 `scripts/deploy-daemon.sh` 头部说明）。

## 项目结构

```
peco/
├── crates/
│   ├── peco-core/              # Agent 引擎：ReAct Loop、Session、Workflow、WorkSpace、MCP、Skills、Tools
│   ├── peco-daemon/            # 唯一后端：本地单用户 HTTP/SSE、多工作空间、任务台账、静态托管
│   ├── model-provider/         # LLM 统一抽象层（DeepSeek / Qwen / OpenAI 完整实现）
│   ├── knowledge-base/         # RAG 引擎：解析→分块→嵌入→混合检索（InMemory/LanceDB/HelixDB）
│   ├── peco-agents/            # 内置 Workspace 模板（编译时嵌入）
│   └── peco-derive/            # 过程宏（#[peco_tool]）
├── peco-webui/                 # React 19 前端（Vite，5174）
│   ├── src/api/                # API 层（axios + SSE 流解析器）
│   ├── src/pages/              # 页面（Peco 面板 / 团队 / 空间 / 日程 / 设置 / 认证）
│   ├── src/components/         # UI 组件（shadcn/ui）
│   └── src/stores/             # Zustand 状态管理
├── scripts/
│   └── deploy-daemon.sh        # daemon 构建与 systemd --user 部署
├── docs/
│   └── design/multi-workspace-daemon.md   # 多工作空间架构设计
├── Cargo.toml                  # Rust workspace
└── README.md
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
  model: "deepseek-flash"
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
max_iterations: 30
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

Workflow 支持 Shell、Agent 两种步骤类型（Llm、Tool 已定义类型但尚未实现），通过 `depends_on` 定义 DAG 拓扑，`condition` 控制条件执行，`{{ steps.X.output }}` 在步骤间传递数据。

## 配置

### 环境变量（daemon）

| 变量 | 必需 | 说明 |
|------|------|------|
| `DEEPSEEK_API_KEY` / `DASHSCOPE_API_KEY` 等 | ✓ | 由 `providers.toml` 通过 `${ENV_VAR}` 引用 |
| `PECO_HOME` | - | daemon 数据根（默认 `~/.peco-daemon`） |
| `PECO_DAEMON_BIND_ADDR` | - | 监听地址（默认 `127.0.0.1:6971`，仅回环） |
| `PECO_DAEMON_EXIT_POLICY` | - | 退出时对空间的默认策略（`ask` / `keep` / `stop`） |
| `PECO_PROVIDERS_CONFIG` | - | 系统层 `providers.toml` 的绝对路径（缺省 `$PECO_HOME/providers.toml`） |
| `PECO_WEBUI_DIST` | - | 前端产物目录；设置后 daemon 同源托管 SPA（缺省不挂静态层） |
| `PECO_LOG` | - | 日志过滤（`EnvFilter` 语法，默认 `peco_daemon=debug,warn`） |

部署脚本另有一组 `PECO_DAEMON_*` 覆盖项（`PECO_DAEMON_HOME`、`PECO_DAEMON_BIN`、`PECO_DAEMON_UNIT_DIR`、`PECO_DAEMON_LOG_DIR`、`PECO_DAEMON_WORKDIR`、`PECO_WEBUI_DIST` 等），见 `scripts/deploy-daemon.sh` 头部。

### providers.toml（LLM Provider 配置）

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
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"

[providers.qwen.default]
model = "qwen3.7-plus"
temperature = 0.7
max_tokens = 4096
stream = true

[providers.openai]
type = "openai"
api_key = "${OPENAI_API_KEY}"
base_url = "https://api.openai.com/v1"   # 可覆盖为兼容网关（vLLM/OpenRouter）
```

Provider 类型定义支持 `deepseek`、`qwen`、`openai`、`anthropic`、`ollama`、`groq`（当前 `deepseek` / `qwen` / `openai` 有完整实现）。`deepseek` 与 `qwen` 默认走 Responses 端点，`openai` 默认走 Chat Completions；均支持 `api` 字段切换。配置支持 `${ENV_VAR}` 语法引用环境变量。

### daemon.toml（可选）

位于 `$PECO_HOME/daemon.toml`，可配监听地址、退出策略、TLS 证书/私钥路径等；未知字段一律忽略。对应环境变量优先级更高。

## 技术栈

| 层 | 技术 |
|----|------|
| Agent 引擎 | Rust (Tokio async) |
| 后端 daemon | Axum + axum-server + SQLite (sqlx) |
| 本地认证 | 密码 + 会话 Cookie（setup token 引导） |
| LLM 抽象 | async-trait 动态分发（DeepSeek / Qwen / OpenAI） |
| RAG 引擎 | LanceDB + FastEmbed (ONNX) + BM25 |
| MCP 协议 | rmcp (Stdio + SSE + StreamableHTTP) |
| 前端 | React 19 + TypeScript + Vite |
| UI | Tailwind CSS v4 + shadcn/ui (Radix) |
| 状态管理 | Zustand + TanStack Query |
| 部署 | systemd --user |

## 开发

```bash
# 运行所有测试
cargo test --workspace

# 运行特定 crate 测试
cargo test -p peco-core
cargo test -p peco-core -- --nocapture          # 显示测试输出
cargo test -p peco-daemon
cargo test -p knowledge-base

# 前端测试 / 类型检查 / 构建
cd peco-webui && npm run test
cd peco-webui && npx tsc -b
cd peco-webui && npm run build

# 格式化与 Lint
cargo fmt --all
cargo clippy --workspace -- -D warnings
```

## 实现路线

- [x] 多工作空间 daemon（团队 / 空间 / 回收站 / 任务台账 / 唤醒 / 单实例锁）
- [x] Agent 引擎（单状态机 ReAct + 流式/batch + 统一重试）
- [x] 声明式 Workflow 引擎（DAG + 模板 + 条件 + Continue/Abort/Pause）
- [x] 永续会话压缩 + 记忆双路径 + 每空间独立记忆库
- [x] 混合检索 RAG（向量 + BM25 + 知识图谱）
- [ ] Workflow 高级特性（Llm/Tool 步骤类型、Retry 重试、StepDelta 增量事件、HumanApproval、子 Workflow 嵌套）
- [ ] Anthropic / Ollama / Groq Provider 完整实现
- [ ] 结构化输出（Structured Output Executor）
- [ ] MCP WebSocket 传输

## License

MIT OR Apache-2.0

---

**[English Version](#english-version)** | **[中文版](#peco--本地多工作空间-ai-agent-平台)**

---

<h2 id="english-version">Peco — Local Multi-Workspace AI Agent Platform</h2>
<h3><a href="#peco--本地多工作空间-ai-agent-平台">← 中文版</a></h3>

A local AI Agent platform built on **Rust + React**. Single-user, single-process, bound to loopback, organizing Agent resources into **teams → workspaces**. Provides Agent definition & orchestration, MCP protocol integration, multimodal RAG knowledge base, visual chat, task dispatch, and cron-based scheduling.

### Architecture

```
┌──────────────────────────────────────────────────────────────┐
│              peco-webui (React 19 + TypeScript)               │
│   Peco · Teams · Spaces · Chat · Schedules · Settings (SSE)   │
├──────────────────────────────────────────────────────────────┤
│                 peco-daemon (Axum + Tokio)                    │
│  Local single-user · Multi-workspace · Task ledger · Static   │
├──────────────────────────────────────────────────────────────┤
│                    peco-core (Agent Engine)                    │
│ Agent · Session · ReAct · Workflow · WorkSpace · MCP · Skills · Tools · KB │
├────────────────────────┬─────────────────────────────────────┤
│     model-provider      │         knowledge-base               │
│    LLM Abstraction      │    RAG: Vector + BM25 + Graph        │
├────────────────────────┴─────────────────────────────────────┤
│   SQLite · LanceDB · FastEmbed · DeepSeek/Qwen/OpenAI · MCP   │
└──────────────────────────────────────────────────────────────┘
```

`peco-core` is a transport-agnostic engine; `peco-daemon` is the only backend, wiring the engine into local HTTP/SSE and owning workspace lifecycle, the task ledger, and the single-user identity; `peco-webui` is the React frontend whose build can be served by the daemon (same-origin deployment).

### Key Features

- **Multi-workspace organization**: three-tier Peco / team / workspace model, per-space resource isolation under single-user identity `local`, lazy space open, trash/restore/destroy, SQLite task ledger with automatic Peco wakeup, single-instance lock
- **Agent Engine**: declarative `agent.md` definitions; single-state-machine ReAct loop (`LooperState`) with streaming + batch paths and unified retry (backoff + output-budget raise); DAG workflow orchestration (`workflow.md` with topology parallelism, condition gating, minijinja templates, Continue/Abort/Pause policies); sub-agent delegation; state-machine Session management with rollback and interrupt queue; built-in workspace templates
- **Tool System (41 built-in)**: shell, fetch, web_search, full CRUD for Agents/Skills/Workflows/MCP servers, KB tools, cross-space dispatch tools, scheduling; full MCP protocol (Stdio + SSE + StreamableHTTP) with auto discovery & sync; extensible `Tool`/`ToolDyn` design with `#[peco_tool]` macro
- **Perpetual Context & Memory**: turn-boundary rolling compaction into a pinned summary; single truncation point; memory dual-path (background extraction hook + per-query recall with casual-query gating) over a per-space memory store
- **RAG Knowledge Base**: multi-format parsing, deterministic chunk IDs for idempotent ingestion, hybrid search (vector + BM25 + knowledge graph) with 4-layer adaptive RRF fusion, local ONNX embeddings (`bge-base-zh-v1.5`, 768-dim), three backends (InMemory/LanceDB/HelixDB)
- **Skill System**: 3-tier progressive loading, `SKILL.md` format, automatic directory discovery
- **Frontend**: Peco panel, teams, spaces (incl. trash), SSE streaming chat, cron schedules, settings; local auth (setup-token registration + password/session)
- **Operations**: one-command systemd `--user` deploy via `scripts/deploy-daemon.sh`, same-origin static serving via `PECO_WEBUI_DIST`, graceful shutdown

### Quick Start

**Prerequisites**: Rust 1.85+, Node.js 22+, an LLM API key (e.g. DeepSeek).

```bash
# 1. Clone and write providers.toml under the daemon data root
git clone <repo-url> peco && cd peco
mkdir -p "${PECO_HOME:-$HOME/.peco-daemon}"
cat > "${PECO_HOME:-$HOME/.peco-daemon}/providers.toml" <<'EOF'
default_provider = "deepseek"
[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"
[providers.deepseek.default]
model = "deepseek-flash"
EOF
export DEEPSEEK_API_KEY=sk-your-key-here

# 2. Development (two terminals)
cargo run -p peco-daemon                               # backend on 127.0.0.1:6971
cd peco-webui && npm install && npm run dev            # frontend on :5174 (proxies /api)
# First visit: register a local account using the setup token in ~/.peco-daemon/token

# 3. Production (systemd --user)
cd peco-webui && npm install && npm run build && cd ..
bash scripts/deploy-daemon.sh install                  # builds backend, installs unit
bash scripts/deploy-daemon.sh status
# Daemon serves on http://127.0.0.1:6971 with the frontend hosted same-origin.
```

### Tech Stack

| Layer | Technology |
|-------|-----------|
| Agent Engine | Rust (Tokio async) |
| Backend daemon | Axum + axum-server + SQLite (sqlx) |
| Local Auth | Password + session cookie (setup-token bootstrap) |
| LLM Abstraction | async-trait dynamic dispatch (DeepSeek / Qwen / OpenAI) |
| RAG Engine | LanceDB + FastEmbed (ONNX) + BM25 |
| MCP Protocol | rmcp (Stdio + SSE + StreamableHTTP) |
| Frontend | React 19 + TypeScript + Vite |
| UI | Tailwind CSS v4 + shadcn/ui (Radix) |
| State | Zustand + TanStack Query |
| Deploy | systemd --user |

### Development

```bash
cargo test --workspace              # All tests
cargo test -p peco-core -- --nocapture  # With output
cd peco-webui && npm run test        # Frontend tests
cd peco-webui && npx tsc -b          # TypeScript check
cargo fmt --all                      # Format Rust
cargo clippy --workspace -- -D warnings  # Lint
```

### Roadmap

- [x] Multi-workspace daemon (teams / spaces / trash / task ledger / wakeup / single-instance lock)
- [x] Agent engine (single-state-machine ReAct + streaming/batch + unified retry)
- [x] Declarative Workflow engine (DAG + templates + conditions + Continue/Abort/Pause)
- [x] Rolling compaction + memory dual-path + per-space memory stores
- [x] Hybrid RAG (vector + BM25 + knowledge graph)
- [ ] Workflow advanced features (Llm/Tool step types, Retry, StepDelta streaming, HumanApproval, nested workflows)
- [ ] Anthropic / Ollama / Groq full provider implementations
- [ ] Structured Output Executor
- [ ] MCP WebSocket transport

### License

MIT OR Apache-2.0
