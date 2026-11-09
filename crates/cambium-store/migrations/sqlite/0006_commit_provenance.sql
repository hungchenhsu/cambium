CREATE TABLE commit_provenance (
    tenant_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    commit_id TEXT NOT NULL CHECK (length(commit_id) = 40),
    model TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 255),
    session_id TEXT CHECK (session_id IS NULL OR length(session_id) BETWEEN 1 AND 255),
    created_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, repository_id, commit_id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
