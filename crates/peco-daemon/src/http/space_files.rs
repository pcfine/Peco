//! `/api/spaces/{ws_id}/files` —— 空间工作目录的只读浏览。
//!
//! 只读是这一层的边界：预览要能回答「这个目录里有什么」，写入一律回到磁盘上
//! 的编辑器。因此这里没有 PUT / DELETE，也没有任何 mkdir。
//!
//! 路径校验只有一个判据：**规范化后是否仍落在 `work_dir` 内**。绝对路径、
//! `..` 越界、指向外部的符号链接都在这一条上收敛，不给每类各写一份判断。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::state::AppState;

/// 默认一次读这么多正文；够看一个源文件，又不至于把一张图片读进内存。
const DEFAULT_MAX_BYTES: u64 = 262_144;
/// 上限：再大就该走下载，不该走预览。
const MAX_MAX_BYTES: u64 = 1_048_576;
/// 单层条目上限；超出部分靠 `truncated` 告诉前端「还有」，不在一次响应里全给。
const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Default, Deserialize)]
pub struct BrowseQuery {
    pub path: Option<String>,
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct EntryView {
    pub name: String,
    /// `dir` 或 `file`。符号链接按其目标分类，浏览者看到的就是它指向的东西。
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DirListing {
    /// 被列出目录的相对路径（根目录为空串）。
    pub path: String,
    pub entries: Vec<EntryView>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FileView {
    pub path: String,
    pub kind: &'static str,
    pub size_bytes: u64,
    /// 二进制时缺省 —— 前端据 `binary` 决定给下载链接而不是渲染。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub truncated: bool,
    pub binary: bool,
}

/// 两种响应共用一个返回类型：`path` 指向目录就列条目，指向文件就回正文。
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum FilesResponse {
    Listing(DirListing),
    File(FileView),
}

pub async fn browse(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<FilesResponse>, DaemonError> {
    // 浏览磁盘要有活着的实例：与其它空间作用域接口同一个门槛，避免关闭后
    // 还能读到别人正在写的目录。
    state.spaces.handle(&ws_id).await?;
    let work_dir = state.spaces.work_dir(&ws_id).await?;
    let rel = query.path.unwrap_or_default();
    let target = resolve_within(&work_dir, &rel)?;

    let meta = std::fs::metadata(&target).map_err(|e| DaemonError::InvalidPath {
        path: rel.clone(),
        reason: e.to_string(),
    })?;

    if meta.is_dir() {
        Ok(Json(FilesResponse::Listing(list_dir(&target, &rel)?)))
    } else {
        let max_bytes = query
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .clamp(1, MAX_MAX_BYTES);
        Ok(Json(FilesResponse::File(read_file(
            &target, &rel, &meta, max_bytes,
        )?)))
    }
}

/// 目录条目：目录在前，同类按名称升序。
///
/// 排序在这里做而不是交给前端：分页截断（1000 项）之前必须已经有序，
/// 否则被截掉的可能是排在后面却更该出现的目录。
fn list_dir(dir: &Path, rel: &str) -> Result<DirListing, DaemonError> {
    let read = std::fs::read_dir(dir).map_err(|e| DaemonError::InvalidPath {
        path: rel.to_string(),
        reason: e.to_string(),
    })?;
    let mut entries: Vec<EntryView> = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                // 单个条目读失败不该让整层列不出来（权限位会在遍历时才炸）。
                tracing::warn!(dir = %dir.display(), error = %e, "cannot stat a directory entry");
                continue;
            }
        };
        // `metadata` 跟随符号链接：链接指向目录就按目录处理，与浏览者的直觉一致。
        let meta = match std::fs::metadata(entry.path()) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!(path = %entry.path().display(), error = %e, "cannot stat a directory entry");
                continue;
            }
        };
        let is_dir = meta.is_dir();
        entries.push(EntryView {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: if is_dir { "dir" } else { "file" },
            size_bytes: if is_dir { None } else { Some(meta.len()) },
            modified_at: if is_dir { None } else { mtime_utc(&meta) },
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.cmp(&b.name))
    });
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    Ok(DirListing {
        path: rel.to_string(),
        entries,
        truncated,
    })
}

fn read_file(
    target: &Path,
    rel: &str,
    meta: &std::fs::Metadata,
    max_bytes: u64,
) -> Result<FileView, DaemonError> {
    use std::io::Read as _;

    let io_error = |e: std::io::Error| DaemonError::InvalidPath {
        path: rel.to_string(),
        reason: e.to_string(),
    };
    let mut buf = Vec::new();
    let file = std::fs::File::open(target).map_err(io_error)?;
    // 多读一个字节：只靠 `meta.len()` 判断截断会在文件同时增长时撒谎。
    file.take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(io_error)?;
    let truncated = buf.len() as u64 > max_bytes;
    buf.truncate(max_bytes as usize);

    // 含 NUL 或不是合法 UTF-8 就是二进制：两者都用内容判断，不看扩展名，
    // 因为 `data` 目录里的 `.bin` 与无扩展名的可执行文件都真实存在。
    let binary = buf.contains(&0) || std::str::from_utf8(&buf).is_err();
    Ok(FileView {
        path: rel.to_string(),
        kind: "file",
        size_bytes: meta.len(),
        text: if binary {
            None
        } else {
            Some(String::from_utf8_lossy(&buf).into_owned())
        },
        truncated,
        binary,
    })
}

/// 把请求里的相对路径解析成 `work_dir` 内的真实路径。
///
/// 唯一判据是规范化后的前缀：`canonicalize` 会同时解掉 `..` 与符号链接，
/// 因此越界只有一种表现 —— 结果不在根之下。
fn resolve_within(work_dir: &Path, rel: &str) -> Result<PathBuf, DaemonError> {
    let invalid = |reason: &str| DaemonError::InvalidPath {
        path: rel.to_string(),
        reason: reason.to_string(),
    };
    if rel.contains('\0') {
        return Err(invalid("path contains a NUL byte"));
    }
    if Path::new(rel).is_absolute() {
        return Err(invalid("path must be relative to the space work dir"));
    }
    let root = work_dir
        .canonicalize()
        .map_err(|e| invalid(&format!("work dir unusable: {e}")))?;
    let target = root.join(rel).canonicalize().map_err(|e| {
        // 不存在的路径同样是「这个空间里没有这个东西」，与越界归一类，
        // 免得给前端多一个需要分支的错误码。
        invalid(&e.to_string())
    })?;
    if !target.starts_with(&root) {
        return Err(invalid("path escapes the space work dir"));
    }
    Ok(target)
}

/// 时间串与库里的 `datetime('now')` 同形（UTC，无时区标记）：前端的
/// `parseServerTimestamp` 只认这一种，多一套格式就多一处会静默变 `Invalid Date`。
fn mtime_utc(meta: &std::fs::Metadata) -> Option<String> {
    meta.modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}
