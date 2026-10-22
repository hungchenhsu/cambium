use std::{path::PathBuf, sync::Arc};

use cambium_backend_enroute::{EnrouteBackend, hooks::HookAdapter};
use cambium_protocol::{ApiError, ErrorCode, IdentityId, RequestId, Scope, TenantId, TokenId};
use cambium_server::{
    deployment::{self, Config},
    git_policy::Policy,
    secrets::{new_token, token_hash},
    service::{App, Clock, SystemClock},
    transport::{PublicState, hook_router, public_router},
};
use cambium_store::{Bootstrap, MintToken, SqliteStore, Store, StoreError};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Parser)]
#[command(
    name = "cambium",
    version,
    about = "Agent-first repository operations over private git storage"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".cambium")]
    data_dir: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create private deployment keys, configuration, and an empty metadata store.
    Init,
    /// Start the public API/MCP and private signed-hook listeners.
    Serve,
    /// Host-only tenant, human credential, and revocation administration.
    Admin {
        #[command(subcommand)]
        command: Admin,
    },
}

#[derive(Subcommand)]
enum Admin {
    Bootstrap {
        #[arg(long)]
        tenant_slug: String,
        #[arg(long)]
        human_name: String,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: u64,
    },
    IssueHumanToken {
        #[arg(long)]
        tenant_id: TenantId,
        #[arg(long)]
        identity_id: IdentityId,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: u64,
    },
    RevokeToken {
        #[arg(long)]
        tenant_id: TenantId,
        #[arg(long)]
        token_id: TokenId,
    },
    DisableIdentity {
        #[arg(long)]
        tenant_id: TenantId,
        #[arg(long)]
        identity_id: IdentityId,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(&cli).await {
        if cli.json {
            let api_error = if let Some(error) = error.downcast_ref::<ApiError>() {
                error.clone()
            } else if let Some(error) = error.downcast_ref::<StoreError>() {
                ApiError::new(error.code())
            } else {
                ApiError::new(ErrorCode::Internal)
            };
            eprintln!("{}", json!(api_error.response(RequestId::new())));
        } else {
            eprintln!("cambium: {error}");
        }
        std::process::exit(1);
    }
}

async fn run(cli: &Cli) -> deployment::Result<()> {
    if matches!(cli.command, Command::Init) {
        deployment::initialize(&cli.data_dir, &Config::default()).await?;
        println!(
            "{}",
            json!({"initialized": true, "config": cli.data_dir.join("cambium.toml")})
        );
        return Ok(());
    }
    let config = Config::load(&cli.data_dir)?;
    deployment::check_private_file(&cli.data_dir.join("metadata.sqlite"))?;
    // Only the service claims ownership and recovers abandoned running operations.
    let _lock = if matches!(cli.command, Command::Serve) {
        Some(deployment::acquire_process_lock(&cli.data_dir)?)
    } else {
        None
    };
    let store = Arc::new(
        SqliteStore::open(
            cli.data_dir.join("metadata.sqlite"),
            config.token_settings()?,
        )
        .await?,
    );
    match &cli.command {
        Command::Init => unreachable!(),
        Command::Admin { command } => {
            let now = SystemClock.now_ms();
            let value = match command {
                Admin::Bootstrap {
                    tenant_slug,
                    human_name,
                    ttl_seconds,
                } => {
                    let token = new_token()?;
                    let principal = store
                        .deployment_bootstrap(
                            &Bootstrap {
                                tenant_slug: tenant_slug.clone(),
                                human_name: human_name.clone(),
                                token_hash: token_hash(&token)?,
                                expires_at_ms: expiry(now, *ttl_seconds)?,
                            },
                            now,
                        )
                        .await?;
                    credential(&principal, token)?
                }
                Admin::IssueHumanToken {
                    tenant_id,
                    identity_id,
                    ttl_seconds,
                } => {
                    let token = new_token()?;
                    let principal = store
                        .deployment_issue_human_token(
                            tenant_id,
                            &MintToken {
                                identity_id: identity_id.clone(),
                                token_hash: token_hash(&token)?,
                                scope: Scope::TenantAdmin,
                                repositories: None,
                                expires_at_ms: expiry(now, *ttl_seconds)?,
                            },
                            now,
                        )
                        .await?;
                    credential(&principal, token)?
                }
                Admin::RevokeToken {
                    tenant_id,
                    token_id,
                } => {
                    store
                        .deployment_revoke_token(tenant_id, token_id, now)
                        .await?;
                    json!({"token_id": token_id, "revoked": true})
                }
                Admin::DisableIdentity {
                    tenant_id,
                    identity_id,
                } => {
                    store
                        .deployment_disable_identity(tenant_id, identity_id, now)
                        .await?;
                    json!({"identity_id": identity_id, "disabled": true})
                }
            };
            println!("{value}");
        }
        Command::Serve => {
            let key = deployment::response_key(&cli.data_dir)?;
            let keys = config.verifying_keys(&cli.data_dir)?;
            let clock = Arc::new(SystemClock);
            let copies = Arc::new(cambium_server::internal_copy::CopyTokens {
                store: store.clone(),
                clock: clock.clone(),
            });
            let backend = EnrouteBackend::connect(
                &config.backend_endpoint,
                &config.backend_git_url,
                Some(copies),
            )
            .await?;
            let mut app = App::new(store, Arc::new(backend), clock, &key, config.public_git_url);
            app.lifecycle = config.branches;
            app.compaction = config.compaction;
            let app = Arc::new(app);
            let hooks = Arc::new(HookAdapter::new(
                keys,
                &config.hook_endpoint,
                Arc::new(Policy { app: app.clone() }),
            )?);
            let hook_listener = tokio::net::TcpListener::bind(config.hooks_listen).await?;
            let public_listener = tokio::net::TcpListener::bind(config.api_listen).await?;
            app.recover_at_startup().await?;
            let worker_app = app.clone();
            let maintenance = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(
                    config.maintenance_interval_ms,
                ));
                loop {
                    interval.tick().await;
                    if worker_app
                        .store
                        .deployment_purge_expired_responses(worker_app.clock.now_ms())
                        .await
                        .is_err()
                    {
                        eprintln!("cambium: expired response cleanup failed; will retry");
                    }
                    if worker_app.run_lifecycle_once().await.is_err() {
                        eprintln!("cambium: branch lifecycle recovery failed; will retry");
                    }
                    if worker_app.resume_repositories().await.is_err() {
                        eprintln!("cambium: repository recovery failed; will retry");
                    }
                }
            });
            eprintln!(
                "cambium: API/MCP listening on {}; private hooks on {}",
                config.api_listen, config.hooks_listen
            );
            let public = axum::serve(
                public_listener,
                public_router(PublicState {
                    app,
                    allowed_origins: config.allowed_origins,
                }),
            );
            let hooks = axum::serve(hook_listener, hook_router(hooks));
            tokio::select! {
                result = public => result?,
                result = hooks => result?,
                result = tokio::signal::ctrl_c() => result?,
            }
            maintenance.abort();
        }
    }
    Ok(())
}

fn expiry(now: i64, seconds: u64) -> deployment::Result<i64> {
    seconds
        .checked_mul(1000)
        .and_then(|value| i64::try_from(value).ok())
        .and_then(|value| now.checked_add(value))
        .filter(|value| *value > now)
        .ok_or_else(|| ApiError::invalid("ttl_seconds", "Expected a positive lifetime").into())
}

fn credential(principal: &cambium_protocol::Principal, token: String) -> deployment::Result<Value> {
    let expires_at =
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(principal.expires_at_ms) * 1_000_000)?
            .format(&Rfc3339)?;
    Ok(
        json!({"tenant_id": principal.tenant_id, "identity_id": principal.actor.identity_id, "kind": principal.actor.kind, "token_id": principal.token_id, "scope": principal.scope, "expires_at": expires_at, "token": token}),
    )
}
