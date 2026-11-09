CREATE TABLE commit_provenance (
    tenant_id TEXT COLLATE "C" NOT NULL,
    repository_id TEXT COLLATE "C" NOT NULL,
    commit_id TEXT COLLATE "C" NOT NULL CHECK (length(commit_id) = 40),
    model TEXT COLLATE "C" CHECK (model IS NULL OR length(model) BETWEEN 1 AND 255),
    session_id TEXT COLLATE "C" CHECK (session_id IS NULL OR length(session_id) BETWEEN 1 AND 255),
    created_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, repository_id, commit_id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
