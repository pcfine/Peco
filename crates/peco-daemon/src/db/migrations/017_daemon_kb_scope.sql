-- memory_audit 的两套语义分列：user_id 存单用户身份，kb_scope 存知识库隔离维度。
-- 该表由迁移 009 建立，所以补列只能作为更晚的一步执行。
ALTER TABLE memory_audit ADD COLUMN kb_scope TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_memory_audit_scope ON memory_audit(kb_scope, deleted_at DESC);
