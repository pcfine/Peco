//! HelixDB Schema 初始化。
//!
//! 根据 `HelixSchema` 配置幂等创建向量索引和全文索引。
//! 使用 `create_index_if_not_exists` 语义 — 重复调用是安全的。

use serde_json::json;
use tracing::info;

use crate::error::KnowledgeError;

use super::client::HelixDbClient;
use super::types::{HelixSchema, IndexType};

/// 根据 `HelixSchema` 初始化索引（幂等）。
///
/// 创建：
/// - 片段节点的向量索引（ANN）
/// - 片段节点的全文索引（BM25）
/// - 内容节点的向量索引（ANN）
/// - 所有 `extra_indexes` 中的额外索引
///
/// 节点标签和边标签在首次写入时自动推断（HelixDB 特性），
/// 无需显式 DDL。
pub async fn init_schema(
    client: &HelixDbClient,
    schema: &HelixSchema,
) -> Result<(), KnowledgeError> {
    info!(
        content_label = %schema.content_node_label,
        fragment_label = %schema.fragment_node_label,
        "初始化 HelixDB schema"
    );

    // 片段节点向量索引
    create_vector_index(
        client,
        &schema.fragment_node_label,
        &schema.fragment_vector_property,
    )
    .await?;

    // 片段节点全文索引
    create_text_index(
        client,
        &schema.fragment_node_label,
        &schema.fragment_text_property,
    )
    .await?;

    // 内容节点向量索引
    create_vector_index(
        client,
        &schema.content_node_label,
        &schema.content_vector_property,
    )
    .await?;

    // 内容节点全文索引
    create_text_index(
        client,
        &schema.content_node_label,
        &schema.content_text_property,
    )
    .await?;

    // 内容节点 id 相等索引（用于按 id 属性查找文档）
    if schema.id_property != "$id" {
        create_equality_index(client, &schema.content_node_label, &schema.id_property).await?;
        create_equality_index(client, &schema.fragment_node_label, &schema.id_property).await?;
    }

    // 额外索引
    for idx in &schema.extra_indexes {
        create_index_spec(client, idx).await?;
    }

    info!("HelixDB schema 初始化完成");
    Ok(())
}

/// 创建向量索引（幂等）。
async fn create_vector_index(
    client: &HelixDbClient,
    node_label: &str,
    property: &str,
) -> Result<(), KnowledgeError> {
    info!(%node_label, %property, "创建向量索引");
    let query = json!({
        "request_type": "write",
        "query": {
            "queries": [{
                "Query": {
                    "name": "idx",
                    "steps": [{
                        "CreateIndex": {
                            "spec": {
                                "NodeVector": {
                                    "label": node_label,
                                    "property": property
                                }
                            },
                            "if_not_exists": true
                        }
                    }],
                    "condition": null
                }
            }],
            "returns": []
        }
    });
    client.execute_write(query).await?;
    Ok(())
}

/// 创建全文索引（幂等）。
async fn create_text_index(
    client: &HelixDbClient,
    node_label: &str,
    property: &str,
) -> Result<(), KnowledgeError> {
    info!(%node_label, %property, "创建全文索引");
    let query = json!({
        "request_type": "write",
        "query": {
            "queries": [{
                "Query": {
                    "name": "idx",
                    "steps": [{
                        "CreateIndex": {
                            "spec": {
                                "NodeText": {
                                    "label": node_label,
                                    "property": property
                                }
                            },
                            "if_not_exists": true
                        }
                    }],
                    "condition": null
                }
            }],
            "returns": []
        }
    });
    client.execute_write(query).await?;
    Ok(())
}

/// 创建相等索引（幂等）。
async fn create_equality_index(
    client: &HelixDbClient,
    node_label: &str,
    property: &str,
) -> Result<(), KnowledgeError> {
    info!(%node_label, %property, "创建相等索引");
    let query = json!({
        "request_type": "write",
        "query": {
            "queries": [{
                "Query": {
                    "name": "idx",
                    "steps": [{
                        "CreateIndex": {
                            "spec": {
                                "NodeEquality": {
                                    "label": node_label,
                                    "property": property
                                }
                            },
                            "if_not_exists": true
                        }
                    }],
                    "condition": null
                }
            }],
            "returns": []
        }
    });
    client.execute_write(query).await?;
    Ok(())
}

/// 创建自定义额外索引（幂等）。
async fn create_index_spec(
    client: &HelixDbClient,
    idx: &super::types::HelixIndexSpec,
) -> Result<(), KnowledgeError> {
    info!(
        index_type = ?idx.index_type,
        label = %idx.node_label,
        property = %idx.property,
        "创建额外索引"
    );

    let spec = match idx.index_type {
        IndexType::Equality => json!({
            "NodeEquality": {"label": idx.node_label, "property": idx.property}
        }),
        IndexType::UniqueEquality => json!({
            "NodeEquality": {"label": idx.node_label, "property": idx.property, "unique": true}
        }),
        IndexType::Range => json!({
            "NodeRange": {"label": idx.node_label, "property": idx.property}
        }),
        IndexType::RangeDesc => json!({
            "NodeRangeDesc": {"label": idx.node_label, "property": idx.property}
        }),
        IndexType::Vector => json!({
            "NodeVector": {"label": idx.node_label, "property": idx.property}
        }),
        IndexType::Text => json!({
            "NodeText": {"label": idx.node_label, "property": idx.property}
        }),
    };

    let query = json!({
        "request_type": "write",
        "query": {
            "queries": [{
                "Query": {
                    "name": "extra_idx",
                    "steps": [{
                        "CreateIndex": {
                            "spec": spec,
                            "if_not_exists": true
                        }
                    }],
                    "condition": null
                }
            }],
            "returns": []
        }
    });

    client.execute_write(query).await?;
    Ok(())
}
