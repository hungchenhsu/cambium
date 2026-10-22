//! Private deployment files and process ownership, separate from tenant requests.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::Path,
};

use cambium_backend_enroute::signature::{SigningKey, VerifyingKey};
use cambium_protocol::ApiError;
use cambium_store::{SqliteStore, TokenSettings};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub api_listen: SocketAddr,
    pub hooks_listen: SocketAddr,
    pub hook_endpoint: String,
    pub backend_endpoint: String,
    pub backend_git_url: String,
    pub public_git_url: String,
    pub allowed_origins: Vec<String>,
    pub hook_public_keys: Vec<String>,
    pub human_max_lifetime_seconds: u64,
    pub agent_max_lifetime_seconds: u64,
    pub maintenance_interval_ms: u64,
    pub branches: crate::lifecycle::LifecycleSettings,
    pub compaction: crate::compaction::CompactionSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_listen: "127.0.0.1:8080".parse().unwrap(),
            hooks_listen: "127.0.0.1:8081".parse().unwrap(),
            hook_endpoint: "http://127.0.0.1:8081/api/enroute/hooks".into(),
            backend_endpoint: "http://127.0.0.1:8082".into(),
            backend_git_url: "http://127.0.0.1:8083/".into(),
            public_git_url: "http://127.0.0.1:8083/".into(),
            allowed_origins: Vec::new(),
            hook_public_keys: vec!["hook-public.pem".into()],
            human_max_lifetime_seconds: 30 * 24 * 3600,
            agent_max_lifetime_seconds: 24 * 3600,
            maintenance_interval_ms: 1000,
            branches: crate::lifecycle::LifecycleSettings::default(),
            compaction: crate::compaction::CompactionSettings::default(),
        }
    }
}

impl Config {
    pub fn load(directory: &Path) -> Result<Self> {
        check_private_directory(directory)?;
        let contents = read_private(&directory.join("cambium.toml"), 64 * 1024)?;
        let config: Self = toml::from_str(std::str::from_utf8(&contents)?)?;
        if config.api_listen == config.hooks_listen {
            return Err(ApiError::invalid("hooks_listen", "Use a separate hook listener").into());
        }
        let endpoint = url::Url::parse(&config.hook_endpoint)?;
        if endpoint.path() != "/api/enroute/hooks" {
            return Err(ApiError::invalid("hook_endpoint", "Expected /api/enroute/hooks").into());
        }
        for value in [&config.backend_git_url, &config.public_git_url] {
            let url = url::Url::parse(value)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.path().ends_with('/')
            {
                return Err(ApiError::invalid(
                    "git_url",
                    "Expected an HTTP base URL ending in / without credentials",
                )
                .into());
            }
        }
        if !(1..=2).contains(&config.hook_public_keys.len()) {
            return Err(ApiError::invalid(
                "hook_public_keys",
                "Configure one or two verification keys",
            )
            .into());
        }
        if !(1..=60_000).contains(&config.maintenance_interval_ms)
            || config.branches.max_ttl_seconds == 0
            || config.branches.max_ttl_seconds > (i64::MAX / 1000) as u64
        {
            return Err(ApiError::invalid(
                "lifecycle",
                "Expected positive TTL and a maintenance interval from 1 to 60000 milliseconds",
            )
            .into());
        }
        if config.compaction.max_delay_ms < 0
            || config.compaction.max_expired <= 0
            || config.compaction.settle_ms <= 0
            || config.compaction.drain_ms < 0
        {
            return Err(ApiError::invalid(
                "compaction",
                "Expected nonnegative delays and positive expiry and settle thresholds",
            )
            .into());
        }
        Ok(config)
    }

    pub fn token_settings(&self) -> Result<TokenSettings> {
        let milliseconds = |seconds: u64| -> Result<i64> {
            let value = seconds
                .checked_mul(1000)
                .and_then(|value| i64::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    ApiError::invalid(
                        "token_lifetime",
                        "Expected positive seconds within i64 milliseconds",
                    )
                })?;
            Ok(value)
        };
        Ok(TokenSettings {
            human_max_lifetime_ms: milliseconds(self.human_max_lifetime_seconds)?,
            agent_max_lifetime_ms: milliseconds(self.agent_max_lifetime_seconds)?,
            ..TokenSettings::default()
        })
    }

    pub fn verifying_keys(&self, directory: &Path) -> Result<Vec<VerifyingKey>> {
        self.hook_public_keys
            .iter()
            .map(|name| {
                let path = Path::new(name);
                if path.components().count() != 1
                    || !matches!(
                        path.components().next(),
                        Some(std::path::Component::Normal(_))
                    )
                {
                    return Err(ApiError::invalid(
                        "hook_public_keys",
                        "Use filenames inside the private deployment directory",
                    )
                    .into());
                }
                let bytes = read_private(&directory.join(path), 8192)?;
                Ok(VerifyingKey::from_pem(std::str::from_utf8(&bytes)?)?)
            })
            .collect()
    }
}

pub async fn initialize(directory: &Path, config: &Config) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    check_private_directory(directory)?;
    let mut response_key = [0; 32];
    let mut signing_seed = [0; 32];
    getrandom::fill(&mut response_key)?;
    getrandom::fill(&mut signing_seed)?;
    let signing = SigningKey::from_seed(&signing_seed);
    write_new_private(&directory.join("responses.key"), &response_key)?;
    write_new_private(
        &directory.join("hook-signing.pem"),
        signing.to_pem()?.as_bytes(),
    )?;
    write_new_private(
        &directory.join("hook-public.pem"),
        signing.verifying_key().to_pem()?.as_bytes(),
    )?;
    write_new_private(
        &directory.join("cambium.toml"),
        toml::to_string_pretty(config)?.as_bytes(),
    )?;
    // Create the database with private permissions before SQLite opens sidecars.
    write_new_private(&directory.join("metadata.sqlite"), b"")?;
    let _store =
        SqliteStore::open(directory.join("metadata.sqlite"), config.token_settings()?).await?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub fn response_key(directory: &Path) -> Result<[u8; 32]> {
    read_private(&directory.join("responses.key"), 32)?
        .try_into()
        .map_err(|_| {
            ApiError::invalid(
                "response_key",
                "Expected exactly 32 bytes; restore the existing deployment key",
            )
            .into()
        })
}

pub fn acquire_process_lock(directory: &Path) -> Result<File> {
    check_private_directory(directory)?;
    let path = directory.join("server.lock");
    if path.try_exists()? {
        check_private_file(&path)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock().map_err(|_| {
        ApiError::invalid(
            "data_dir",
            "Another server owns this deployment or the filesystem cannot lock it",
        )
    })?;
    Ok(file)
}

pub fn check_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(
            ApiError::invalid("data_dir", "Expected a private directory, not a symlink").into(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(ApiError::invalid("data_dir", "Directory must have mode 0700").into());
        }
    }
    Ok(())
}

pub fn check_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(ApiError::invalid(
            "private_file",
            "Expected a regular private file, not a symlink",
        )
        .into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(ApiError::invalid("private_file", "File must have mode 0600").into());
        }
    }
    Ok(())
}

fn read_private(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    check_private_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(ApiError::invalid("private_file", "File exceeds its size limit").into());
    }
    Ok(bytes)
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
