-- 023: teams 增加 icon
-- 团队图标是 UI 偏好的索引字段，不是能力根的一部分：只写 DB，不写 team.json。
-- 旧行补空串，前端按 team_id 生成像素图标。注释内不要出现半角分号，迁移按分号拆分。
ALTER TABLE teams ADD COLUMN icon TEXT NOT NULL DEFAULT '';
