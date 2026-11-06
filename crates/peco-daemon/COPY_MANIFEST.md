# COPY_MANIFEST —— `peco-server` → `peco-daemon` 的拷贝账
#
# 每行四个字段：`<源路径> <目标路径> <处置> <改造摘要>`，路径相对仓库根。
# 处置取值：`copy` / `copy+mod` / `不copy`（本轮明确不拷）/ `未copy`（后续阶段再拷）。
# 机器核验见 crates/peco-daemon/tests/copy_manifest.rs：源存在、目标存在、目标不含 `use crate::auth`。

crates/peco-server/src/db/mod.rs crates/peco-daemon/src/db/mod.rs copy+mod 剪裁 mod 列表（删 sync/workspace_hashes），补 017 迁移门控
crates/peco-server/src/db/schema.sql crates/peco-daemon/src/db/schema.sql copy+mod 加 teams/workspaces/d_dispatches 三表与 users 种子
crates/peco-server/src/db/agents.rs crates/peco-daemon/src/db/agents.rs copy 无
crates/peco-server/src/db/artifacts.rs crates/peco-daemon/src/db/artifacts.rs copy 无
crates/peco-server/src/db/compaction_log.rs crates/peco-daemon/src/db/compaction_log.rs copy 无
crates/peco-server/src/db/context_shadow.rs crates/peco-daemon/src/db/context_shadow.rs copy 无
crates/peco-server/src/db/conversations.rs crates/peco-daemon/src/db/conversations.rs copy 无
crates/peco-server/src/db/documents.rs crates/peco-daemon/src/db/documents.rs copy 无
crates/peco-server/src/db/knowledge_bases.rs crates/peco-daemon/src/db/knowledge_bases.rs copy 无
crates/peco-server/src/db/memory_audit.rs crates/peco-daemon/src/db/memory_audit.rs copy+mod insert 增 kb_scope 列，行内补读
crates/peco-server/src/db/memory_consolidation_optin.rs crates/peco-daemon/src/db/memory_consolidation_optin.rs copy 无
crates/peco-server/src/db/memory_consolidation_state.rs crates/peco-daemon/src/db/memory_consolidation_state.rs copy 无
crates/peco-server/src/db/memory_recall_stats.rs crates/peco-daemon/src/db/memory_recall_stats.rs copy 无
crates/peco-server/src/db/memory_supersede.rs crates/peco-daemon/src/db/memory_supersede.rs copy 无
crates/peco-server/src/db/messages.rs crates/peco-daemon/src/db/messages.rs copy 无
crates/peco-server/src/db/session_archive.rs crates/peco-daemon/src/db/session_archive.rs copy 无
crates/peco-server/src/db/workflow_executions.rs crates/peco-daemon/src/db/workflow_executions.rs copy 无
crates/peco-server/src/db/workflow_schedules.rs crates/peco-daemon/src/db/workflow_schedules.rs copy 无
crates/peco-server/src/db/migrations/002_slim_agents.sql crates/peco-daemon/src/db/migrations/002_slim_agents.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/003_conversations_v2.sql crates/peco-daemon/src/db/migrations/003_conversations_v2.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/004_agent_background.sql crates/peco-daemon/src/db/migrations/004_agent_background.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/005_workflow_executions.sql crates/peco-daemon/src/db/migrations/005_workflow_executions.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/006_drop_agent_color.sql crates/peco-daemon/src/db/migrations/006_drop_agent_color.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/007_peco_compaction_log.sql crates/peco-daemon/src/db/migrations/007_peco_compaction_log.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/008_peco_session_archives.sql crates/peco-daemon/src/db/migrations/008_peco_session_archives.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/009_peco_memory_consolidation.sql crates/peco-daemon/src/db/migrations/009_peco_memory_consolidation.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/010_peco_memory_consolidation_optin.sql crates/peco-daemon/src/db/migrations/010_peco_memory_consolidation_optin.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/011_peco_memory_supersede_shadow.sql crates/peco-daemon/src/db/migrations/011_peco_memory_supersede_shadow.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/012_peco_memory_supersede_intent.sql crates/peco-daemon/src/db/migrations/012_peco_memory_supersede_intent.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/013_peco_context_shadow.sql crates/peco-daemon/src/db/migrations/013_peco_context_shadow.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/014_documents_manifest.sql crates/peco-daemon/src/db/migrations/014_documents_manifest.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/015_artifacts.sql crates/peco-daemon/src/db/migrations/015_artifacts.sql copy 既有迁移，include_str! 依赖
crates/peco-server/src/db/migrations/016_conversation_scope.sql crates/peco-daemon/src/db/migrations/016_conversation_scope.sql copy 既有迁移，include_str! 依赖
- crates/peco-daemon/src/db/migrations/017_daemon_kb_scope.sql new daemon 新增（kb_scope 补列）
crates/peco-server/src/workspace/memory_audit.rs crates/peco-daemon/src/space/memory_audit.rs copy+mod user_id 归一为 LOCAL_USER_ID，scope 写 kb_scope

crates/peco-server/src/agent/handler.rs crates/peco-daemon/src/agent/handler.rs copy+mod AuthUser→LocalUser、user_id→LOCAL_USER_ID、workspace_manager→spaces；删 workspace_hashes 与 upload 清理写入；加保留名闸门
crates/peco-server/src/agent/mod.rs crates/peco-daemon/src/agent/mod.rs copy+mod 路由状态类型 Arc<AppState>→AppState
crates/peco-server/src/skill/handler.rs crates/peco-daemon/src/skill/handler.rs copy+mod 同上；删 skills 哈希回写
crates/peco-server/src/skill/mod.rs crates/peco-daemon/src/skill/mod.rs copy+mod 同上
crates/peco-server/src/knowledge/handler.rs crates/peco-daemon/src/knowledge/handler.rs copy+mod 同上；docs_dir 取实例 root；后台摄入任务持 Arc<WorkSpace>
crates/peco-server/src/knowledge/mod.rs crates/peco-daemon/src/knowledge/mod.rs copy+mod 同上；路径保持 /api/knowledge
crates/peco-server/src/knowledge/reconcile.rs crates/peco-daemon/src/knowledge/reconcile.rs copy+mod error::ApiError→api_error::ApiError
crates/peco-server/src/mcp_config/handler.rs crates/peco-daemon/src/mcp_config/handler.rs copy+mod 同上；删 mcp 哈希回写
crates/peco-server/src/mcp_config/mod.rs crates/peco-daemon/src/mcp_config/mod.rs copy+mod 同上

crates/peco-server/src/session_store/mod.rs crates/peco-daemon/src/session_store/mod.rs copy+mod 只做编译适配，本期不接线（下一阶段 chat 直接用）；测试夹具按 serde 形状表达输入条目，避免为此新增 provider 依赖
crates/peco-server/src/file_watcher/mod.rs crates/peco-daemon/src/file_watcher/mod.rs copy+mod 监听目标改为团队根，句柄挂到空间句柄上（实例摘表即停）；另持 Weak<WorkSpace> 兜底
crates/peco-server/src/file_watcher/watcher.rs crates/peco-daemon/src/file_watcher/watcher.rs copy+mod 删 db::sync / workspace_hashes 依赖，改调 space::index + space::fanout；只监听 agents/skills（知识库与 provider 的 per-space 重载在团队根上会变成 N 倍重复劳动）
- crates/peco-daemon/src/space/index.rs new 磁盘 → DB 的 agent 索引补齐（只补不删，幂等返回新增行数）
- crates/peco-daemon/src/space/fanout.rs new 团队级 Agent 缓存失效扇出（rescan 刷 Tier-1 + 逐名 invalidate 清 Tier-2）

- crates/peco-daemon/src/api_error.rs new 由 peco-server/src/error.rs 改写：加 Coded 变体与 From<DaemonError>，去掉 JWT/bcrypt 转换
- crates/peco-daemon/src/auth.rs new LocalUser 提取器（X-Peco-Space 头 / ?ws= 兜底），单用户语义
- crates/peco-daemon/src/space/registry.rs new ws_id→实例适配层，唯一真源仍是 WorkspaceHub

crates/peco-server/src/db/sync.rs - 不copy 只服务多用户 DB↔磁盘双向同步
crates/peco-server/src/db/workspace_hashes.rs - 不copy 只服务多用户哈希增量同步
crates/peco-server/src/workspace/manager.rs - 不copy 单 workspace + LRU，换成 registry.rs；审计注入点改到 space/open.rs
crates/peco-server/src/auth - 不copy 单用户无鉴权
crates/peco-server/src/middleware - 不copy 只留 CORS + trace

crates/peco-server/src/peco - 未copy 后续阶段（记忆双路径、Peco 会话）
crates/peco-server/src/workflow - 未copy 后续阶段
crates/peco-server/src/provider - 未copy 后续阶段
crates/peco-server/src/usage - 未copy 后续阶段

crates/peco-server/src/chat/mod.rs crates/peco-daemon/src/chat/mod.rs copy+mod 删 /api/conversations legacy 路由与 5 个 legacy wrapper；Router<Arc<AppState>>→Router<AppState>
crates/peco-server/src/chat/handler.rs crates/peco-daemon/src/chat/handler.rs copy+mod AuthUser→LocalUser、state.db→state.pool、workspace_manager.get_agent→spaces.workspace().agent_manager()、会话查询全部按 workspace_id；响应加 workspace_id；删 archive_markdown（peco 归档专用）
crates/peco-server/src/chat/sse.rs crates/peco-daemon/src/chat/sse.rs copy 无（只依赖 artifact::ArtifactItem 与 core 事件类型）
crates/peco-server/src/chat/active.rs crates/peco-daemon/src/chat/active.rs copy 无
crates/peco-server/src/chat/conversation.rs crates/peco-daemon/src/chat/conversation.rs copy+mod 容量上限按 workspace_id 计数
crates/peco-server/src/chat/images.rs crates/peco-daemon/src/chat/images.rs copy+mod 上传目录由 data_dir/uploads/chat/<user_id> 改为 $PECO_HOME/uploads/chat/<ws_id>；归属校验按会话的 workspace_id
crates/peco-server/src/artifact/mod.rs crates/peco-daemon/src/artifact/mod.rs copy+mod 落盘根由 WorkspaceManager 的 data_dir/work/<user_id> 改为 ws.work_dir()；Router<Arc<AppState>>→Router<AppState>
crates/peco-server/src/artifact/handler.rs crates/peco-daemon/src/artifact/handler.rs copy+mod AuthUser→LocalUser；归属改查会话的 workspace_id；取流路径由空间 work 根拼出
crates/peco-server/src/session_dto.rs crates/peco-daemon/src/session_dto.rs copy 无
