-- 024: agents 不再存 icon。
-- 列表卡片改用「显示名首字 + 由 id 派生的色相」，不再需要图标列。
-- 注释内不要出现半角分号，迁移执行器按分号拆分语句。
ALTER TABLE agents DROP COLUMN icon;
