-- 025: agents 不再存 background_color。
-- 卡片背景色改为按 id 确定性生成，不再由用户配置。
-- 004 只在 018 之前的旧库上补该列，门控已同步收窄，不会把列加回来。
ALTER TABLE agents DROP COLUMN background_color;
