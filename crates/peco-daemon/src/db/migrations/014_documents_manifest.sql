-- Migration 014: documents 表作为摄入路径统一的 REST 面注册表（manifest 投影）
-- 自然键是相对 docs/ 的路径（scan 支持递归，子目录同名文件只有相对路径能区分）；
-- doc_id 是「注册表 ↔ KB 后端」的 join key。
-- 存量 upload 行都平铺在 docs/ 根目录，自然键取 filename：必须先回填再建唯一索引，
-- 否则它们 relative_path 为 NULL 匹配不到，首次 reconcile 会重复插行（计数翻倍）。
-- 回填后按自然键去重（保留每组最新一行），避免建索引时撞键。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。

ALTER TABLE documents ADD COLUMN relative_path TEXT;
ALTER TABLE documents ADD COLUMN doc_id TEXT;
UPDATE documents SET relative_path = filename WHERE relative_path IS NULL;
DELETE FROM documents WHERE relative_path IS NOT NULL
  AND rowid NOT IN (SELECT MAX(rowid) FROM documents GROUP BY kb_id, relative_path);
CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_kb_relative ON documents(kb_id, relative_path);
