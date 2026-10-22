ALTER TABLE repositories ADD COLUMN orphan_sweep_after_ms INTEGER NOT NULL DEFAULT 0;
CREATE INDEX compaction_history ON compactions(tenant_id, repository_id, completed_at_ms);
