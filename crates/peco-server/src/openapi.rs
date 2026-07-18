// ============================================================================
// OpenAPI / Swagger 文档定义
// ============================================================================

use utoipa::OpenApi;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};

/// 顶层 OpenAPI 文档结构。
///
/// 访问路径：`/docs` — Swagger UI
///          `/api-docs/openapi.json` — OpenAPI JSON
#[derive(OpenApi)]
#[openapi(
    info(
        title = "peco-server API",
        version = "0.1.0",
        description = "AI Agent 平台后端 RESTful API\n\n\
            提供多用户 Agent 管理、SSE 流式对话、知识库（文档上传+混合检索）、\
            定时任务（Cron 调度 Agent 执行）等功能。\n\n\
            ## 认证\n\
            除 `/api/auth/*` 外，所有接口需 Bearer Token 认证：\n\
            ```\n\
            Authorization: Bearer <token>\n\
            ```\n\n\
            ## SSE 流式对话\n\
            `GET /api/conversations/:id/stream?message=xxx`\n\
            事件类型：`text_delta`, `tool_call_start`, `tool_result`, \
            `agent_call_start`, `agent_call_end`, `turn_complete`, `done`, `error`",
        contact(name = "peco", url = "https://github.com/pcfine/peco"),
    ),
    servers((url = "http://localhost:9227", description = "本地开发服务器")),
    modifiers(&SecurityAddon),
    tags(
        (name = "Auth", description = "用户认证 — 注册、登录、获取当前用户"),
        (name = "Agents", description = "AI Agent 管理 — CRUD + 状态"),
        (name = "Conversations", description = "对话管理 — 创建/列表/删除 + SSE 流式聊天"),
        (name = "Knowledge", description = "知识库 — 创建/列表/删除 + 文档管理"),
        (name = "Tasks", description = "定时任务 — Cron 调度 Agent 执行"),
    ),
)]
pub struct ApiDoc;

/// 安全方案：JWT Bearer Token。
struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}
