use super::*;
use std::fs;
use std::path::PathBuf;
use trellis_runtime::{RuntimeConfig, RuntimeMode, StorageBackend};

#[test]
fn validate_output_dir_rejects_non_empty_without_force() {
    let temp = tempfile::tempdir().expect("temp dir");
    fs::write(temp.path().join("existing"), "x").expect("write file");

    let error = validate_output_dir(temp.path(), false).expect_err("should reject");

    assert!(matches!(
        error,
        BootstrapError::OutputDirectoryNotEmpty { .. }
    ));
}

#[test]
fn validate_output_dir_accepts_non_empty_with_force() {
    let temp = tempfile::tempdir().expect("temp dir");
    fs::write(temp.path().join("existing"), "x").expect("write file");

    validate_output_dir(temp.path(), true).expect("force should allow non-empty dir");
}

#[test]
fn nats_bootstrap_rejects_empty_operator_name() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = nats_options(temp.path());
    options.config.names.operator_name = " ".to_string();

    assert!(matches!(
        generate_nats_bootstrap(&options),
        Err(BootstrapError::MissingRequiredOption("operator_name"))
    ));
}

#[test]
fn trellis_bootstrap_rejects_empty_system_account() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.nats.names.system_account = "".to_string();

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::MissingRequiredOption("system_account"))
    ));
}

#[test]
fn trellis_bootstrap_rejects_empty_name() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.runtime.name = " ".to_string();

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::MissingRequiredOption("name"))
    ));
}

#[test]
fn trellis_bootstrap_rejects_empty_server_name_override() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.nats.names.server_name = Some(" ".to_string());

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::MissingRequiredOption("server_name"))
    ));
}

#[test]
fn trellis_bootstrap_rejects_control_characters_in_generated_text_values() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.nats.names.server_name = Some("nats\ninclude /tmp/other.conf".to_string());

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::InvalidGeneratedTextValue("server_name"))
    ));

    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.nats.names.auth_account = "AUTH\nEXTRA=value".to_string();

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::InvalidGeneratedTextValue("auth_account"))
    ));
}

#[test]
fn nats_bootstrap_honors_server_name_override() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = nats_options(temp.path());
    options.config.names.server_name = Some("nats-prod-1".to_string());

    generate_nats_bootstrap(&options).expect("generate nats");
    let config = fs::read_to_string(temp.path().join("nats.conf")).expect("read nats config");

    assert!(config.contains("server_name: nats-prod-1"));
}

#[test]
fn slug_from_name_slugs_trellis_name() {
    assert_eq!(slug_from_name("Trellis"), "trellis");
    assert_eq!(slug_from_name("Acme Trellis"), "acme-trellis");
    assert_eq!(slug_from_name("  Acme__Trellis!!  "), "acme-trellis");
    assert_eq!(slug_from_name("!!!"), "trellis");
}

#[test]
fn nats_bootstrap_generates_native_layout_without_transients() {
    let temp = tempfile::tempdir().expect("temp dir");
    generate_nats_bootstrap(&nats_options(temp.path())).expect("generate nats");

    assert!(temp.path().join("nats.conf").is_file());
    assert!(temp.path().join("jwt.conf").is_file());
    assert!(temp.path().join("creds/system.creds").is_file());
    assert!(temp.path().join("creds/auth-auth.creds").is_file());
    assert!(temp.path().join("creds/trellis-auth.creds").is_file());
    assert!(temp
        .path()
        .join("secrets/auth-issuer-signing.seed")
        .is_file());
    assert!(temp
        .path()
        .join("secrets/auth-target-signing.seed")
        .is_file());
    assert!(temp.path().join("secrets/auth-sx.seed").is_file());
    assert!(temp.path().join("auth-callout.env").is_file());
    assert!(!temp.path().join("manifest.json").exists());
    assert!(!temp.path().join("bootstrap-nsc.sh").exists());
    assert!(!temp.path().join("generated").exists());
    assert!(!temp.path().join(".nsc").exists());
    assert!(!temp.path().join(".nkeys").exists());

    for path in [
        "creds/system.creds",
        "creds/auth-auth.creds",
        "creds/trellis-auth.creds",
    ] {
        let creds = fs::read_to_string(temp.path().join(path)).expect("read creds");
        async_nats::ConnectOptions::with_credentials(&creds).expect("parse generated creds");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            fs::metadata(temp.path().join("creds"))
                .expect("creds metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(temp.path().join("secrets"))
                .expect("secrets metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for path in [
            "creds/system.creds",
            "creds/auth-auth.creds",
            "creds/trellis-auth.creds",
            "secrets/auth-issuer-signing.seed",
            "secrets/auth-target-signing.seed",
            "secrets/auth-sx.seed",
        ] {
            assert_eq!(
                fs::metadata(temp.path().join(path))
                    .unwrap_or_else(|error| panic!("metadata for {path}: {error}"))
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{path} should be private"
            );
        }
    }

    let jwt_count = fs::read_dir(temp.path().join("data/jwt"))
        .expect("read jwt dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jwt")
        })
        .count();
    assert_eq!(jwt_count, 3);

    let jwt_config = fs::read_to_string(temp.path().join("jwt.conf")).expect("jwt config");
    assert!(jwt_config.contains("operator: "));
    assert!(jwt_config.contains("system_account: "));
    assert!(jwt_config.contains("resolver: {"));
    assert!(jwt_config.contains("type: full"));
    assert!(jwt_config.contains("dir: /data/jwt"));
    assert!(jwt_config.contains("resolver_preload: {"));
}

#[test]
fn nats_bootstrap_names_trellis_user_claim() {
    let temp = tempfile::tempdir().expect("temp dir");
    generate_nats_bootstrap(&nats_options(temp.path())).expect("generate nats");

    let creds = fs::read_to_string(temp.path().join("creds/trellis-auth.creds"))
        .expect("read trellis creds");
    let claims =
        nats_jwt_rs::Claims::<nats_jwt_rs::user::User>::decode(user_jwt_from_creds(&creds))
            .expect("decode trellis user jwt");

    assert_eq!(claims.name.as_deref(), Some("trellis"));
}

#[test]
fn trellis_bootstrap_generates_online_authorization_issuer_only() {
    let temp = tempfile::tempdir().expect("temp dir");

    generate_trellis_bootstrap(&trellis_options(temp.path())).expect("generate Trellis");

    assert!(!temp.path().join("manifest.json").exists());
    assert!(!temp.path().join("nats/manifest.json").exists());
    assert!(temp.path().join("nats/nats.conf").is_file());
    assert!(temp.path().join("config.toml").is_file());
    assert!(!temp.path().join("data").exists());
    assert!(temp.path().join("auth/authorization-issuer.seed").is_file());
    assert!(!temp.path().join("auth/authorization-root.json").exists());
    assert!(!temp.path().join("auth/authorization-root.seed").exists());
    assert!(!temp.path().join("trust/authorization-root.seed").exists());
}

#[test]
fn trellis_config_uses_expected_paths_urls_and_name() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.runtime.name = "Acme Trellis".to_string();
    options.runtime.trellis_port = 4242;
    options.runtime.nats_server_url = "nats://nats.example.test:4222".to_string();
    options.runtime.nats_websocket_url = "wss://nats.example.test/ws".to_string();
    options.runtime.public_origin = "https://trellis.example.test/".to_string();
    let config = render_trellis_config(&options);

    assert!(config.contains("instance_name = \"Acme Trellis\""));
    assert!(!config.contains("instance_id"));
    assert!(config.contains("[http]"));
    assert!(config.contains("port = 4242"));
    assert!(config.contains("public_origin = \"https://trellis.example.test/\""));
    assert!(config.contains("[nats]"));
    assert!(config.contains("servers = \"nats://nats.example.test:4222\""));
    assert!(config.contains("[nats.runtime]"));
    assert!(config.contains("system_creds_path = \"./nats/creds/system.creds\""));
    assert!(config.contains("trellis_creds_path = \"./nats/creds/trellis-auth.creds\""));
    assert!(config.contains("auth_creds_path = \"./nats/creds/auth-auth.creds\""));
    assert!(config.contains("[nats.auth_callout]"));
    assert!(
        config.contains("issuer_signing_seed_file = \"./nats/secrets/auth-issuer-signing.seed\"")
    );
    assert!(
        config.contains("target_signing_seed_file = \"./nats/secrets/auth-target-signing.seed\"")
    );
    assert!(config.contains("xkey_seed_file = \"./nats/secrets/auth-sx.seed\""));
    assert!(config.contains("ws_nats_servers = [\"wss://nats.example.test/ws\"]"));
    assert!(config.contains("nats_servers = [\"nats://nats.example.test:4222\"]"));
    assert!(config.contains("[platform.storage]"));
    assert!(!config.contains("event_context_digest_file"));
    assert!(!config.contains("[paths]"));
    assert!(!config.contains("\npath ="));
    assert!(config.contains("journal_mode = \"wal\""));
    assert!(config.contains("busy_timeout_ms = 5000"));
    assert!(config.contains("single_writer = true"));
    assert!(config.contains("[auth.local_identity]"));
    assert!(config.contains("enabled = true"));
    assert!(config.contains("password_min_length = 8"));
    assert!(config.contains("[auth.authorization]"));
    assert!(config.contains("issuer_signing_seed_file = \"./auth/authorization-issuer.seed\""));
    assert!(!config.contains("trust_root_file"));
    assert!(!config.contains("issuer_manifest_file"));
    assert!(!config.contains("authorization-root.seed"));
    assert!(config.contains("[leases]"));
    assert!(config.contains("replicas = 1"));
    assert!(config.contains("ttl_ms = 15000"));
    assert!(config.contains("renew_ms = 5000"));
    assert!(config.contains("redirect_base = \"https://trellis.example.test/auth/callback\""));
    assert!(!config.contains("providers"));
    assert!(!config.contains("sessionKeySeedFile"));
    assert!(!config.contains("ADYAUTH"));
    assert!(!config.contains("ADYTRELLIS"));
    assert!(!config.contains("github"));
    assert!(!config.contains("client_secret_file"));

    let temp = tempfile::tempdir().expect("config tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, &config).expect("write runtime config");
    let parsed = RuntimeConfig::load_from_path(&config_path).expect("load runtime config");
    parsed
        .validate_for_mode(RuntimeMode::All)
        .expect("validate all mode");
    assert_eq!(parsed.instance_name.as_deref(), Some("Acme Trellis"));
    assert_eq!(parsed.http_port(), 4242);
    assert_eq!(
        parsed
            .auth
            .as_ref()
            .and_then(|auth| auth.local_identity.as_ref())
            .and_then(|local_identity| local_identity.password_min_length),
        Some(8)
    );
    assert_eq!(
        parsed
            .client
            .as_ref()
            .and_then(|client| client.ws_nats_servers.as_ref()),
        Some(&vec!["wss://nats.example.test/ws".to_string()])
    );
    assert_eq!(
        parsed
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.redirect_base.as_deref()),
        Some("https://trellis.example.test/auth/callback")
    );
    assert_eq!(
        parsed.leases.as_ref().and_then(|leases| leases.replicas),
        Some(1)
    );
    assert!(matches!(
        parsed.platform_storage_backend().expect("platform storage"),
        StorageBackend::Sqlite(storage)
            if storage.path == temp.path().join("platform.sqlite")
                && storage.journal_mode.as_deref() == Some("wal")
                && storage.busy_timeout_ms == Some(5000)
                && storage.single_writer == Some(true)
    ));
}

#[test]
fn trellis_options_use_shared_defaults() {
    let options = TrellisBootstrapOptions::new("./trellis");

    assert_eq!(options.out, PathBuf::from("./trellis"));
    assert!(!options.force);
    assert_eq!(options.nats.names.operator_name, DEFAULT_OPERATOR_NAME);
    assert_eq!(options.nats.names.system_account, DEFAULT_SYSTEM_ACCOUNT);
    assert_eq!(options.nats.names.auth_account, DEFAULT_AUTH_ACCOUNT);
    assert_eq!(options.nats.names.trellis_account, DEFAULT_TRELLIS_ACCOUNT);
    assert_eq!(options.nats.names.server_name, None);
    assert_eq!(options.runtime.name, DEFAULT_TRELLIS_NAME);
    assert_eq!(options.runtime.trellis_port, DEFAULT_TRELLIS_PORT);
    assert_eq!(options.runtime.nats_server_url, DEFAULT_NATS_SERVER_URL);
    assert_eq!(
        options.runtime.nats_websocket_url,
        DEFAULT_NATS_WEBSOCKET_URL
    );
    assert_eq!(options.runtime.public_origin, DEFAULT_PUBLIC_ORIGIN);
}

#[test]
fn nats_config_uses_rendered_server_name() {
    let config = render_nats_config("trellis");

    assert!(config.contains("server_name: trellis"));
    assert!(config.contains("listen: 0.0.0.0:4222"));
    assert!(config.contains("http: 0.0.0.0:8222"));
    assert!(config.contains("timeout: \"30s\""));
    assert!(config.contains("listen: 0.0.0.0:8080"));
    assert!(config.contains("no_tls: true"));
    assert!(config.contains("store_dir: /data"));
    assert!(config.contains("include ./jwt.conf"));
}

#[test]
fn local_nats_config_uses_host_paths() {
    let config = render_local_nats_config(
        "trellis",
        "/tmp/trellis/nats/data",
        "/tmp/trellis/nats/jwt.local.conf",
        4222,
        8080,
        8222,
    );

    assert!(config.contains("server_name: trellis"));
    assert!(config.contains("listen: 127.0.0.1:4222"));
    assert!(config.contains("http: 127.0.0.1:8222"));
    assert!(config.contains("timeout: \"30s\""));
    assert!(config.contains("listen: 127.0.0.1:8080"));
    assert!(config.contains("no_tls: true"));
    assert!(config.contains("store_dir: /tmp/trellis/nats/data"));
    assert!(config.contains("include /tmp/trellis/nats/jwt.local.conf"));
    assert!(
        !config.contains("0.0.0.0"),
        "local render binds loopback only"
    );
}

#[test]
fn container_nats_config_keeps_public_bindings() {
    let config = render_nats_config("trellis");

    assert!(config.contains("listen: 0.0.0.0:4222"));
    assert!(config.contains("http: 0.0.0.0:8222"));
    assert!(config.contains("listen: 0.0.0.0:8080"));
}

#[test]
fn trellis_bootstrap_generates_private_session_seed_matching_expected_format() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    let temp = tempfile::tempdir().expect("temp dir");
    generate_trellis_bootstrap(&trellis_options(temp.path())).expect("generate Trellis");

    let seed_path = temp.path().join("session.seed");
    let contents = fs::read_to_string(&seed_path).expect("read session seed");
    assert!(
        contents.ends_with('\n'),
        "session seed must end with a newline"
    );
    let decoded = URL_SAFE_NO_PAD
        .decode(contents.trim())
        .expect("decode session seed");
    assert_eq!(decoded.len(), 32, "session seed must be 32 bytes");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&seed_path)
                .expect("session seed metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "session seed must be private like the other secret files"
        );
    }
}

fn nats_options(out: impl Into<PathBuf>) -> NatsBootstrapOptions {
    NatsBootstrapOptions::new(out)
}

fn trellis_options(out: impl Into<PathBuf>) -> TrellisBootstrapOptions {
    TrellisBootstrapOptions::new(out)
}

fn user_jwt_from_creds(creds: &str) -> &str {
    let mut lines = creds.lines();
    while let Some(line) = lines.next() {
        if line == "-----BEGIN NATS USER JWT-----" {
            return lines.next().expect("user JWT line");
        }
    }
    panic!("missing NATS user JWT block")
}
