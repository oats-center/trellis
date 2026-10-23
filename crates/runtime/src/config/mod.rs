use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};

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
    /// Native identity seed files for built-in live providers, keyed by role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_provider_seed_files: Option<LiveProviderSeedFilesConfig>,
    /// Host path-root overrides. Relative values resolve against this config file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<RuntimePathsConfig>,
    /// HTTP listener configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpConfig>,
    /// Installation TLS metadata shared by every runtime listener.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
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
        self.validate_http_listener()?;
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
        if matches!(mode, RuntimeMode::All | RuntimeMode::Platform) {
            self.resolve_oauth_listener()?;
        }
        if matches!(mode, RuntimeMode::All | RuntimeMode::Platform) {
            self.resolve_authorization()?;
        }

        Ok(())
    }

    /// Resolves the dedicated authorization listener and checks installation identity.
    pub fn resolve_oauth_listener(&self) -> Result<&OAuthListenerConfig, ConfigError> {
        let auth = self
            .auth
            .as_ref()
            .ok_or(ConfigError::MissingSection { section: "auth" })?;
        let instance = auth
            .instance_id
            .as_deref()
            .ok_or(ConfigError::MissingSection {
                section: "auth.instance_id",
            })?;
        if ulid::Ulid::from_string(instance).is_err()
            || instance.len() != 26
            || instance != instance.to_ascii_uppercase()
        {
            return Err(invalid_authorization(
                "instance_id",
                "must be a canonical installation ULID provisioned at initialization",
            ));
        }
        if auth
            .oauth_storage_key_file
            .as_ref()
            .is_none_or(|path| path.as_os_str().is_empty())
        {
            return Err(invalid_authorization(
                "oauth_storage_key_file",
                "must identify the existing installation-owned OAuth key file",
            ));
        }
        let oauth = auth.oauth.as_ref().ok_or(ConfigError::MissingSection {
            section: "auth.oauth",
        })?;
        let origin = Url::parse(&oauth.origin)
            .map_err(|_| invalid_authorization("oauth.origin", "must be an absolute origin"))?;
        if origin.host().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || oauth.port == 0
        {
            return Err(invalid_authorization(
                "oauth",
                "requires a nonzero listener port and an origin without credentials, \
                path, query, or fragment",
            ));
        }
        validate_listener_transport(&origin, oauth.bind_address, &oauth.transport)
            .map_err(|reason| invalid_authorization("oauth.transport", reason))?;
        let console_origin = self.public_origin();
        for other in std::iter::once(&console_origin).chain(
            self.http
                .iter()
                .flat_map(|http| http.origins.iter().flatten()),
        ) {
            let other = Url::parse(other).map_err(|_| {
                invalid_authorization(
                    "oauth.origin",
                    "configured application origins must be valid URLs",
                )
            })?;
            if other.origin() == origin.origin() {
                return Err(invalid_authorization(
                    "oauth.origin",
                    "must differ from every configured application origin",
                ));
            }
            warn_oauth_cookie_isolation(&origin, &other);
        }
        if let Some(servers) = self.nats.as_ref().and_then(|nats| nats.servers.as_deref()) {
            for server in servers.split(',') {
                let endpoint = Url::parse(server.trim()).map_err(|_| {
                    invalid_authorization("nats.servers", "must contain absolute NATS endpoints")
                })?;
                let loopback = match endpoint.host() {
                    Some(Host::Domain(host)) => {
                        host == "localhost"
                            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
                    }
                    Some(Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(Host::Ipv6(ip)) => ip.is_loopback(),
                    _ => false,
                };
                if endpoint.host_str().is_none()
                    || !endpoint.username().is_empty()
                    || endpoint.password().is_some()
                    || !matches!(endpoint.path(), "" | "/")
                    || endpoint.query().is_some()
                    || endpoint.fragment().is_some()
                    || !(matches!(endpoint.scheme(), "tls" | "wss")
                        || (matches!(oauth.transport, ListenerTransport::LoopbackDevelopment)
                            && loopback
                            && matches!(endpoint.scheme(), "nats" | "ws")))
                {
                    return Err(invalid_authorization(
                        "nats.servers",
                        "requires TLS; plaintext is restricted to explicit loopback development",
                    ));
                }
            }
        }
        Ok(oauth)
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

    /// Returns the configured HTTP bind address.
    ///
    /// An omitted address binds loopback for plaintext development and the
    /// unspecified IPv4 address for TLS or proxy transport.
    #[must_use]
    pub fn http_bind_address(&self) -> IpAddr {
        self.http
            .as_ref()
            .and_then(|http| http.bind_address)
            .unwrap_or(match self.http_transport() {
                ListenerTransport::LoopbackDevelopment => IpAddr::V4(Ipv4Addr::LOCALHOST),
                _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            })
    }

    /// Returns the application listener transport; omission selects loopback development.
    #[must_use]
    pub fn http_transport(&self) -> &ListenerTransport {
        self.http
            .as_ref()
            .and_then(|http| http.transport.as_ref())
            .unwrap_or(&ListenerTransport::LoopbackDevelopment)
    }

    /// Resolves the canonical public browser origin, defaulting to loopback HTTP.
    #[must_use]
    pub fn public_origin(&self) -> String {
        self.http
            .as_ref()
            .and_then(|http| http.public_origin.as_deref())
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("http://localhost:{}", self.http_port()))
    }

    /// Validates that the application and OAuth listener transports can protect
    /// their public origins and that broker endpoints use TLS outside loopback
    /// development, without reading any referenced file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] naming the listener or endpoint that is unsafe.
    pub fn validate_listeners(&self) -> Result<(), ConfigError> {
        self.validate_http_listener()?;
        self.resolve_oauth_listener().map(|_| ())
    }

    /// Rejects an application listener whose transport cannot protect its public origin.
    fn validate_http_listener(&self) -> Result<(), ConfigError> {
        let public_origin = self.public_origin();
        let origin = Url::parse(&public_origin)
            .ok()
            .filter(|url| CanonicalOrigin::parse(url.as_str()).is_some())
            .ok_or_else(|| ConfigError::InvalidPublicOrigin {
                origin: public_origin.clone(),
            })?;
        validate_listener_transport(&origin, self.http_bind_address(), self.http_transport())
            .map_err(|reason| ConfigError::InvalidHttpTransport {
                origin: public_origin,
                reason,
            })
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
        if let Some(seeds) = &mut self.live_provider_seed_files {
            resolve_path(base_dir, &mut seeds.platform);
            resolve_path(base_dir, &mut seeds.health);
            resolve_path(base_dir, &mut seeds.jobs);
            resolve_path(base_dir, &mut seeds.events);
        }
        if let Some(tls) = &mut self.tls {
            resolve_path(base_dir, &mut tls.generated_ca_certificate);
        }
        if let Some(nats) = &mut self.nats {
            resolve_path(base_dir, &mut nats.ca_certificate);
            if let Some(runtime) = &mut nats.runtime {
                resolve_path(base_dir, &mut runtime.auth_creds_path);
                resolve_path(base_dir, &mut runtime.trellis_creds_path);
                resolve_path(base_dir, &mut runtime.system_creds_path);
            }
            if let Some(auth_callout) = &mut nats.auth_callout {
                resolve_path(base_dir, &mut auth_callout.sentinel_public_key_file);
                resolve_path(base_dir, &mut auth_callout.issuer_signing_seed_file);
                resolve_path(base_dir, &mut auth_callout.target_signing_seed_file);
                resolve_path(base_dir, &mut auth_callout.xkey_seed_file);
            }
        }
        if let Some(ListenerTransport::Tls {
            certificate_chain,
            private_key,
        }) = self.http.as_mut().and_then(|http| http.transport.as_mut())
        {
            resolve_required_path(base_dir, certificate_chain);
            resolve_required_path(base_dir, private_key);
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

        if let Some(auth) = &mut self.auth {
            resolve_path(base_dir, &mut auth.oauth_storage_key_file);
            if let Some(OAuthListenerConfig {
                transport:
                    ListenerTransport::Tls {
                        certificate_chain,
                        private_key,
                    },
                ..
            }) = &mut auth.oauth
            {
                resolve_required_path(base_dir, certificate_chain);
                resolve_required_path(base_dir, private_key);
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

/// Origins isolate JavaScript, but cookies are hostname-scoped, not port-scoped.
pub(crate) fn warn_oauth_cookie_isolation(oauth: &Url, application: &Url) {
    if oauth.host_str() == application.host_str() && oauth.origin() != application.origin() {
        tracing::warn!(
            security_event = "oauth_cookie_isolation_at_risk",
            oauth_origin = %oauth.origin().ascii_serialization(),
            application_origin = %application.origin().ascii_serialization(),
            "SECURITY WARNING: OAuth login cookies can be sent to application \
            servers on this shared hostname. Different ports and HttpOnly do NOT \
            isolate these cookies. A compromised application server could steal a \
            portal session. Use a dedicated OAuth hostname."
        );
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

impl RuntimePathDefaults {
    /// Resolve Linux system/FHS mutable roots for server and offline administration.
    ///
    /// # Errors
    /// Fails on platforms where the system host profile is unsupported.
    pub fn system_profile() -> Result<Self, std::io::Error> {
        #[cfg(not(target_os = "linux"))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "--system is supported only on Linux",
        ));
        #[cfg(target_os = "linux")]
        Ok(Self {
            data: "/var/lib/trellis".into(),
            state: "/var/lib/trellis".into(),
            cache: "/var/cache/trellis".into(),
            runtime: "/run/trellis".into(),
            logs: "/var/log/trellis".into(),
        })
    }

    /// Resolve the ordinary user host's mutable roots for both server startup
    /// and offline administration, using the same XDG environment policy.
    ///
    /// # Errors
    /// Fails when neither HOME nor USERPROFILE identifies the user profile.
    pub fn user_profile(
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, std::io::Error> {
        let home = get("HOME")
            .or_else(|| get("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot resolve user profile: HOME is not set",
                )
            })?;
        let root = |variable: &str, fallback: &str| {
            get(variable)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(fallback))
                .join("trellis")
        };
        let state = root("XDG_STATE_HOME", ".local/state");
        let runtime = get("XDG_RUNTIME_DIR")
            .map(|value| PathBuf::from(value).join("trellis"))
            .unwrap_or_else(|| {
                #[cfg(unix)]
                // SAFETY: geteuid has no memory preconditions and reads only this process's
                // identity.
                let user = unsafe { libc::geteuid() }.to_string();
                #[cfg(not(unix))]
                let user = get("USERNAME")
                    .unwrap_or_else(|| "user".into())
                    .to_string_lossy()
                    .into_owned();
                std::env::temp_dir()
                    .join(format!("trellis-runtime-{user}"))
                    .join("trellis")
            });
        Ok(Self {
            data: root("XDG_DATA_HOME", ".local/share"),
            state: state.clone(),
            cache: root("XDG_CACHE_HOME", ".cache"),
            runtime,
            logs: state.join("log"),
        })
    }
}

/// HTTP listener configuration for the runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HttpConfig {
    /// TCP port for the HTTP server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Address the HTTP listener binds to. Absent binds the unspecified IPv4 address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<IpAddr>,
    /// Public browser origin for runtime URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_origin: Option<String>,
    /// Allowed browser origins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origins: Option<Vec<String>>,
    /// Listener transport. Absent selects loopback-only plaintext development.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<ListenerTransport>,
}

/// A canonical `scheme://host[:port]` browser origin.
struct CanonicalOrigin;

impl CanonicalOrigin {
    /// Accepts only an HTTP(S) origin without credentials, query, or fragment.
    fn parse(value: &str) -> Option<Self> {
        let url = Url::parse(value.trim()).ok()?;
        (matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none())
        .then_some(Self)
    }
}

/// NATS configuration for runtime connections and auth callout material.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NatsConfig {
    /// NATS server URL or comma-separated URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<String>,
    /// Additional PEM CA trusted for runtime NATS TLS connections.
    ///
    /// Required when the broker certificate chains to a private CA, including the
    /// Trellis-generated installation CA. Publicly trusted brokers omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_certificate: Option<PathBuf>,
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
    /// Server-only identity of the non-expiring default sentinel.
    pub sentinel_public_key_file: Option<PathBuf>,
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
            ca_certificate: self.ca_certificate.clone(),
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
            sentinel_public_key_file: require_path(
                "nats.auth_callout",
                "sentinel_public_key_file",
                auth_callout.sentinel_public_key_file.as_ref(),
            )?,
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
    /// Additional PEM CA trusted for NATS TLS connections.
    pub ca_certificate: Option<PathBuf>,
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
    /// Server-only sentinel public key file.
    pub sentinel_public_key_file: PathBuf,
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
    /// Optional external profile-image resolution policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<AvatarConfig>,
    /// Stable installation ULID, provisioned once and retained across key rotations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Dedicated OAuth listener, distinct from application origins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthListenerConfig>,
    /// Installation-owned base64url 32-byte key sealing durable OAuth credentials.
    /// Provision once; retain across restarts and issuer rotations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_storage_key_file: Option<PathBuf>,
    /// Local username/password identity configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_identity: Option<LocalIdentityConfig>,
    /// Authorization trust and short-lived context runtime configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<AuthorizationConfig>,
    /// Safeguards on delegating administrative capabilities to apps.
    #[serde(default)]
    pub administration: AdministrationConfig,
}

/// Safeguards on delegating `trellis.*` administrative capabilities to an app.
/// Every safeguard is on unless the operator turns it off.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdministrationConfig {
    /// Consent asks for a separate confirmation that the app may manage this
    /// Trellis installation.
    pub confirm_on_consent: bool,
    /// Another holder of `trellis.managePlatform` must approve the delegation
    /// before the app receives it.
    pub require_peer_approval: bool,
    /// Only OAuth clients an administrator registered may receive
    /// administrative capabilities.
    pub registered_clients_only: bool,
}

impl Default for AdministrationConfig {
    fn default() -> Self {
        Self {
            confirm_on_consent: true,
            require_peer_approval: true,
            registered_clients_only: true,
        }
    }
}

/// External image policy. Disabling immediately prevents serving remote-derived
/// images, including already cached ones. Local initials remain available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AvatarConfig {
    /// Fetch public HTTPS OIDC, owner-override and verified-email Gravatar images.
    pub external_fetch_enabled: bool,
}

/// Explicit dedicated OAuth listener configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthListenerConfig {
    /// Public authorization origin, distinct from every application origin.
    pub origin: String,
    /// Dedicated authorization listener address.
    pub bind_address: IpAddr,
    /// Dedicated listener port.
    pub port: u16,
    /// TLS, reverse-proxy, or explicitly insecure loopback-only development transport.
    pub transport: ListenerTransport,
    /// Reverse-proxy addresses or CIDR ranges whose `X-Forwarded-For` header
    /// identifies the client for request admission. Forwarding headers from
    /// any other peer are ignored. Empty means the listener faces clients.
    ///
    /// A `proxy` transport already serves only its own trusted proxies and
    /// presents the forwarded client; this list is for a proxy in front of a
    /// TLS listener, such as one that re-encrypts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_proxies: Vec<ipnet::IpNet>,
}

/// Transport for a runtime HTTP listener; failed TLS never becomes plaintext.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ListenerTransport {
    /// HTTPS using PEM certificate and key files, generated or operator-supplied.
    ///
    /// The runtime reloads replaced files without restarting.
    Tls {
        /// PEM leaf certificate followed by intermediates.
        certificate_chain: PathBuf,
        /// PEM private key matching the certificate.
        private_key: PathBuf,
    },
    /// Plaintext behind an HTTPS-terminating reverse proxy.
    ///
    /// The public origin remains HTTPS. Only peers inside `trusted_proxies` are
    /// served, and only their `X-Forwarded-For` entries identify the client.
    Proxy {
        /// Networks of the reverse proxies allowed to connect.
        trusted_proxies: Vec<ipnet::IpNet>,
    },
    /// Explicitly insecure development mode restricted to loopback.
    LoopbackDevelopment,
}

/// Checks that `transport` can protect `origin` when bound at `bind_address`.
pub(crate) fn validate_listener_transport(
    origin: &Url,
    bind_address: IpAddr,
    transport: &ListenerTransport,
) -> Result<(), &'static str> {
    match transport {
        ListenerTransport::Tls {
            certificate_chain,
            private_key,
        } => {
            if origin.scheme() != "https"
                || certificate_chain.as_os_str().is_empty()
                || private_key.as_os_str().is_empty()
            {
                return Err("tls requires an HTTPS origin and certificate/key paths");
            }
        }
        ListenerTransport::Proxy { trusted_proxies } => {
            if origin.scheme() != "https" || trusted_proxies.is_empty() {
                return Err(
                    "proxy requires an HTTPS public origin and at least one trusted proxy network",
                );
            }
        }
        ListenerTransport::LoopbackDevelopment => {
            let local = match origin.host() {
                Some(Host::Ipv4(ip)) => ip.is_loopback(),
                Some(Host::Ipv6(ip)) => ip.is_loopback(),
                Some(Host::Domain(host)) => host == "localhost",
                None => false,
            };
            if origin.scheme() != "http" || !local || !bind_address.is_loopback() {
                return Err(
                    "plaintext is restricted to loopback development: use an HTTP loopback \
                    origin and a loopback bind address, or select tls or proxy transport",
                );
            }
        }
    }
    Ok(())
}

/// Installation TLS metadata.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate of the Trellis-generated installation CA.
    ///
    /// Present only when Trellis generated the serving certificates. The runtime
    /// then warns operators and every Console user, and reports the CA
    /// fingerprint and leaf expiry so trust can be verified out of band.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_ca_certificate: Option<PathBuf>,
}

/// Native identity seed files for built-in live providers.
///
/// These files hold long-lived provisioned native identity seeds, not
/// per-observation records. The runtime derives each provider's normal
/// provisioned identity and materialized authority from them; a connected
/// process still receives a fresh runtime signing key and logical connection id
/// through the ordinary bootstrap path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LiveProviderSeedFilesConfig {
    /// Platform Operation provider identity seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<PathBuf>,
    /// Health API live provider identity seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<PathBuf>,
    /// Jobs API live provider identity seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs: Option<PathBuf>,
    /// Events API live provider identity seed file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<PathBuf>,
}

/// File-backed authorization trust and context-runtime policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorizationConfig {
    /// Active issuer's canonical unpadded base64url Ed25519 seed file.
    pub issuer_signing_seed_file: PathBuf,
    /// Maximum newly issued context lifetime in seconds.
    pub context_lifetime_seconds: u64,
    /// Verification clock skew in seconds.
    pub allowed_clock_skew_seconds: u64,
    /// Maximum signed OAuth admission lifetime in seconds.
    #[serde(default = "default_oauth_admission_lifetime_seconds")]
    pub oauth_admission_lifetime_seconds: u64,
}

fn default_oauth_admission_lifetime_seconds() -> u64 {
    300
}

impl AuthorizationConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.issuer_signing_seed_file.as_os_str().is_empty() {
            return Err(invalid_authorization(
                "issuer_signing_seed_file",
                "must not be empty",
            ));
        }
        if self.context_lifetime_seconds == 0 || self.context_lifetime_seconds > 3_600 {
            return Err(invalid_authorization(
                "context_lifetime_seconds",
                "must be between 1 and 3600",
            ));
        }
        if self.allowed_clock_skew_seconds > 300
            || self.allowed_clock_skew_seconds >= self.context_lifetime_seconds
        {
            return Err(invalid_authorization(
                "allowed_clock_skew_seconds",
                "must not exceed 300 and must be less than context_lifetime_seconds",
            ));
        }
        if !(1..=300).contains(&self.oauth_admission_lifetime_seconds) {
            return Err(invalid_authorization(
                "oauth_admission_lifetime_seconds",
                "must be between 1 and 300",
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
    #[error(
        "runtime subsystems [{first}] and [{second}] must use separate SQLite \
        files, but both use {path}"
    )]
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
    /// The configured HTTP public origin is not a valid absolute HTTP(S) origin.
    #[error(
        "invalid runtime config [http] public_origin '{origin}': must be an \
        absolute HTTP(S) origin"
    )]
    InvalidPublicOrigin {
        /// Offending public origin value.
        origin: String,
    },
    /// The application listener transport cannot protect its public origin.
    #[error("invalid runtime config [http] transport for public origin '{origin}': {reason}")]
    InvalidHttpTransport {
        /// Configured public origin.
        origin: String,
        /// Why the transport was rejected.
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
