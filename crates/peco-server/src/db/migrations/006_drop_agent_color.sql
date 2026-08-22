-- Migration 006: Drop the unused `color` column from agents table.
-- The `color` (theme/accent) field was never rendered by the frontend —
-- only `background_color` is used for the emoji tile background.
-- Requires SQLite >= 3.35 (ALTER TABLE ... DROP COLUMN).

ALTER TABLE agents DROP COLUMN color;
