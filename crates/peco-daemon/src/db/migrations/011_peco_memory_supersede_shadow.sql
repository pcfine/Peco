-- ============================================================================
-- Migration 011: 记忆取代机制 · shadow 观测表（阶段一）
-- ============================================================================
-- 只观测不动作：记录每轮「候选快照 + 提取事实 + 取代决策」，
-- 供效果门（design-v4 §10）标定。不参与任何检索/展示路径。

CREATE TABLE IF NOT EXISTS memory_supersede_shadow (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id             TEXT NOT NULL,
    created_at          TEXT NOT NULL,          -- ISO 8601
    candidates_json     TEXT NOT NULL,          -- [{id,source,text,channel}]
    facts_json          TEXT NOT NULL,          -- [{category,content,topic,supersedes}]
    decisions_json      TEXT NOT NULL,          -- {per_turn_cap,would_act,items:[...]}
    acted               INTEGER NOT NULL DEFAULT 0,
    extracted_topic_cnt INTEGER NOT NULL DEFAULT 0,
    episodic_cnt        INTEGER NOT NULL DEFAULT 0,
    oracle_hit          INTEGER,                -- 离线/人工回填
    manual_label        TEXT                    -- 'state_update'|'not'|NULL
);

CREATE INDEX IF NOT EXISTS idx_mss_user_created
    ON memory_supersede_shadow(user_id, created_at DESC);
