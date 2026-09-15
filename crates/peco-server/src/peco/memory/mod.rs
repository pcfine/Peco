// ============================================================================
// memory — Peco 记忆双路径（写 hook + 读 dynamic_context）
// ============================================================================
//
// 与 compaction 的分工：compaction 解决"会话内上下文放不下"；
// 本模块解决"跨会话/超长期的知识"。
//
// - 写路径（hook.rs）：每轮成功完成后由 Flash 模型提取记忆，
//   后台写入 `@private_memory` KB。
// - 读路径（recall.rs）：每个新用户 query 前检索相关记忆，
//   经既有 DynamicContext 机制注入 instructions。
//
// 存储载体是 workspace 内的 `@private_memory` 知识库（personal 模板
// 幂等安装），本模块不引入新的存储抽象 — 直接使用 `KnowledgeManager`。
//
// V1 语义：自动路径只做 add（提取新信息）；记忆的更新/删除由
// `@memory` 子 agent 的显式工具路径负责，两条路径职责正交。

pub mod analyzer;
pub mod config;
pub mod consolidation;
pub mod cron;
pub mod dedup;
pub mod hook;
pub mod recall;

pub use analyzer::{MemoryCategory, MemoryFact, ModelTurnAnalyzer, TurnAnalyzer};
pub use config::{ConsolidationConfig, MemoryConfig};
pub use consolidation::{ConsolidationWorker, Distiller, ModelDistiller, RunStats, WorkerError};
pub use hook::MemoryExtractionHook;
pub use recall::MemoryRecallContext;

use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

/// 装配 per-user 自动整理 worker。
///
/// 手动触发端点（`peco::handler::consolidate_now`）与 cron tick
/// （`cron::run_tick`）共用同一装配口径 —— 两条通道只在「谁触发」上不同，
/// worker 的依赖（KB、统计库、配置、Flash provider）必须完全一致。
pub async fn build_worker(
    state: &Arc<AppState>,
    user_id: &str,
    memory: &MemoryConfig,
) -> Result<ConsolidationWorker, ApiError> {
    let ws = state
        .workspace_manager
        .get_synced(user_id, &state.db)
        .await?;
    let km = Arc::clone(ws.knowledge_manager());
    // Flash 档 provider 复用主 Agent 的（与 compaction / 记忆提取同范式）
    let agent = state.workspace_manager.get_agent(user_id, "@assistant")?;

    Ok(ConsolidationWorker::new(
        km,
        state.db.clone(),
        &memory.kb_name,
        &memory.model,
        memory.consolidation.clone(),
        agent.provider().clone(),
    ))
}
