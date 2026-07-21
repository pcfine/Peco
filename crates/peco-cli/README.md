# peco-cli — 命令行 AI 助手

基于 [peco-core](../peco-core/) Agent 引擎的交互式终端 AI 助手。通过 WorkSpace 统一管理 Agent、Skills 和知识库，在终端中直接与 AI 对话。

## 快速开始

```bash
# 构建
cargo build -p peco-cli

# 设置 API Key
export DEEPSEEK_API_KEY=sk-your-key-here

# 启动（使用默认 workspace 和 Agent）
cargo run -p peco-cli
```

## 架构

```
┌────────────────────────────────────────────┐
│                 peco-cli                    │
│         REPL Loop · Console Renderer        │
├────────────────────────────────────────────┤
│              peco-core                      │
│   WorkSpace · Agent · ReAct Loop · Session  │
├──────────────┬──────────────┬──────────────┤
│   agents/    │   skills/    │  knowledge/   │
│  agent.md   │  SKILL.md   │  LanceDB +    │
│             │              │  FastEmbed    │
└──────────────┴──────────────┴──────────────┘
```

peco-cli 与 [peco-server](../peco-server/) 共享同一个 `peco-core` 引擎层，区别在于它省去了 Web 服务、数据库和前端层，直接通过终端提供完整的 Agent 体验。

## WorkSpace 目录结构

peco-cli 使用 **WorkSpace** 管理所有 Agent、Skill 和知识库。默认 workspace 根目录为当前目录（`./`），可通过 `--workspace` / `-w` 指定。

```
<workspace>/
├── agents/                          # Agent 定义目录
│   ├── personal-assistant/
│   │   └── agent.md                 # Agent 配置 + System Prompt
│   └── code-reviewer/
│       └── agent.md
├── skills/                          # Skill 定义目录
│   ├── code-review/
│   │   └── SKILL.md                 # Skill 配置 + 提示词
│   └── web-research/
│       └── SKILL.md
├── knowledge/                       # 知识库数据（LanceDB）
├── providers.toml                   # LLM Provider 配置（可选）
└── mcpconfig.json                   # MCP 服务器配置（可选）
```

仓库中的 [`local-space/`](local-space/) 目录包含了一个预配置的 workspace，可作为参考或直接使用：

```bash
cd crates/peco-cli/local-space
cargo run -p peco-cli
```

## CLI 参数

| 参数 | 环境变量 | 默认值 | 说明 |
|------|---------|--------|------|
| `-a`, `--agent` | `PECO_AGENT_PATH` | `personal-assistant` | Agent 名称（从 workspace 的 `agents/` 目录加载）或 `agent.md` 文件路径 |
| `-w`, `--workspace` | `PECO_WORKSPACE` | `./` | WorkSpace 根目录 |
| `-s`, `--session` | — | — | 恢复指定 ID 的会话 |
| `--no-persist` | — | false | 禁用会话持久化 |
| `--list-sessions` | — | — | 列出已保存的会话并退出 |
| `--sessions-dir` | `PC_AGENT_SESSIONS_DIR` | — | 会话存储目录 |
| `--no-color` | `NO_COLOR` | false | 禁用彩色输出 |
| `--show-reasoning` | — | true | 显示模型推理过程 |
| `--show-tools` | — | true | 显示工具调用详情 |

## Agent 定义

每个 Agent 对应 `agents/<name>/agent.md` 文件，格式为 YAML frontmatter + Markdown 正文：

```yaml
---
agent:
  name: "my-assistant"
  description: "我的个人助手"
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
  temperature: 0.7
  max_tokens: 4096
  stream: true
tools:
  - shell
  - fetch
  - search_knowledge
skills:
  - code-review
mcp:
  - helixdb-docs
max_turns: 30
---

# System Prompt

你是一个专业的 AI 助手...
```

### 工具列表

| 工具名 | 说明 |
|--------|------|
| `shell` | 执行终端命令 |
| `fetch` | 获取网页内容 |
| `read_skill` | 读取已注册的 Skill 内容 |
| `delegate_sub_agent` | 委派子 Agent 串行执行 |
| `run_parallel_sub_agents` | 启动多个子 Agent 并行执行 |
| `search_knowledge` | 搜索知识库 |
| `list_knowledge_bases` | 列出知识库 |
| `add_to_knowledge_base` | 向知识库添加文档 |
| `sync_knowledge_base` | 同步知识库 |
| `get_knowledge_base_docs` | 获取知识库文档列表 |

## 使用示例

```bash
# 使用默认 workspace（./）和默认 Agent（personal-assistant）
peco

# 使用指定的 Agent 名称（从 workspace 的 agents/ 目录加载）
peco --agent code-reviewer

# 直接指定 agent.md 文件路径（向后兼容）
peco --agent /path/to/my-agent.md

# 指定 workspace 目录
peco --workspace ~/my-peco-workspace --agent personal-assistant

# 恢复之前的会话
peco --session <session-id>

# 列出已保存的会话
peco --list-sessions
```

## 终端快捷键

| 快捷键 | 功能 |
|--------|------|
| `Ctrl+D` | 退出 CLI |
| `Ctrl+C` | 中断当前输入 |
| ↑ / ↓ | 浏览历史命令 |

## 斜杠命令

| 命令 | 说明 |
|------|------|
| `/help`、`/h`、`/?` | 显示帮助 |
| `/exit`、`/quit`、`/q` | 退出 CLI |

## Provider 配置

LLM Provider 通过 `providers.toml` 配置。WorkSpace 内的配置（`<workspace>/providers.toml`）优先级高于系统级配置（`~/.config/peco/providers.toml`）。

```toml
default_provider = "deepseek"

[providers.deepseek]
type = "deepseek"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com/v1"

[providers.deepseek.default]
model = "deepseek-v4-flash"
temperature = 0.7
max_tokens = 4096
stream = true
```

## 与 peco-server 的关系

| | peco-cli | peco-server |
|---|---|---|
| 运行方式 | 终端 REPL | HTTP 服务 |
| 用户界面 | 终端彩色输出 | React Web UI |
| 用户管理 | 单用户 | 多用户 + JWT 认证 |
| 数据存储 | 文件系统 | SQLite + 文件系统 |
| Agent 引擎 | peco-core | peco-core |
| 适用场景 | 开发调试、个人使用 | 生产部署、团队协作 |
