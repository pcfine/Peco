//! 同步引擎类型 — `SyncReport` 和同步辅助逻辑。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SyncReport
// ---------------------------------------------------------------------------

/// 同步操作的结果报告。
///
/// 包含新增、更新、删除、跳过的文件数量统计，以及失败详情。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    /// 知识库名称
    pub kb_name: String,
    /// 新增的文件数
    pub added: usize,
    /// 已更新（内容变更）的文件数
    pub updated: usize,
    /// 已删除（磁盘上消失）的文件数
    pub removed: usize,
    /// 跳过的文件数（哈希未变）
    pub skipped: usize,
    /// 处理失败的文件（相对路径 + 错误原因）
    pub errors: Vec<(String, String)>,
    /// 新增/更新的文件列表
    pub changed_files: Vec<String>,
    /// 同步耗时（毫秒）
    pub duration_ms: u64,
}

impl SyncReport {
    /// 创建指定知识库的空白同步报告。
    pub fn new(kb_name: impl Into<String>) -> Self {
        Self {
            kb_name: kb_name.into(),
            added: 0,
            updated: 0,
            removed: 0,
            skipped: 0,
            errors: Vec::new(),
            changed_files: Vec::new(),
            duration_ms: 0,
        }
    }

    /// 是否有任何变更。
    pub fn has_changes(&self) -> bool {
        self.added > 0 || self.updated > 0 || self.removed > 0
    }

    /// 总变更文件数（新增 + 更新 + 删除）。
    pub fn total_changes(&self) -> usize {
        self.added + self.updated + self.removed
    }

    /// 是否有错误。
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

impl std::fmt::Display for SyncReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "知识库 '{}' 同步完成: 新增 {}, 更新 {}, 删除 {}, 跳过 {} ({}ms)",
            self.kb_name, self.added, self.updated, self.removed, self.skipped, self.duration_ms
        )?;
        if self.has_errors() {
            write!(f, ", {} 个错误", self.errors.len())?;
        }
        Ok(())
    }
}
