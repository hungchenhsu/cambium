CREATE TABLE tenants (
    id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE identities (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
    name TEXT NOT NULL,
    owner_id TEXT,
    created_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    disabled_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, owner_id) REFERENCES identities(tenant_id, id),
    CHECK ((kind = 'human' AND owner_id IS NULL) OR (kind = 'agent' AND owner_id IS NOT NULL))
);

CREATE TABLE tokens (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    id TEXT NOT NULL,
    identity_id TEXT NOT NULL,
    parent_id TEXT,
    secret_hash BLOB NOT NULL CHECK (length(secret_hash) = 32),
    scope TEXT NOT NULL CHECK (scope IN ('repo:read', 'repo:write', 'repo:admin', 'tenant:admin')),
    repositories_json TEXT,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > created_at_ms),
    revoked_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, secret_hash),
    FOREIGN KEY (tenant_id, identity_id) REFERENCES identities(tenant_id, id),
    FOREIGN KEY (tenant_id, parent_id) REFERENCES tokens(tenant_id, id)
);
CREATE INDEX token_hash_lookup ON tokens(secret_hash);
CREATE INDEX token_parent_lookup ON tokens(tenant_id, parent_id);

CREATE TABLE repositories (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    active_generation INTEGER NOT NULL DEFAULT 1 CHECK (active_generation > 0),
    next_generation INTEGER NOT NULL DEFAULT 2 CHECK (next_generation > active_generation),
    state TEXT NOT NULL CHECK (state IN ('creating', 'ready', 'deleting', 'deleted')),
    fenced INTEGER NOT NULL DEFAULT 0 CHECK (fenced IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX repository_name ON repositories(tenant_id, name) WHERE state != 'deleted';

CREATE TABLE generations (
    tenant_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    number INTEGER NOT NULL CHECK (number > 0),
    backend_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'building', 'draining', 'sealed', 'deleted')),
    created_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, repository_id, number),
    UNIQUE (tenant_id, backend_id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);

CREATE TABLE branches (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    name TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('creating', 'active', 'expiring', 'expired', 'promoted')),
    created_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
CREATE UNIQUE INDEX active_branch_name ON branches(tenant_id, repository_id, name)
    WHERE state IN ('creating', 'active', 'expiring');
CREATE INDEX branch_expiry ON branches(state, expires_at_ms);

CREATE TABLE expiries (
    tenant_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    reclaimed_at_ms INTEGER,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, branch_id),
    FOREIGN KEY (tenant_id, branch_id) REFERENCES branches(tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);

CREATE TABLE compactions (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    old_generation INTEGER NOT NULL,
    new_generation INTEGER NOT NULL,
    stage TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
CREATE UNIQUE INDEX running_compaction ON compactions(tenant_id, repository_id)
    WHERE completed_at_ms IS NULL;

CREATE TABLE idempotency (
    tenant_id TEXT NOT NULL,
    identity_id TEXT NOT NULL,
    key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    state TEXT NOT NULL CHECK (state IN ('running', 'retryable', 'complete')),
    reservation_json TEXT,
    response_status INTEGER,
    response_json TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_by TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, identity_id, key),
    FOREIGN KEY (tenant_id, identity_id) REFERENCES identities(tenant_id, id)
);

CREATE TABLE internal_tokens (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    compaction_id TEXT NOT NULL,
    destination_backend_id TEXT NOT NULL,
    secret_hash BLOB NOT NULL CHECK (length(secret_hash) = 32),
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    created_by_kind TEXT NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, secret_hash),
    FOREIGN KEY (tenant_id, compaction_id) REFERENCES compactions(tenant_id, id)
);
CREATE INDEX internal_token_hash_lookup ON internal_tokens(secret_hash);
