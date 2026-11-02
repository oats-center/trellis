//! The installation's private keys, kept together in `secrets.toml` beside
//! `config.toml` so the settings can be read, shared and diffed without them.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ConfigError;

/// File name of an installation's secrets, beside its `config.toml`.
pub const SECRETS_FILE: &str = "secrets.toml";

/// Largest `secrets.toml` the runtime reads.
const MAX_SECRETS_BYTES: u64 = 64 * 1024;

/// Every private key Trellis itself reads. `trellis-server init` writes them
/// once; the runtime only reads them.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    /// Canonical base64url 32-byte key sealing durable OAuth credentials.
    /// Keep it across restarts and issuer rotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_storage_key: Option<String>,
    /// Canonical base64url Ed25519 seed the authorization issuer signs with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_issuer_seed: Option<String>,
    /// Identity seeds of the built-in live providers.
    #[serde(default)]
    pub live_providers: LiveProviderSeeds,
    /// NATS users and auth-callout keys.
    #[serde(default)]
    pub nats: NatsSecrets,
}

/// Base64url identity seeds of the built-in live providers, by role.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiveProviderSeeds {
    /// Platform provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Health provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    /// Jobs provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<String>,
    /// Events provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<String>,
}

/// NATS material only the runtime uses.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NatsSecrets {
    /// The NATS users the runtime connects as.
    #[serde(default)]
    pub users: NatsUsers,
    /// Keys the runtime answers NATS auth callouts with.
    #[serde(default)]
    pub auth_callout: AuthCalloutSecrets,
}

/// The runtime's NATS users, one per account.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NatsUsers {
    /// User in the system account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<NatsUser>,
    /// User in the auth account, which answers auth callouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<NatsUser>,
    /// User in the Trellis account, where services and clients run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trellis: Option<NatsUser>,
}

/// One NATS user: its JWT and its nkey seed.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NatsUser {
    /// User JWT signed by its account.
    pub jwt: String,
    /// User nkey seed.
    pub seed: String,
}

impl NatsUser {
    /// The user as a NATS `.creds` file, as NATS clients and the `nats` CLI read it.
    #[must_use]
    pub fn creds(&self) -> String {
        format!(
            "-----BEGIN NATS USER JWT-----\n{}\n------END NATS USER JWT------\n\n\
            ************************* IMPORTANT *************************\n\
            NKEY Seed printed below can be used sign and prove identity.\n\
            NKEYs are sensitive and should be treated as secrets.\n\n\
            -----BEGIN USER NKEY SEED-----\n{}\n------END USER NKEY SEED------\n\n\
            *************************************************************\n",
            self.jwt, self.seed
        )
    }
}

/// Keys for answering NATS auth callouts.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthCalloutSecrets {
    /// Auth-account signing seed that signs callout responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer_signing_seed: Option<String>,
    /// Trellis-account signing seed that signs the users callouts admit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_signing_seed: Option<String>,
    /// Curve seed that decrypts callout requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xkey_seed: Option<String>,
    /// Public key of the sentinel user clients connect with before a callout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentinel_public_key: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secrets { .. }")
    }
}

impl std::fmt::Debug for LiveProviderSeeds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LiveProviderSeeds { .. }")
    }
}

impl std::fmt::Debug for NatsSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NatsSecrets { .. }")
    }
}

impl std::fmt::Debug for NatsUsers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NatsUsers { .. }")
    }
}

impl std::fmt::Debug for NatsUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NatsUser { .. }")
    }
}

impl std::fmt::Debug for AuthCalloutSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthCalloutSecrets { .. }")
    }
}

impl Secrets {
    /// Reads `path`. A missing file has no secrets; each one's user then
    /// reports which it needs.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Secrets`] when the file is a link, another
    /// user's, readable by anyone else, too large, or not valid TOML for this
    /// schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        use std::io::Read as _;

        let invalid = |reason: String| ConfigError::Secrets {
            path: path.to_path_buf(),
            reason,
        };
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(invalid(format!("cannot be read: {error}"))),
        };
        if !metadata.is_file() {
            return Err(invalid("must be a regular file".into()));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        // Open without following a link and check the opened file, so a swap
        // between inspection and read cannot substitute another file.
        let file = options
            .open(path)
            .map_err(|error| invalid(format!("cannot be read: {error}")))?;
        let opened = file
            .metadata()
            .map_err(|error| invalid(format!("cannot be read: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            // SAFETY: geteuid has no arguments or borrowed memory.
            let owner = unsafe { libc::geteuid() };
            if metadata.dev() != opened.dev()
                || metadata.ino() != opened.ino()
                || opened.mode() & 0o077 != 0
                || opened.uid() != owner
                || opened.nlink() != 1
            {
                return Err(invalid(
                    "must be a single-link file owned by this user without group or world \
                    access (chmod 600)"
                        .into(),
                ));
            }
        }
        if !opened.is_file() || opened.len() > MAX_SECRETS_BYTES {
            return Err(invalid("must be a regular file of at most 64 KiB".into()));
        }
        let mut contents = String::new();
        file.take(MAX_SECRETS_BYTES)
            .read_to_string(&mut contents)
            .map_err(|error| invalid(format!("cannot be read: {error}")))?;
        toml::from_str(&contents).map_err(|error| {
            // The parser's own rendering quotes the offending line, and a type
            // error quotes the string it found; either may be a key. Report
            // where it is and what is wrong, without any quoted string. Names
            // and expected tokens are in backticks.
            let (mut reason, mut quoted, mut named) = (String::new(), false, false);
            for c in error.message().chars() {
                match c {
                    '`' if !quoted => named = !named,
                    '"' if !named => {
                        quoted = !quoted;
                        if quoted {
                            reason.push_str("\"…\"");
                        }
                        continue;
                    }
                    _ => {}
                }
                if !quoted {
                    reason.push(c);
                }
            }
            if let Some(span) = error.span() {
                let before = &contents[..span.start];
                let line = before.matches('\n').count() + 1;
                let column = before[before.rfind('\n').map_or(0, |at| at + 1)..]
                    .chars()
                    .count()
                    + 1;
                reason = format!("line {line}, column {column}: {reason}");
            }
            invalid(format!("is not valid TOML for secrets: {reason}"))
        })
    }

    /// Renders these secrets as `secrets.toml`.
    ///
    /// # Panics
    ///
    /// Never: every field is a string TOML can hold.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let rendered = toml::to_string_pretty(self).expect("serializing secrets should not fail");
        format!(
            "# Private keys of this Trellis installation, written once by `trellis-server \
            init`.\n# Only trellis-server reads this file. Keep it owner-only and back it up \
            with the\n# installation: replacing a key locks out what it protects.\n{rendered}"
        )
    }
}

/// A secret the installation needs but `secrets.toml` doesn't have.
pub(crate) fn require<'a>(
    value: Option<&'a String>,
    name: &'static str,
) -> Result<&'a str, ConfigError> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError::MissingSecret { name })
}
