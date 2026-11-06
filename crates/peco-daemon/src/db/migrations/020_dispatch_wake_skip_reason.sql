-- 020: d_dispatches 增加 wake_skip_reason
-- 历史 skipped 行的成因无法可靠反推，保留 NULL，由应用层显示为原因未记录
ALTER TABLE d_dispatches ADD COLUMN wake_skip_reason TEXT
  CHECK (wake_skip_reason IS NULL OR wake_skip_reason IN (
    'not_peco_origin', 'peco_identity_missing', 'orphaned',
    'space_closed', 'attempts_exhausted'
  ));
