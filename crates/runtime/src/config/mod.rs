use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{RuntimeMode, SubsystemName};

/// TOML runtime configuration for `trellis-server`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeConfig {
    /// Human-readable Trellis instance name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    /// Session seed used to sign Trellis-owned runtime events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_session_seed_file: Option<PathBuf>,
    /// Authorization-context digest bound into Trellis-owned runtime event proofs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_context_digest_file: Option<PathBuf>,
    /// Host path-root overrides. Relative values resolve against this config file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<RuntimePathsConfig>,
    /// HTTP listener configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpConfig>,
    /// NATS server and runtime identity configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats: Option<NatsConfig>,
    /// Browser/client connection hints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientConfig>,
    /// Runtime lease configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leases: Option<LeasesConfig>,
    /// Authentication configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
    /// OAuth provider configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthConfig>,
    /// Platform subsystem configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<SubsystemConfig>,
    /// Jobs subsystem configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs: Option<SubsystemConfig>,
    /// Health subsystem configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<SubsystemConfig>,
    /// Event log subsystem configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<SubsystemConfig>,
}

impl RuntimeConfig {
    /// Loads a runtime configuration from a TOML file on disk.
    ///
    /// This function intentionally accepts only `.toml` paths. JSONC and other
    /// legacy runtime config formats are not supported by the Rust runtime.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let defaults = RuntimePathDefaults {
            data: base_dir.to_path_buf(),
            state: base_dir.to_path_buf(),
            cache: base_dir.to_path_buf(),
            runtime: base_dir.to_path_buf(),
            logs: base_dir.join("log"),
        };
        Self::load_from_path_with_defaults(path, defaults).map(|(config, _)| config)
    }

    /// Loads a runtime configuration and applies host-selected defaults to omitted mutable paths.
    pub fn load_from_path_with_defaults(
        path: impl AsRef<Path>,
        defaults: RuntimePathDefaults,
    ) -> Result<(Self, RuntimePathDefaults), ConfigError> {
        let path = path.as_ref();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            return Err(ConfigError::UnsupportedFormat {
                path: path.to_path_buf(),
            });
        }

        let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;

        let mut config = Self::from_toml_str(&contents)?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let effective = config.resolve_path_defaults(base_dir, defaults);
        config.apply_mutable_path_defaults(&effective);
        config.resolve_relative_paths(base_dir);
        Ok((config, effective))
    }

    /// Parses a runtime configuration from TOML source text.
    pub fn from_toml_str(contents: &str) -> Result<Self, ConfigError> {
        toml::from_str(contents).map_err(ConfigError::Parse)
    }

    /// Validates that this configuration contains the sections needed by `mode`.
    ///
    pub fn validate_for_mode(&self, mode: RuntimeMode) -> Result<(), ConfigError> {
        self.validate_distinct_sqlite_paths()?;
        self.validate_oauth_provider_secrets()?;
        self.resolve_nats_runtime()?;
        self.resolve_leases()?;
        if matches!(mode, RuntimeMode::All | RuntimeMode::Platform) {
            self.resolve_nats_auth_callout()?;
        }

        for subsystem in mode.subsystems() {
            match subsystem {
                SubsystemName::Platform => {
                    self.validate_subsystem(SubsystemName::Platform, self.platform.as_ref())?
                }
                SubsystemName::Jobs => {
                    self.validate_subsystem(SubsystemName::Jobs, self.jobs.as_ref())?
                }
                SubsystemName::Health => {
                    self.validate_subsystem(SubsystemName::Health, self.health.as_ref())?
                }
                SubsystemName::Events => {
                    self.validate_subsystem(SubsystemName::Events, self.events.as_ref())?
                }
            }
        }
        if matches!(
            mode,
            RuntimeMode::All | RuntimeMode::Platform | RuntimeMode::Jobs | RuntimeMode::Events
        ) {
            self.resolve_authorization()?;
        }

        Ok(())
    }

    fn validate_distinct_sqlite_paths(&self) -> Result<(), ConfigError> {
        let path_identity = |path: &PathBuf| {
            let mut candidate = path.clone();
            for _ in 0..40 {
                if !std::fs::symlink_metadata(&candidate)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    break;
                }
                let Ok(target) = std::fs::read_link(&candidate) else {
                    break;
                };
                candidate = if target.is_absolute() {
                    target
                } else {
                    candidate
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join(target)
                };
            }
            let normalized = candidate
                .canonicalize()
                .or_else(|_| {
                    candidate
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .canonicalize()
                        .map(|parent| {
                            candidate
                                .file_name()
                                .map_or(parent.clone(), |name| parent.join(name))
                        })
                })
                .or_else(|_| std::path::absolute(&candidate))
                .unwrap_or(candidate.clone());
            #[cfg(unix)]
            let file_identity = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&candidate)
                    .ok()
                    .map(|metadata| (metadata.dev(), metadata.ino()))
            };
            #[cfg(not(unix))]
            let file_identity: Option<()> = None;
            (normalized, file_identity)
        };
        let subsystems = [
            ("platform", self.platform.as_ref()),
            ("jobs", self.jobs.as_ref()),
            ("health", self.health.as_ref()),
            ("events", self.events.as_ref()),
        ];
        for (index, (left_name, left)) in subsystems.iter().enumerate() {
            let Some(left_path) = left
                .and_then(|subsystem| subsystem.storage.as_ref())
                .filter(|storage| storage.kind.trim() == "sqlite")
                .and_then(|storage| storage.path.as_ref())
            else {
                continue;
            };
            let left_identity = path_identity(left_path);
            for (right_name, right) in &subsystems[index + 1..] {
                if right
                    .and_then(|subsystem| subsystem.storage.as_ref())
                    .filter(|storage| storage.kind.trim() == "sqlite")
                    .and_then(|storage| storage.path.as_ref())
                    .is_some_and(|right_path| {
                        let right_identity = path_identity(right_path);
                        right_identity.0 == left_identity.0
                            || left_identity
                                .1
                                .is_some_and(|identity| right_identity.1 == Some(identity))
                    })
                {
                    return Err(ConfigError::SharedSqlitePath {
                        path: left_path.clone(),
                        first: left_name,
                        second: right_name,
                    });
                }
            }
        }
        Ok(())
    }

    /// Returns the configured HTTP port, or the documented local default.
    #[must_use]
    pub fn http_port(&self) -> u16 {
        self.http
            .as_ref()
            .and_then(|http| http.port)
            .unwrap_or(3000)
    }

    /// Resolves validated storage for the platform subsystem.
    pub fn platform_storage_backend(&self) -> Result<StorageBackend, ConfigError> {
        self.subsystem_storage_backend(SubsystemName::Platform, self.platform.as_ref())
    }

    /// Resolves validated storage for the jobs subsystem.
    pub fn jobs_storage_backend(&self) -> Result<StorageBackend, ConfigError> {
        self.subsystem_storage_backend(SubsystemName::Jobs, self.jobs.as_ref())
    }

    /// Resolves validated storage for the health subsystem.
    pub fn health_storage_backend(&self) -> Result<StorageBackend, ConfigError> {
        self.subsystem_storage_backend(SubsystemName::Health, self.health.as_ref())
    }

    /// Resolves validated storage for the Events subsystem.
    pub fn events_storage_backend(&self) -> Result<StorageBackend, ConfigError> {
        self.subsystem_storage_backend(SubsystemName::Events, self.events.as_ref())
    }

    /// Resolves runtime NATS connection settings into required, non-optional values.
    pub fn resolve_nats_runtime(&self) -> Result<ResolvedRuntimeNatsConfig, ConfigError> {
        let nats = self
            .nats
            .as_ref()
            .ok_or(ConfigError::MissingSection { section: "nats" })?;
        nats.resolve_runtime()
    }

    /// Resolves runtime NATS connection settings, optionally replacing the configured
    /// server list with `servers_override` (the managed local NATS server used by
    /// `trellis-server`). Everything else resolves identically to
    /// [`RuntimeConfig::resolve_nats_runtime`].
    pub fn resolve_nats_runtime_with(
        &self,
        servers_override: Option<&str>,
    ) -> Result<ResolvedRuntimeNatsConfig, ConfigError> {
        let mut resolved = self.resolve_nats_runtime()?;
        if let Some(servers) = servers_override {
            resolved.servers = servers.to_string();
        }
        Ok(resolved)
    }

    /// Resolves NATS auth-callout seed paths into required, non-optional values.
    pub fn resolve_nats_auth_callout(&self) -> Result<ResolvedNatsAuthCalloutConfig, ConfigError> {
        let nats = self
            .nats
            .as_ref()
            .ok_or(ConfigError::MissingSection { section: "nats" })?;
        nats.resolve_auth_callout()
    }

    /// Resolves runtime lease settings with defaults applied.
    pub fn resolve_leases(&self) -> Result<ResolvedLeasesConfig, ConfigError> {
        let leases = self
            .leases
            .as_ref()
            .ok_or(ConfigError::MissingSection { section: "leases" })?;
        leases.resolve()
    }

    /// Resolves and validates authorization trust/context configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the authorization section is missing or a
    /// trust path, policy bound, bucket name, or replica count is invalid.
    pub fn resolve_authorization(&self) -> Result<&AuthorizationConfig, ConfigError> {
        let authorization = self
            .auth
            .as_ref()
            .and_then(|auth| auth.authorization.as_ref())
            .ok_or(ConfigError::MissingSection {
                section: "auth.authorization",
            })?;
        authorization.validate()?;
        Ok(authorization)
    }

    fn validate_subsystem(
        &self,
        name: SubsystemName,
        subsystem: Option<&SubsystemConfig>,
    ) -> Result<(), ConfigError> {
        let subsystem = subsystem.ok_or(ConfigError::MissingSection {
            section: name.as_str(),
        })?;
        let storage = subsystem
            .storage
            .as_ref()
            .ok_or(ConfigError::MissingSection {
                section: storage_section_name(name),
            })?;

        storage.resolve_backend(storage_section_name(name))?;

        Ok(())
    }

    fn validate_oauth_provider_secrets(&self) -> Result<(), ConfigError> {
        let Some(oauth) = &self.oauth else {
            return Ok(());
        };

        for (provider_id, provider) in &oauth.providers {
            if provider.client_secret.is_some() && provider.client_secret_file.is_some() {
                return Err(ConfigError::InvalidSecretConfig {
                    section: "oauth.providers",
                    field: "client_secret",
                    provider_id: provider_id.clone(),
                });
            }
        }

        Ok(())
    }

    fn subsystem_storage_backend(
        &self,
        name: SubsystemName,
        subsystem: Option<&SubsystemConfig>,
    ) -> Result<StorageBackend, ConfigError> {
        let subsystem = subsystem.ok_or(ConfigError::MissingSection {
            section: name.as_str(),
        })?;
        let storage = subsystem
            .storage
            .as_ref()
            .ok_or(ConfigError::MissingSection {
                section: storage_section_name(name),
            })?;
        storage.resolve_backend(storage_section_name(name))
    }

    fn resolve_relative_paths(&mut self, base_dir: &Path) {
        resolve_path(base_dir, &mut self.event_session_seed_file);
        resolve_path(base_dir, &mut self.event_context_digest_file);
        if let Some(nats) = &mut self.nats {
            if let Some(runtime) = &mut nats.runtime {
                resolve_path(base_dir, &mut runtime.auth_creds_path);
                resolve_path(base_dir, &mut runtime.trellis_creds_path);
                resolve_path(base_dir, &mut runtime.system_creds_path);
            }
            if let Some(auth_callout) = &mut nats.auth_callout {
                resolve_path(base_dir, &mut auth_callout.issuer_signing_seed_file);
                resolve_path(base_dir, &mut auth_callout.target_signing_seed_file);
                resolve_path(base_dir, &mut auth_callout.xkey_seed_file);
            }
        }
        if let Some(http) = &mut self.http {
            for source in [
                &mut http.web_source,
                &mut http.portal_source,
                &mut http.console_source,
            ] {
                if let Some(WebSourceConfig::Directory(path)) = source {
                    if path.is_relative() {
                        *path = base_dir.join(&*path);
                    }
                }
            }
        }

        for storage in [
            self.platform
                .as_mut()
                .and_then(|subsystem| subsystem.storage.as_mut()),
            self.jobs
                .as_mut()
                .and_then(|subsystem| subsystem.storage.as_mut()),
            self.health
                .as_mut()
                .and_then(|subsystem| subsystem.storage.as_mut()),
            self.events
                .as_mut()
                .and_then(|subsystem| subsystem.storage.as_mut()),
        ]
        .into_iter()
        .flatten()
        {
            resolve_path(base_dir, &mut storage.path);
        }

        if let Some(oauth) = &mut self.oauth {
            for provider in oauth.providers.values_mut() {
                resolve_path(base_dir, &mut provider.client_secret_file);
            }
        }
        if let Some(authorization) = self
            .auth
            .as_mut()
            .and_then(|auth| auth.authorization.as_mut())
        {
            resolve_required_path(base_dir, &mut authorization.issuer_signing_seed_file);
        }
    }

    fn resolve_path_defaults(
        &mut self,
        base_dir: &Path,
        defaults: RuntimePathDefaults,
    ) -> RuntimePathDefaults {
        let paths = self.paths.get_or_insert_with(RuntimePathsConfig::default);
        for path in [
            &mut paths.data,
            &mut paths.state,
            &mut paths.cache,
            &mut paths.runtime,
            &mut paths.logs,
        ] {
            resolve_path(base_dir, path);
        }
        RuntimePathDefaults {
            data: paths.data.clone().unwrap_or(defaults.data),
            state: paths.state.clone().unwrap_or(defaults.state),
            cache: paths.cache.clone().unwrap_or(defaults.cache),
            runtime: paths.runtime.clone().unwrap_or(defaults.runtime),
            logs: paths.logs.clone().unwrap_or(defaults.logs),
        }
    }

    fn apply_mutable_path_defaults(&mut self, paths: &RuntimePathDefaults) {
        if self.event_context_digest_file.is_none() {
            self.event_context_digest_file = Some(paths.state.join("event-context.digest"));
        }
        for (name, subsystem) in [
            ("platform", self.platform.as_mut()),
            ("jobs", self.jobs.as_mut()),
            ("health", self.health.as_mut()),
            ("events", self.events.as_mut()),
        ] {
            let Some(storage) = subsystem.and_then(|subsystem| subsystem.storage.as_mut()) else {
                continue;
            };
            if storage.kind.trim() == "sqlite" && storage.path.is_none() {
                storage.path = Some(paths.data.join(format!("{name}.sqlite")));
            }
        }
    }
}

/// Optional host path-root overrides from the `[paths]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimePathsConfig {
    /// Default root for subsystem databases and other durable data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<PathBuf>,
    /// Default root for mutable runtime state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<PathBuf>,
    /// Default root for downloaded and reusable cache entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<PathBuf>,
    /// Default root for process-lifetime files such as pid files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<PathBuf>,
    /// Default root for server and managed-child logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<PathBuf>,
}

/// Effective mutable path roots selected by the host profile and runtime config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePathDefaults {
    /// Root for durable subsystem data.
    pub data: PathBuf,
    /// Root for mutable runtime state.
    pub state: PathBuf,
    /// Root for reusable cache entries.
    pub cache: PathBuf,
    /// Root for process-lifetime files.
    pub runtime: PathBuf,
    /// Root for log files.
    pub logs: PathBuf,
}

/// HTTP listener configuration for the runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HttpConfig {
    /// TCP port for the HTTP server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Public browser origin for runtime URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_origin: Option<String>,
    /// Allowed browser origins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origins: Option<Vec<String>>,
    /// Insecure origins allowed for local development.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_insecure_origins: Option<Vec<String>>,
    /// Shared source for built-in web surfaces. Absent uses embedded assets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_source: Option<WebSourceConfig>,
    /// Login Portal source overriding `web_source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_source: Option<WebSourceConfig>,
    /// Console source overriding `web_source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_source: Option<WebSourceConfig>,
    /// Maximum requests per rate-limit window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_max: Option<u32>,
    /// Rate-limit window in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_window_ms: Option<u64>,
}

impl HttpConfig {
    /// Returns true when `origin` is explicitly allow-listed as an insecure origin.
    pub(crate) fn allows_insecure_origin(&self, origin: &str) -> bool {
        let origin = origin.trim_end_matches('/');
        self.allow_insecure_origins.as_ref().is_some_and(|origins| {
            origins
                .iter()
                .any(|allowed| allowed.trim_end_matches('/') == origin)
        })
    }
}

/// A filesystem or reverse-proxy source for a Trellis web surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSourceConfig {
    /// Serve a static SvelteKit artifact directory.
    Directory(PathBuf),
    /// Reverse proxy requests to an HTTP or HTTPS upstream.
    Proxy(String),
}

/// NATS configuration for runtime connections and auth callout material.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NatsConfig {
    /// NATS server URL or comma-separated URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<String>,
    /// Generated runtime credential paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<NatsRuntimeConfig>,
    /// Auth-callout signing and xkey seed paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_callout: Option<NatsAuthCalloutConfig>,
}

/// Generated NATS credential paths used by the runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NatsRuntimeConfig {
    /// Auth-account runtime user creds path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_creds_path: Option<PathBuf>,
    /// Trellis-account runtime user creds path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trellis_creds_path: Option<PathBuf>,
    /// System-account runtime user creds path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_creds_path: Option<PathBuf>,
}

/// NATS auth-callout signing and encryption material paths.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NatsAuthCalloutConfig {
    /// Auth issuer signing seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer_signing_seed_file: Option<PathBuf>,
    /// Trellis target signing seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_signing_seed_file: Option<PathBuf>,
    /// Auth-callout xkey seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xkey_seed_file: Option<PathBuf>,
}

impl NatsConfig {
    /// Resolves this raw NATS section into runtime connection settings.
    pub(crate) fn resolve_runtime(&self) -> Result<ResolvedRuntimeNatsConfig, ConfigError> {
        let runtime = self.runtime.as_ref().ok_or(ConfigError::MissingSection {
            section: "nats.runtime",
        })?;

        Ok(ResolvedRuntimeNatsConfig {
            servers: require_string("nats", "servers", self.servers.as_deref())?,
            auth_creds_path: require_path(
                "nats.runtime",
                "auth_creds_path",
                runtime.auth_creds_path.as_ref(),
            )?,
            trellis_creds_path: require_path(
                "nats.runtime",
                "trellis_creds_path",
                runtime.trellis_creds_path.as_ref(),
            )?,
            system_creds_path: require_path(
                "nats.runtime",
                "system_creds_path",
                runtime.system_creds_path.as_ref(),
            )?,
        })
    }

    /// Resolves this raw NATS section into auth-callout seed paths.
    pub(crate) fn resolve_auth_callout(
        &self,
    ) -> Result<ResolvedNatsAuthCalloutConfig, ConfigError> {
        let auth_callout = self
            .auth_callout
            .as_ref()
            .ok_or(ConfigError::MissingSection {
                section: "nats.auth_callout",
            })?;

        Ok(ResolvedNatsAuthCalloutConfig {
            issuer_signing_seed_file: require_path(
                "nats.auth_callout",
                "issuer_signing_seed_file",
                auth_callout.issuer_signing_seed_file.as_ref(),
            )?,
            target_signing_seed_file: require_path(
                "nats.auth_callout",
                "target_signing_seed_file",
                auth_callout.target_signing_seed_file.as_ref(),
            )?,
            xkey_seed_file: require_path(
                "nats.auth_callout",
                "xkey_seed_file",
                auth_callout.xkey_seed_file.as_ref(),
            )?,
        })
    }
}

/// Resolved runtime NATS credential configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRuntimeNatsConfig {
    /// NATS server URL or comma-separated URLs.
    pub servers: String,
    /// Auth-account runtime user creds path.
    pub auth_creds_path: PathBuf,
    /// Trellis-account runtime user creds path.
    pub trellis_creds_path: PathBuf,
    /// System-account runtime user creds path.
    pub system_creds_path: PathBuf,
}

/// Resolved NATS auth-callout signing and encryption material paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedNatsAuthCalloutConfig {
    /// Auth issuer signing seed file.
    pub issuer_signing_seed_file: PathBuf,
    /// Trellis target signing seed file.
    pub target_signing_seed_file: PathBuf,
    /// Auth-callout xkey seed file.
    pub xkey_seed_file: PathBuf,
}

/// Browser/client connection hints emitted by the runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClientConfig {
    /// WebSocket NATS server URLs for browser clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_nats_servers: Option<Vec<String>>,
    /// NATS server URLs for non-browser clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_servers: Option<Vec<String>>,
}

/// Runtime lease configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeasesConfig {
    /// NATS KV bucket used for runtime leases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    /// NATS KV replica count for runtime lease durability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u16>,
    /// Lease time-to-live in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// Lease renewal interval in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renew_ms: Option<u64>,
}

impl LeasesConfig {
    /// Resolves this raw lease config with defaults applied.
    pub(crate) fn resolve(&self) -> Result<ResolvedLeasesConfig, ConfigError> {
        let ttl_ms = self.ttl_ms.unwrap_or(30_000);
        let renew_ms = self.renew_ms.unwrap_or(5_000);
        if ttl_ms == 0 {
            return Err(ConfigError::InvalidLeasesConfig {
                section: "leases",
                field: "ttl_ms",
                reason: "must be greater than zero",
            });
        }
        if renew_ms == 0
            || renew_ms
                .checked_mul(3)
                .is_none_or(|interval| interval > ttl_ms)
        {
            return Err(ConfigError::InvalidLeasesConfig {
                section: "leases",
                field: "renew_ms",
                reason: "must be greater than zero and at most one third of ttl_ms",
            });
        }
        Ok(ResolvedLeasesConfig {
            bucket: self
                .bucket
                .as_deref()
                .unwrap_or("trellis_runtime_leases")
                .to_owned(),
            replicas: self.replicas.ok_or(ConfigError::InvalidLeasesConfig {
                section: "leases",
                field: "replicas",
                reason: "must be configured explicitly",
            })?,
            ttl_ms,
            renew_ms,
        })
    }
}

/// Resolved runtime lease configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLeasesConfig {
    /// NATS KV bucket used for runtime leases.
    pub bucket: String,
    /// NATS KV replica count for runtime lease durability.
    pub replicas: u16,
    /// Lease time-to-live in milliseconds.
    pub ttl_ms: u64,
    /// Lease renewal interval in milliseconds.
    pub renew_ms: u64,
}

/// Runtime authentication configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthConfig {
    /// Local username/password identity configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_identity: Option<LocalIdentityConfig>,
    /// Authorization trust and short-lived context runtime configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<AuthorizationConfig>,
}

/// File-backed authorization trust and context-runtime policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorizationConfig {
    /// Active issuer's canonical unpadded base64url Ed25519 seed file.
    pub issuer_signing_seed_file: PathBuf,
    /// Maximum newly issued context lifetime in seconds.
    pub context_lifetime_seconds: u64,
    /// Refresh lead before context expiry in seconds.
    pub refresh_lead_seconds: u64,
    /// Maximum deterministic earlier-only refresh jitter in seconds.
    pub refresh_jitter_seconds: u64,
    /// Minimum useful context lease in seconds.
    pub minimum_context_lifetime_seconds: u64,
    /// Maximum lifetime of a deny-all bootstrap JWT in seconds.
    pub maximum_bootstrap_jwt_lifetime_seconds: u64,
    /// Verification clock skew in seconds.
    pub allowed_clock_skew_seconds: u64,
    /// Maximum canonical signed-context JSON size in UTF-8 bytes.
    pub maximum_context_bytes: usize,
    /// Maximum permission atoms allowed in one context.
    pub maximum_permissions: usize,
    /// Context/revocation registry KV bucket.
    pub context_bucket: String,
    /// Replica count for both registry buckets.
    pub registry_replicas: usize,
}

impl AuthorizationConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        for (field, path) in [("issuer_signing_seed_file", &self.issuer_signing_seed_file)] {
            if path.as_os_str().is_empty() {
                return Err(invalid_authorization(field, "must not be empty"));
            }
        }
        if self.context_lifetime_seconds == 0 || self.context_lifetime_seconds > 3_600 {
            return Err(invalid_authorization(
                "context_lifetime_seconds",
                "must be between 1 and 3600",
            ));
        }
        let useful_context_lifetime = self
            .context_lifetime_seconds
            .saturating_sub(self.allowed_clock_skew_seconds);
        if self.refresh_lead_seconds >= useful_context_lifetime {
            return Err(invalid_authorization(
                "refresh_lead_seconds",
                "must be less than context_lifetime_seconds minus allowed_clock_skew_seconds",
            ));
        }
        if self.refresh_jitter_seconds > 300
            || self
                .refresh_lead_seconds
                .saturating_add(self.refresh_jitter_seconds)
                >= useful_context_lifetime
        {
            return Err(invalid_authorization(
                "refresh_jitter_seconds",
                "must not exceed 300 and must leave a usable context window",
            ));
        }
        if self.minimum_context_lifetime_seconds <= self.allowed_clock_skew_seconds
            || self.minimum_context_lifetime_seconds > useful_context_lifetime
            || self.minimum_context_lifetime_seconds
                <= self
                    .refresh_lead_seconds
                    .saturating_add(self.refresh_jitter_seconds)
        {
            return Err(invalid_authorization(
                "minimum_context_lifetime_seconds",
                "must exceed clock skew and the complete refresh window without exceeding useful lifetime",
            ));
        }
        if !(60..=86_400).contains(&self.maximum_bootstrap_jwt_lifetime_seconds) {
            return Err(invalid_authorization(
                "maximum_bootstrap_jwt_lifetime_seconds",
                "must be between 60 and 86400",
            ));
        }
        if self.allowed_clock_skew_seconds > 300 {
            return Err(invalid_authorization(
                "allowed_clock_skew_seconds",
                "must not exceed 300",
            ));
        }
        if self.maximum_context_bytes == 0 || self.maximum_context_bytes > 1_048_576 {
            return Err(invalid_authorization(
                "maximum_context_bytes",
                "must be between 1 and 1048576",
            ));
        }
        if self.maximum_permissions == 0 || self.maximum_permissions > 16_384 {
            return Err(invalid_authorization(
                "maximum_permissions",
                "must be between 1 and 16384",
            ));
        }
        if self.context_bucket.trim().is_empty() {
            return Err(invalid_authorization("context_bucket", "must not be empty"));
        }
        if self.registry_replicas == 0 {
            return Err(invalid_authorization(
                "registry_replicas",
                "must be positive",
            ));
        }
        Ok(())
    }
}

/// Local identity provider configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalIdentityConfig {
    /// Enables local identity authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Minimum local password length.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_min_length: Option<u16>,
}

/// OAuth configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OAuthConfig {
    /// Base URL for OAuth redirect callbacks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_base: Option<String>,
    /// Forces display of the provider chooser when true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always_show_provider_chooser: Option<bool>,
    /// OAuth providers keyed by provider identifier.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub providers: std::collections::BTreeMap<String, OAuthProviderConfig>,
}

/// OAuth provider configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OAuthProviderConfig {
    /// Provider type, currently expected to be `oidc`.
    #[serde(rename = "type")]
    pub provider_type: String,
    /// OIDC issuer URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// OAuth client id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Inline OAuth client secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// File-backed OAuth client secret path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret_file: Option<PathBuf>,
    /// Display name for UI surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Requested OAuth scopes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// JSON Pointers selecting provider role claims from the verified ID token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub role_claims: Vec<String>,
}

/// Configuration for a built-in runtime subsystem.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubsystemConfig {
    /// Storage configuration for the subsystem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfig>,
    /// Health history retention in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_retention_days: Option<u32>,
    /// Health transport replay retention in hours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_retention_hours: Option<u32>,
    /// Health transport maximum retained bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_max_bytes: Option<u64>,
    /// Events retention in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    /// Platform TTL settings in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<PlatformTtlConfig>,
}

/// Platform TTL settings in milliseconds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlatformTtlConfig {
    /// Session TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u64>,
    /// OAuth flow TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<u64>,
    /// Device flow TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_flow: Option<u64>,
    /// Pending auth TTL in milliseconds (default 900000, maximum 86400000,
    /// matching browser-flow record retention).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_auth: Option<u64>,
    /// Connection tracking TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections: Option<u64>,
    /// NATS JWT TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nats_jwt: Option<u64>,
}

/// Storage configuration for a built-in runtime subsystem.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageConfig {
    /// Storage backend kind. Only `sqlite` is implemented today.
    pub kind: String,
    /// SQLite database path. Required when `kind` is `sqlite`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Postgres connection URL. Reserved for the future Postgres backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// SQLite journal mode, commonly `wal`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal_mode: Option<String>,
    /// SQLite busy timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy_timeout_ms: Option<u64>,
    /// Whether the runtime should treat this SQLite store as single-writer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_writer: Option<bool>,
}

impl StorageConfig {
    /// Resolves this raw storage config into the currently implemented backend.
    pub fn resolve_backend(&self, section: &'static str) -> Result<StorageBackend, ConfigError> {
        match self.kind.trim() {
            "sqlite" => {
                let path = self
                    .path
                    .as_ref()
                    .filter(|path| !path.as_os_str().is_empty())
                    .ok_or(ConfigError::InvalidStorage {
                        section,
                        reason: "sqlite storage requires path",
                    })?;
                if self.url.is_some() {
                    return Err(ConfigError::InvalidStorage {
                        section,
                        reason: "sqlite storage must not set url",
                    });
                }
                Ok(StorageBackend::Sqlite(SqliteStorageConfig {
                    path: path.clone(),
                    journal_mode: self.journal_mode.clone(),
                    busy_timeout_ms: self.busy_timeout_ms,
                    single_writer: self.single_writer,
                }))
            }
            "postgres" => Err(ConfigError::UnsupportedStorageBackend {
                section,
                backend: "postgres",
            }),
            "" => Err(ConfigError::InvalidStorage {
                section,
                reason: "kind must not be empty",
            }),
            _ => Err(ConfigError::InvalidStorage {
                section,
                reason: "kind must be sqlite",
            }),
        }
    }
}

/// Resolved storage backend for a subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageBackend {
    /// SQLite storage backend.
    Sqlite(SqliteStorageConfig),
}

/// Resolved SQLite storage configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteStorageConfig {
    /// SQLite database path.
    pub path: PathBuf,
    /// SQLite journal mode, commonly `wal`.
    pub journal_mode: Option<String>,
    /// SQLite busy timeout in milliseconds.
    pub busy_timeout_ms: Option<u64>,
    /// Whether the runtime should treat this SQLite store as single-writer.
    pub single_writer: Option<bool>,
}

/// Error returned while loading or validating runtime configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config path does not use the TOML extension.
    #[error("runtime config must be a TOML file: {path}")]
    UnsupportedFormat {
        /// Path that used an unsupported extension.
        path: std::path::PathBuf,
    },
    /// The config file could not be read.
    #[error("failed to read runtime config {path}: {source}")]
    Read {
        /// Path that could not be read.
        path: std::path::PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The config file was not valid TOML for the runtime config schema.
    #[error("failed to parse runtime config TOML: {0}")]
    Parse(#[source] toml::de::Error),
    /// A required config section is missing for the selected runtime mode.
    #[error("missing required runtime config section [{section}]")]
    MissingSection {
        /// Missing section name.
        section: &'static str,
    },
    /// A subsystem storage section is present but invalid.
    #[error("invalid runtime config section [{section}]: {reason}")]
    InvalidStorage {
        /// Invalid section name.
        section: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },
    /// Two subsystems were configured to write the same SQLite file.
    #[error("runtime subsystems [{first}] and [{second}] must use separate SQLite files, but both use {path}")]
    SharedSqlitePath {
        /// Duplicated SQLite path.
        path: PathBuf,
        /// First subsystem using the path.
        first: &'static str,
        /// Second subsystem using the path.
        second: &'static str,
    },
    /// A known storage backend is planned but not implemented yet.
    #[error("storage backend '{backend}' is not supported yet for [{section}]")]
    UnsupportedStorageBackend {
        /// Storage section containing the unsupported backend.
        section: &'static str,
        /// Unsupported backend name.
        backend: &'static str,
    },
    /// Inline and file-backed secret fields were both provided.
    #[error("invalid runtime config section [{section}.{provider_id}]: {field} and {field}_file are mutually exclusive")]
    InvalidSecretConfig {
        /// Section containing the invalid secret configuration.
        section: &'static str,
        /// Provider id containing the invalid secret configuration.
        provider_id: String,
        /// Inline secret field that conflicts with the file-backed form.
        field: &'static str,
    },
    /// A required NATS path field is missing or empty.
    #[error("invalid runtime config section [{section}]: {field} {reason}")]
    InvalidNatsConfig {
        /// Invalid section name.
        section: &'static str,
        /// Invalid field name.
        field: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },
    /// A required lease field is missing or empty.
    #[error("invalid runtime config section [{section}]: {field} {reason}")]
    InvalidLeasesConfig {
        /// Invalid section name.
        section: &'static str,
        /// Invalid field name.
        field: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },
    /// Authorization trust/context runtime configuration is invalid.
    #[error("invalid runtime config section [auth.authorization]: {field} {reason}")]
    InvalidAuthorizationConfig {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },
}

fn resolve_path(base_dir: &Path, path: &mut Option<PathBuf>) {
    let Some(value) = path else {
        return;
    };
    if value.is_relative() {
        *value = base_dir.join(&value);
    }
}

fn resolve_required_path(base_dir: &Path, path: &mut PathBuf) {
    if path.is_relative() {
        *path = base_dir.join(&*path);
    }
}

fn invalid_authorization(field: &'static str, reason: &'static str) -> ConfigError {
    ConfigError::InvalidAuthorizationConfig { field, reason }
}

fn storage_section_name(subsystem: SubsystemName) -> &'static str {
    match subsystem {
        SubsystemName::Platform => "platform.storage",
        SubsystemName::Jobs => "jobs.storage",
        SubsystemName::Health => "health.storage",
        SubsystemName::Events => "events.storage",
    }
}

fn require_path(
    section: &'static str,
    field: &'static str,
    path: Option<&PathBuf>,
) -> Result<PathBuf, ConfigError> {
    path.filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .ok_or(ConfigError::InvalidNatsConfig {
            section,
            field,
            reason: "must not be missing or empty",
        })
}

fn require_string(
    section: &'static str,
    field: &'static str,
    value: Option<&str>,
) -> Result<String, ConfigError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(ConfigError::InvalidNatsConfig {
            section,
            field,
            reason: "must not be missing or empty",
        })
}

#[cfg(test)]
mod tests;
