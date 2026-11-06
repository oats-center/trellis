//! Completes an installation when Trellis starts, so `config.toml` holds only
//! what differs from one installation to the next.
//!
//! - Every key Trellis owns is created the first time it is missing and kept
//!   in `secrets.toml` beside `config.toml`, readable by its owner only.
//! - Given a NATS operator's system credentials and a signing key the operator
//!   made for it, Trellis creates its own accounts and users on the NATS
//!   servers, and updates them on every start.
//! - `generated` TLS gets a CA and certificate Trellis renews itself.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;

use crate::config::{ListenerTransport, NatsUser, RuntimeConfig, Secrets, TlsConfig, SECRETS_FILE};
use crate::mode::RuntimeMode;

/// Generated certificates are reissued once they expire within this long.
pub const GENERATED_TLS_RENEW_WITHIN: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Failure to complete an installation at start.
#[derive(Debug, thiserror::Error)]
pub enum InstallationError {
    /// A file the installation names could not be read or written.
    #[error("{action} '{path}': {source}")]
    File {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// NATS keys or JWTs could not be made or read.
    #[error("NATS keys: {0}")]
    Keys(String),
    /// The NATS servers did not take Trellis's accounts.
    #[error("NATS servers refused Trellis's accounts: {0}")]
    Nats(String),
    /// Generated TLS could not be created or renewed.
    #[error("generated TLS: {0}")]
    Tls(String),
}

/// Completes `config`, loaded from `config_dir`, before Trellis validates and
/// starts it in `mode`. Data such as generated certificates goes in `data_dir`.
///
/// Only a role that runs the platform creates keys and NATS accounts; split
/// roles started on their own use what it created. Without `push_accounts`
/// the accounts are created but not sent to NATS, for a command that only
/// touches the database; the next start sends them.
///
/// # Errors
///
/// Returns [`InstallationError`] when a key, the NATS servers or generated TLS
/// can't be prepared.
pub async fn prepare(
    config: &mut RuntimeConfig,
    config_dir: &Path,
    data_dir: &Path,
    mode: RuntimeMode,
    push_accounts: bool,
) -> Result<(), InstallationError> {
    let platform = matches!(mode, RuntimeMode::All | RuntimeMode::Platform);
    if platform {
        let mut secrets = config.secrets.clone();
        let mut changed = create_missing_keys(&mut secrets);
        if let Some(auth) = config.auth.as_mut() {
            if auth.instance_id.is_none() {
                if secrets.instance_id.is_none() {
                    secrets.instance_id = Some(ulid::Ulid::generate().to_string());
                    changed = true;
                }
                auth.instance_id.clone_from(&secrets.instance_id);
            }
        }
        if let Some(provisioning) = nats_provisioning(config)? {
            changed |= create_missing_nats_accounts(&mut secrets)?;
            if push_accounts {
                if let Err(error) = push_nats_accounts(config, &provisioning, &secrets).await {
                    // Servers that already have the accounts still admit
                    // Trellis; only what needs the system user, such as
                    // closing connections, waits until it works again.
                    if !accounts_resolve(config, &secrets).await {
                        return Err(error);
                    }
                    tracing::warn!(%error, "NATS refused Trellis's account update; using the accounts it has");
                }
            }
            secrets.nats.users.system = Some(provisioning.system_user);
        }
        if changed {
            save_secrets(config_dir, &secrets)?;
        }
        config.secrets = secrets;
    } else if let (Some(auth), Some(instance)) =
        (config.auth.as_mut(), config.secrets.instance_id.clone())
    {
        auth.instance_id.get_or_insert(instance);
    }
    if let Some((system, _)) = nats_files(config) {
        // Every role reads the operator's system user from its file.
        config.secrets.nats.users.system = Some(read_creds(&system)?);
    }
    prepare_generated_tls(config, data_dir)?;
    Ok(())
}

/// Reissues the certificate of `generated` TLS when it is close to expiry.
/// The listener picks up replaced files by itself.
///
/// # Errors
///
/// Returns [`InstallationError::Tls`] when the certificate can't be reissued.
pub fn renew_generated_tls(
    config: &RuntimeConfig,
    data_dir: &Path,
) -> Result<(), InstallationError> {
    if config
        .tls
        .as_ref()
        .and_then(|tls| tls.generated_ca_certificate.as_ref())
        .is_none()
    {
        return Ok(());
    }
    trellis_bootstrap::ensure_generated_tls(
        &data_dir.join("tls"),
        &generated_ca_name(config),
        &generated_hosts(config),
        time_duration(GENERATED_TLS_RENEW_WITHIN),
    )
    .map(|_| ())
    .map_err(|error| InstallationError::Tls(error.to_string()))
}

/// Creates each key of `secrets` that is missing; true when one was.
fn create_missing_keys(secrets: &mut Secrets) -> bool {
    let key =
        || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let mut changed = false;
    for slot in [
        &mut secrets.oauth_storage_key,
        &mut secrets.authorization_issuer_seed,
        &mut secrets.live_providers.platform,
        &mut secrets.live_providers.health,
        &mut secrets.live_providers.jobs,
        &mut secrets.live_providers.events,
    ] {
        if slot.is_none() {
            *slot = Some(key());
            changed = true;
        }
    }
    changed
}

/// What Trellis needs from a NATS operator to manage its own accounts.
struct NatsProvisioning {
    system_user: NatsUser,
    operator_signing_seed: String,
}

/// The operator's system credentials and signing key files, when configured.
fn nats_files(config: &RuntimeConfig) -> Option<(PathBuf, PathBuf)> {
    let nats = config.nats.as_ref()?;
    Some((
        nats.system_credentials.clone()?,
        nats.operator_signing_key.clone()?,
    ))
}

fn nats_provisioning(
    config: &RuntimeConfig,
) -> Result<Option<NatsProvisioning>, InstallationError> {
    let Some((system, signing)) = nats_files(config) else {
        return Ok(None);
    };
    let seed = read_file(&signing)?.trim().to_owned();
    if !seed.starts_with("SO") {
        return Err(InstallationError::Keys(format!(
            "'{}' must hold a NATS operator signing key seed (SO...)",
            signing.display()
        )));
    }
    Ok(Some(NatsProvisioning {
        system_user: read_creds(&system)?,
        operator_signing_seed: seed,
    }))
}

/// Creates Trellis's NATS accounts the first time; true when it did.
fn create_missing_nats_accounts(secrets: &mut Secrets) -> Result<bool, InstallationError> {
    let nats = &mut secrets.nats;
    if nats.accounts.auth.is_some() && nats.accounts.trellis.is_some() {
        return Ok(false);
    }
    let accounts = trellis_bootstrap::TrellisAccounts::new()
        .map_err(|error| InstallationError::Keys(error.to_string()))?;
    let user = |keys: trellis_bootstrap::NatsUserKeys| NatsUser {
        jwt: keys.jwt,
        seed: keys.seed,
    };
    nats.accounts.auth = Some(accounts.auth_seed);
    nats.accounts.trellis = Some(accounts.trellis_seed);
    nats.users.auth = Some(user(accounts.auth_user));
    nats.users.trellis = Some(user(accounts.trellis_user));
    let callout = &mut nats.auth_callout;
    callout.issuer_signing_seed = Some(accounts.auth_signing_seed);
    callout.target_signing_seed = Some(accounts.trellis_signing_seed);
    callout.xkey_seed = Some(accounts.xkey_seed);
    callout.sentinel_public_key = Some(accounts.sentinel_public_key);
    callout.sentinel_jwt = Some(accounts.sentinel_jwt);
    Ok(true)
}

/// Pushes Trellis's two accounts to the NATS servers as the system user.
async fn push_nats_accounts(
    config: &RuntimeConfig,
    provisioning: &NatsProvisioning,
    secrets: &Secrets,
) -> Result<(), InstallationError> {
    let missing = |name: &str| InstallationError::Keys(format!("secrets.toml lacks {name}"));
    let nats = &secrets.nats;
    let callout = &nats.auth_callout;
    let accounts = trellis_bootstrap::TrellisAccounts {
        auth_seed: nats
            .accounts
            .auth
            .clone()
            .ok_or_else(|| missing("nats.accounts.auth"))?,
        trellis_seed: nats
            .accounts
            .trellis
            .clone()
            .ok_or_else(|| missing("nats.accounts.trellis"))?,
        auth_signing_seed: callout
            .issuer_signing_seed
            .clone()
            .ok_or_else(|| missing("issuer_signing_seed"))?,
        trellis_signing_seed: callout
            .target_signing_seed
            .clone()
            .ok_or_else(|| missing("target_signing_seed"))?,
        auth_user: nats
            .users
            .auth
            .clone()
            .map(|user| trellis_bootstrap::NatsUserKeys {
                jwt: user.jwt,
                seed: user.seed,
            })
            .ok_or_else(|| missing("nats.users.auth"))?,
        trellis_user: nats
            .users
            .trellis
            .clone()
            .map(|user| trellis_bootstrap::NatsUserKeys {
                jwt: user.jwt,
                seed: user.seed,
            })
            .ok_or_else(|| missing("nats.users.trellis"))?,
        xkey_seed: callout
            .xkey_seed
            .clone()
            .ok_or_else(|| missing("xkey_seed"))?,
        sentinel_public_key: callout.sentinel_public_key.clone().unwrap_or_default(),
        sentinel_jwt: callout.sentinel_jwt.clone().unwrap_or_default(),
    };
    let jwts = accounts
        .account_jwts(&provisioning.operator_signing_seed)
        .map_err(|error| InstallationError::Keys(error.to_string()))?;
    let servers = config
        .nats
        .as_ref()
        .and_then(|nats| nats.servers.clone())
        .unwrap_or_default();
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let mut options =
        async_nats::ConnectOptions::with_credentials(&provisioning.system_user.creds())
            .map_err(|error| InstallationError::Nats(error.to_string()))?
            .name("trellis-provisioning")
            .connection_timeout(Duration::from_secs(10));
    if let Some(ca) = config
        .nats
        .as_ref()
        .and_then(|nats| nats.ca_certificate.clone())
    {
        options = options.add_root_certificates(ca);
    }
    let urls: Vec<String> = servers
        .split(',')
        .map(|server| server.trim().to_owned())
        .collect();
    let client = options
        .connect(urls)
        .await
        .map_err(|error| InstallationError::Nats(format!("connect as the system user: {error}")))?;
    for jwt in jwts {
        let reply = client
            .request("$SYS.REQ.CLAIMS.UPDATE", jwt.into())
            .await
            .map_err(|error| InstallationError::Nats(error.to_string()))?;
        let reply: serde_json::Value = serde_json::from_slice(&reply.payload)
            .map_err(|error| InstallationError::Nats(error.to_string()))?;
        if reply["data"]["code"].as_u64() != Some(200) {
            return Err(InstallationError::Nats(
                reply["error"]["description"]
                    .as_str()
                    .unwrap_or("the servers rejected an account")
                    .to_owned(),
            ));
        }
    }
    let _ = client.drain().await;
    Ok(())
}

/// Whether the NATS servers admit Trellis's own user, which they do only when
/// they have its account.
async fn accounts_resolve(config: &RuntimeConfig, secrets: &Secrets) -> bool {
    let Some(user) = secrets.nats.users.trellis.as_ref() else {
        return false;
    };
    let creds = trellis_bootstrap::NatsUserKeys {
        jwt: user.jwt.clone(),
        seed: user.seed.clone(),
    }
    .creds();
    let Ok(mut options) = async_nats::ConnectOptions::with_credentials(&creds) else {
        return false;
    };
    options = options
        .name("trellis-provisioning")
        .connection_timeout(Duration::from_secs(10));
    let nats = config.nats.as_ref();
    if let Some(ca) = nats.and_then(|nats| nats.ca_certificate.clone()) {
        options = options.add_root_certificates(ca);
    }
    let urls: Vec<String> = nats
        .and_then(|nats| nats.servers.clone())
        .unwrap_or_default()
        .split(',')
        .map(|server| server.trim().to_owned())
        .collect();
    match options.connect(urls).await {
        Ok(client) => {
            let _ = client.drain().await;
            true
        }
        Err(_) => false,
    }
}

/// Writes `secrets` to `secrets.toml`, replacing it whole and readable by its
/// owner only.
fn save_secrets(config_dir: &Path, secrets: &Secrets) -> Result<(), InstallationError> {
    let path = config_dir.join(SECRETS_FILE);
    let staged = config_dir.join(format!("{SECRETS_FILE}.new"));
    let file_error = |action, source| InstallationError::File {
        action,
        path: path.clone(),
        source,
    };
    trellis_bootstrap::write_private_file(&staged, secrets.to_toml())
        .map_err(|error| file_error("write", std::io::Error::other(error.to_string())))?;
    std::fs::rename(&staged, &path).map_err(|source| file_error("replace", source))
}

/// Reads a NATS `.creds` file.
fn read_creds(path: &Path) -> Result<NatsUser, InstallationError> {
    let contents = read_file(path)?;
    let block = |begin: &str| {
        let start = contents.find(begin)? + begin.len();
        let rest = &contents[start..];
        let end = rest.find("------END")?;
        Some(rest[..end].trim().to_owned())
    };
    match (
        block("-----BEGIN NATS USER JWT-----"),
        block("-----BEGIN USER NKEY SEED-----"),
    ) {
        (Some(jwt), Some(seed)) if !jwt.is_empty() && seed.starts_with("SU") => {
            Ok(NatsUser { jwt, seed })
        }
        _ => Err(InstallationError::Keys(format!(
            "'{}' is not a NATS user .creds file",
            path.display()
        ))),
    }
}

fn read_file(path: &Path) -> Result<String, InstallationError> {
    std::fs::read_to_string(path).map_err(|source| InstallationError::File {
        action: "read",
        path: path.to_path_buf(),
        source,
    })
}

/// Turns a `generated` transport into the certificate files it gets.
fn prepare_generated_tls(
    config: &mut RuntimeConfig,
    data_dir: &Path,
) -> Result<(), InstallationError> {
    let generated = config
        .http
        .as_ref()
        .and_then(|http| http.transport.as_ref())
        .is_some_and(|transport| *transport == ListenerTransport::Generated);
    if !generated {
        return Ok(());
    }
    let files = trellis_bootstrap::ensure_generated_tls(
        &data_dir.join("tls"),
        &generated_ca_name(config),
        &generated_hosts(config),
        time_duration(GENERATED_TLS_RENEW_WITHIN),
    )
    .map_err(|error| InstallationError::Tls(error.to_string()))?;
    if let Some(http) = config.http.as_mut() {
        http.transport = Some(ListenerTransport::Tls {
            certificate_chain: files.certificate_chain,
            private_key: files.private_key,
        });
    }
    config.tls = Some(TlsConfig {
        generated_ca_certificate: Some(files.ca_certificate),
    });
    Ok(())
}

fn generated_ca_name(config: &RuntimeConfig) -> String {
    format!(
        "{} development CA",
        config.instance_name.as_deref().unwrap_or("Trellis")
    )
}

/// The hosts of the Trellis and Console origins.
fn generated_hosts(config: &RuntimeConfig) -> Vec<String> {
    let mut hosts = Vec::new();
    for origin in [
        config
            .http
            .as_ref()
            .and_then(|http| http.public_origin.clone()),
        config
            .console
            .as_ref()
            .and_then(|console| console.origin.clone()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(host) = url::Url::parse(&origin)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
        {
            hosts.push(host);
        }
    }
    hosts
}

fn time_duration(duration: Duration) -> time::Duration {
    time::Duration::seconds(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}
