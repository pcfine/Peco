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
│   ├── @assistant/
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
| `-t`, `--init-template` | `PECO_INIT_TEMPLATE` | — | 从内置模板初始化 workspace（personal / minimal / developer） |
| `-w`, `--workspace` | `PECO_WORKSPACE` | `./` | WorkSpace 根目录 |
| `--no-color` | `NO_COLOR` | false | 禁用彩色输出 |
| `--show-reasoning` | — | true | 显示模型推理过程 |
| `--show-tools` | — | true | 显示工具调用详情 |

启动时通过终端交互菜单选择 Agent 和 Session，无需通过命令行指定。

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

CLI 中 Agent 可用的工具由 workspace 中 agent.md 的 `tools` 字段声明。完整工具列表参见 peco-core 的 `ToolRegister`（26 个工具）：

| 分类 | 工具 |
|------|------|
| 通用 | `shell`, `fetch`, `show_workspace` |
| Agent | `delegate_sub_agent`, `run_parallel_sub_agents`, `save_agent`, `read_agent`, `delete_agent` |
| Skill | `read_skill`, `list_skills`, `save_skill`, `delete_skill` |
| Workflow | `execute_workflow`, `list_workflows`, `save_workflow`, `delete_workflow` |
| MCP | `list_mcp_servers`, `save_mcp_server`, `delete_mcp_server` |
| 知识库 | `search_knowledge`, `list_knowledge_bases`, `add_to_knowledge_base`, `sync_knowledge_base`, `get_knowledge_base_docs`, `add_facts_to_knowledge_base`, `query_entity_facts` |

## 使用示例

```bash
# 初始化 workspace（从内置模板，首次使用推荐）
peco -t personal       # 个人助手
peco -t developer      # 开发辅助
peco -t minimal        # 最轻量对话

# 使用默认 workspace（./）启动交互式对话
peco

# 指定 workspace 目录
peco --workspace ~/my-peco-workspace

# 禁用彩色输出和工具显示
peco --no-color --show-tools=false
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
