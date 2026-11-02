use std::fs;

use tempfile::tempdir;

use super::{
    AuthCalloutSecrets, ConfigError, NatsSecrets, NatsUser, NatsUsers, RuntimeConfig,
    RuntimePathDefaults, Secrets, SqliteStorageConfig, StorageBackend,
};
use crate::RuntimeMode;

/// Every secret an installation needs, as placeholders config resolution
/// carries through without checking.
fn secrets() -> Secrets {
    let user = |name: &str| {
        Some(NatsUser {
            jwt: format!("{name}.jwt"),
            seed: format!("{name}.seed"),
        })
    };
    Secrets {
        nats: NatsSecrets {
            users: NatsUsers {
                system: user("system"),
                auth: user("auth"),
                trellis: user("trellis"),
            },
            auth_callout: AuthCalloutSecrets {
                issuer_signing_seed: Some("issuer".into()),
                target_signing_seed: Some("target".into()),
                xkey_seed: Some("xkey".into()),
                sentinel_public_key: Some("sentinel".into()),
            },
        },
        ..Secrets::default()
    }
}

/// Parses `source` as an installation whose `secrets.toml` has every secret.
fn from_toml(source: &str) -> Result<RuntimeConfig, ConfigError> {
    RuntimeConfig::from_toml_str(source).map(|mut config| {
        config.secrets = secrets();
        config
    })
}

const COMPLETE_CONFIG: &str = r#"
instance_name = "Trellis"

[http]
port = 39123

[nats]
servers = "nats://127.0.0.1:4222"

[auth]
instance_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV"

[auth.authorization]
context_lifetime_seconds = 300
allowed_clock_skew_seconds = 30

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
    let config = from_toml(&source).expect("parse config");

    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath {
            first: "platform",
            second: "health",
            ..
        })
    ));

    let source = COMPLETE_CONFIG.replace("./data/health.sqlite", "./data/./platform.sqlite");
    let config = from_toml(&source).expect("parse equivalent path config");
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
    let config = from_toml(&source).expect("parse symlink config");
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
    let config = from_toml(&source_with_chain).expect("parse symlink chain");
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::All),
        Err(ConfigError::SharedSqlitePath { .. })
    ));
    std::fs::remove_file(second_alias).expect("remove second symlink");

    std::fs::remove_file(&alias).expect("remove symlink");
    std::fs::write(&target, []).expect("create target");
    std::fs::hard_link(&target, &alias).expect("create hard link");
    let config = from_toml(&source).expect("parse hard-link config");
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
fn console_serves_on_the_trellis_origin_or_a_distinct_one_of_the_same_scheme() {
    let config = from_toml(COMPLETE_CONFIG).unwrap();
    assert_eq!(
        config.resolve_console_origin().unwrap(),
        "http://localhost:39123"
    );
    let own = format!("{COMPLETE_CONFIG}\n[console]\norigin = \"http://127.0.0.1:39123/\"\n");
    assert_eq!(
        from_toml(&own).unwrap().resolve_console_origin().unwrap(),
        "http://127.0.0.1:39123"
    );
    for (before, after) in [
        ("instance_id = \"01ARZ3NDEKTSV4RRFFQ69G5FAV\"", ""),
        ("01ARZ3NDEKTSV4RRFFQ69G5FAV", "01arz3ndektsv4rrffq69g5fav"),
        ("nats://127.0.0.1:4222", "nats://broker.example:4222"),
        ("nats://127.0.0.1:4222", "ws://broker.example:4222"),
        ("nats://127.0.0.1:4222", "nats://192.168.1.1:4222"),
        ("nats://127.0.0.1:4222", "https://broker.example:4222"),
        ("http://127.0.0.1:39123/", "http://localhost:39123"),
        ("http://127.0.0.1:39123/", "https://127.0.0.1:39123"),
        ("http://127.0.0.1:39123/", "http://127.0.0.1:39123/console"),
        ("[console]\n", "[console]\nenabled = false\n"),
    ] {
        let config = from_toml(&own.replace(before, after)).unwrap();
        assert!(
            config.resolve_console_origin().is_err(),
            "accepted unsafe configuration: {before} -> {after}"
        );
    }
}

#[test]
fn a_managed_broker_replaces_only_the_configured_servers() {
    let config = from_toml("[nats]\nservers = \"nats://127.0.0.1:4222\"\n").expect("parse config");
    let nats = config.resolve_nats_runtime().expect("resolve nats");
    let overridden = config
        .resolve_nats_runtime_with(Some("nats://127.0.0.1:4223"))
        .expect("resolve nats with override");
    assert_eq!(overridden.servers, "nats://127.0.0.1:4223");
    assert_eq!(overridden.trellis_creds, nats.trellis_creds);
}

#[test]
fn resolves_relative_paths_against_config_directory() {
    // Named relative to the working directory, as `--config dir/config.toml`.
    let working = std::env::current_dir().expect("working directory");
    let directory = tempfile::tempdir_in(&working).expect("create temp directory");
    let path = directory
        .path()
        .strip_prefix(&working)
        .expect("relative")
        .join("trellis.toml");
    let directory = std::path::absolute(directory.path()).expect("absolute");
    fs::write(
        &path,
        r#"
[paths]
data = "./data"

[nats]
servers = "nats://127.0.0.1:4222"
ca_certificate = "./tls/ca.crt"

[auth.authorization]
context_lifetime_seconds = 300
allowed_clock_skew_seconds = 30

[platform.storage]
kind = "sqlite"
"#,
    )
    .expect("write config");

    let config = RuntimeConfig::load_from_path(&path).expect("load config");

    assert_eq!(
        config.platform_storage_backend().expect("storage"),
        StorageBackend::Sqlite(SqliteStorageConfig {
            path: directory.join("data/platform.sqlite"),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        })
    );
    assert_eq!(
        config
            .nats
            .as_ref()
            .and_then(|nats| nats.ca_certificate.as_ref()),
        Some(&directory.join("tls/ca.crt"))
    );
}

#[test]
fn authorization_config_rejects_root_seed_and_invalid_policy() {
    let config = from_toml(
        r#"
[auth.authorization]
context_lifetime_seconds = 30
allowed_clock_skew_seconds = 30
"#,
    )
    .expect("parse authorization config");
    assert!(matches!(
        config.resolve_authorization(),
        Err(ConfigError::InvalidAuthorizationConfig {
            field: "allowed_clock_skew_seconds",
            ..
        })
    ));
}

#[test]
fn oauth_admission_lifetime_defaults_and_enforces_bounds() {
    let source = r#"
[auth.authorization]
context_lifetime_seconds = 60
allowed_clock_skew_seconds = 2
"#;
    let parse = |value: Option<u64>| {
        let configured = match value {
            Some(seconds) => format!("{source}\noauth_admission_lifetime_seconds = {seconds}"),
            None => source.to_owned(),
        };
        let config = from_toml(&configured).expect("valid TOML");
        config
            .resolve_authorization()
            .map(|auth| auth.oauth_admission_lifetime_seconds)
    };
    assert_eq!(parse(None).expect("old config remains valid"), 300);
    assert_eq!(parse(Some(18)).expect("short admission is valid"), 18);
    for value in [0, 301] {
        assert!(matches!(
            parse(Some(value)),
            Err(ConfigError::InvalidAuthorizationConfig {
                field: "oauth_admission_lifetime_seconds",
                ..
            })
        ));
    }
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
    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[auth.authorization]
context_lifetime_seconds = 300
allowed_clock_skew_seconds = 30

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
    let config = from_toml(COMPLETE_CONFIG).expect("parse config");

    config
        .validate_for_mode(RuntimeMode::All)
        .expect("all config is valid");

    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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
    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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
    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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

    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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
fn every_mode_needs_its_nats_users_and_platform_needs_callout_keys() {
    let source = r#"
[nats]
servers = "nats://127.0.0.1:4222"

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"

[leases]
replicas = 1
"#;
    let mut config = from_toml(source).expect("parse config");
    config.secrets.nats.users.trellis = None;
    assert!(matches!(
        config.validate_for_mode(RuntimeMode::Jobs),
        Err(ConfigError::MissingSecret {
            name: "nats.users.trellis"
        })
    ));

    // A split role elsewhere holds only the Trellis user; the platform also
    // needs the auth and system users and the callout keys.
    for missing in ["nats.users.system", "nats.auth_callout.xkey_seed"] {
        let mut config = from_toml(source).expect("parse config");
        match missing {
            "nats.users.system" => config.secrets.nats.users.system = None,
            _ => config.secrets.nats.auth_callout.xkey_seed = None,
        }
        config
            .validate_for_mode(RuntimeMode::Jobs)
            .expect("jobs needs neither");
        assert!(matches!(
            config.validate_for_mode(RuntimeMode::Platform),
            Err(ConfigError::MissingSecret { name }) if name == missing
        ));
    }
}

#[test]
fn all_modes_require_nats_servers() {
    let config = from_toml(
        r#"
[nats]

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
    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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

    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

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
    let config = from_toml(
        r#"
[nats]
servers = "nats://127.0.0.1:4222"

[auth.authorization]
context_lifetime_seconds = 300
allowed_clock_skew_seconds = 30

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

#[cfg(unix)]
#[test]
fn secrets_others_could_read_or_swap_are_refused() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().expect("create temp directory");
    let config = directory.path().join("config.toml");
    fs::write(&config, "").expect("write config");
    let secrets = directory.path().join(super::SECRETS_FILE);
    fs::write(&secrets, "oauth_storage_key = \"key\"\n").expect("write secrets");
    let load = || RuntimeConfig::load_from_path(&config);

    for mode in [0o644, 0o640] {
        fs::set_permissions(&secrets, fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(load(), Err(ConfigError::Secrets { .. })),
            "{mode:o}"
        );
    }
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        load().unwrap().secrets.oauth_storage_key.as_deref(),
        Some("key")
    );

    // A second name lets someone else's file stand in for it.
    let alias = directory.path().join("alias.toml");
    fs::hard_link(&secrets, &alias).unwrap();
    assert!(matches!(load(), Err(ConfigError::Secrets { .. })));
    fs::remove_file(&alias).unwrap();

    let elsewhere = directory.path().join("elsewhere.toml");
    fs::rename(&secrets, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &secrets).unwrap();
    assert!(matches!(load(), Err(ConfigError::Secrets { .. })));
}

/// A mistyped `secrets.toml` is rejected without its keys reaching the error,
/// which goes to the terminal and the journal, yet the error still says where.
#[test]
fn a_malformed_secrets_file_is_reported_without_its_keys() {
    const MARKER: &str = "SUPERSECRETSEEDMARKER";
    let directory = tempdir().expect("create temp directory");
    let path = directory.path().join("secrets.toml");
    for (source, line) in [
        // The closing quote is missing.
        (
            format!("oauth_storage_key = \"x\"\nauthorization_issuer_seed = \"{MARKER}\n"),
            2,
        ),
        // A seed where a user's table belongs.
        (format!("[nats.users]\n\ntrellis = \"{MARKER}\"\n"), 3),
    ] {
        fs::write(&path, source).expect("write secrets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        let error = Secrets::load(&path)
            .expect_err("malformed secrets load")
            .to_string();
        assert!(!error.contains(MARKER), "the error exposed a key: {error}");
        assert!(
            error.contains(&format!("line {line}")),
            "no location: {error}"
        );
    }
}
