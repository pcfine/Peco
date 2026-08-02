-- Migration 004: Add background_color to agents table
-- This column stores the card background color for Agent cards in the UI.
-- Empty string means no custom background (use default).

ALTER TABLE agents ADD COLUMN background_color TEXT NOT NULL DEFAULT '';
