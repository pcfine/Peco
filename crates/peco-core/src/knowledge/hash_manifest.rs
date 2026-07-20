//! 文件哈希清单 — 记录知识库 docs/ 目录下所有文件的 SHA-256 哈希。
//!
//! 持久化为每个知识库目录下的 `file_hashes.json`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::KnowledgeModuleError;

// ---------------------------------------------------------------------------
// FileHashManifest
// ---------------------------------------------------------------------------

/// 文件哈希清单 — 记录知识库 `docs/` 目录下所有文件的 SHA-256 哈希。
///
/// 持久化为每个知识库目录下的 `file_hashes.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileHashManifest {
    /// 相对于 `docs/` 目录的文件路径 → 文件条目
    pub files: HashMap<String, FileEntry>,
    /// 清单最后更新时间（ISO 8601）
    pub updated_at: String,
}

/// 单个文件的哈希记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// SHA-256 哈希值（hex 编码）
    pub hash: String,
    /// 文件大小（字节）
    pub size: u64,
    /// 数据库中对应的 document_id（用于更新时删除旧数据）
    pub doc_id: String,
    /// 首次摄入时间（ISO 8601）
    pub ingested_at: String,
}

impl FileHashManifest {
    /// 从知识库目录加载哈希清单。
    ///
    /// `kb_dir` 是知识库根目录（`{base_dir}/{kb_name}/`），
    /// 清单文件位于 `{kb_dir}/file_hashes.json`。
    pub async fn load(kb_dir: &Path) -> Result<Self, KnowledgeModuleError> {
        let manifest_path = kb_dir.join("file_hashes.json");
        if manifest_path.exists() {
            let data = tokio::fs::read_to_string(&manifest_path)
                .await
                .map_err(KnowledgeModuleError::Io)?;
            let manifest: FileHashManifest =
                serde_json::from_str(&data).map_err(KnowledgeModuleError::Json)?;
            Ok(manifest)
        } else {
            Ok(Self::default())
        }
    }

    /// 保存哈希清单到知识库目录。
    pub async fn save(&self, kb_dir: &Path) -> Result<(), KnowledgeModuleError> {
        let manifest_path = kb_dir.join("file_hashes.json");
        let json = serde_json::to_string_pretty(self).map_err(KnowledgeModuleError::Json)?;
        tokio::fs::write(&manifest_path, json)
            .await
            .map_err(KnowledgeModuleError::Io)?;
        Ok(())
    }

    /// 根据相对路径查找文件条目。
    pub fn get(&self, relative_path: &str) -> Option<&FileEntry> {
        self.files.get(relative_path)
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 计算文件的 SHA-256 哈希和大小（流式读取，支持大文件）。
pub async fn compute_file_hash(path: &Path) -> Result<(String, u64), KnowledgeModuleError> {
    use sha2::{Digest, Sha256};

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(KnowledgeModuleError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    let mut total_size = 0u64;

    loop {
        let n = tokio::io::AsyncReadExt::read(&mut file, &mut buffer)
            .await
            .map_err(KnowledgeModuleError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total_size += n as u64;
    }

    Ok((hex::encode(hasher.finalize()), total_size))
}

/// 受支持的文件扩展名列表。
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "pdf", "md", "markdown", "html", "htm", "txt", "rs", "py", "js", "ts", "go", "java", "c",
    "cpp", "h", "hpp", "toml", "yaml", "yml", "json", "xml", "csv", "sql", "r", "rb", "swift",
    "kt", "scala", "sh", "bash", "zsh", "fish", "css", "scss", "vue", "svelte",
];

/// 递归扫描目录，收集所有受支持的文件。
///
/// 跳过隐藏文件（`.` 开头）和二进制格式。
/// 返回相对于 `dir` 的路径。
pub async fn scan_supported_files(
    dir: &Path,
    recursive: bool,
) -> Result<Vec<PathBuf>, KnowledgeModuleError> {
    let mut files = Vec::new();

    if !dir.exists() {
        return Ok(files);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(KnowledgeModuleError::Io)?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(KnowledgeModuleError::Io)?
    {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // 跳过隐藏文件
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() && recursive {
            let sub_files = Box::pin(scan_supported_files(&path, recursive)).await?;
            files.extend(sub_files);
        } else if path.is_file()
            && let Some(ext) = path.extension()
        {
            let ext_str = ext.to_string_lossy().to_lowercase();
            if SUPPORTED_EXTENSIONS.contains(&ext_str.as_str()) {
                files.push(path);
            }
        }
    }

    Ok(files)
}

/// 生成当前时间的 ISO 8601 字符串。
pub fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_manifest_loads_default() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = FileHashManifest::load(tmp.path()).await.unwrap();
        assert!(manifest.files.is_empty());
    }

    #[tokio::test]
    async fn manifest_save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut manifest = FileHashManifest::default();
        manifest.files.insert(
            "test.md".into(),
            FileEntry {
                hash: "abc123".into(),
                size: 100,
                doc_id: "abc123def".into(),
                ingested_at: "2026-07-16T00:00:00+00:00".into(),
            },
        );
        manifest.updated_at = "2026-07-16T00:00:00+00:00".into();

        manifest.save(tmp.path()).await.unwrap();
        let loaded = FileHashManifest::load(tmp.path()).await.unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files.get("test.md").unwrap().hash, "abc123");
    }

    #[tokio::test]
    async fn compute_hash_of_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("hello.txt");
        tokio::fs::write(&file_path, b"hello world").await.unwrap();

        let (hash, size) = compute_file_hash(&file_path).await.unwrap();
        assert_eq!(size, 11);
        assert_eq!(hash.len(), 64); // SHA-256 hex
    }

    #[tokio::test]
    async fn scan_supported_files_filters_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("doc.md"), b"# Hello")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("data.txt"), b"text")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("image.png"), b"fake png")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join(".hidden.md"), b"hidden")
            .await
            .unwrap();

        let files = scan_supported_files(tmp.path(), false).await.unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert!(names.contains(&"doc.md"));
        assert!(names.contains(&"data.txt"));
        assert!(!names.contains(&"image.png"));
        assert!(!names.contains(&".hidden.md"));
    }

    #[tokio::test]
    async fn scan_recursive_finds_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("subdir");
        tokio::fs::create_dir(&sub).await.unwrap();
        tokio::fs::write(sub.join("nested.md"), b"# Nested")
            .await
            .unwrap();

        let files = scan_supported_files(tmp.path(), true).await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("subdir/nested.md"));
    }
}
