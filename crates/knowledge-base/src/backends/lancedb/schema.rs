//! LanceDB 表 Schema 定义。

use arrow_schema::{DataType, Field, Fields, Schema};

/// 构建分块表的 Arrow Schema。
pub fn chunk_table_schema(ndims: usize) -> Schema {
    Schema::new(Fields::from(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("document_id", DataType::Utf8, true),
        Field::new("text", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, true),
        Field::new("source_path", DataType::Utf8, true),
        Field::new("content", DataType::Utf8, true),
        Field::new("parent_id", DataType::Utf8, true),
        Field::new("sequence_index", DataType::UInt32, true),
        Field::new("page_number", DataType::UInt32, true),
        Field::new("file_type", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Float32, true)),
                ndims as i32,
            ),
            true,
        ),
    ]))
}

pub const ID_COL: &str = "id";
pub const TEXT_COL: &str = "text";
pub const DOCUMENT_ID_COL: &str = "document_id";
#[allow(dead_code)]
pub const CONTENT_COL: &str = "content";
#[allow(dead_code)]
pub const PARENT_ID_COL: &str = "parent_id";
