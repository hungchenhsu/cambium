-- Text columns use the "C" collation so ordering matches SQLite's byte order.
CREATE TABLE tenants (
    id TEXT COLLATE "C" PRIMARY KEY NOT NULL,
    slug TEXT COLLATE "C" NOT NULL UNIQUE,
    created_at_ms BIGINT NOT NULL
);

CREATE TABLE identities (
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES tenants(id),
    id TEXT COLLATE "C" NOT NULL,
    kind TEXT COLLATE "C" NOT NULL CHECK (kind IN ('human', 'agent')),
    name TEXT COLLATE "C" NOT NULL,
    owner_id TEXT COLLATE "C",
    created_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    disabled_at_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, owner_id) REFERENCES identities(tenant_id, id),
    CHECK ((kind = 'human' AND owner_id IS NULL) OR (kind = 'agent' AND owner_id IS NOT NULL))
);

CREATE TABLE tokens (
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES tenants(id),
    id TEXT COLLATE "C" NOT NULL,
    identity_id TEXT COLLATE "C" NOT NULL,
    parent_id TEXT COLLATE "C",
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    scope TEXT COLLATE "C" NOT NULL CHECK (scope IN ('repo:read', 'repo:write', 'repo:admin', 'tenant:admin')),
    repositories_json TEXT COLLATE "C",
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > created_at_ms),
    revoked_at_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, secret_hash),
    FOREIGN KEY (tenant_id, identity_id) REFERENCES identities(tenant_id, id),
    FOREIGN KEY (tenant_id, parent_id) REFERENCES tokens(tenant_id, id)
);
CREATE INDEX token_hash_lookup ON tokens(secret_hash);
CREATE INDEX token_parent_lookup ON tokens(tenant_id, parent_id);

CREATE TABLE repositories (
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES tenants(id),
    id TEXT COLLATE "C" NOT NULL,
    name TEXT COLLATE "C" NOT NULL,
    default_branch TEXT COLLATE "C" NOT NULL,
    active_generation BIGINT NOT NULL DEFAULT 1 CHECK (active_generation > 0),
    next_generation BIGINT NOT NULL DEFAULT 2 CHECK (next_generation > active_generation),
    state TEXT COLLATE "C" NOT NULL CHECK (state IN ('creating', 'ready', 'deleting', 'deleted')),
    fenced BOOLEAN NOT NULL DEFAULT FALSE,
    created_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX repository_name ON repositories(tenant_id, name) WHERE state != 'deleted';

CREATE TABLE generations (
    tenant_id TEXT COLLATE "C" NOT NULL,
    repository_id TEXT COLLATE "C" NOT NULL,
    number BIGINT NOT NULL CHECK (number > 0),
    backend_id TEXT COLLATE "C" NOT NULL,
    state TEXT COLLATE "C" NOT NULL CHECK (state IN ('pending', 'active', 'building', 'draining', 'sealed', 'deleted')),
    created_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, repository_id, number),
    UNIQUE (tenant_id, backend_id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);

CREATE TABLE branches (
    -- Insertion order, which the SQLite store reads from rowid.
    seq BIGINT GENERATED ALWAYS AS IDENTITY,
    tenant_id TEXT COLLATE "C" NOT NULL,
    id TEXT COLLATE "C" NOT NULL,
    repository_id TEXT COLLATE "C" NOT NULL,
    name TEXT COLLATE "C" NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    state TEXT COLLATE "C" NOT NULL CHECK (state IN ('creating', 'active', 'expiring', 'expired', 'promoted')),
    created_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
CREATE UNIQUE INDEX active_branch_name ON branches(tenant_id, repository_id, name)
    WHERE state IN ('creating', 'active', 'expiring');
CREATE INDEX branch_expiry ON branches(state, expires_at_ms);

CREATE TABLE expiries (
    tenant_id TEXT COLLATE "C" NOT NULL,
    branch_id TEXT COLLATE "C" NOT NULL,
    repository_id TEXT COLLATE "C" NOT NULL,
    recorded_at_ms BIGINT NOT NULL,
    reclaimed_at_ms BIGINT,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_at_ms BIGINT NOT NULL,
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, branch_id),
    FOREIGN KEY (tenant_id, branch_id) REFERENCES branches(tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);

CREATE TABLE compactions (
    -- Insertion order, which the SQLite store reads from rowid.
    seq BIGINT GENERATED ALWAYS AS IDENTITY,
    tenant_id TEXT COLLATE "C" NOT NULL,
    id TEXT COLLATE "C" NOT NULL,
    repository_id TEXT COLLATE "C" NOT NULL,
    old_generation BIGINT NOT NULL,
    new_generation BIGINT NOT NULL,
    stage TEXT COLLATE "C" NOT NULL,
    state_json TEXT COLLATE "C" NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    completed_at_ms BIGINT,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, repository_id) REFERENCES repositories(tenant_id, id)
);
CREATE UNIQUE INDEX running_compaction ON compactions(tenant_id, repository_id)
    WHERE completed_at_ms IS NULL;

CREATE TABLE idempotency (
    tenant_id TEXT COLLATE "C" NOT NULL,
    identity_id TEXT COLLATE "C" NOT NULL,
    key TEXT COLLATE "C" NOT NULL,
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    state TEXT COLLATE "C" NOT NULL CHECK (state IN ('running', 'retryable', 'complete')),
    reservation_json TEXT COLLATE "C",
    response_status BIGINT,
    response_json TEXT COLLATE "C",
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    updated_by TEXT COLLATE "C" NOT NULL,
    updated_by_kind TEXT COLLATE "C" NOT NULL CHECK (updated_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, identity_id, key),
    FOREIGN KEY (tenant_id, identity_id) REFERENCES identities(tenant_id, id)
);

CREATE TABLE internal_tokens (
    tenant_id TEXT COLLATE "C" NOT NULL,
    id TEXT COLLATE "C" NOT NULL,
    compaction_id TEXT COLLATE "C" NOT NULL,
    destination_backend_id TEXT COLLATE "C" NOT NULL,
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    created_by TEXT COLLATE "C" NOT NULL,
    created_by_kind TEXT COLLATE "C" NOT NULL CHECK (created_by_kind IN ('human', 'agent', 'system')),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, secret_hash),
    FOREIGN KEY (tenant_id, compaction_id) REFERENCES compactions(tenant_id, id)
);
CREATE INDEX internal_token_hash_lookup ON internal_tokens(secret_hash);
