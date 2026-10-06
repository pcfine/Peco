-- ============================================================================
-- Migration 013: 上下文压缩边界观测表（shadow）
-- ============================================================================
-- 每个 turn 边界一行，记录该边界的 token 口径与削减评估值。
-- 与 peco_compaction_log 分离：折叠日志只记折叠，观测表记全部边界 ——
-- 混进折叠日志会让「无折叠的边界」无处安放，观测即失真。
-- 只追加、不更新；过期行由启动清扫按保留期删除。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。

CREATE TABLE IF NOT EXISTS peco_context_shadow (
    id                       TEXT PRIMARY KEY,          -- UUID v4
    user_id                  TEXT NOT NULL,
    conversation_id          TEXT NOT NULL,
    turn_index               INTEGER NOT NULL,
    created_at               TEXT NOT NULL DEFAULT (datetime('now')),  -- ISO 8601
    trigger_caliber          TEXT NOT NULL,             -- 'full' | 'full+guard'（本边界折叠由谁触发；无折叠记实际评估口径）
    pinned_tokens            INTEGER NOT NULL,          -- P
    committed_full_tokens    INTEGER NOT NULL,          -- F（全量口径，触发对象）
    committed_view_tokens    INTEGER NOT NULL,          -- V（全部 committed view，非窗口口径）
    view_window_tokens       INTEGER NOT NULL,          -- filter 连续窗口口径（= estimated_view_tokens）
    fold_triggered           INTEGER NOT NULL,          -- 0/1（= BoundaryReduction.folded()）
    evicted_turns            INTEGER,                   -- 折叠驱逐轮数；无折叠为 NULL
    guard_triggered          INTEGER NOT NULL,          -- 0/1
    strip_evaluated          INTEGER NOT NULL,          -- 是否产生削减评估值
    strip_enforced           INTEGER NOT NULL,          -- 生效值是否实际改写（shadow 恒 0）
    stripped_items           INTEGER NOT NULL,          -- 评估值：若执行将削减的条数（enforce 时 == 生效条数）
    released_tokens          INTEGER NOT NULL,          -- 评估值：若执行将释放的估算 token
    tool_output_items        INTEGER NOT NULL,          -- 本轮 committed 的 FunctionCallOutput 条数
    tool_output_over_cap     INTEGER NOT NULL,          -- 输出 > tool_output_max_chars 的条数
    image_output_items       INTEGER NOT NULL,          -- output 为 Parts 且含 Image 的条数
    image_base64_bytes       INTEGER NOT NULL,          -- 图片 data URI 字节
    snapshot_json_bytes      INTEGER NOT NULL           -- 落盘快照字节（含图 ⇒ >> token 界）
);
CREATE INDEX IF NOT EXISTS idx_pcs_user_created ON peco_context_shadow(user_id, created_at);
