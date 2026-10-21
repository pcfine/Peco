//! 记忆删除编排（agent 工具与 REST 端点共用）：outbox pending → 删除 → done/cancelled。
//!
//! 两个调用方各自映射错误（工具 → `ToolError`，HTTP → `ApiError`），编排本身只表达
//! 失败分类。合并成一份的理由是 outbox 顺序必须只有一套：漏写 `mark_cancelled`
//! 就会留下「东西没删、审计说删了」的假成功行，而这种缺陷只在测试里表现为偶发脏数据。

use std::sync::Arc;

use serde_json::json;
use tracing::{info, warn};

use super::deps::{KnowledgeAccess, MemoryAuditAccess, MemoryAuditEntry};
use crate::knowledge::hash_manifest::now_iso8601;
use crate::knowledge::{KnowledgeManager, KnowledgeModuleError};
use knowledge_base::KnowledgeError;

/// 偏好类记忆的硬守卫标签 —— source_path 为该值的文档一律拒绝删除。
///
/// 画像由系统持续重建，单条删除只会让它下一轮又被写回来；两个入口（agent 工具 /
/// REST 端点）都在写审计之前拒绝，不存在绕过路径。
pub const PROFILE_SOURCE: &str = "ppa_profile";

/// 图事实在 `memory_audit.source` 上的显式标记 —— 回滚重放按它分流
/// （事实走 `add_facts`，文档走文档重建）。
pub(crate) const GRAPH_FACT_SOURCE: &str = "graph_fact";

/// 删除动作来源 —— 决定审计行的 `deleted_by` / `reason`。
///
/// 收敛在枚举里，避免两个调用方各填一套字面量后漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOrigin {
    Agent,
    User,
}

impl DeleteOrigin {
    pub fn deleted_by(self) -> &'static str {
        match self {
            Self::Agent => "agent:@memory",
            Self::User => "user",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Agent => "manual_organize",
            Self::User => "user_delete",
        }
    }
}

/// 删除失败分类 —— 调用方各自映射成自己的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum MemoryDeleteError {
    /// 目标不存在（含并发重复删除）→ HTTP 404。
    #[error("{0}")]
    NotFound(String),
    /// 目标受保护（profile 文档）→ HTTP 403。
    #[error("{0}")]
    Forbidden(String),
    /// 非级联删除仍残留关联边 → HTTP 409。
    #[error("{0}")]
    Conflict(String),
    /// 审计存储不可用（fail-closed）→ HTTP 500。
    #[error("{0}")]
    Unavailable(String),
    /// 其余失败 → HTTP 500。
    #[error("{0}")]
    Internal(String),
}

/// 单条文档删除的收口结果（outbox 已迁移为 done）。
pub struct DeletedDoc {
    pub doc_id: String,
    pub title: String,
    pub audit_id: i64,
    pub removed_chunks: usize,
}

/// 单个实体节点删除的收口结果（outbox 已迁移为 done）。
pub struct DeletedEntity {
    pub entity_id: String,
    /// 审计 title 用的人类可读名。
    pub label: String,
    pub audit_id: i64,
    pub removed_edges: usize,
    pub removed_nodes: usize,
}

/// 单条文档删除编排（outbox：pending → done / cancelled）。
///
/// 取原文（title / content / source 是审计记录与回滚重放的依据）→ profile
/// 硬守卫 → 写 pending 审计 → 删除 → 成功 mark_done / 失败 mark_cancelled
/// 并返回错误。文档不存在时不产生审计行：不存在的东西没有「删除」可审计。
///
/// 文档删除成功后 mark_done 失败只降级为 warn：删除已是既成事实，向调用方报错会
/// 得到「文档已删却报告失败」的假失败（调用方可能重试并撞上文档不存在）；残留的
/// pending 行保留完整原文，可人工恢复。
pub async fn delete_document(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    doc_id: &str,
    origin: DeleteOrigin,
) -> Result<DeletedDoc, MemoryDeleteError> {
    let km = access.knowledge_manager();
    km.ensure_loaded()
        .await
        .map_err(|e| MemoryDeleteError::Internal(e.to_string()))?;

    let doc = match km.get_document(kb_name, doc_id).await {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            return Err(MemoryDeleteError::NotFound(format!(
                "Document not found: '{doc_id}' (knowledge base '{kb_name}')"
            )));
        }
        // 库不存在也是 404：对调用方而言「这里没有这条文档」。
        Err(e @ KnowledgeModuleError::NotFound(_)) => {
            return Err(MemoryDeleteError::NotFound(e.to_string()));
        }
        Err(e) => return Err(MemoryDeleteError::Internal(e.to_string())),
    };

    // 守卫必须在 record_pending 之前：否则会留下「文档还在、审计说删了」的脏行。
    if doc.source_path == PROFILE_SOURCE {
        return Err(MemoryDeleteError::Forbidden(format!(
            "Deletion rejected: document '{doc_id}' is a profile memory ({PROFILE_SOURCE}) and cannot be deleted."
        )));
    }

    let audit_id = audit
        .record_pending(MemoryAuditEntry {
            user_id: access.user_id().to_string(),
            kb_name: kb_name.to_string(),
            doc_id: doc.id.clone(),
            title: doc.title.clone(),
            content: doc.content.clone(),
            source: doc.source_path.clone(),
            reason: origin.reason().to_string(),
            deleted_by: origin.deleted_by().to_string(),
            deleted_at: now_iso8601(),
            topic_key: None,
            successor_doc_id: None,
        })
        .await
        .map_err(MemoryDeleteError::Internal)?;

    match km.delete_document(kb_name, &doc.id).await {
        Ok(report) => {
            if let Err(e) = audit.mark_done(audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Failed to mark audit row done; a pending row remains (original content retained for manual restore)"
                );
            }
            info!(
                kb = %kb_name,
                doc_id = %doc.id,
                audit_id,
                removed_chunks = report.removed_chunks,
                "Memory document deleted"
            );
            Ok(DeletedDoc {
                doc_id: doc.id,
                title: doc.title,
                audit_id,
                removed_chunks: report.removed_chunks,
            })
        }
        Err(e) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(MemoryDeleteError::Internal(e.to_string()))
        }
    }
}

/// 按实体名删除 —— agent 工具的入参是名字，图谱没给它 id。
pub async fn delete_entity_by_name(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    entity_name: &str,
    cascade: bool,
    origin: DeleteOrigin,
) -> Result<DeletedEntity, MemoryDeleteError> {
    let km = access.knowledge_manager();
    delete_entity_flow(
        km,
        access.user_id(),
        audit,
        kb_name,
        EntityTarget::Name(entity_name),
        cascade,
        origin,
    )
    .await
}

/// 按节点 id 删除 —— 图谱下发的 id 即真值，不得用名称反算（反算错了图后端静默 no-op）。
pub async fn delete_entity_by_id(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    entity_id: &str,
    cascade: bool,
    origin: DeleteOrigin,
) -> Result<DeletedEntity, MemoryDeleteError> {
    let km = access.knowledge_manager();
    delete_entity_flow(
        km,
        access.user_id(),
        audit,
        kb_name,
        EntityTarget::Id(entity_id),
        cascade,
        origin,
    )
    .await
}

/// 实体的定位方式：工具按名、图谱端点按 id。
enum EntityTarget<'a> {
    Name(&'a str),
    Id(&'a str),
}

impl EntityTarget<'_> {
    /// 报错文案里的「你指的是谁」——按名删除时回名字，按 id 删除时只能回 id。
    fn describe(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Id(id) => id,
        }
    }
}

/// 实体删除编排（outbox：pending → done / cancelled）。
///
/// 第 2 步的「先读」不可省略：图后端删不存在的节点是静默 no-op（无错误、无信号），
/// 不看前置读就没有任何办法区分「删掉了」与「本来就没有」。因此「实体不存在」是
/// **错误**而非幂等成功 —— 报错是调用方发现「名字拼错了」的唯一信号。
///
/// 比事实删除多一道 `cascade` 门，且这道门必须在**写审计之前**：拒绝时什么都没删，
/// 不该留下任何审计行。「有残留边却不带 `cascade`」是最容易发生的误操作（想忘掉
/// 某个实体的年龄，却升级成忘掉整个实体），因此拒绝信息要给出边数并指出 `cascade`。
async fn delete_entity_flow(
    km: &KnowledgeManager,
    user_id: &str,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    target: EntityTarget<'_>,
    cascade: bool,
    origin: DeleteOrigin,
) -> Result<DeletedEntity, MemoryDeleteError> {
    km.ensure_loaded()
        .await
        .map_err(|e| MemoryDeleteError::Internal(e.to_string()))?;

    let (entity_id, node, edges) = match &target {
        EntityTarget::Name(name) => km
            .read_entity(kb_name, name)
            .await
            .map_err(delete_entity_read_error)?,
        EntityTarget::Id(id) => {
            let (node, edges) = km
                .read_entity_by_id(kb_name, id)
                .await
                .map_err(delete_entity_read_error)?;
            ((*id).to_string(), node, edges)
        }
    };

    if node.is_none() && edges.is_empty() {
        return Err(MemoryDeleteError::NotFound(format!(
            "Entity not found: '{}' (knowledge base '{kb_name}')",
            target.describe()
        )));
    }

    if !cascade && !edges.is_empty() {
        return Err(MemoryDeleteError::Conflict(format!(
            "Deletion rejected: entity '{}' still has {} relation(s). \
             Use delete_entity_fact to remove a single relation, or pass cascade: true \
             to delete the entity together with all its relations.",
            target.describe(),
            edges.len()
        )));
    }

    let label = entity_label(&node, &entity_id);

    let audit_id = audit
        .record_pending(MemoryAuditEntry {
            user_id: user_id.to_string(),
            kb_name: kb_name.to_string(),
            doc_id: entity_id.clone(),
            title: format!("entity:{label}"),
            content: entity_snapshot_json(&label, &entity_id, &node, &edges).to_string(),
            source: GRAPH_FACT_SOURCE.to_string(),
            reason: origin.reason().to_string(),
            deleted_by: origin.deleted_by().to_string(),
            deleted_at: now_iso8601(),
            topic_key: None,
            successor_doc_id: None,
        })
        .await
        .map_err(MemoryDeleteError::Internal)?;

    match km.delete_entity_by_id(kb_name, &entity_id, cascade).await {
        Ok((removed_edges, removed_nodes)) => {
            if let Err(e) = audit.mark_done(audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Failed to mark audit row done; a pending row remains (snapshot retained for manual restore)"
                );
            }
            info!(
                kb = %kb_name,
                entity_id = %entity_id,
                removed_edges,
                removed_nodes,
                audit_id,
                "Memory entity deleted"
            );
            Ok(DeletedEntity {
                entity_id,
                label,
                audit_id,
                removed_edges,
                removed_nodes,
            })
        }
        // 读-删之间被并发删掉：第二次请求拿 404，而不是把「已经没了」报成内部错误。
        // 本请求没有执行删除（节点已被他人删掉），故收口为 cancelled 而非 done；
        // 不收口就会留下一条没有清理者的 pending 行（启动期对账只认 superseded）。
        Err(e @ KnowledgeModuleError::Knowledge(KnowledgeError::NotFound(_))) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(MemoryDeleteError::NotFound(e.to_string()))
        }
        Err(e) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(MemoryDeleteError::Internal(e.to_string()))
        }
    }
}

/// 读实体失败 → 删除侧错误分类：库/实体不存在是「目标没了」，其余是内部故障。
fn delete_entity_read_error(e: KnowledgeModuleError) -> MemoryDeleteError {
    match &e {
        KnowledgeModuleError::Knowledge(KnowledgeError::NotFound(_))
        | KnowledgeModuleError::NotFound(_) => MemoryDeleteError::NotFound(e.to_string()),
        _ => MemoryDeleteError::Internal(e.to_string()),
    }
}

/// 审计 title 与快照用的人类可读名。
///
/// 实体节点没有名字时不能留空：审计行的 title 会变成 `entity:`，人无从分辨删了谁。
/// 回退到 id 末 8 个字符 —— **按字符切**而非字节，id 里出现非 ASCII 时不能 panic。
fn entity_label(node: &Option<knowledge_base::GraphNode>, entity_id: &str) -> String {
    if let Some(name) = node
        .as_ref()
        .and_then(|n| n.properties.get("name"))
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
    {
        return name.to_string();
    }

    let chars: Vec<char> = entity_id.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(8)..].iter().collect();
    format!("#{tail}")
}

/// 实体删除的审计快照 —— 写进 `MemoryAuditEntry::content`。
///
/// 级联删除要重建的远不止一条边，因此快照必须包含删除前的完整状况：节点属性 +
/// **两个方向**的全部关联边。`edges[]` 里每条边都带 `weight`，回滚按
/// `(source_id 对应的实体名, via 标签, target_id)` 逐条重放。
fn entity_snapshot_json(
    label: &str,
    entity_id: &str,
    node: &Option<knowledge_base::GraphNode>,
    edges: &[knowledge_base::KnowledgeEdge],
) -> serde_json::Value {
    json!({
        "entity_name": label,
        "entity_id": entity_id,
        "node": node.as_ref().map(|n| json!({
            "labels": n.labels,
            "properties": n.properties,
        })),
        "edges": edges
            .iter()
            .map(|e| {
                json!({
                    "source_id": e.source_id,
                    "target_id": e.target_id,
                    "predicate": e.edge_type.as_label(),
                    "weight": e.weight,
                    "properties": e.properties,
                })
            })
            .collect::<Vec<_>>(),
    })
}
