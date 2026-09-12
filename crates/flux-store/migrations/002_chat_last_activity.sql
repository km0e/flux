-- 002: last_activity_at — the chat's most recent message-append time.
-- The sidebar sorts and labels by ACTIVITY, not creation; a nullable
-- column + backfill from created_at keeps existing rows intact (SQLite
-- ALTER TABLE ADD COLUMN only accepts constant defaults, so the backfill
-- is a separate UPDATE).
ALTER TABLE chats ADD COLUMN last_activity_at TEXT;
UPDATE chats SET last_activity_at = created_at WHERE last_activity_at IS NULL;
