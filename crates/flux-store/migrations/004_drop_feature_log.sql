-- Feature mode moved to the feature-mode branch; the table has no
-- consumer on main. Dropping keeps the schema honest (chat deletion
-- already cascaded; the rows are feature-orchestration scaffolding).
DROP INDEX IF EXISTS idx_feature_log_chat;
DROP TABLE IF EXISTS feature_log;
