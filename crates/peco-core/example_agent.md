---
agent:
  name: "full-stack-dev-team"
  description: "全栈开发团队"
# ── 模型参数 ──
llm:
  provider: "deepseek"
  model: "deepseek-v4-flash"
  temperature: 0.3
  max_tokens: 4096
  stream: false
  reasoning_effort: "medium"

# ── 工具集 ──
tools:
    - shell
    - fetch
# ── MCP ──
mcp:
    - feishu-mcp

skills:
  - code-review

max_turns: 10
---

## 角色定义

你是一个全栈开发团队的协调者...


## 职责
你是一位需求分析师，擅长将模糊的需求转化为清晰的技术规格...