use std::{collections::HashSet, str::FromStr};

use async_trait::async_trait;
use cambium_protocol::{
    Actor, ApiError, ErrorCode, IdentityId, IdentityKind, Principal, RepositoryId, Scope, TenantId,
    TokenId,
};
use sqlx::{
    Connection as _, Executor as _, PgConnection, PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions, PgRow},
};

use crate::{
    Bootstrap, CreateIdentity, IdentityRecord, MintToken, RepositoryRecord, Store, StoreError,
    StoreResult, TokenHash, TokenSettings,
};

mod compaction;
mod deployment;
mod lifecycle;
mod operations;

/// Session advisory lock key held by the deployment's single writer.
const WRITER_LOCK: i64 = 0x6361_6d62_6975_6d00;

pub struct PostgresStore {
    writer: PgPool,
    readers: PgPool,
    settings: TokenSettings,
}

fn request(code: ErrorCode) -> StoreError {
    ApiError::new(code).into()
}
fn invalid(field: &str, reason: &str) -> StoreError {
    ApiError::invalid(field, reason).into()
}
fn parse<T: FromStr>(value: String) -> StoreResult<T> {
    value.parse().map_err(|_| StoreError::Corrupt)
}
fn kind_name(kind: IdentityKind) -> &'static str {
    match kind {
        IdentityKind::Human => "human",
        IdentityKind::Agent => "agent",
        IdentityKind::System => "system",
    }
}
fn kind(value: String) -> StoreResult<IdentityKind> {
    match value.as_str() {
        "human" => Ok(IdentityKind::Human),
        "agent" => Ok(IdentityKind::Agent),
        "system" => Ok(IdentityKind::System),
        _ => Err(StoreError::Corrupt),
    }
}
fn scope(value: String) -> StoreResult<Scope> {
    match value.as_str() {
        "repo:read" => Ok(Scope::RepoRead),
        "repo:write" => Ok(Scope::RepoWrite),
        "repo:admin" => Ok(Scope::RepoAdmin),
        "tenant:admin" => Ok(Scope::TenantAdmin),
        _ => Err(StoreError::Corrupt),
    }
}
fn actor(row: &PgRow) -> StoreResult<Actor> {
    Ok(Actor {
        identity_id: parse(row.try_get("created_by")?)?,
        kind: kind(row.try_get("created_by_kind")?)?,
    })
}
fn identity(row: &PgRow) -> StoreResult<IdentityRecord> {
    Ok(IdentityRecord {
        tenant_id: parse(row.try_get("tenant_id")?)?,
        id: parse(row.try_get("id")?)?,
        kind: kind(row.try_get("kind")?)?,
        name: row.try_get("name")?,
        owner_id: row
            .try_get::<Option<String>, _>("owner_id")?
            .map(parse)
            .transpose()?,
        created_at_ms: row.try_get("created_at_ms")?,
        created_by: actor(row)?,
        disabled_at_ms: row.try_get("disabled_at_ms")?,
    })
}
fn repository(row: &PgRow) -> StoreResult<RepositoryRecord> {
    Ok(RepositoryRecord {
        tenant_id: parse(row.try_get("tenant_id")?)?,
        id: parse(row.try_get("id")?)?,
        name: row.try_get("name")?,
        default_branch: row.try_get("default_branch")?,
        active_generation: row.try_get("active_generation")?,
        next_generation: row.try_get("next_generation")?,
        state: row.try_get("state")?,
        fenced: row.try_get("fenced")?,
        created_at_ms: row.try_get("created_at_ms")?,
        created_by: actor(row)?,
    })
}
fn map_unique(error: sqlx::Error) -> StoreError {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        request(ErrorCode::AlreadyExists)
    } else {
        error.into()
    }
}

impl PostgresStore {
    /// Opens the store as the deployment's only writer (ADR-0020).
    ///
    /// The write connection holds a session advisory lock. A replacement
    /// connection must take it again; until it can, writes fail after the acquire
    /// timeout. So this process never writes while another one holds the lock.
    pub async fn open(url: &str, settings: TokenSettings) -> StoreResult<Self> {
        if settings.human_max_lifetime_ms <= 0
            || settings.agent_max_lifetime_ms <= 0
            || settings.max_delegation_depth == 0
            || settings.max_delegation_depth > 256
        {
            return Err(invalid(
                "token_settings",
                "lifetimes must be positive and delegation depth must be between 1 and 256",
            ));
        }
        let options: PgConnectOptions = url
            .parse()
            .map_err(|_| invalid("database_url", "Expected a postgres:// URL"))?;
        // Fail fast when another server holds the lock; the pool below enforces it again.
        let mut probe = PgConnection::connect_with(&options).await?;
        let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(WRITER_LOCK)
            .fetch_one(&mut probe)
            .await?;
        probe.close().await?;
        if !free {
            return Err(invalid(
                "database_url",
                "Another Cambium server owns this database",
            ));
        }
        let writer = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                        .bind(WRITER_LOCK)
                        .fetch_one(&mut *connection)
                        .await?;
                    if !locked {
                        // sqlx retries a failed connection until acquire_timeout,
                        // then the operation fails; it never writes without the lock.
                        return Err(sqlx::Error::Protocol(
                            "another Cambium server owns this database".into(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect_with(options.clone())
            .await?;
        sqlx::migrate!("./migrations/postgres").run(&writer).await?;
        let readers = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    connection
                        .execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        Ok(Self {
            writer,
            readers,
            settings,
        })
    }

    pub async fn close(&self) {
        self.readers.close().await;
        self.writer.close().await;
    }

    async fn identity_at(
        connection: &mut PgConnection,
        tenant: &TenantId,
        id: &IdentityId,
    ) -> StoreResult<IdentityRecord> {
        let row = sqlx::query("SELECT * FROM identities WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_optional(connection)
            .await?
            .ok_or_else(|| request(ErrorCode::NotFound))?;
        identity(&row)
    }

    /// Revalidate the entire chain inside the transaction that performs a mutation.
    async fn resolve(
        &self,
        connection: &mut PgConnection,
        tenant: &TenantId,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<(Principal, usize)> {
        let mut current = token.clone();
        let mut seen = HashSet::new();
        let mut principal = None;
        loop {
            if !seen.insert(current.clone()) || seen.len() > self.settings.max_delegation_depth {
                return Err(request(ErrorCode::Unauthenticated));
            }
            let row = sqlx::query("SELECT t.*, i.kind AS identity_kind, i.disabled_at_ms FROM tokens t JOIN identities i ON i.tenant_id = t.tenant_id AND i.id = t.identity_id WHERE t.tenant_id = $1 AND t.id = $2")
                .bind(tenant.as_str()).bind(current.as_str()).fetch_optional(&mut *connection).await?.ok_or_else(|| request(ErrorCode::Unauthenticated))?;
            let expires_at_ms: i64 = row.try_get("expires_at_ms")?;
            if row.try_get::<Option<i64>, _>("revoked_at_ms")?.is_some()
                || row.try_get::<Option<i64>, _>("disabled_at_ms")?.is_some()
                || expires_at_ms <= now_ms
            {
                return Err(request(ErrorCode::Unauthenticated));
            }
            if principal.is_none() {
                let repositories: Option<String> = row.try_get("repositories_json")?;
                principal = Some(Principal {
                    tenant_id: tenant.clone(),
                    token_id: token.clone(),
                    actor: Actor {
                        identity_id: parse(row.try_get("identity_id")?)?,
                        kind: kind(row.try_get("identity_kind")?)?,
                    },
                    scope: scope(row.try_get("scope")?)?,
                    repositories: repositories
                        .map(|v| serde_json::from_str(&v).map_err(|_| StoreError::Corrupt))
                        .transpose()?,
                    expires_at_ms,
                });
            }
            match row.try_get::<Option<String>, _>("parent_id")? {
                Some(parent) => current = parse(parent)?,
                None => return Ok((principal.ok_or(StoreError::Corrupt)?, seen.len())),
            }
        }
    }

    fn check_expiry(&self, kind: IdentityKind, expiry: i64, now_ms: i64) -> StoreResult<()> {
        let maximum = match kind {
            IdentityKind::Human => self.settings.human_max_lifetime_ms,
            IdentityKind::Agent => self.settings.agent_max_lifetime_ms,
            IdentityKind::System => return Err(request(ErrorCode::PermissionDenied)),
        };
        if expiry
            .checked_sub(now_ms)
            .is_none_or(|duration| duration <= 0 || duration > maximum)
        {
            return Err(invalid(
                "expires_at",
                "expiry exceeds the identity lifetime or is not in the future",
            ));
        }
        Ok(())
    }

    async fn check_mint_at(
        &self,
        connection: &mut PgConnection,
        principal: &Principal,
        input: &MintToken,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let (principal, depth) = self
            .resolve(
                &mut *connection,
                &principal.tenant_id,
                &principal.token_id,
                now_ms,
            )
            .await?;
        let target =
            Self::identity_at(&mut *connection, &principal.tenant_id, &input.identity_id).await?;
        if target.id != principal.actor.identity_id
            && (target.kind != IdentityKind::Agent || principal.scope != Scope::TenantAdmin)
        {
            return Err(request(ErrorCode::PermissionDenied));
        }
        if !principal.scope.includes(input.scope) || input.expires_at_ms > principal.expires_at_ms {
            return Err(request(ErrorCode::PermissionDenied));
        }
        if let Some(parent_repositories) = &principal.repositories
            && input
                .repositories
                .as_ref()
                .is_none_or(|child| child.iter().any(|id| !parent_repositories.contains(id)))
        {
            return Err(request(ErrorCode::PermissionDenied));
        }
        if depth >= self.settings.max_delegation_depth {
            return Err(invalid("parent_token", "delegation depth limit reached"));
        }
        self.check_expiry(target.kind, input.expires_at_ms, now_ms)?;
        if target.disabled_at_ms.is_some() {
            return Err(request(ErrorCode::PermissionDenied));
        }
        if let Some(repositories) = &input.repositories {
            for repository in repositories {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM repositories WHERE tenant_id = $1 AND id = $2 AND state != 'deleted')")
                    .bind(principal.tenant_id.as_str()).bind(repository.as_str()).fetch_one(&mut *connection).await?;
                if !exists {
                    return Err(request(ErrorCode::NotFound));
                }
            }
        }
        Ok(principal)
    }

    async fn insert_identity(
        connection: &mut PgConnection,
        tenant: &TenantId,
        id: &IdentityId,
        input: &CreateIdentity,
        actor: &Actor,
        now_ms: i64,
    ) -> StoreResult<()> {
        sqlx::query("INSERT INTO identities (tenant_id, id, kind, name, owner_id, created_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
            .bind(tenant.as_str()).bind(id.as_str()).bind(kind_name(input.kind)).bind(&input.name).bind(input.owner_id.as_ref().map(IdentityId::as_str)).bind(now_ms).bind(actor.identity_id.as_str()).bind(kind_name(actor.kind)).bind(now_ms).bind(actor.identity_id.as_str()).bind(kind_name(actor.kind)).execute(connection).await?;
        Ok(())
    }

    async fn insert_token(
        &self,
        connection: &mut PgConnection,
        tenant: &TenantId,
        input: &MintToken,
        parent: Option<&TokenId>,
        actor: &Actor,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let identity = Self::identity_at(connection, tenant, &input.identity_id).await?;
        if identity.disabled_at_ms.is_some() {
            return Err(request(ErrorCode::PermissionDenied));
        }
        self.check_expiry(identity.kind, input.expires_at_ms, now_ms)?;
        if let Some(repositories) = &input.repositories {
            for repository in repositories {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM repositories WHERE tenant_id = $1 AND id = $2 AND state != 'deleted')").bind(tenant.as_str()).bind(repository.as_str()).fetch_one(&mut *connection).await?;
                if !exists {
                    return Err(request(ErrorCode::NotFound));
                }
            }
        }
        // The deployment-wide hash lookup must never resolve an ambiguous token.
        let collision: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tokens WHERE secret_hash = $1)")
                .bind(input.token_hash.0.as_slice())
                .fetch_one(&mut *connection)
                .await?;
        if collision {
            return Err(request(ErrorCode::Internal));
        }
        let token = TokenId::new();
        let repositories = input
            .repositories
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        sqlx::query("INSERT INTO tokens (tenant_id, id, identity_id, parent_id, secret_hash, scope, repositories_json, created_at_ms, expires_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)")
            .bind(tenant.as_str()).bind(token.as_str()).bind(input.identity_id.as_str()).bind(parent.map(TokenId::as_str)).bind(input.token_hash.0.as_slice()).bind(input.scope.as_str()).bind(repositories).bind(now_ms).bind(input.expires_at_ms).bind(actor.identity_id.as_str()).bind(kind_name(actor.kind)).bind(now_ms).bind(actor.identity_id.as_str()).bind(kind_name(actor.kind)).execute(&mut *connection).await?;
        Ok(Principal {
            tenant_id: tenant.clone(),
            token_id: token,
            actor: Actor {
                identity_id: identity.id,
                kind: identity.kind,
            },
            scope: input.scope,
            repositories: input.repositories.clone(),
            expires_at_ms: input.expires_at_ms,
        })
    }
}

#[async_trait]
impl Store for PostgresStore {
    async fn close(&self) {
        PostgresStore::close(self).await;
    }
    async fn deployment_bootstrap(&self, input: &Bootstrap, now_ms: i64) -> StoreResult<Principal> {
        if input.tenant_slug.is_empty()
            || input.tenant_slug.len() > 100
            || !input
                .tenant_slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid(
                "tenant_slug",
                "use 1 to 100 lowercase ASCII letters, digits, or hyphens",
            ));
        }
        validate_name(&input.human_name)?;
        self.check_expiry(IdentityKind::Human, input.expires_at_ms, now_ms)?;
        let mut tx = self.writer.begin().await?;
        let tenant = TenantId::new();
        let human = IdentityId::new();
        let actor = Actor {
            identity_id: human.clone(),
            kind: IdentityKind::System,
        };
        sqlx::query("INSERT INTO tenants (id, slug, created_at_ms) VALUES ($1, $2, $3)")
            .bind(tenant.as_str())
            .bind(&input.tenant_slug)
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(map_unique)?;
        Self::insert_identity(
            &mut tx,
            &tenant,
            &human,
            &CreateIdentity {
                kind: IdentityKind::Human,
                name: input.human_name.clone(),
                owner_id: None,
            },
            &actor,
            now_ms,
        )
        .await?;
        let principal = self
            .insert_token(
                &mut tx,
                &tenant,
                &MintToken {
                    identity_id: human,
                    token_hash: input.token_hash,
                    scope: Scope::TenantAdmin,
                    repositories: None,
                    expires_at_ms: input.expires_at_ms,
                },
                None,
                &actor,
                now_ms,
            )
            .await?;
        tx.commit().await?;
        Ok(principal)
    }

    async fn deployment_authenticate(
        &self,
        hash: TokenHash,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let mut tx = self.readers.begin().await?;
        let rows = sqlx::query("SELECT tenant_id, id FROM tokens WHERE secret_hash = $1 LIMIT 2")
            .bind(hash.0.as_slice())
            .fetch_all(&mut *tx)
            .await?;
        if rows.len() != 1 {
            return Err(request(ErrorCode::Unauthenticated));
        }
        let tenant = parse(rows[0].try_get("tenant_id")?)?;
        let token = parse(rows[0].try_get("id")?)?;
        let (principal, _) = self.resolve(&mut tx, &tenant, &token, now_ms).await?;
        tx.commit().await?;
        Ok(principal)
    }

    async fn deployment_issue_human_token(
        &self,
        tenant: &TenantId,
        input: &MintToken,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let mut tx = self.writer.begin().await?;
        let identity = Self::identity_at(&mut tx, tenant, &input.identity_id).await?;
        if identity.kind != IdentityKind::Human {
            return Err(invalid(
                "identity_id",
                "deployment token issuance is for human identities",
            ));
        }
        let actor = Actor {
            identity_id: identity.id,
            kind: IdentityKind::System,
        };
        let result = self
            .insert_token(&mut tx, tenant, input, None, &actor, now_ms)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn create_identity(
        &self,
        principal: &Principal,
        input: &CreateIdentity,
        now_ms: i64,
    ) -> StoreResult<IdentityRecord> {
        validate_name(&input.name)?;
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        principal.require(Scope::TenantAdmin)?;
        match (input.kind, &input.owner_id) {
            (IdentityKind::Human, None) => {}
            (IdentityKind::Agent, Some(owner)) => {
                let owner = Self::identity_at(&mut tx, &principal.tenant_id, owner).await?;
                if owner.kind != IdentityKind::Human || owner.disabled_at_ms.is_some() {
                    return Err(invalid("owner_id", "an agent needs an enabled human owner"));
                }
            }
            _ => {
                return Err(invalid(
                    "kind",
                    "humans have no owner; agents require a human owner",
                ));
            }
        }
        let id = IdentityId::new();
        Self::insert_identity(
            &mut tx,
            &principal.tenant_id,
            &id,
            input,
            &principal.actor,
            now_ms,
        )
        .await?;
        let record = Self::identity_at(&mut tx, &principal.tenant_id, &id).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn get_identity(
        &self,
        tenant: &TenantId,
        id: &IdentityId,
    ) -> StoreResult<IdentityRecord> {
        let mut tx = self.readers.begin().await?;
        let result = Self::identity_at(&mut tx, tenant, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn mint_token(
        &self,
        principal: &Principal,
        input: &MintToken,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let mut tx = self.writer.begin().await?;
        let principal = self
            .check_mint_at(&mut tx, principal, input, now_ms)
            .await?;
        let result = self
            .insert_token(
                &mut tx,
                &principal.tenant_id,
                input,
                Some(&principal.token_id),
                &principal.actor,
                now_ms,
            )
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn revoke_token(
        &self,
        principal: &Principal,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tokens WHERE tenant_id = $1 AND id = $2)",
        )
        .bind(principal.tenant_id.as_str())
        .bind(token.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(request(ErrorCode::NotFound));
        }
        principal.require(Scope::TenantAdmin)?;
        sqlx::query("UPDATE tokens SET revoked_at_ms = COALESCE(revoked_at_ms, $1), updated_at_ms = $2, updated_by = $3, updated_by_kind = $4 WHERE tenant_id = $5 AND id = $6").bind(now_ms).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(token.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn disable_identity(
        &self,
        principal: &Principal,
        id: &IdentityId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        Self::identity_at(&mut tx, &principal.tenant_id, id).await?;
        principal.require(Scope::TenantAdmin)?;
        sqlx::query("UPDATE identities SET disabled_at_ms = COALESCE(disabled_at_ms, $1), updated_at_ms = $2, updated_by = $3, updated_by_kind = $4 WHERE tenant_id = $5 AND id = $6").bind(now_ms).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn reserve_repository(
        &self,
        principal: &Principal,
        name: &str,
        default_branch: &str,
        now_ms: i64,
    ) -> StoreResult<RepositoryRecord> {
        if name.is_empty()
            || name.len() > 100
            || name.ends_with(".git")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || name == "."
            || name == ".."
        {
            return Err(invalid(
                "name",
                "use an ASCII repository name without a .git suffix",
            ));
        }
        if !valid_branch(default_branch) {
            return Err(invalid("default_branch", "invalid git branch name"));
        }
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        principal.require(Scope::RepoAdmin)?;
        if principal.repositories.is_some() {
            return Err(request(ErrorCode::PermissionDenied));
        }
        let id = RepositoryId::new();
        let backend_id = format!("{}/{}/g1", principal.tenant_id, id);
        sqlx::query("INSERT INTO repositories (tenant_id, id, name, default_branch, state, created_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) VALUES ($1, $2, $3, $4, 'creating', $5, $6, $7, $8, $9, $10)")
            .bind(principal.tenant_id.as_str()).bind(id.as_str()).bind(name).bind(default_branch).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).execute(&mut *tx).await.map_err(map_unique)?;
        sqlx::query("INSERT INTO generations (tenant_id, repository_id, number, backend_id, state, created_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) VALUES ($1, $2, 1, $3, 'pending', $4, $5, $6, $7, $8, $9)")
            .bind(principal.tenant_id.as_str()).bind(id.as_str()).bind(backend_id).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT * FROM repositories WHERE tenant_id = $1 AND id = $2")
            .bind(principal.tenant_id.as_str())
            .bind(id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        let result = repository(&row)?;
        tx.commit().await?;
        Ok(result)
    }

    async fn get_repository(
        &self,
        tenant: &TenantId,
        id: &RepositoryId,
    ) -> StoreResult<RepositoryRecord> {
        let mut tx = self.readers.begin().await?;
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = $1 AND id = $2 AND state != 'deleted'",
        )
        .bind(tenant.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        let result = repository(&row)?;
        tx.commit().await?;
        Ok(result)
    }

    async fn mark_repository_ready(
        &self,
        principal: &Principal,
        id: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM repositories WHERE tenant_id = $1 AND id = $2 AND state != 'deleted'",
        )
        .bind(principal.tenant_id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        principal.require_repository(id, Scope::RepoAdmin)?;
        if state != "creating" && state != "ready" {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        sqlx::query("UPDATE repositories SET state = 'ready', updated_at_ms = $1, updated_by = $2, updated_by_kind = $3 WHERE tenant_id = $4 AND id = $5").bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE generations SET state = 'active', updated_at_ms = $1, updated_by = $2, updated_by_kind = $3 WHERE tenant_id = $4 AND repository_id = $5 AND number = 1 AND state = 'pending'").bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

fn validate_name(value: &str) -> StoreResult<()> {
    if value.trim().is_empty() || value.len() > 200 || value.chars().any(char::is_control) {
        return Err(invalid(
            "name",
            "use 1 to 200 bytes without control characters",
        ));
    }
    Ok(())
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value != "HEAD"
        && value.len() <= 1024
        && !value.starts_with('-')
        && !value.ends_with('.')
        && !value.contains("..")
        && !value.contains("@{")
        && !value
            .bytes()
            .any(|b| b <= 32 || b == 127 || b"~^:?*[\\".contains(&b))
        && value
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}
