# peco-webui

AI Agent 平台的 React + TypeScript 前端，对接 [peco-server](../crates/peco-server/) 后端 API，提供完整的 Agent 管理、SSE 流式对话、知识库和定时任务调度界面。

## 快速开始

```bash
cd webui
npm install
npm run dev        # 开发模式，默认 http://localhost:9233
```

Vite 自动将 `/api` 请求代理到 `http://localhost:9227`（peco-server 默认端口）。
确保后端已启动且 `DEEPSEEK_API_KEY` 已设置。

## 技术栈

| 领域 | 技术 |
|------|------|
| 框架 | React 19 + TypeScript |
| 构建 | Vite 8 |
| 路由 | React Router v7 |
| UI | Tailwind CSS v4 + shadcn/ui (Radix) |
| 状态管理 | Zustand |
| 表单 | React Hook Form + Zod |
| HTTP 客户端 | Axios（拦截器自动注入 JWT + 处理 401/429） |
| SSE 流 | fetch + ReadableStream（自定义 SSE 协议解析） |
| Markdown | react-markdown + remark-gfm + rehype-highlight |
| 测试 | Vitest + React Testing Library |
| Cron 描述 | cronstrue |

## 项目结构

```
webui/
├── index.html
├── package.json
├── vite.config.ts              # Vite 配置 + proxy + Vitest
├── tsconfig.json
├── components.json             # shadcn/ui 配置
└── src/
    ├── main.tsx                # 入口
    ├── App.tsx                 # 路由 + Provider (ErrorBoundary, Suspense, Tooltip)
    │
    ├── types/                  # 与后端 API 对齐的类型定义
    │   ├── auth.ts             # User, AuthResponse, LoginRequest
    │   ├── agent.ts            # AgentListItem, AgentDetail, CreateAgentRequest
    │   ├── chat.ts             # Conversation, Message, ChatSseEvent (9 种事件 tagged union)
    │   ├── knowledge.ts        # KnowledgeBase, Document, SyncResult
    │   ├── task.ts             # Task, TaskLog
    │   └── common.ts           # SuccessResponse
    │
    ├── api/                    # API 层
    │   ├── client.ts           # axios 实例 + JWT 拦截器 + 401/429 处理
    │   ├── auth.ts             # POST /auth/register · /login · /me
    │   ├── agents.ts           # CRUD /agents
    │   ├── conversations.ts    # CRUD /conversations + session snapshot
    │   ├── stream.ts           # ★ SSE 流解析器（纯函数，可单元测试）
    │   ├── knowledge.ts        # CRUD /knowledge + 文档上传/同步
    │   ├── tasks.ts            # CRUD /tasks + toggle + 日志
    │   └── __tests__/
    │       └── stream.test.ts  # SSE 解析管道测试 (12 tests)
    │
    ├── stores/                 # Zustand
    │   ├── authStore.ts        # user/token/login/logout (localStorage 持久化)
    │   └── sidebarStore.ts     # 侧栏折叠状态
    │
    ├── components/
    │   ├── layout/
    │   │   ├── AppLayout.tsx   # 桌面侧栏 + 移动端 Sheet
    │   │   ├── Sidebar.tsx     # 导航菜单 + 用户信息 + 登出
    │   │   └── Header.tsx      # 页面标题 + 用户下拉菜单
    │   ├── ui/                 # shadcn/ui 组件 (17 个)
    │   ├── common/
    │   │   ├── LoadingSpinner.tsx
    │   │   ├── EmptyState.tsx
    │   │   └── ErrorBanner.tsx
    │   ├── ErrorBoundary.tsx   # 渲染崩溃捕获 + 重置
    │   └── ProtectedRoute.tsx  # 路由守卫 (无 token → /login)
    │
    ├── pages/
    │   ├── auth/
    │   │   ├── LoginPage.tsx       # 邮箱 + 密码登录
    │   │   └── RegisterPage.tsx    # 用户名 + 邮箱 + 密码注册
    │   ├── chat/
    │   │   ├── ChatListPage.tsx    # 对话列表（新建/删除）
    │   │   └── ChatDetailPage.tsx  # ★ SSE 流式聊天（文本/工具调用/Agent/推理）
    │   ├── agents/
    │   │   ├── AgentListPage.tsx   # 卡片网格
    │   │   ├── AgentCreatePage.tsx # 创建
    │   │   ├── AgentEditPage.tsx   # 编辑
    │   │   └── components/
    │   │       └── AgentForm.tsx   # 表单（名称/模型/工具/icon/color/temperature）
    │   ├── knowledge/
    │   │   ├── KnowledgeListPage.tsx   # 知识库列表
    │   │   └── KnowledgeDetailPage.tsx # 文档管理 + 上传 + 同步
    │   ├── tasks/
    │   │   ├── TaskListPage.tsx    # 任务列表 + toggle
    │   │   ├── TaskCreatePage.tsx  # 创建（Cron + Agent 选择）
    │   │   └── TaskLogsPage.tsx    # 执行日志
    │   └── settings/
    │       └── SettingsPage.tsx    # 用户信息 + 登出
    │
    └── lib/
        ├── utils.ts            # cn() tailwind 工具
        ├── validators.ts       # Zod schema (login, register, agent, kb)
        └── constants.ts        # 模型/Provider/工具选项
```

## SSE 流式对话架构

前端通过 `fetch + ReadableStream` 直接消费 peco-server 的自定义 SSE 协议：

```
peco-server SSE (event: + data: 行)
  ↓ parseSSELines()         — 行解析 + 缓冲区管理
  ↓ toChatSseEvent()        — 事件映射为 typed ChatSseEvent
  ↓ handleSSEEvent()        — 更新 React 消息状态
ChatBubble 组件渲染
```

支持的 9 种 SSE 事件类型：

| 事件 | 前端行为 |
|------|---------|
| `text_delta` | 追加到当前 assistant 消息 content |
| `reasoning_delta` | 追加到可折叠的推理块 |
| `tool_call_start` | 插入工具调用卡片（loading 态） |
| `tool_result` | 更新卡片为完成态，展示结果 |
| `agent_call_start` | 插入子 Agent 卡片（含 call_id 配对） |
| `agent_call_end` | 更新子 Agent 卡片（按 call_id 匹配） |
| `turn_complete` | 标记本轮完成 |
| `done` | 关闭流 |
| `error` | 显示错误 |

`agent_call_start` 和 `agent_call_end` 通过 `call_id` 字段直接配对（无需 FIFO），
`call_id` 由后端透传 LLM 的 `tool_call_id`（并行任务为 `{tool_call_id}:{index}`）。

## 路由设计

```
/login                  → LoginPage          (公开)
/register               → RegisterPage       (公开)
/chat                   → ChatListPage       (需认证)
/chat/:conversationId   → ChatDetailPage     (需认证)
/agents                 → AgentListPage      (需认证)
/agents/new             → AgentCreatePage    (需认证)
/agents/:agentId/edit   → AgentEditPage      (需认证)
/knowledge              → KnowledgeListPage  (需认证)
/knowledge/:kbId        → KnowledgeDetailPage(需认证)
/tasks                  → TaskListPage       (需认证)
/tasks/new              → TaskCreatePage     (需认证)
/tasks/:taskId/logs     → TaskLogsPage       (需认证)
/settings               → SettingsPage       (需认证)
```

## 认证流程

1. 用户登录/注册 → 后端返回 JWT token（7 天有效期）
2. Token 存入 Zustand + localStorage，axios 拦截器自动注入 `Authorization: Bearer`
3. 页面刷新 → 从 localStorage 恢复 token + 后端 `/auth/me` 验证有效性
4. 401 响应 → 自动清除 token 并跳转 `/login`
5. 429 响应 → Toast 提示 `Retry-After`

## 命令行

```bash
npm run dev         # 启动开发服务器 (http://localhost:9233)
npm run build       # TypeScript 编译 + Vite 生产构建
npm run preview     # 预览生产构建
npx vitest run      # 运行单元测试
npx tsc --noEmit    # TypeScript 类型检查
```

## 环境要求

- Node.js 22+
- npm 10+
- peco-server 后端运行在 `localhost:9227`

## 生产部署

构建产物位于 `dist/` 目录，可直接由 nginx 或其他静态文件服务器托管：

```bash
npm run build
# 将 dist/ 部署到 Web 服务器
# 确保 /api 路径反向代理到 peco-server
```

Nginx 配置示例：

```nginx
server {
    listen 80;
    root /var/www/peco-webui/dist;
    index index.html;

    # SPA fallback
    location / {
        try_files $uri $uri/ /index.html;
    }

    # API 代理
    location /api/ {
        proxy_pass http://localhost:9227;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_buffering off;           # SSE 流式对话需要关闭缓冲
    }
}
```

## 设计文档

完整设计方案、框架对比（Vercel AI SDK / CopilotKit / assistant-ui / LangChain / Mastra）、
设计评审和分阶段开发计划见 [peco 设计文档](../.claude/plans/swirling-whistling-cookie.md)。
