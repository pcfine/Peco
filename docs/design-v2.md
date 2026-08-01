# Peco 架构重构设计文档 v2

> 作者：Peco Team & Claude
> 日期：2026-07-31
> 状态：草稿 — 待讨论确认后进入实现阶段

---

## 目录

1. [重构动机](#1-重构动机)
2. [新导航与信息架构](#2-新导航与信息架构)
3. [前后端模块映射](#3-前后端模块映射)
4. [后端变更详述](#4-后端变更详述)
5. [前端变更详述](#5-前端变更详述)
6. [数据层变更](#6-数据层变更)
7. [废弃模块清单](#7-废弃模块清单)
8. [实现路线图](#8-实现路线图)
9. [行业对标与补充设计](#9-行业对标与补充设计)

---

## 1. 重构动机

### 1.1 当前问题

| # | 问题 | 影响 |
|---|------|------|
| P1 | `ensure_omni_agent()` 在 Rust 代码中硬编码 Agent 定义 | 违背「agent.md 是唯一真相源」原则；修改需重新编译 |
| P2 | 两个"个人助理"模块并存 (`assistant::PersonalAssistantManager` + `personal_agent::PersonalAgentManager`) | 代码重复，维护双份逻辑 |
| P3 | 前端「个人助理」和「对话」是两个独立入口，功能高度重叠；且无对话历史列表 | 用户困惑，UI 碎片化；无法回溯历史对话 |
| P4 | Provider、Skill、MCP 配置在前端完全没有管理入口 | 只能通过编辑文件配置，门槛高 |
| P5 | 导航无层级，6 个平级菜单 | 随着功能增长会越来越臃肿 |
| P6 | Conversation 持久化存在但未有效利用 | 对话历史能力闲置，用户无法管理多轮对话 |

### 1.2 设计原则

1. **文件是唯一真相源** — Agent 定义始终来自 `agent.md`，server 不硬编码任何 agent
2. **peco-agents 模板统一入口** — 新用户初始化通过 `BuiltinTemplate::personal()` 安装默认 agents
3. **配置收敛到「管理」** — Provider/Agent/Skill/MCP/Knowledge 均为用户 workspace 级别配置，一个用户不可见另一个用户的配置
4. **代码保留，功能移除** — 废弃的模块保留代码并标注 `#[deprecated]`，不直接删除
5. **渐进迁移** — 第一版聚焦核心结构调整，Workflow 模块留作后续独立规划
6. **对话全量持久化** — 所有对话（Peco 永续对话 + Agent 对话）均持久化，每个 Agent 下对话上限 100 条
7. **LLM 辅助创建** — Agent 创建由大模型辅助生成 agent.md，降低手动编写门槛
8. **`@` 前缀即系统 Agent** — 以 `@` 开头的 Agent 为 Peco 内置系统 Agent（如 `@assistant`、`@memory`），不可见、不可编辑、不可删除，由 peco-agents 模板管理

---

## 2. 新导航与信息架构

### 2.1 一级导航（侧边栏）

```
┌──────────────┐
│ ✨  Peco     │  ← 登录后首页，永续对话（原 /personal）
├──────────────┤
│ 💬  对话     │  ← 折叠菜单，按 Agent 分组展示历史对话列表
│   ├ developer│
│   │  ├ 修bug │
│   │  └ 写周报 │
│   ├ code-reviewer│
│   │  └ 重构  │
│   └ ...      │
├──────────────┤
│ ⚙  管理     │  ← 折叠菜单，含 Provider / Agent / Skill / MCP / KnowledgeBase
│   ├ Provider │
│   ├ Agent    │
│   ├ Skill    │
│   ├ MCP      │
│   └ Knowledge│
├──────────────┤
│ 📋  任务     │  ← 定时任务 + Workflow（本次仅保留占位，后续详细设计）
├──────────────┤
│ 🔧  设置     │  ← 用户信息、系统偏好（原 /settings）
└──────────────┘
```

**设计约束：**
- 一级菜单两个中文字（"对话"、"管理"、"任务"、"设置"），品牌页保留英文 "Peco"
- **「对话」折叠菜单**：按 Agent 分组展示历史对话列表。点击 Agent 名称跳转该 Agent 的对话页（`/chat/:agentId`）；点击具体对话项跳转到该 Agent 下的特定对话（`/chat/:agentId/:conversationId`）
- **「管理」折叠菜单**：每个子项英文名 + 图标，风格一致。所有子项均为用户 workspace 级别配置（Provider 和 MCP 也是用户级别，非全局），不同用户之间隔离
- **侧边栏折叠后仅显示图标**；对话历史列表在折叠状态下无法直接访问（通过 tooltip 提示）
- 对话列表按最近活跃时间降序排列；超过上限（100条/Agent）时最旧的对话自动归档（隐藏但可恢复）
- KnowledgeBase 归入"管理"大类（它是可独立管理的资源，不是独立对话入口）

### 2.1.1 空状态处理

「对话」菜单**始终显示**（不因无对话而隐藏），根据内部状态切换展示：

```
┌─ 无对话、无 Agent ─────────────────┐
│ 💬  对话                       ▾   │
│   ─────────────────────────────    │
│   💡 暂无对话                       │
│   前往「管理 > Agent」创建第一个    │
│   Agent，然后在这里开始对话         │
│   ─────────────────────────────    │
└────────────────────────────────────┘

┌─ 无对话、有 Agent ─────────────────┐
│ 💬  对话                       ▾   │
│   ─────────────────────────────    │
│   developer               + 新对话 │
│   code-reviewer           + 新对话 │
│   ─────────────────────────────    │
└────────────────────────────────────┘

┌─ 有对话（正常状态）─────────────────┐
│ 💬  对话                       ▾   │
│   ─────────────────────────────    │
│   developer                        │
│     📝 修 bug              2h 前   │
│     📝 写周报              昨天    │
│   code-reviewer                    │
│     📝 重构项目            3天前   │
│   ─────────────────────────────    │
│   📦 已归档 (3)                    │
└────────────────────────────────────┘
```

**三种状态的切换逻辑：**

| 状态 | 条件 | 展示内容 |
|------|------|----------|
| **空：无 Agent** | workspace 中 0 个 Agent | 引导文案：「前往管理 > Agent 创建第一个 Agent」 |
| **空：有 Agent 无对话** | ≥1 个 Agent，但 0 条活跃对话 | 列出所有 Agent，每个附带「+ 新对话」按钮 |
| **正常** | ≥1 条活跃对话 | 按 Agent 分组展示对话列表 |

**关键交互规则：**

1. **「对话」默认展开/折叠**：无对话时默认折叠（减少空白面积）；有对话时默认展开（方便快速切换）
2. **Agent 列表来源**：空状态下的 Agent 列表来自 `GET /api/agents`，但**自动过滤以 `@` 开头的系统 Agent**（`@assistant`、`@memory` 及未来扩展的 `@xxx`）。系统 Agent 不参与对话列表展示，详见 [2.1.2](#212-前缀系统-agent-约定)
3. **「+ 新对话」按钮**：点击 → 调用 `POST /api/chat/:agentId/conversations` → 跳转到 `/chat/:agentId/:newConversationId`
4. **新建 Agent 后的即时更新**：用户在「管理 > Agent」中创建新 Agent 后，回到「对话」时应立即看到新 Agent 出现在空状态列表中（通过 React Query cache invalidation）
5. **Peco 永续对话不在「对话」列表中**：它始终通过顶部「Peco」入口访问，不与 Agent 对话混排



### 2.1.2 `@` 前缀系统 Agent 约定

以 `@` 开头的 Agent 名称为**系统内置 Agent**，由 peco-agents 模板安装，是 Peco 项目的组成部分。它们与用户创建的 Agent 有本质区别：

| 特性 | 系统 Agent（`@xxx`） | 用户 Agent（普通名称） |
|------|---------------------|----------------------|
| **来源** | peco-agents 模板（`BuiltinTemplate`）安装 | 用户手动创建或导入 |
| **可见性** | 不在「管理 > Agent」列表中展示 | 正常展示 |
| **编辑** | ❌ 禁止（`agent.md` 为只读） | ✅ 允许 |
| **删除** | ❌ 禁止（受保护） | ✅ 允许 |
| **出现在侧边栏「对话」** | ❌ 不展示 | ✅ 展示（空状态 + 对话列表） |
| **可被对话使用** | ✅ Peco 永续对话使用 `@assistant`；`@memory` 由 `@assistant` 工具调用 | ✅ 正常使用 |
| **命名规则** | 以 `@` 开头，如 `@assistant`、`@memory`、`@planner` | 不以 `@` 开头 |

**当前内置系统 Agent：**

| Agent | 职责 | 使用方式 |
|-------|------|----------|
| `@assistant` | Peco 永续对话的默认 Agent | `/peco` 入口自动使用，不支持独立对话 |
| `@memory` | 记忆管理（工具调用） | 由 `@assistant` 在 ReAct 循环中通过 `remember`/`recall`/`forget` 工具调用 |

**扩展机制：**

后续版本的 peco-agents 模板可以新增更多 `@` Agent（如 `@planner`、`@reviewer`），通过 `BuiltinTemplate` 版本升级自动安装。用户无需手动操作。

**后端保护：**

```rust
// crates/peco-server/src/agent/handler.rs

/// 判断是否为系统 Agent（名称以 @ 开头）
fn is_system_agent(name: &str) -> bool {
    name.starts_with('@')
}

// 在 update_agent / delete_agent handler 中：
if is_system_agent(&agent_name) {
    return Err(AppError::forbidden("系统 Agent 不可修改或删除"));
}

// 在 list_agents handler 中：
let visible_agents = agents
    .into_iter()
    .filter(|a| !is_system_agent(&a.name))
    .collect();
```

**前端过滤：**

侧边栏「对话」和「管理 > Agent」页面均过滤 `@` 前缀 Agent，用户无感知系统 Agent 的存在。唯一感知点：Peco 顶部入口的副标题「由 @assistant 驱动」。

### 2.2 路由表

| 路由 | 页面 | 对应原来 | 说明 |
|------|------|----------|------|
| `/` | 重定向 → `/peco` | `/` → `/personal` | |
| `/peco` | PecoChatPage | `/personal` | 统一聊天入口（永续对话），继承 ChatDetailPage 的完整 UI |
| `/chat/:agentId` | AgentChatPage | 新设计 | 与指定 Agent 的对话页（展示该 Agent 下的对话列表 + 对话区） |
| `/chat/:agentId/:conversationId` | AgentChatPage | 新设计 | 指定 Agent 的特定历史对话 |
| `/manage/providers` | ProviderListPage | **新增** | 模型供应商列表（用户 workspace 级别） |
| `/manage/agents` | AgentListPage | `/agents` | Agent 管理（`@` 前缀系统 Agent 不可见） |
| `/manage/agents/new` | AgentCreatePage | `/agents/new` | 创建 Agent 引导页（引导用户在 Peco 对话中通过 @assistant 创建，无独立表单） |
| `/manage/agents/:id/edit` | AgentEditPage | `/agents/:id/edit` | `@` Agent 禁止进入编辑页（后端返回 403） |
| `/manage/skills` | SkillListPage | **新增** | Skill 列表 |
| `/manage/skills/:name` | SkillDetailPage | **新增** | Skill 详情/编辑 |
| `/manage/mcp` | McpConfigPage | **新增** | MCP 服务器配置（用户 workspace 级别） |
| `/manage/knowledge` | KnowledgeListPage | `/knowledge` | 知识库管理 |
| `/manage/knowledge/:kbId` | KnowledgeDetailPage | `/knowledge/:kbId` | |
| `/tasks` | TaskListPage | `/tasks` | 任务列表（本次仅保留占位） |
| `/settings` | SettingsPage | `/settings` | |

### 2.3 对话模型：全量持久化 + 容量上限

#### 2.3.1 两种对话类型

| | Peco 永续对话 (/peco) | Agent 对话 (/chat/:agentId) |
|---|---|---|
| Agent | 固定 @assistant（模板安装） | 任意已配置的 Agent |
| Session | 持久化（`session_snapshots` 表） | 持久化（`conversations` + `messages` + `session_snapshots` 表） |
| 对话数量 | 1 条（唯一） | 每个 Agent 上限 **100 条** |
| 超限策略 | 不适用 | 最旧对话自动归档（软删除，可手动恢复）；创建新对话时若已达上限，提示用户清理 |
| 记忆 | @memory Agent（工具调用） | 无（纯对话，不触发记忆提取） |
| PPA 钩子 | 预留后续接入 | 不需要 |
| UI | 完整 ChatView 组件 | 复用同一个 ChatView 组件 |
| 侧边栏可见 | 「对话」区域顶部固定项 | 「对话」区域按 Agent 分组展示 |

#### 2.3.2 对话生命周期

```
┌──────────┐    发送第一条消息    ┌──────────┐   用户删除/自动归档   ┌──────────┐
│  (无对话) │ ─────────────────→ │  活跃    │ ──────────────────→ │  已归档  │
└──────────┘                    └──────────┘                     └──────────┘
                                     │                                  │
                                     │ 继续对话                          │ 手动恢复
                                     ↓                                  ↓
                                ┌──────────┐                      ┌──────────┐
                                │  活跃    │                      │  活跃    │
                                └──────────┘                      └──────────┘
```

- **创建**：用户在 Agent 对话页发送第一条消息时自动创建（标题由 LLM 生成首条消息摘要）
- **活跃**：正常对话，出现在侧边栏列表中
- **归档**：软删除（`archived_at` 字段标记），从前端默认视图隐藏
  - 自动归档：Agent 下对话数超过 100 条时，最早的活跃对话自动归档
  - 手动归档：用户可手动归档不再需要的对话
- **恢复**：已归档对话可手动恢复为活跃状态
- **真删除**：用户可永久删除对话（清空 `conversations` + `messages` + `session_snapshots` 记录）

#### 2.3.3 对话上限实现

```rust
// crates/peco-server/src/chat/conversation.rs

/// 每个 Agent 最大活跃对话数
const MAX_ACTIVE_CONVERSATIONS_PER_AGENT: usize = 100;

impl ConversationManager {
    /// 创建对话前检查是否超限。
    /// 若已达上限，自动归档最旧的 N 条活跃对话，确保创建成功。
    pub async fn auto_archive_oldest_if_needed(
        &self,
        user_id: &str,
        agent_name: &str,
    ) -> Result<usize, AppError> {
        let active_count = self.count_active(user_id, agent_name).await?;
        if active_count >= MAX_ACTIVE_CONVERSATIONS_PER_AGENT {
            let to_archive = active_count - MAX_ACTIVE_CONVERSATIONS_PER_AGENT + 1;
            self.archive_oldest(user_id, agent_name, to_archive).await?;
            Ok(to_archive)
        } else {
            Ok(0)
        }
    }
}
```

---

## 3. 前后端模块映射

### 3.1 后端 API 重组

```
/api/peco/                    ← 新：永续聊天入口（替代 /api/personal-agent）
  GET  /stream?message=       SSE 流式对话（perpetual session，持久化）
  GET  /session               Session 快照（含完整历史 turn）
  DELETE /session             清除会话（清空 session_snapshots 记录）
  POST /feedback              提交反馈（👍/👎）
  GET  /session/export        导出永续对话（?format=json|markdown）

/api/chat/                    ← 重构：Agent 对话（替代 /api/conversations）
  GET  /:agentId/conversations        列出某 Agent 下的对话列表
  POST /:agentId/conversations        创建新对话
  GET  /:agentId/conversations/:id    获取对话详情（含消息历史）
  PATCH /:agentId/conversations/:id   更新对话（重命名、归档/恢复）
  DELETE /:agentId/conversations/:id  永久删除对话
  GET  /:agentId/conversations/:id/stream?message=  SSE 流式对话（持久化）
  POST /:agentId/conversations/:id/feedback         提交反馈 {message_id, rating, comment?}
  GET  /:agentId/conversations/:id/export           导出对话（?format=json|markdown）

/api/providers/               ← 新：Provider 配置管理（用户 workspace 级别）
  GET  /                      列表
  PUT  /:name                 创建或更新
  DELETE /:name               删除
  POST /:name/test            测试连接

/api/agents/                  ← 增强：新增 POST /api/agents 直接创建
  POST /                      直接创建 agent.md（手动表单，不走对话）

/api/skills/                  ← 新：Skill 管理
  GET  /                      列出所有 skills
  GET  /:name                 获取 Skill 详情
  PUT  /:name                 创建或更新
  DELETE /:name               删除 Skill
  GET  /:name/export          导出为 .zip
  POST /import                从 .zip/SKILL.md 导入
/api/mcp/                     ← 新：MCP 配置管理（用户 workspace 级别）
  GET  /                      获取 mcp_config.json 内容
  PUT  /                      全量更新 mcp_config.json
  POST /:name/test            测试单个 MCP 服务器连接
/api/knowledge/               ← 已有，保持不变
/api/auth/                    ← 已有，保持不变
/api/tasks/                   ← 已有，保留但暂时不在导航中显示
/api/usage/                   ← 新：Token 用量统计
  GET  /summary?period=7d    用量摘要（token 总数、预估成本、按 Agent 拆分）
```

### 3.2 前端页面与 API 对应

| 前端页面 | 使用的 API |
|----------|-----------|
| PecoChatPage | `/api/peco/stream`, `/api/peco/session` |
| Sidebar (对话列表) | `GET /api/chat/:agentId/conversations` |
| AgentChatPage | `/api/chat/:agentId/conversations/`, `/api/chat/:agentId/conversations/:id/stream` |
| ProviderListPage | `/api/providers/` |
| AgentListPage | `/api/agents/` |
| AgentCreatePage | 对话式入口 → Peco 对话页（@assistant + `save_agent`）；手动入口 → `POST /api/agents` |
| AgentEditPage | `GET/PUT /api/agents/:id` |
| SkillListPage | `/api/skills/`（含 GET/PUT/DELETE/EXPORT/IMPORT） |
| McpConfigPage | `/api/mcp/` |
| KnowledgeListPage | `/api/knowledge/` |
| TaskListPage | `/api/tasks/` |
| SettingsPage | 无需 API（仅 authStore） |

---

## 4. 后端变更详述

### 4.1 统一聊天入口：`/api/peco`

**合并** `personal_agent` + (精简后的) `chat` 功能：

- 使用 `BuiltinTemplate::personal()` 模板 → `@assistant` + `@memory`
- LooperConfig 使用 `PersonalAgentMessageFilter`（纯工具模式，无 PPA 钩子）
- Session ID 格式：`{user_id}-private-session`（沿用现有）
- 前端使用完整版聊天 UI（含工具调用卡片、推理过程、子 Agent 可视化）

#### 4.1.1 为 PPA 钩子预留改造空间

首版不加 PPA 钩子，但 `PecoConfig` 结构体预留注入点，后续接入时无需改动 handler 核心逻辑：

```rust
// crates/peco-server/src/peco/config.rs

use std::sync::Arc;
use peco_core::agent::{DynamicContext, LooperConfig};
use peco_core::agent::hooks::LooperHook;

/// Peco 对话配置（可扩展）。
///
/// 首版所有可选字段为 None，后续接入 PPA 时只需填充对应字段。
pub struct PecoConfig {
    /// 事件通道缓冲区大小
    pub event_buffer: usize,
    /// 每轮超时
    pub per_turn_timeout_secs: u64,
    /// 总超时
    pub total_timeout_secs: u64,
    /// 历史消息滑动窗口大小
    pub max_history_messages: usize,

    // ── 以下为 PPA / 可观测性钩子预留 ──────────────────────
    /// 动态上下文（读路径）：每次用户 query 前自动检索并注入。
    /// 后续接入 PPA 时设为 `Some(Arc::new(PpaDynamicContext::new(...)))`。
    pub dynamic_context: Option<Arc<dyn DynamicContext>>,
    /// Looper 钩子（写路径）：每轮完成后触发记忆提取、token 用量记录等。
    /// 后续接入 PPA 时填充 `vec![Arc::new(PpaMemoryHook::new(...))]`。
    /// 后续接入可观测性时填充 `vec![Arc::new(MetricsCollector::new(pool))]`。
    /// 钩子按注册顺序执行，相互独立。
    pub hooks: Vec<Arc<dyn LooperHook>>,
}

impl Default for PecoConfig {
    fn default() -> Self {
        Self {
            event_buffer: 256,
            per_turn_timeout_secs: 300,
            total_timeout_secs: 1800,
            max_history_messages: 10,
            dynamic_context: None,   // 后续接入 PPA
            hooks: Vec::new(),       // 后续接入 PPA
        }
    }
}

impl PecoConfig {
    /// 从 PecoConfig 构建 LooperConfig（复用构建逻辑，避免散落在 handler 中）。
    pub fn to_looper_config(&self, message_filter: ...) -> LooperConfig {
        LooperConfig {
            event_buffer: self.event_buffer,
            per_turn_timeout: Some(Duration::from_secs(self.per_turn_timeout_secs)),
            total_timeout: Some(Duration::from_secs(self.total_timeout_secs)),
            persist_on_failure: true,
            dynamic_context: self.dynamic_context.clone(),
            hooks: self.hooks.clone(),
            message_filter: Some(message_filter),
            ..LooperConfig::default()
        }
    }
}
```

后续接入 PPA 时，只需在 `PecoManager::new()` 中：
```rust
let config = PecoConfig {
    dynamic_context: Some(Arc::new(PpaDynamicContext::new(...))),
    hooks: vec![Arc::new(PpaMemoryHook::new(...))],
    ..PecoConfig::default()
};
```
handler 核心逻辑无需任何改动。

#### 4.1.2 文件结构

```
crates/peco-server/src/peco/           ← 新建模块
  mod.rs           模块声明
  config.rs        PecoConfig + Default + to_looper_config()
  manager.rs       合并 personal_agent::manager 逻辑（模板安装 + Agent 加载）
  handler.rs       GET /stream, GET /session, DELETE /session
  filter.rs        复用 PersonalAgentMessageFilter（从 personal_agent::filter 移入）
  session.rs       复用 private_session_id（从 personal_agent::session 移入）

crates/peco-server/src/lib.rs
  - pub mod personal_agent;          ← 废弃，逻辑移入 peco
  - pub mod assistant;               ← 废弃，保留代码
  - pub mod chat;                    ← 精简：移除 conversation CRUD，仅保留 SSE 映射
  + pub mod peco;                    ← 新增

路由注册变更：
  - .nest("/api/personal-agent", personal_agent::handler::router())
  - .nest("/api/conversations", chat::conversation_router())
  + .nest("/api/peco", peco::handler::router())
```

### 4.2 chat 模块：保留对话管理 + 新增容量控制

原 `/api/conversations` 端点迁移到 `/api/chat/:agentId/conversations`，语义从「通用对话 CRUD」变为「Agent 下的对话管理」。

**`chat/handler.rs` 变更：**

| 函数 / 逻辑 | 处理方式 |
|-------------|----------|
| `ensure_omni_agent()` | **直接删除** — 硬编码 Agent 定义违背设计原则 |
| `create_conversation()` | **保留重构** — 迁移到 agent 作用域下，新增上限检查 |
| `list_conversations()` | **保留重构** — 按 agent 筛选，支持分页和归档状态过滤 |
| `get_conversation()` | **保留** |
| `update_conversation()` | **保留增强** — 支持重命名、归档/恢复操作 |
| `delete_conversation()` | **保留增强** — 支持软删除（归档）和真删除两种模式 |
| `list_messages()` | **保留** |
| `create_message()` | **保留** |

**新增逻辑：**

| 端点 | 说明 |
|------|------|
| `GET /api/chat/:agentId/conversations` | 列出某 Agent 下用户的所有对话（活跃/已归档，分页） |
| `POST /api/chat/:agentId/conversations` | 创建新对话（自动检查上限，超限时自动归档最旧对话） |
| `PATCH /api/chat/:agentId/conversations/:id` | 更新对话元数据（重命名 `title`、归档 `archive`、恢复 `unarchive`） |
| `DELETE /api/chat/:agentId/conversations/:id` | 永久删除（清空关联 messages + session_snapshots） |
| `GET /api/chat/:agentId/conversations/:id/stream?message=` | SSE 流式对话（复用现有 SSE 映射逻辑，消息通过 query param 传递，与 `/api/peco/stream` 模式一致） |

**容量控制实现：**

```rust
// crates/peco-server/src/chat/conversation.rs

const MAX_ACTIVE_PER_AGENT: usize = 100;

async fn create_conversation(
    state: AppState,
    user: AuthUser,
    Path(agent_id): Path<String>,
    Json(body): Json<CreateConversationBody>,
) -> Result<Json<Conversation>, AppError> {
    // 1. 检查上限，自动归档超出的最旧对话
    let archived = conversation_manager
        .auto_archive_oldest_if_needed(&user.id, &agent_id)
        .await?;
    if archived > 0 {
        tracing::info!(user=%user.id, agent=%agent_id, archived, "auto-archived old conversations");
    }

    // 2. 创建新对话
    let conv = conversation_manager
        .create(&user.id, &agent_id, &body.title)
        .await?;

    Ok(Json(conv))
}
```

**保留不动并增强的部分：**
- `chat::sse` — SSE 事件映射（`ChatSseEvent`, `map_looper_event`, `UsageData`）被 peco 和 chat 共用
- `chat::handler` 中的 SSE 流式辅助逻辑

**子 Agent SSE 可视化逻辑提取：**

当前 `chat/handler.rs` 中约 180 行的子 Agent 事件映射逻辑（`SubAgentInfo` 注册表、`AgentCallStart`/`AgentCallEnd` 配对、`extract_sub_agent_result`）需要被 peco handler 和新的 AgentChatPage handler 共用。将其提取为 `chat::sse` 中的共享辅助函数：

```rust
// crates/peco-server/src/chat/sse.rs（新增）

/// 子 Agent 调用信息，在 ToolCallStart 阶段写入，ToolResult 阶段读取。
struct SubAgentInfo {
    call_id: String,
    agent_id: String,
    agent_name: String,
}

/// 从 LooperEvent::ToolCallStart 中解析子 Agent 调用信息。
/// - delegate_sub_agent：返回单个 SubAgentInfo，call_id = tool_call_id
/// - run_parallel_sub_agents：返回多个 SubAgentInfo，call_id = "{tool_call_id}:{index}"
fn parse_sub_agent_infos(
    tool_call_id: &str,
    tool_name: &str,
    arguments: &str,
    resolve_agent_id: impl Fn(&str) -> String,
) -> Vec<SubAgentInfo> { ... }

/// 从子 Agent tool result 中提取单个子 Agent 的输出。
fn extract_sub_agent_result(tool_result: &str, info: &SubAgentInfo, tool_name: &str) -> String { ... }
```

peco handler 和 chat handler 均调用这些共享函数，避免在三个地方各自实现。

**精简后的 `chat` 模块结构：**
```
crates/peco-server/src/chat/
  mod.rs           保留，导出 sse 子模块 + 注册对话路由
  sse.rs           增强 — SSE 事件类型 + LooperEvent 映射 + 子 Agent 可视化共享逻辑
  handler.rs       重构 — Agent 作用域下的对话 CRUD + SSE 流式
  conversation.rs  新增 — 对话上限检查、自动归档逻辑
```

### 4.3 废弃 `assistant::PersonalAssistantManager`

**不删除文件**，但代码标注废弃：

```rust
// crates/peco-server/src/assistant/mod.rs

//! ⚠️ DEPRECATED — 本模块不再被任何路由使用。
//!
//! 保留原因：
//! - `PersonalAssistantManager` 的 PPA 集成模式（DynamicContext + MemoryHook +
//!   MessageFilter 三位一体）是设计参考，后续可能迁移到 peco 模块
//! - `PersonalAssistantMessageFilter` 的区分当前轮/历史轮的策略比 personal_agent 更精细
//! - `build_ppa_components()` 展示了如何组装 PPA 读/写路径
//!
//! 当前活跃的聊天入口：`crate::peco::handler`
```

**具体操作：**

```rust
// 在 mod.rs 顶部添加
#![allow(dead_code)]
// 模块级注释改为 deprecation notice

// 在各 pub 项上标注
#[deprecated(since = "0.2.0", note = "use `crate::peco` instead")]
pub const PERSONAL_ASSISTANT_ID: &str = "personal_assistant";

#[deprecated(since = "0.2.0", note = "use `crate::peco` instead")]
pub struct PersonalAssistantManager { ... }
```

### 4.4 废弃 `personal_agent` 模块

`personal_agent` 的逻辑合并到 peco 后，原模块标注废弃：

```rust
// crates/peco-server/src/personal_agent/mod.rs

//! ⚠️ DEPRECATED — 本模块已合并到 `crate::peco`。
//!
//! 保留原因：
//! - `PersonalAgentMessageFilter` 可能被 peco 模块复用
//! - `session.rs` 中的私有会话 ID 生成逻辑仍被使用
//! - 作为从 peco-agents 模板加载 Agent 的参考实现
```

**但 `personal_agent::filter` 和 `personal_agent::session` 中的逻辑会被 peco 模块直接复用（通过 `pub(crate)` 可见性或复制）。**

实际上，更干净的做法是：
- `session.rs` → 移动到 `peco/session.rs`，删除原文件
- `filter.rs` → 移动到 `peco/filter.rs`，删除原文件
- `manager.rs` → 逻辑合并到 `peco/manager.rs`
- `handler.rs` → 标注废弃
- `mod.rs` → 标注废弃

### 4.5 新增 Provider 管理 API

**后端设计：**

```
crates/peco-server/src/provider/        ← 新建模块
  mod.rs
  handler.rs
```

**API 设计：**

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/providers` | 列出 providers.toml 中所有 provider |
| GET | `/api/providers/:name` | 单个 provider 详情 |
| PUT | `/api/providers/:name` | 创建或更新 provider |
| DELETE | `/api/providers/:name` | 删除 provider |
| POST | `/api/providers/:name/test` | 测试连接 |

**数据来源：** 直接读写用户 workspace 下的 `providers.toml` 文件（路径：`{user_workspace}/providers.toml`），通过 peco-core 的现有解析逻辑。

Provider 是**用户 workspace 级别**配置 — 每个用户拥有独立的 `providers.toml`，不同用户之间完全隔离。不进入 SQLite 索引层。

### 4.6 新增 Skill 管理 API

```
crates/peco-server/src/skill/           ← 新建模块
  mod.rs
  handler.rs
```

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/skills` | 列出用户 WorkSpace 中所有 skills |
| GET | `/api/skills/:name` | Skill 详情（SKILL.md 内容） |
| PUT | `/api/skills/:name` | 创建或更新 SKILL.md |
| DELETE | `/api/skills/:name` | 删除 Skill（移除整个 Skill 目录） |
| GET | `/api/skills/:name/export` | 导出 Skill 为 .zip（含 SKILL.md + scripts/ + references/ + assets/） |
| POST | `/api/skills/import` | 从上传的 .zip 或单个 SKILL.md 导入 Skill

Skill 纯文件管理，没有 DB 索引。后端直接通过 `SkillRegister` 或文件系统操作。

> **已知限制**：Skill 配置修改后，当前已加载的 Agent 不会自动感知变更。后续在 peco-core 的 WorkSpace 中添加 `SkillRegister` 热刷新功能后解决。当前需重启服务或等待 WorkSpace 缓存自然过期。

### 4.7 新增 MCP 配置管理 API

```
crates/peco-server/src/mcp_config/      ← 新建模块（注意不与 peco-core 的 mcp 模块混淆）
  mod.rs
  handler.rs
```

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/mcp` | 获取 mcp_config.json 内容 |
| PUT | `/api/mcp` | 全量更新 mcp_config.json |
| POST | `/api/mcp/:name/test` | 测试单个 MCP 服务器连接 |

操作对象：`{user_workspace}/mcp_config.json`（用户 workspace 级别配置，与 Provider 一致，每个用户独立隔离）。

> **已知限制**：MCP 配置修改后，已建立连接的 MCP session 不会自动重连。与 Skill 配置刷新问题相同，后续在 peco-core 的 WorkSpace 中统一解决热刷新。

### 4.8 Server 路由注册变更

**`lib.rs` 修改前后对比：**

```rust
// === 修改前 ===
pub mod agent;
pub mod assistant;
pub mod chat;
pub mod knowledge;
pub mod personal_agent;
pub mod personal_assistant;
pub mod task;

let protected_routes = Router::new()
    .nest("/api/agents", agent::router())
    .nest("/api/conversations", chat::conversation_router())
    .nest("/api/knowledge", knowledge::router())
    .nest("/api/tasks", task::router())
    .nest("/api/personal-agent", personal_agent::handler::router());

// === 修改后 ===
pub mod agent;
pub mod assistant;           // #[deprecated]
pub mod chat;                // 精简：仅 POST /:agentId/stream + sse 映射
pub mod knowledge;
pub mod mcp_config;          // 新增
pub mod peco;                // 新增
pub mod personal_agent;      // #[deprecated]，逻辑已移入 peco
pub mod personal_assistant;  // 保留（被废弃的 assistant 引用）
pub mod provider;            // 新增
pub mod skill;               // 新增
pub mod task;

let protected_routes = Router::new()
    .nest("/api/peco", peco::handler::router())
    .nest("/api/chat", chat::router())              // 精简后仅临时对话
    .nest("/api/providers", provider::router())
    .nest("/api/agents", agent::router())
    .nest("/api/skills", skill::router())
    .nest("/api/mcp", mcp_config::router())
    .nest("/api/knowledge", knowledge::router())
    .nest("/api/tasks", task::router());
```

### 4.9 LLM 辅助创建 Agent（对话式）

Agent 创建**不通过独立 API**，而是通过 Peco 对话由 `@assistant` 完成。`@assistant` 拥有 `save_agent` 工具，用户只需在对话中描述需求，`@assistant` 自动生成并保存 `agent.md`。

**交互流程：**

```
用户 → Peco 输入框
  │   "帮我创建一个代码审查助手，熟悉 Rust 和 TypeScript，
  │    会自动运行 cargo clippy 和 npx tsc"
  ↓
@assistant (ReAct 循环)
  │  1. 分析需求：代码审查、Rust/TS、静态检查
  │  2. 组装 agent.md 内容（YAML frontmatter + Markdown 系统提示词）
  │  3. 调用 save_agent(name="code-reviewer", content="---\nagent:\n  ...")
  │  4. 回复确认信息
  ↓
用户收到回复
    "已创建 Agent 'code-reviewer'。配置如下：
     - 模型：deepseek-v4-flash
     - 工具：shell, fetch
     - 系统提示词：专注 Rust/TypeScript 代码审查
     
     你可以在「对话」侧边栏找到它开始使用，或在「管理 > Agent」中编辑。"
```

**`@assistant` 需要的工具支持：**

`@assistant` 的 `tools` 列表中需包含 `save_agent`：

```yaml
# @assistant/agent.md
tools:
  - shell
  - fetch
  - delegate_sub_agent
  - save_agent              # ← 新增：允许 @assistant 创建/更新 Agent
  - search_knowledge
  - list_knowledge_bases
```

**与手动编辑的关系：**

- 对话式创建满足 80% 场景（用户描述 → @assistant 一步生成）
- 用户如需微调，去「管理 > Agent > 编辑」手动修改 agent.md（`AgentEditPage` 保留）
- 对话式**覆盖**手动创建：用户在 Peco 中说「帮我修改 code-reviewer，加上 knowledge_bases」，@assistant 调用 `save_agent` 更新已有 Agent

**对比 OpenAI Workspace Agents：**

| | OpenAI | Peco |
|---|---|---|
| 创建方式 | 自然语言描述 → 自动拆解 | 自然语言对话 → save_agent 工具 |
| 预览/调整 | 平台 UI 中调整 | 对话中直接修改，或去 AgentEditPage |
| 配置存储 | 云端 | agent.md 文件（可版本控制） |
| 创建 Agent | 平台内置流程 | @assistant 的一个工具调用 |

**手动创建降级路径：**

对话式创建覆盖 80% 场景，但以下情况下用户需要直接手动创建：

- LLM API 不可用（DeepSeek 故障、额度耗尽等）
- 用户明确知道要写什么配置，不想走对话流程
- 批量导入或从其他平台迁移 Agent

降级方案：`AgentCreatePage` 提供两个入口：

```
┌──────────────────────────────────────────┐
│          创建新 Agent                      │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │ 🤖 对话式创建（推荐）                │  │
│  │ 描述你的需求，@assistant 会帮你      │  │
│  │ 自动生成 agent.md                   │  │
│  │ [开始对话]                           │  │
│  └────────────────────────────────────┘  │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │ ✍️ 手动创建                        │  │
│  │ 直接编写 agent.md 配置，适合熟悉    │  │
│  │ 配置格式的高级用户                   │  │
│  │ [手动创建]                           │  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
```

- **对话式入口**：跳转 `/peco`，预填提示词「请帮我创建一个新的 Agent：」+ 用户输入
- **手动入口**：跳转一个 Markdown/YAML 编辑器页面（复用 `AgentEditPage` 的编辑器组件），直接编写 agent.md 内容，通过 `POST /api/agents` 保存

`POST /api/agents` 端点（新增）：

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/agents` | 直接创建 agent.md（接收 `name` + `content`，写入 workspace 并返回创建的 Agent） |

该端点在 LLM 不可用时仍然可用，不依赖任何模型调用。

### 4.10 Skill 导入/导出/删除

Skill 创建当前通过文件系统进行，前端 SkillListPage 提供查看功能。同时新增完整的生命周期管理：

**导入（已有设计）：**

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/skills/import` | 从上传的 `.zip` 或单个 `SKILL.md` 文件导入 Skill |

**导入流程：**
1. 用户上传 `.zip`（含 `SKILL.md` + 可选的 `scripts/`、`references/`、`assets/`）或单个 `SKILL.md`
2. 后端解压/读取，验证 `SKILL.md` 的 YAML frontmatter（name、description 必填）
3. 写入到用户 workspace 的 `skills/<skill-name>/` 目录
4. 触发 `SkillRegister` 热重载（reload）

**导出（新增）：**

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/skills/:name/export` | 打包 Skill 目录为 `.zip` 并返回下载 |

**删除（新增）：**

| 方法 | 路径 | 说明 |
|------|------|------|
| DELETE | `/api/skills/:name` | 删除整个 Skill 目录（`skills/<skill-name>/`），触发 `SkillRegister` 热重载 |

删除操作不可逆（Skill 目录直接从文件系统中移除）。前端需弹确认对话框，提示用户「此操作不可撤销」。

**不支持社区市场**：Skill 分发通过文件共享（Git、网盘、邮件等），Peco 本身不承载商店/市场功能。导入 + 导出覆盖了分发的完整闭环——分享 Skill 就是分享一个 `.zip` 文件。

### 4.11 Workflow 模块规划（延后）

peco-core 已有完整的 Workflow 引擎（DAG 拓扑执行、模板变量、暂停/审批），但本次重构**不暴露前端 Workflow 管理界面**。原因：
- Workflow 引擎 API 稳定，但前端可视化编排的工作量远超本次重构范围
- 需要独立的 UI 设计（画布编辑器 vs 表单式 vs 自然语言描述）
- 需要参考 Dify/Coze 的成熟方案，避免仓促设计后续推翻

**「任务」页面**本次仅保留占位入口，后续独立设计文档覆盖。当前 Workflow 引擎仍可通过 Agent 的 `execute_workflow` 工具调用（Agent 在 ReAct 循环中触发），只是缺少前端管理界面。

### 4.12 文件共享与分发

Peco 不内置 Agent/Skill 市场，但文件驱动架构天然支持通过 Git、网盘等渠道分发：

```
用户 A 的 workspace                用户 B 的 workspace
  agents/                            agents/
    code-reviewer/          ──→        code-reviewer/       ← git clone / 解压 zip
      agent.md                           agent.md
  skills/                             skills/
    code-review/            ──→         code-review/         ← 同上
      SKILL.md                           SKILL.md
```

对比大厂平台：
- ChatGPT GPTs / Coze 商店：云端存储，依赖平台分发
- Dify：DSL 文件（`.yml`），可导出导入
- **Peco**：原生文件，零平台锁定。GitHub repo = Agent 市场，网盘链接 = Skill 分发

本次不构建分发基础设施，但保留以下扩展点：
- Agent 导出：打包 `agent.md` + 关联 Skill 目录为 `.zip`
- Agent 导入：POST `/api/agents/import`（解析 zip → 写入 workspace）
- Skill 导入：POST `/api/skills/import`（见 4.10）

---

## 5. 前端变更详述

### 5.1 侧边栏重构

**文件：** `webui/src/components/layout/Sidebar.tsx`

```tsx
import {
  Sparkles,      // Peco
  MessageSquare, // 对话
  Settings2,     // 管理
  Clock,         // 任务
  Settings,      // 设置
  ChevronDown,   // 展开箭头
  Cpu,           // Provider 子项
  Bot,           // Agent 子项
  Puzzle,        // Skill 子项
  Plug,          // MCP 子项
  BookOpen,      // KnowledgeBase 子项
  Plus,          // 新建对话
  Archive,       // 已归档对话
  Trash2,        // 删除
} from 'lucide-react'

interface ConversationSummary {
  id: string
  agentId: string
  title: string           // LLM 根据首条消息自动生成
  lastMessageAt: string   // ISO 时间戳
  archived: boolean
}

interface AgentConversationGroup {
  agentId: string
  agentName: string
  conversations: ConversationSummary[]   // 按 lastMessageAt 降序，仅显示活跃的
}

interface NavItem {
  to: string
  label: string
  icon: React.ComponentType
  children?: { to: string; label: string; icon: React.ComponentType }[]
}

// 静态导航项
const NAV_ITEMS: NavItem[] = [
  { to: '/peco', label: 'Peco', icon: Sparkles },
  {
    to: '/chat',
    label: '对话',
    icon: MessageSquare,
    children: [],   // 动态填充：从 API 加载对话列表，按 Agent 分组
  },
  {
    to: '/manage',
    label: '管理',
    icon: Settings2,
    children: [
      { to: '/manage/providers', label: 'Provider', icon: Cpu },
      { to: '/manage/agents', label: 'Agent', icon: Bot },
      { to: '/manage/skills', label: 'Skill', icon: Puzzle },
      { to: '/manage/mcp', label: 'MCP', icon: Plug },
      { to: '/manage/knowledge', label: 'KnowledgeBase', icon: BookOpen },
    ],
  },
  { to: '/tasks', label: '任务', icon: Clock },
  { to: '/settings', label: '设置', icon: Settings },
]
```

**交互逻辑：**
- 点击「对话」展开/折叠对话列表（手风琴模式）
- **空状态（核心逻辑）**：
  - 无 Agent：展示引导文案，引导用户前往「管理 > Agent」创建
  - 有 Agent 但无对话：展示 Agent 列表，每个 Agent 附带「+ 新对话」按钮
  - 有对话：按 Agent 分组展示历史对话
- 每个 Agent 分组下列出活跃对话（标题 + 时间），点击进入对应对话
- Agent 分组头部点击跳转该 Agent 的对话主页（`/chat/:agentId`，显示空白对话 + 该 Agent 的对话列表）
- 对话项右键 / 长按菜单：重命名、归档、删除
- 「已归档」入口在对话列表底部，有归档对话时显示数量角标，点击展示已归档对话列表（支持恢复）
- 「管理」子菜单的交互与「对话」一致（手风琴模式）
- 侧边栏折叠时只显示父级图标，子菜单通过 tooltip 展示
- 「对话」默认展开/折叠策略：无对话时默认折叠，有对话时默认展开

**数据加载：**
- 侧边栏挂载时并行请求：
  - `GET /api/agents`（后端已过滤 `@` 系统 Agent，仅返回用户可见 Agent）
  - 各可见 Agent 的 `GET /api/chat/:agentId/conversations?status=active`
- 前端也做一次防御性过滤：`agents.filter(a => !a.name.startsWith('@'))`，双重保障
- 根据两次请求的结果判定空状态类型（无 Agent / 有 Agent 无对话 / 正常）
- 新对话创建后（SSE `turn_complete` 事件），乐观更新侧边栏列表
- 新建 Agent 后，通过 React Query cache invalidation 刷新 Agent 列表和空状态
- 使用 React Query 或 SWR 做客户端缓存，避免频繁请求

### 5.2 提取共享聊天组件 `<ChatView />`

**动机：** `PecoChatPage`（永续对话）和未来的 `AgentChatPage`（临时对话）核心聊天 UI 完全一致 — 消息气泡、工具调用卡片、推理折叠、子 Agent 嵌套、SSE 流式处理。唯一区别在于 API 端点和 session 管理方式。

**共享 vs 差异：**

| | PecoChatPage | AgentChatPage |
|---|---|---|
| SSE 端点 | `GET /api/peco/stream?message=` | `GET /api/chat/:agentId/conversations/:id/stream?message=` |
| Session 持久化 | 有（session_snapshots 表） | 有（conversations + messages + session_snapshots 表） |
| 对话数量 | 1 条（永续） | 每个 Agent 上限 100 条 |
| 超限策略 | 不适用 | 自动归档最旧对话 |
| 清除对话 | 有（DELETE /api/peco/session） | 归档或永久删除 |
| 历史加载 | GET /api/peco/session | GET /api/chat/:agentId/conversations/:id（含消息历史） |
| 对话列表 | 无（永续对话始终一条） | 侧边栏按 Agent 分组展示 |
| Agent 来源 | 固定 @assistant | URL params agentId |
| 聊天 UI | ✅ 复用 ChatView | ✅ 复用 ChatView |

**提取方案：**

```tsx
// webui/src/components/chat/ChatView.tsx

interface ChatViewProps {
  /** SSE 端点 URL（不含 query params，message 由组件拼接） */
  streamUrl: string
  /** 初始消息列表（从快照恢复时为历史消息，否则为 []） */
  initialMessages?: ChatMessage[]
  /** 头部操作区右侧插槽（如清除对话按钮） */
  headerActions?: React.ReactNode
  /** 欢迎语（agent 不同则欢迎语不同） */
  welcomeMessage?: React.ReactNode
}

// 消息统一通过 GET + query param 传递：`${streamUrl}?message=${encodeURIComponent(msg)}`
// peco 和 agent chat 使用相同的模式，无需区分 HTTP 方法。

export function ChatView({ streamUrl, initialMessages, ... }: ChatViewProps) {
  // 完整的聊天逻辑：
  // - messages state
  // - SSE 流式读取（fetch + ReadableStream）
  // - SSE 事件处理（text_delta, reasoning_delta, tool_call_start, tool_result,
  //   agent_call_start, agent_call_end, turn_complete, done, error）
  // - 自动滚动
  // - 停止生成
  // - 消息渲染（ChatBubble，含反馈按钮 👍/👎/🔄）
  // - 反馈提交（POST /api/chat/:agentId/conversations/:id/feedback）
  // - 回答再生（resend last user message via SSE endpoint）
}
```

**PecoChatPage 使用方式：**
```tsx
// webui/src/pages/peco/PecoChatPage.tsx
export function PecoChatPage() {
  const [initialMessages, setInitialMessages] = useState<ChatMessage[]>([])

  useEffect(() => {
    // 加载持久化 session 快照
    getPecoSession().then(snap => setInitialMessages(snapshotToMessages(snap)))
  }, [])

  return (
    <ChatView
      streamUrl={`${API_BASE}/api/peco/stream`}
      initialMessages={initialMessages}
      headerActions={<ClearSessionButton />}
      welcomeMessage={<PecoWelcome />}
    />
  )
}
```

**AgentChatPage 使用方式：**
```tsx
// webui/src/pages/chat/AgentChatPage.tsx
export function AgentChatPage() {
  const { agentId, conversationId } = useParams()
  const [initialMessages, setInitialMessages] = useState<ChatMessage[]>([])

  useEffect(() => {
    if (conversationId) {
      // 加载已有对话的历史消息
      getConversation(agentId!, conversationId).then(conv =>
        setInitialMessages(conv.messages)
      )
    }
  }, [agentId, conversationId])

  const handleNewConversation = async (title: string) => {
    // 创建新对话 → 跳转到 /chat/:agentId/:newConversationId
    const conv = await createConversation(agentId!, title)
    navigate(`/chat/${agentId}/${conv.id}`)
  }

  return (
    <ChatView
      streamUrl={`${API_BASE}/api/chat/${agentId}/conversations/${conversationId}/stream`}
      initialMessages={initialMessages}
      headerActions={
        conversationId && <ArchiveButton conversationId={conversationId} />
      }
      welcomeMessage={<AgentWelcome agentId={agentId} />}
    />
  )
}
```

### 5.3 PecoChatPage（永续对话页）

`webui/src/pages/peco/PecoChatPage.tsx`

- 使用共享组件 `ChatView`，消息通过 GET + query param 传递
- 后端默认 Agent：`@assistant`（系统内置，用户不可见）
- 挂载时从 `GET /api/peco/session` 加载持久化快照
- 顶部操作区：副标题「由 @assistant 驱动」+ 清除对话按钮
- 欢迎语：「👋 你好！我是 Peco，你的个人 AI 助理。」

### 5.4 管理子页面

**已有页面的变更：**

**AgentListPage** (`webui/src/pages/manage/AgentListPage.tsx`，原 `/agents` → `/manage/agents`)
- 列表自动过滤 `@` 前缀系统 Agent（`@assistant`、`@memory` 等不可见）
- 用户 Agent 正常展示，支持编辑、删除
- 「创建 Agent」按钮 → 跳转 Peco 对话页，预填提示词「请帮我创建一个新的 Agent：」

**AgentCreatePage** (`webui/src/pages/manage/AgentCreatePage.tsx`，原表单页 → 双入口引导页)
- **不再是单一表单页**，而是一个双入口引导页
- **入口 1 — 对话式创建（推荐）**：简短文字引导 + 「开始对话创建」按钮
  - 点击按钮 → 跳转 `/peco`，带入预设消息（如「请帮我创建一个新的 Agent，需求如下：」+ 用户输入）
  - 用户在 Peco 对话中与 @assistant 交互 → @assistant 调用 `save_agent` 工具完成创建
  - 可选增强：页面提供常用 Agent 模板描述（如「代码审查助手」「数据分析师」），点击直接带入 Peco 对话
- **入口 2 — 手动创建（降级路径）**：适用于 LLM 不可用或用户熟悉配置格式的场景
  - 点击「手动创建」→ 跳转 Markdown/YAML 编辑器页面（复用 `AgentEditPage` 的编辑器组件）
  - 直接编辑 agent.md 内容，通过 `POST /api/agents` 保存
  - 该端点纯文件写入，不依赖 LLM

**AgentEditPage** (`webui/src/pages/manage/AgentEditPage.tsx`)
- 保留独立编辑页面（agent.md 文件编辑器），用于创建后的手动微调
- `@` Agent 禁止进入（后端 403，前端按钮置灰）

**新增页面：**

#### ProviderListPage

`webui/src/pages/manage/ProviderListPage.tsx`

- 表格列出所有 providers（name、type、model、status）
- 操作：测试连接、编辑、删除
- 新建/编辑 → ProviderEditDialog（内联对话框或新页）

#### SkillListPage

`webui/src/pages/manage/SkillListPage.tsx`

- 卡片列出用户 WorkSpace 中的 skills
- 操作：查看详情（SKILL.md 渲染）、启用/禁用
- 作为第一阶段，skill 只有查看功能（创建通过文件系统）

#### McpConfigPage

`webui/src/pages/manage/McpConfigPage.tsx`

- 表单编辑 mcp_config.json（JSON 编辑器或结构化表单）
- 按 server 分组，每个 server 有 name、type (stdio/http)、command/url
- 测试连接按钮

### 5.5 App.tsx 路由变更

```tsx
// 修改后
<Routes>
  <Route path="/login" element={<LoginPage />} />
  <Route path="/register" element={<RegisterPage />} />
  <Route element={<ProtectedRoute />}>
    <Route element={<AppLayout />}>
      <Route path="/" element={<Navigate to="/peco" replace />} />

      {/* Peco 永续聊天 */}
      <Route path="/peco" element={<PecoChatPage />} />

      {/* Agent 临时对话（本期仅后端占位，前端路由后续启用） */}
      <Route path="/chat/:agentId" element={<AgentChatPage />} />

      {/* 管理 */}
      <Route path="/manage/providers" element={<ProviderListPage />} />
      <Route path="/manage/agents" element={<AgentListPage />} />
      <Route path="/manage/agents/new" element={<AgentCreatePage />} />
      <Route path="/manage/agents/:agentId/edit" element={<AgentEditPage />} />
      <Route path="/manage/skills" element={<SkillListPage />} />
      <Route path="/manage/mcp" element={<McpConfigPage />} />
      <Route path="/manage/knowledge" element={<KnowledgeListPage />} />
      <Route path="/manage/knowledge/:kbId" element={<KnowledgeDetailPage />} />

      {/* 任务 */}
      <Route path="/tasks" element={<TaskListPage />} />

      {/* 设置 */}
      <Route path="/settings" element={<SettingsPage />} />
    </Route>
  </Route>
</Routes>
```

移除：
```diff
- <Route path="/personal" element={<PersonalAgentPage />} />
- <Route path="/chat" element={<ChatListPage />} />
- <Route path="/chat/:conversationId" element={<ChatDetailPage />} />
- <Route path="/knowledge" element={<KnowledgeListPage />} />
- <Route path="/knowledge/:kbId" element={<KnowledgeDetailPage />} />
```

### 5.6 前端文件迁移

```
新建：
  webui/src/components/chat/ChatView.tsx        ← 共享聊天组件（从 ChatDetailPage 提取）
  webui/src/pages/peco/PecoChatPage.tsx         ← peco 永续聊天页（使用 ChatView）
  webui/src/pages/chat/AgentChatPage.tsx        ← Agent 对话页（使用 ChatView）
  webui/src/pages/manage/ProviderListPage.tsx
  webui/src/pages/manage/SkillListPage.tsx
  webui/src/pages/manage/McpConfigPage.tsx
  webui/src/api/peco.ts
  webui/src/api/providers.ts
  webui/src/api/skills.ts
  webui/src/api/mcp.ts

移动（路径变更）：
  pages/agents/       → pages/manage/
  pages/knowledge/    → pages/manage/

删除（功能替代）：
  pages/personal/PersonalAgentPage.tsx          ← 被 PecoChatPage 替代
  pages/chat/ChatListPage.tsx                   ← 对话列表迁移到侧边栏
  pages/chat/ChatDetailPage.tsx                 ← 逻辑提取到 ChatView 后删除
  api/personal-agent.ts                         ← 被 api/peco.ts 替代

保留重构：
  webui/src/api/conversations.ts                ← 适配 /api/chat/:agentId/conversations 端点
  webui/src/components/layout/Sidebar.tsx       ← 新增「对话」折叠菜单 + 对话历史列表

保留不变：
  pages/tasks/        ← "任务"一级导航，本次仅占位
  pages/settings/
  pages/auth/
```

---

## 6. 数据层变更

### 6.1 数据库

**新增字段：**

`conversations` 表新增字段：
| 字段 | 类型 | 说明 |
|------|------|------|
| `agent_name` | TEXT NOT NULL | 对话所属的 Agent 名称 |
| `title` | TEXT | LLM 根据首条消息自动生成的对话标题 |
| `archived_at` | TIMESTAMP NULL | 归档时间（NULL = 活跃） |

`conversations` 表新增索引：
```sql
CREATE INDEX idx_conversations_user_agent_active
  ON conversations(user_id, agent_name, archived_at, last_message_at DESC);
```

**已有表使用情况：**
- `conversations` 表：**继续使用** — Agent 对话的持久化存储。Peco 永续对话不使用此表（perpetual session 仅在 `session_snapshots` 中）
- `messages` 表：**保留使用** — Agent 对话的消息持久化（备选方案：直接使用 `session_snapshots` 中的消息数据，避免双写。本期优先使用现有 `messages` 表，后续评估合并）
- `agents` 表：继续使用（Agent CRUD 不变）
- `session_snapshots` 表：继续使用（peco 永续对话 + Agent 对话的 session 快照）

**数据库迁移策略：**

`conversations` 表新增 `agent_name NOT NULL` 字段，已有行需要回填。迁移分两步：

**Step 1 — DDL（允许 NULL，补数据后再加 NOT NULL）：**
```sql
-- 1. 添加字段（暂时允许 NULL）
ALTER TABLE conversations ADD COLUMN agent_name TEXT;
ALTER TABLE conversations ADD COLUMN title TEXT;
ALTER TABLE conversations ADD COLUMN archived_at TIMESTAMP NULL;

-- 2. 回填已有数据：从关联的 messages 或 session_snapshots 推断 agent_name
--    如果 conversations 表已有 agent_id 字段，直接关联查询：
UPDATE conversations SET agent_name = (
    SELECT a.name FROM agents a WHERE a.id = conversations.agent_id
) WHERE agent_name IS NULL;

--    如果没有 agent_id 字段（当前设计可能是通用对话），则根据对话来源推断：
--    - 来自 /api/personal-agent 的对话 → agent_name = '@assistant'
--    - 来自 /api/conversations 的对话 → agent_name = 关联的 agent（从请求路径或消息中提取）
--    兜底：无法推断的旧对话 → agent_name = 'unknown'（标记为待清理）

-- 3. 回填 title：已有对话若 title 为空，取第一条用户消息的前 50 字符
UPDATE conversations SET title = (
    SELECT SUBSTR(m.content, 1, 50) FROM messages m
    WHERE m.conversation_id = conversations.id AND m.role = 'user'
    ORDER BY m.created_at ASC LIMIT 1
) WHERE title IS NULL;

-- 4. 加 NOT NULL 约束 + 索引
ALTER TABLE conversations ALTER COLUMN agent_name SET NOT NULL;
CREATE INDEX idx_conversations_user_agent_active
  ON conversations(user_id, agent_name, archived_at, last_message_at DESC);
```

**Step 2 — 代码层迁移（Rust migration script）：**
```rust
// crates/peco-server/src/db/migrations/v2_conversations.rs
//
// 迁移逻辑：
// 1. 读取所有 conversations 行
// 2. 对于 agent_name 为 NULL 的行：
//    a. 检查关联的 messages 中是否记录了 agent 信息
//    b. 检查 session_snapshots 中是否有 agent_name 字段
//    c. 若都无法推断，根据 user_id 和 created_at 范围标记为 '@assistant'（旧版仅此一种对话）
// 3. 执行 UPDATE 回填
// 4. 执行 ALTER TABLE 加 NOT NULL 约束
```

**回退策略：** 迁移脚本在修改约束前先做完整备份（`conversations_backup` 表），若回填失败可回退。迁移在服务启动时自动执行（`sqlx::migrate!` 宏），不通过独立脚本。

**消息双存储合并评估（Phase 5，不在本次范围）：**

当前 Agent 对话的消息同时存在于 `messages` 表和 `session_snapshots` 表中（`session_snapshots` 包含完整的 `AnnotatedMessage` 序列化数据）。这导致：
- **数据冗余**：同一条消息存了两份
- **一致性风险**：两个表可能不同步
- **存储浪费**：SQLite 体积膨胀

**评估标准（Phase 5 决策依据）：**

| 维度 | `messages` 表 | `session_snapshots` 表 |
|------|---------------|------------------------|
| 数据完整性 | 仅 user/assistant 文本，不含 tool_call/tool_result 详情 | 完整 `AnnotatedMessage` 序列化，包含工具调用、推理等全部信息 |
| 查询效率 | 结构化 SQL 查询，适合分页、搜索 | JSON blob，查询需反序列化 |
| 写入路径 | 需要显式 INSERT（当前 chat handler 负责） | 自动写入（Looper 在轮次边界调用 `persister.save()`） |
| 恢复能力 | 文本级恢复 | 完整状态恢复（含工具调用、推理、中间状态） |
| 维护成本 | 需要维护写入逻辑 | 零额外成本（Looper 内置） |

**合并方向（推荐）：** 以 `session_snapshots` 为主存储，`messages` 表逐步废弃。

**实施前提：**
1. 前端 `ChatView` 能直接从 `session_snapshots` 的 JSON 数据渲染历史消息（而非依赖 `GET /api/chat/:agentId/conversations/:id` 返回的 `messages` 数组）
2. 对话列表页（侧边栏）的标题、时间等元数据继续使用 `conversations` 表（轻量索引），但消息内容从 `session_snapshots` 获取
3. 确认 `session_snapshots` 的序列化/反序列化性能满足前端加载需求（千条消息 < 200ms）

**评估时间线：** 本次重构（Phase 1-4）保持双写不变。Phase 5（预计 Phase 4 完成后 1 个月内）基于以下触发条件启动：
- 生产环境观察到 `messages` 表与 `session_snapshots` 数据不一致的 bug
- SQLite 体积超过 100MB 且分析确认 JSON blob 占比 > 60%
- 需要实现消息全文搜索（此时需要重新评估存储方案）

Phase 5 启动前，在 `chat/handler.rs` 的消息写入路径加 `// TODO(Phase 5): evaluate removing direct messages table writes in favor of session_snapshots-only` 注释标记。

### 6.2 文件系统

**peco 模块的 Agent 安装路径：**
```
{user_workspace}/
  agents/
    @assistant/
      agent.md           ← BuiltinTemplate::personal() 安装
    @memory/
      agent.md           ← 同上
  knowledge/
    @private_memory/
      kb_config.json     ← 同上
```

与 `personal_agent` 现有行为完全一致。

### 6.3 配置文件

| 文件 | 管理方式 | 对应 UI |
|------|----------|---------|
| `{user_workspace}/providers.toml` | 新增 CRUD API（用户 workspace 级别） | ProviderListPage |
| `{user_workspace}/mcp_config.json` | 新增 CRUD API（用户 workspace 级别） | McpConfigPage |
| `{user_workspace}/agents/*/agent.md` | 已有 CRUD API | AgentListPage |
| `{user_workspace}/skills/*/SKILL.md` | 新增 Read/Import/Export API | SkillListPage |
| `{user_workspace}/knowledge/*/kb_config.json` | 已有 CRUD API | KnowledgeListPage |

---

## 7. 废弃模块清单

### 7.1 功能移除（代码保留）

| 模块 | 文件 | 处理 |
|------|------|------|
| `assistant` | `src/assistant/mod.rs` | 顶部加 deprecation notice，所有 pub 项加 `#[deprecated]` |
| `assistant` | `src/assistant/manager.rs` | `#[deprecated]` 标注，保留 `PersonalAssistantManager` + `PersonalAssistantMessageFilter` |
| `assistant` | `src/assistant/personal_assistant_agent.md` | **删除** — 嵌入的 agent 定义文件不再需要 |
| `personal_agent` | `src/personal_agent/mod.rs` | 顶部加 deprecation notice |
| `personal_agent` | `src/personal_agent/manager.rs` | `#[deprecated]` 标注 |
| `personal_agent` | `src/personal_agent/handler.rs` | `#[deprecated]` 标注 |
| `personal_agent` | `src/personal_agent/filter.rs` | 逻辑移入 peco 模块后**删除** |
| `personal_agent` | `src/personal_agent/session.rs` | 逻辑移入 peco 模块后**删除** |
| `chat` | `src/chat/handler.rs` | **删除** `ensure_omni_agent`；conversation CRUD **保留并重构**为 Agent 作用域；新增上限控制 |
| 前端 | `pages/personal/PersonalAgentPage.tsx` | **删除**（被 PecoChatPage 替代） |
| 前端 | `pages/chat/ChatListPage.tsx` | **删除**（对话列表迁移到侧边栏） |
| 前端 | `pages/chat/ChatDetailPage.tsx` | 逻辑提取到 `components/chat/ChatView.tsx` 后**删除** |
| 前端 | `api/personal-agent.ts` | **删除**（被 api/peco.ts 替代） |
| 前端 | `api/conversations.ts` | **保留重构** — 适配 `/api/chat/:agentId/conversations` 端点 |

### 7.2 数据层清理

| 表 | 处理 |
|----|------|
| `conversations` | **继续使用** — Agent 对话持久化，新增 `agent_name` + `title` + `archived_at` 字段 |
| `messages` | **保留使用** — Agent 对话消息持久化 |
| `session_snapshots` | 继续使用（peco 永续对话 + Agent 对话的 session 快照） |

### 7.3 为什么保留而不删除

1. **PPA 钩子体系** (`personal_assistant`) — `PpaDynamicContext` + `PpaMemoryHook` 是成熟的设计模式，后续可能接入 peco 模块
2. **MessageFilter 双实现** — `PersonalAssistantMessageFilter` 和 `PersonalAgentMessageFilter` 有不同的过滤策略，作为设计对比保留
3. **历史代码可追溯** — 如果 peco 模块出现问题，废弃模块可作为回退参考

---

## 8. 实现路线图

### Phase 1：后端统一（预计 3-4 天）

```
□ Step 1.1  新建 peco 模块
             - peco/config.rs: PecoConfig（预留钩子注入点）
             - peco/manager.rs: 合并 personal_agent::manager 逻辑
             - peco/filter.rs: 从 personal_agent::filter 移入
             - peco/session.rs: 从 personal_agent::session 移入
             - peco/handler.rs: GET /stream, GET /session, DELETE /session
             - peco/mod.rs

□ Step 1.2  注册路由
             - lib.rs: .nest("/api/peco", peco::handler::router())
             - 保留旧路由并行运行（Phase 2 前端完全切换后即移除，并行期 ≤ 1 周）
             - 旧端点调用时记录 warn 日志，方便追踪未迁移的调用方

□ Step 1.3  重构 chat 模块（对话管理）
             - 删除 ensure_omni_agent()
             - conversation CRUD 迁移到 Agent 作用域（/api/chat/:agentId/conversations）
             - 新增 conversation.rs：上限检查 + 自动归档逻辑
             - 新增 GET /:agentId/conversations/:id/stream?message=（持久化 SSE，与 peco 统一用 GET）
             - 数据库迁移：conversations 表新增 agent_name, title, archived_at 字段 + 历史数据回填脚本
             - 数据库迁移包含回退策略（conversations_backup 表）

□ Step 1.4  配置 @assistant 的 save_agent 工具 + 新增 POST /api/agents
             - 在 @assistant 模板的 agent.md 中，tools 列表添加 save_agent
             - 确认 save_agent 对 @assistant 可见且可调用
             - 验证：用户在 Peco 对话中描述 Agent 需求 → @assistant 调用 save_agent
             - 新增 POST /api/agents（直接创建 agent.md，不依赖 LLM）作为手动创建降级路径

□ Step 1.5  标记废弃模块
             - assistant/ 添加 deprecation notice
             - personal_agent/ 添加 deprecation notice
             - 删除 assistant/personal_assistant_agent.md

□ Step 1.6  补充 — agent.md 环境变量解析（§9.5）
             - Agent::from_file() 中调用 resolve_env_vars() 处理 ${VAR} 引用
             - 覆盖范围：llm.model, tools 名称, mcp 名称, Markdown 正文
             - P2 优先级，一行改动

□ Step 1.7  补充 — MetricsCollector 钩子（§9.1）
             - 新建 peco-core/src/agent/hooks/metrics.rs
             - 实现 LooperHook，在 on_after_response 中提取 Usage 写入 usage_logs 表
             - usage_logs 表 DDL 加入 schema.sql

□ Step 1.8  测试
             - cargo test --workspace
             - cargo clippy --workspace -- -D warnings
             - 手动测试 /api/peco/stream SSE 流式对话
             - 手动测试对话 CRUD + 上限自动归档
```

### Phase 2：前端 ChatView 提取 + 导航重构（预计 2-3 天）

```
□ Step 2.1  提取 ChatView 共享组件
             - 从 ChatDetailPage 提取核心聊天逻辑
             - Props 设计：streamUrl, initialMessages, headerActions 等
             - 消息统一通过 GET + query param 传递（`?message=`）
             - 所有 SSE 事件类型处理（9 种）
             - 内置反馈组件：👍/👎/🔄（调用反馈 API + 再生逻辑，见 §9.3）

□ Step 2.2  新建 PecoChatPage
             - 使用 ChatView，消息通过 GET + query param 传递
             - 挂载时加载 session 快照（GET /api/peco/session）
             - 清除对话按钮 → DELETE /api/peco/session

□ Step 2.3  新建 AgentChatPage
             - 使用 ChatView，消息通过 GET + query param 传递
             - URL params 获取 agentId 和可选的 conversationId
             - 支持从对话列表进入（/chat/:agentId/:conversationId）
             - 新建对话时自动生成标题

□ Step 2.4  重构 Sidebar（核心变更）
             - 新导航结构：Peco / 对话 / 管理 / 任务 / 设置
             - 「对话」菜单：从 API 加载对话列表，按 Agent 分组展示
             - 对话项右键菜单（重命名、归档、删除）
             - 「管理」子菜单展开/折叠（手风琴模式）
             - 折叠状态 tooltip

□ Step 2.5  更新 App.tsx 路由
             - 路由表更新
             - 重定向 / → /peco

□ Step 2.6  管理子页
             - ProviderListPage（基础表格 + 测试连接）
             - AgentCreatePage（双入口引导页：对话式创建 → Peco 对话 + 手动创建 → YAML 编辑器 → POST /api/agents）
             - SkillListPage（卡片列表 + 导入按钮）
             - McpConfigPage（表单）

□ Step 2.7  清理废弃前端文件
             - 删除 pages/personal/, pages/chat/ChatListPage.tsx, pages/chat/ChatDetailPage.tsx
             - 删除 api/personal-agent.ts
             - 保留并重构 api/conversations.ts

□ Step 2.8  补充 — 反馈 API（§9.3）
             - 新增 DB 表 conversation_feedback（message_id, rating, comment, created_at）
             - 新增 POST /api/chat/:agentId/conversations/:id/feedback
             - 新增 POST /api/peco/feedback
             - 新增 api/feedback.ts 前端 API 模块
```

### Phase 3：Provider / Skill / MCP 管理 API（预计 2-3 天）

```
□ Step 3.1  Provider CRUD + Fallback（§9.4）
             - provider/handler.rs
             - 读写 providers.toml（用户 workspace 级别）
             - 测试连接端点
             - providers.toml 新增 fallback 链字段
             - FallbackModelProvider 包装器（按顺序尝试 provider 链）
             - 新增 OpenAI provider 实现（StreamingProfile + ModelProvider）

□ Step 3.2  Skill 列表/详情 + 导入/导出/删除
             - skill/handler.rs
             - 读取 Workspace skills 目录
             - GET /api/skills — 列表
             - GET /api/skills/:name — 详情
             - PUT /api/skills/:name — 创建/更新
             - DELETE /api/skills/:name — 删除 Skill 目录
             - GET /api/skills/:name/export — 导出为 .zip
             - POST /api/skills/import（从 zip/SKILL.md 导入）

□ Step 3.3  MCP 配置管理
             - mcp_config/handler.rs
             - 读写 mcp_config.json（用户 workspace 级别）
             - 测试连接端点

□ Step 3.4  补充 — 用量统计 API（§9.1）
             - GET /api/usage/summary?period=7d&agent=:name
             - SQL 聚合 usage_logs 表，计算 token 总量 + 预估成本
             - 前端设置页/简易仪表板展示

□ Step 3.5  路由注册
             - lib.rs 注册 /api/providers, /api/skills, /api/mcp, /api/usage
```

### Phase 4：清理旧路由 + 收尾（预计 1 天）

```
□ Step 4.1  移除 /api/personal-agent 路由
             - 前端已全部切换完毕
             - /api/conversations 路由保持不变（chat 模块继续使用 conversations 表）
             - 旧路由废弃时间线：Phase 2 前端切换完成即移除，并行期不超过 1 周
             - 移除前在日志中 warn 级别记录仍在使用旧端点的调用方

□ Step 4.2  端到端测试
             - 新用户注册 → 自动安装模板 → Peco 对话 → 清除对话
             - 管理 > Agent CRUD + LLM 辅助生成正常
             - 管理 > Provider/Skill/MCP 页面正常
             - 对话历史列表正常：创建、切换、归档、删除
             - 上限控制正常：100条/Agent 后自动归档最旧对话
             - KnowledgeBase 不受影响

□ Step 4.3  补充 — 对话导出（§9.6）
             - GET /api/chat/:agentId/conversations/:id/export?format=json|markdown
             - GET /api/peco/session/export?format=json|markdown
             - JSON 格式：直接序列化 SessionSnapshot
             - Markdown 格式：人类可读转录（工具调用折叠 + 推理内容）

□ Step 4.4  文档更新
             - README.md 更新导航说明
             - CLAUDE.md 更新模块结构
```



---

## 9. 行业对标与补充设计

> 本节基于对主流 Agent 平台（Dify、Coze、LangChain/LangSmith、CrewAI、AutoGen）的分析，列出 Peco v2 设计中当前的空白点及补充方案。各条目标注了优先级（P0–P2）和建议阶段。

### 9.1 可观测性与 Token 用量追踪（P1）

**行业现状：** Dify 提供完整的监测仪表板（token 用量、延迟、成本、对话记录）、LangSmith 提供分布式链路追踪。

**Peco 当前基础：**
- `tracing` + `tower_http::TraceLayer` — HTTP 层基础日志
- `Usage` 结构体（`input_tokens`, `output_tokens`, `total_tokens`）已存在，但仅在模型调用层临时使用，不持久化、不聚合、不在前端展示
- `LooperHook::on_after_response` 已携带 `Usage` 信息，但当前未持久化

**补充方案：**

1. **`MetricsCollector` 钩子（复用 LooperHook 基础设施）**：在 `PecoConfig.hooks` 中添加一个可选钩子，拦截 `on_after_response` 和 `on_turn_complete`，将每次模型调用的 token 用量写入 `usage_logs` 表。

   ```sql
   CREATE TABLE usage_logs (
       id TEXT PRIMARY KEY,
       user_id TEXT NOT NULL,
       agent_name TEXT NOT NULL,
       conversation_id TEXT,
       input_tokens INTEGER NOT NULL,
       output_tokens INTEGER NOT NULL,
       model TEXT NOT NULL,
       created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
   );
   CREATE INDEX idx_usage_logs_user_created ON usage_logs(user_id, created_at);
   ```

2. **用量统计 API**：

   | 方法 | 路径 | 说明 |
   |------|------|------|
   | GET | `/api/usage/summary?period=7d&agent=:name` | 总 token 用量、预估成本、按 Agent 拆分 |

   聚合逻辑为纯 SQL SUM/GROUP BY 查询 `usage_logs` 表，无新增基础设施。

3. **成本估算**：`providers.toml` 中每个 provider 新增可选 `pricing` 字段：

   ```toml
   [providers.deepseek.pricing]
   input_per_1k = 0.00014    # $ per 1K input tokens
   output_per_1k = 0.00028   # $ per 1K output tokens
   ```

4. **前端展示**：设置页或简易仪表板展示近 7 天用量、预估费用、Top 消耗 Agent。

**实现阶段：** Phase 1 — `MetricsCollector` 钩子 + `usage_logs` 表；Phase 3 — 用量统计 API + 前端卡片。

---

### 9.2 敏感工具人机协同审批（P2）

**行业现状：** Dify 支持对 shell 执行等工具配置「需要审批」；AutoGen 在执行代码前可要求人工确认。

**Peco 当前基础：**
- Workflow 引擎已实现 `OnFailure::Pause` + `WorkflowHandle::approve()` 审批模式
- `ToolAllowlistHook` 已有工具级拦截模式（`on_before_tool` 可 Reject/Override）
- Agent ReAct 循环通过 `LooperHook` 的 8 个拦截点驱动

**补充方案：**

1. **agent.md 工具审批配置**：

   ```yaml
   tools:
     - name: shell
       approval: manual    # "auto"（默认）| "manual"（需用户确认）
     - name: fetch
       approval: auto
   ```

2. **`ToolApprovalHook`（扩展 LooperHook 模式）**：在 `on_before_tool` 中检查工具是否需要审批。需要审批时：
   - 发射 `LooperEvent::ApprovalRequired { tool_call_id, tool_name, arguments }`
   - 暂停 looper 内层循环，等待 intercom 通道接收 `approve_tool` / `deny_tool` 信号
   - 超时策略（默认 120s）：超时则拒绝 + 返回错误给 LLM

3. **前端渲染**：ChatView 中工具调用卡片显示「Approve / Deny」按钮（仅 `approval: manual` 的工具）。按钮在工具执行前替换 loading 状态，点击通过 SSE 事件回传审批决定。

**实现阶段：** Phase 5（基础设施完备，但 UX 设计需要时间，不宜仓促放入 v2 首版）。

---

### 9.3 用户反馈与回答再生（P1）

**行业现状：** 所有 AI 对话产品（ChatGPT、Dify、Coze）均提供 👍/👎 反馈和「重新生成」功能。

**Peco 当前基础：** 无。

**补充方案：**

1. **反馈 API**：

   | 方法 | 路径 | 说明 |
   |------|------|------|
   | POST | `/api/chat/:agentId/conversations/:id/feedback` | 提交反馈 `{message_id, rating: "up"|"down", comment?}` |

2. **数据库表**：

   ```sql
   CREATE TABLE conversation_feedback (
       id TEXT PRIMARY KEY,
       conversation_id TEXT NOT NULL REFERENCES conversations(id),
       message_id TEXT NOT NULL,
       user_id TEXT NOT NULL,
       rating TEXT NOT NULL CHECK(rating IN ('up', 'down')),
       comment TEXT,
       created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
   );
   ```

3. **前端组件**：ChatView 中每条 assistant 消息底部渲染 👍/👎/🔄 图标。
   - 👍/👎：调用反馈 API，切换高亮状态
   - 🔄：重新发送上一条用户消息（同一 SSE 端点），替换当前 assistant turn 的消息

4. **Peco 永续对话同样支持**：`POST /api/peco/feedback`。

**实现阶段：** Phase 2 — ChatView 提取时同步加入反馈 UI + API。

---

### 9.4 模型回退与多 Provider 支持（P1）

**行业现状：** Dify 支持多 provider 配置 + 回退链；Coze 内置多模型切换。

**Peco 当前基础：**
- `ModelProvider` trait 已干净抽象 provider 行为
- SSE 传输层已有 `RetryPolicy` / `ExponentialBackoff`
- `process_normalized_sse_stream()` 是 provider 无关的状态机 — 添加新 provider 只需实现 `StreamingProfile`
- 当前仅 DeepSeek 实现了 provider trait；仅支持单一 `default_provider`

**补充方案：**

1. **`providers.toml` fallback 链**：

   ```toml
   default_provider = "deepseek"

   [providers.deepseek]
   type = "deepseek"
   api_key = "${DEEPSEEK_API_KEY}"
   fallback = ["openai"]          # ← 新增：回退链

   [providers.openai]
   type = "openai"                # ← 新增 provider 类型
   api_key = "${OPENAI_API_KEY}"
   base_url = "https://api.openai.com/v1"
   fallback = []                  # 链尾无回退
   ```

2. **`FallbackModelProvider` 包装器**：实现 `ModelProvider` trait，按顺序尝试 provider 链。任意 provider 返回成功 → 立即返回；失败 → 记录 warn 日志 → 尝试下一个；所有 provider 失败 → 返回聚合错误。

3. **新增 OpenAI provider**：实现 `ModelProvider` trait + `StreamingProfile`，复用 `process_normalized_sse_stream()` SSE 状态机。与 DeepSeek 实现模式一致。

4. **前端**：ProviderListPage 展示 provider 状态（`health` 字段 + 回退链可视化）。

**实现阶段：** Phase 3（Provider CRUD API 是此功能的自然基础）。

---

### 9.5 agent.md 环境变量解析（P2）

**行业现状：** Dify 提供应用级环境变量，Coze 提供变量系统供 bot 配置使用。

**Peco 当前基础：** `resolve_env_vars()` 已存在并用于 MCP 配置和 `providers.toml`（`${VAR}` 语法），但未应用于 agent.md。

**补充方案：**

在 `Agent::from_file()` 解析 agent.md 时，对以下字段调用 `resolve_env_vars()`：
- `llm.model`
- `tools` 列表中的工具名
- `mcp` 列表中的服务器名
- Markdown 正文（系统提示词）

使用示例：

```yaml
llm:
  model: "${PECO_MODEL:-deepseek-v4-flash}"
  temperature: 0.3
```

不同环境通过设置 `PECO_MODEL=deepseek-v4-pro` 即可覆盖，无需修改 agent.md 文件。

**实现阶段：** Phase 1（一行调用，改动极小，与 peco 模块同期构建）。

---

### 9.6 对话导出（P2）

**行业现状：** ChatGPT 支持数据导出（JSON），Dify 支持对话日志下载。

**Peco 当前基础：** `SessionSnapshot` 序列化已完备，`GET /api/conversations/:id/session` 已返回完整快照 JSON。

**补充方案：**

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/chat/:agentId/conversations/:id/export?format=json` | JSON 导出（完整 `SessionSnapshot`，含工具调用、推理内容） |
| GET | `/api/chat/:agentId/conversations/:id/export?format=markdown` | Markdown 导出（人类可读转录，工具调用折叠为 `<details>`） |
| GET | `/api/peco/session/export?format=json\|markdown` | Peco 永续对话导出 |

Markdown 导出格式示例：

```markdown
# 对话记录 — developer / 修bug

**时间**：2026-07-31 14:30
**Agent**：developer (deepseek-v4-pro)
**Token 用量**：输入 1,234 · 输出 567

---

## 用户
帮我修一下 src/main.rs 第42行的 bug

## 助手
让我先看看代码。
> 🔧 shell: cat src/main.rs
>   (输出省略)

问题在于...（修复说明）
```

**实现阶段：** Phase 4（纯 `SessionSnapshot` → 格式转换，无新基础设施）。

---

### 9.7 优先级汇总与路线图影响

| 编号 | 补充项 | 优先级 | 建议阶段 | 对原路线图的影响 |
|------|--------|--------|----------|-----------------|
| 9.1 | 可观测性与 Token 用量追踪 | P1 | Phase 1 + 3 | Phase 1 `PecoConfig` 预留钩子；Phase 3 新增 API |
| 9.3 | 用户反馈与回答再生 | P1 | Phase 2 | ChatView 提取时加入反馈 UI |
| 9.4 | 模型回退与多 Provider | P1 | Phase 3 | 与 Provider CRUD 同步开发 |
| 9.5 | agent.md 环境变量 | P2 | Phase 1 | 一行改动，无工作量影响 |
| 9.2 | 工具人机协同审批 | P2 | Phase 5 | 新房 Phase，不放入本次 v2 |
| 9.6 | 对话导出 | P2 | Phase 4 | 收尾阶段加两个 endpoint |

**不纳入本次设计的内容（及其原因）：**

| 内容 | 原因 |
|------|------|
| **Agent 评估框架**（测试数据集、评分、对比） | 当前用户量级不需要；LangSmith 按量计费，Peco 自建 ROI 低。Phase 5+ 可考虑引入社区方案。 |
| **API 密钥管理**（将 Agent 暴露为外部 API） | 不同产品方向 — Peco 定位是个人/团队内部使用的 Agent 平台，不是 API 托管服务。 |
| **审计日志** | 企业级需求，个人/小团队使用场景价值有限。自然操作痕迹（`messages`、`session_snapshots`、`task_logs`）已覆盖基本追溯需求。 |
| **可视化工作流编排**（画布/拖拽编辑器） | Workflow 引擎已有完整 DAG 引擎，但 UI 编排独立于本次重构。需独立设计文档，参考 Dify/Coze 形态后决策。 |
| **社区 Agent/Skill 市场** | 已在 §4.10 和 §4.12 中明确排除 — Peco 通过文件共享分发，GitHub = 市场。 |

---



```
┌──────────────────────────────────────────────────────────┐
│ ← → Peco                                          🗑 清除 │
│──────────────────────────────────────────────────────────│
│                                                          │
│  👋 你好！我是 Peco，你的个人 AI 助理。                        │
│  我可以执行命令、管理记忆、搜索知识库、调用子 Agent。              │
│                                                          │
│  ┌──────────────────────────────────────────────────┐     │
│  │ 用户: 帮我查看今天的 git log                          │     │
│  └──────────────────────────────────────────────────┘     │
│                                                          │
│  ┌──────────────────────────────────────────────────┐     │
│  │ Peco:                                            │     │
│  │ > 推理过程 ▼                                      │     │
│  │                                                 │     │
│  │ 🔧 shell (git log --since="2026-07-31" --oneline) │     │
│  │   ✓ b5e562a chore: 清理运行产生的配置文件            │     │
│  │                                                 │     │
│  │ 你今天的提交记录如下：                              │     │
│  │ - b5e562a chore: 清理配置文件                      │     │
│  │ - 61ee553 docs: add Workflow module docs          │     │
│  └──────────────────────────────────────────────────┘     │
│                                                          │
│                                                          │
│──────────────────────────────────────────────────────────│
│ [输入框                                          ] [发送] │
└──────────────────────────────────────────────────────────┘
```

## 附录 B：侧边栏交互

```
折叠状态（仅图标）：
┌────┐
│ ✨ │  Peco
│ 💬 │  对话
│ ⚙ │  管理
│ 📋 │  任务
│ 🔧 │  设置
└────┘

展开状态：
┌──────────────────┐
│ ✨  Peco         │
│                  │
│ 💬  对话     ▾   │
│   ─────────────  │
│   developer      │
│     📝 修 bug    │
│     📝 写周报    │
│   code-reviewer  │
│     📝 重构项目  │
│   ...            │
│   ─────────────  │
│   📦 已归档 (3)  │
│                  │
│ ⚙  管理     ▾   │
│   🖥 Provider   │
│   🤖 Agent     │
│   🧩 Skill     │
│   🔌 MCP       │
│   📚 Knowledge │
│                  │
│ 📋  任务         │
│ 🔧  设置         │
└──────────────────┘
```
