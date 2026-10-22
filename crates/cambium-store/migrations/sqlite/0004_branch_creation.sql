-- Preserve the exact backend request across a lost reply or process restart.
ALTER TABLE branches ADD COLUMN creation_backend_id TEXT;
ALTER TABLE branches ADD COLUMN start_commit TEXT;
