use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;

use super::{
    ConfigError, RuntimeConfig, RuntimePathDefaults, SqliteStorageConfig, StorageBackend,
    WebSourceConfig,
};
use crate::RuntimeMode;

const COMPLETE_CONFIG: &str = r#"
instance_name = "Trellis"
event_session_seed_file = "./session.seed"

[http]
port = 39123

[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"
xkey_seed_file = "./nats/auth-callout-xkey.seed"

[auth.authorization]
issuer_signing_seed_file = "./auth/authorization-issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[health.storage]
kind = "sqlite"
path = "./data/health.sqlite"

[events.storage]
kind = "sqlite"
path = "./data/events.sqlite"

[leases]
replicas = 1
"#;

#[test]
fn loads_toml_config_from_path() {
    let directory = tempdir().expect("create temp directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, COMPLETE_CONFIG).expect("write config");

    let config = RuntimeConfig::load_from_path(&path).expect("load config");

    assert_eq!(config.instance_name.as_deref(), Some("Trellis"));
    assert_eq!(
        config.event_session_seed_file,
        Some(directory.path().join("./session.seed"))
    );
    assert_eq!(config.http_port(), 39123);
    assert_eq!(
        config
            .platform
            .as_ref()
            .and_then(|platform| platform.storage.as_ref())
            .map(|storage| storage.kind.as_str()),
        Some("sqlite")
    );
    assert_eq!(
        config.platform_storage_backend().expect("platform storage"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: directory.path().join("./data/platform.sqlite"),
            journal_mode: Some("wal".to_owned()),
            busy_timeout_ms: Some(5000),
            single_writer: Some(true),
        })
    );
}

fn path_defaults(root: &std::path::Path) -> RuntimePathDefaults {
    RuntimePathDefaults {
        data: root.join("default-data"),
        state: root.join("default-state"),
        cache: root.join("default-cache"),
        runtime: root.join("default-runtime"),
        logs: root.join("default-logs"),
    }
}

#[test]
fn resolves_every_path_root_override_against_profile_or_config() {
    type Getter = fn(&RuntimePathDefaults) -> &std::path::Path;
    let directory = tempdir().expect("create temp directory");
    let config = directory.path().join("config.toml");
    let defaults = path_defaults(directory.path());
    let fields: [(&str, Getter); 5] = [
        ("data", |paths| &paths.data),
        ("state", |paths| &paths.state),
        ("cache", |paths| &paths.cache),
        ("runtime", |paths| &paths.runtime),
        ("logs", |paths| &paths.logs),
    ];

    fs::write(&config, "").expect("write omitted config");
    let (_, omitted) = RuntimeConfig::load_from_path_with_defaults(&config, defaults.clone())
        .expect("load omitted paths");
    assert_eq!(omitted, defaults);

    for (field, get) in fields {
        fs::write(
            &config,
            format!("[paths]\n{field} = \"relative/{field}\"\n"),
        )
        .expect("write relative config");
        let (_, relative) = RuntimeConfig::load_from_path_with_defaults(&config, defaults.clone())
            .expect("load relative path");
        assert_eq!(
            get(&relative),
            directory.path().join("relative").join(field)
        );

        let absolute = directory.path().join("absolute").join(field);
        fs::write(
            &config,
            format!("[paths]\n{field} = {:?}\n", absolute.display().to_string()),
        )
        .expect("write absolute config");
        let (_, resolved) = RuntimeConfig::load_from_path_with_defaults(&config, defaults.clone())
            .expect("load absolute path");
        assert_eq!(get(&resolved), absolute);
    }
}

#[test]
fn resolves_only_directory_web_sources_against_config() {
    let directory = tempdir().expect("create temp directory");
    let config_path = directory.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
[http]
web_source = { directory = "./web" }
portal_source = { proxy = "http://127.0.0.1:5173" }
console_source = { directory = "./console" }
"#,
    )
    .expect("write web source config");

    let (config, _) =
        RuntimeConfig::load_from_path_with_defaults(&config_path, path_defaults(directory.path()))
            .expect("load web source config");
    let http = config.http.expect("HTTP config");
    assert_eq!(
        http.web_source,
        Some(WebSourceConfig::Directory(directory.path().join("web")))
    );
    assert_eq!(
        http.portal_source,
        Some(WebSourceConfig::Proxy("http://127.0.0.1:5173".to_owned()))
    );
    assert_eq!(
        http.console_source,
        Some(WebSourceConfig::Directory(directory.path().join("console")))
    );
}

#[test]
fn sqlite_paths_prefer_resource_then_data_root_then_profile() {
    let directory = tempdir().expect("create temp directory");
    let config_path = directory.path().join("config.toml");
    let absolute_jobs = directory.path().join("absolute-jobs.sqlite");
    fs::write(
        &config_path,
        format!(
            r#"
[paths]
data = "./relocated"

[platform.storage]
kind = "sqlite"

[jobs.storage]
kind = "sqlite"
path = {:?}

[health.storage]
kind = "sqlite"
path = "./individual-health.sqlite"

[events.storage]
kind = "sqlite"
"#,
            absolute_jobs.display().to_string()
        ),
    )
    .expect("write config");

    let (config, paths) =
        RuntimeConfig::load_from_path_with_defaults(&config_path, path_defaults(directory.path()))
            .expect("load config");
    assert_eq!(paths.data, directory.path().join("relocated"));
    assert_eq!(
        config.platform_storage_backend().expect("platform"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: paths.data.join("platform.sqlite"),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
    assert_eq!(
        config.jobs_storage_backend().expect("jobs"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: absolute_jobs,
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
    assert_eq!(
        config.health_storage_backend().expect("health"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: directory.path().join("individual-health.sqlite"),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
    assert_eq!(
        config.events_storage_backend().expect("events"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: paths.data.join("events.sqlite"),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
}

#[test]
fn rejects_shared_sqlite_paths_across_subsystems() {
    let source = COMPLETE_CONFIG.replace("./data/health.sqlite", "./data/platform.sqlite");
    let config = RuntimeConfig::from_toml_str(&source).expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath {
            first: "platform",
            second: "health",
            ..
        })
    ));

    let source = COMPLETE_CONFIG.replace("./data/health.sqlite", "./data/./platform.sqlite");
    let config = RuntimeConfig::from_toml_str(&source).expect("parse equivalent path config");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath { .. })
    ));
}

#[test]
#[cfg(unix)]
fn rejects_shared_sqlite_inode_and_dangling_symlink_aliases() {
    let temp = tempfile::tempdir().expect("create config path tempdir");
    let target = temp.path().join("platform.sqlite");
    let alias = temp.path().join("alias.sqlite");
    std::os::unix::fs::symlink("platform.sqlite", &alias).expect("create dangling symlink");

    let source = COMPLETE_CONFIG
        .replace(
            "./data/platform.sqlite",
            target.to_str().expect("UTF-8 target path"),
        )
        .replace(
            "./data/health.sqlite",
            alias.to_str().expect("UTF-8 alias path"),
        );
    let config = RuntimeConfig::from_toml_str(&source).expect("parse symlink config");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath { .. })
    ));

    let second_alias = temp.path().join("second-alias.sqlite");
    std::os::unix::fs::symlink("alias.sqlite", &second_alias)
        .expect("create second dangling symlink");
    let source_with_chain = source.replace(
        alias.to_str().expect("UTF-8 alias path"),
        second_alias.to_str().expect("UTF-8 second alias path"),
    );
    let config = RuntimeConfig::from_toml_str(&source_with_chain).expect("parse symlink chain");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath { .. })
    ));
    std::fs::remove_file(second_alias).expect("remove second symlink");

    std::fs::remove_file(&alias).expect("remove symlink");
    std::fs::write(&target, []).expect("create target");
    std::fs::hard_link(&target, &alias).expect("create hard link");
    let config = RuntimeConfig::from_toml_str(&source).expect("parse hard-link config");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath { .. })
    ));
}

#[test]
fn accepts_unknown_runtime_config_fields() {
    let directory = tempdir().expect("create temp directory");
    let config = directory.path().join("config.toml");
    fs::write(
        &config,
        r#"
future_root_option = true

[http]
port = 39123
future_http_option = "ignored"

[leases]
replicas = 1
future_lease_option = { enabled = true }
"#,
    )
    .expect("write config with unknown keys");
    let (parsed, _) =
        RuntimeConfig::load_from_path_with_defaults(&config, path_defaults(directory.path()))
            .expect("unknown keys are tolerated");
    assert_eq!(parsed.http.as_ref().and_then(|h| h.port), Some(39123));
    assert_eq!(parsed.leases.as_ref().and_then(|l| l.replicas), Some(1));
}

#[test]
fn parses_plan_shaped_toml_config() {
    let config = RuntimeConfig::from_toml_str(
        r#"
instance_name = "Trellis"
event_session_seed_file = "./session.seed"

[http]
port = 3000
public_origin = "http://localhost:3000"
origins = ["http://localhost:3000"]
allow_insecure_origins = ["http://localhost:3000"]
rate_limit_max = 60
rate_limit_window_ms = 60000

[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"
xkey_seed_file = "./nats/auth-callout-xkey.seed"

[client]
ws_nats_servers = ["ws://localhost:8080"]
nats_servers = ["nats://127.0.0.1:4222"]

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[health]
history_retention_days = 30

[health.storage]
kind = "sqlite"
path = "./data/health.sqlite"

[events]
retention_days = 7

[events.storage]
kind = "sqlite"
path = "./data/events.sqlite"

[leases]
bucket = "trellis_runtime_leases"
replicas = 1
ttl_ms = 15000
renew_ms = 5000

[auth.local_identity]
enabled = true
password_min_length = 8

[auth.authorization]
issuer_signing_seed_file = "./auth/authorization-issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[oauth]
redirect_base = "http://localhost:3000/auth/callback"
always_show_provider_chooser = false

[oauth.providers.google]
type = "oidc"
issuer = "https://accounts.google.com"
client_id = "client-id"
client_secret_file = "./secrets/google-client-secret"
display_name = "Google"
scopes = ["openid", "profile", "email"]

[platform.ttl_ms]
sessions = 86400000
oauth = 300000
device_flow = 1800000
pending_auth = 300000
connections = 7200000
nats_jwt = 3600000
"#,
    )
    .expect("parse config");

    config
        .validate_for_mode(RuntimeMode::All)
        .expect("valid config");
    assert_eq!(
        config
            .health
            .as_ref()
            .and_then(|health| health.history_retention_days),
        Some(30)
    );
    assert_eq!(
        config
            .events
            .as_ref()
            .and_then(|events| events.retention_days),
        Some(7)
    );
    assert_eq!(
        config.leases.as_ref().and_then(|leases| leases.ttl_ms),
        Some(15000)
    );
    assert_eq!(
        config.leases.as_ref().and_then(|leases| leases.replicas),
        Some(1)
    );
    assert_eq!(
        config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.providers.get("google"))
            .and_then(|google| google.client_secret_file.as_ref()),
        Some(&PathBuf::from("./secrets/google-client-secret"))
    );
}

#[test]
fn resolves_required_runtime_sections_and_lease_defaults() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"
xkey_seed_file = "./nats/auth-callout-xkey.seed"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    let nats = config.resolve_nats_runtime().expect("resolve nats");
    assert_eq!(nats.servers, "nats://127.0.0.1:4222");
    assert_eq!(
        nats.auth_creds_path,
        PathBuf::from("./nats/auth-runtime.creds")
    );

    let overridden = config
        .resolve_nats_runtime_with(Some("nats://127.0.0.1:4223"))
        .expect("resolve nats with override");
    assert_eq!(overridden.servers, "nats://127.0.0.1:4223");
    assert_eq!(
        overridden.auth_creds_path, nats.auth_creds_path,
        "override replaces only the server list"
    );
    assert_eq!(
        config
            .resolve_nats_runtime_with(None)
            .expect("resolve nats without override")
            .servers,
        "nats://127.0.0.1:4222",
        "None keeps the configured servers"
    );

    let auth_callout = config
        .resolve_nats_auth_callout()
        .expect("resolve auth callout");
    assert_eq!(
        auth_callout.xkey_seed_file,
        PathBuf::from("./nats/auth-callout-xkey.seed")
    );

    let leases = config.resolve_leases().expect("resolve leases");
    assert_eq!(leases.bucket, "trellis_runtime_leases");
    assert_eq!(leases.replicas, 1);
    assert_eq!(leases.ttl_ms, 30_000);
    assert_eq!(leases.renew_ms, 5_000);
}

#[test]
fn resolves_relative_paths_against_config_directory() {
    let directory = tempdir().expect("create temp directory");
    let path = directory.path().join("trellis.toml");
    fs::write(
        &path,
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth.creds"

[oauth.providers.google]
type = "oidc"
client_secret_file = "./secrets/google"

[auth.authorization]
issuer_signing_seed_file = "./auth/issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"
"#,
    )
    .expect("write config");

    let config = RuntimeConfig::load_from_path(&path).expect("load config");

    assert_eq!(
        config.platform_storage_backend().expect("storage"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: directory.path().join("./data/platform.sqlite"),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
    assert_eq!(
        config
            .nats
            .as_ref()
            .and_then(|nats| nats.runtime.as_ref())
            .and_then(|runtime| runtime.auth_creds_path.as_ref()),
        Some(&directory.path().join("./nats/auth.creds"))
    );
    assert_eq!(
        config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.providers.get("google"))
            .and_then(|google| google.client_secret_file.as_ref()),
        Some(&directory.path().join("./secrets/google"))
    );
    let authorization = config.resolve_authorization().expect("authorization");
    assert_eq!(
        authorization.issuer_signing_seed_file,
        directory.path().join("./auth/issuer.seed")
    );
}

#[test]
fn authorization_config_rejects_root_seed_and_invalid_policy() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[auth.authorization]
issuer_signing_seed_file = "issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 300
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "contexts"
registry_replicas = 1
"#,
    )
    .expect("parse authorization config");
    assert!(matches!(
        config.resolve_authorization(),
        Err(ConfigError::InvalidAuthorizationConfig {
            field: "refresh_lead_seconds",
            ..
        })
    ));
}

#[test]
fn uses_default_http_port_when_missing() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert_eq!(config.http_port(), 3000);
}

#[test]
fn rejects_non_toml_config_path() {
    let directory = tempdir().expect("create temp directory");
    let path = directory.path().join("config.jsonc");
    fs::write(&path, "{}").expect("write config");

    let error = RuntimeConfig::load_from_path(&path).expect_err("reject config");

    assert!(matches!(error, ConfigError::UnsupportedFormat { .. }));
}

#[test]
fn validates_selected_mode_storage_only() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"
xkey_seed_file = "./nats/auth-callout-xkey.seed"

[auth.authorization]
issuer_signing_seed_file = "./auth/issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    config
        .validate_for_mode(RuntimeMode::Jobs)
        .expect("jobs config is valid");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Platform),
        Err(ConfigError::MissingSection {
            section: "platform"
        })
    ));
}

#[test]
fn all_mode_requires_every_subsystem_storage() {
    let config = RuntimeConfig::from_toml_str(COMPLETE_CONFIG).expect("parse config");

    config
        .validate_for_mode(RuntimeMode::All)
        .expect("all config is valid");

    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"
xkey_seed_file = "./nats/auth-callout-xkey.seed"

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::MissingSection { section: "jobs" })
    ));
}

#[test]
fn rejects_postgres_until_backend_is_implemented() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[events.storage]
kind = "postgres"
url = "postgres://trellis-events@localhost/trellis_events"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Events),
        Err(ConfigError::UnsupportedStorageBackend {
            section: "events.storage",
            backend: "postgres"
        })
    ));
}

#[test]
fn rejects_invalid_storage_fields() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[health.storage]
kind = ""
path = "./data/health.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Health),
        Err(ConfigError::InvalidStorage {
            section: "health.storage",
            reason: "kind must not be empty"
        })
    ));

    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[health.storage]
kind = "postgres"
path = "./data/health.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Health),
        Err(ConfigError::UnsupportedStorageBackend {
            section: "health.storage",
            backend: "postgres"
        })
    ));
}

#[test]
fn rejects_missing_nats_runtime_paths() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Jobs),
        Err(ConfigError::InvalidNatsConfig {
            section: "nats.runtime",
            field: "trellis_creds_path",
            reason: "must not be missing or empty"
        })
    ));
}

#[test]
fn platform_modes_require_auth_callout_seed_paths() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[nats.auth_callout]
issuer_signing_seed_file = "./nats/auth-issuer-signing.seed"
target_signing_seed_file = "./nats/trellis-target-signing.seed"

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Platform),
        Err(ConfigError::InvalidNatsConfig {
            section: "nats.auth_callout",
            field: "xkey_seed_file",
            reason: "must not be missing or empty"
        })
    ));
}

#[test]
fn non_platform_split_modes_do_not_require_auth_callout_seed_paths() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[health.storage]
kind = "sqlite"
path = "./data/health.sqlite"

[leases]
replicas = 1
"#,
    )
    .expect("parse config");

    config
        .validate_for_mode(RuntimeMode::Health)
        .expect("health config is valid");
}

#[test]
fn all_modes_require_nats_servers() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Jobs),
        Err(ConfigError::InvalidNatsConfig {
            section: "nats",
            field: "servers",
            reason: "must not be missing or empty"
        })
    ));
}

#[test]
fn runtime_modes_require_leases_section_and_replicas_field() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Jobs),
        Err(ConfigError::MissingSection { section: "leases" })
    ));

    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[leases]
bucket = "trellis_runtime_leases"
"#,
    )
    .expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Jobs),
        Err(ConfigError::InvalidLeasesConfig {
            section: "leases",
            field: "replicas",
            reason: "must be configured explicitly"
        })
    ));
}

#[test]
fn lease_replica_validation_requires_presence_only() {
    let config = RuntimeConfig::from_toml_str(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[nats.runtime]
auth_creds_path = "./nats/auth-runtime.creds"
trellis_creds_path = "./nats/trellis-runtime.creds"
system_creds_path = "./nats/system-runtime.creds"

[auth.authorization]
issuer_signing_seed_file = "./auth/issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[leases]
replicas = 0
"#,
    )
    .expect("parse config");

    config
        .validate_for_mode(RuntimeMode::Jobs)
        .expect("replica acceptability is delegated to NATS");
}
