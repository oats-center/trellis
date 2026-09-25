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
fn slug_from_name_slugs_trellis_name() {
    assert_eq!(slug_from_name("Trellis"), "trellis");
    assert_eq!(slug_from_name("Acme Trellis"), "acme-trellis");
    assert_eq!(slug_from_name("  Acme__Trellis!!  "), "acme-trellis");
    assert_eq!(slug_from_name("!!!"), "trellis");
}

#[test]
fn generated_nats_credentials_are_parseable_and_private() {
    let temp = tempfile::tempdir().expect("temp dir");
    generate_nats_bootstrap(&nats_options(temp.path())).expect("generate nats");

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
}

#[test]
fn generated_runtime_config_loads_custom_values_and_resolves_paths() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut options = trellis_options(temp.path());
    options.runtime.name = "Acme Trellis".to_string();
    options.runtime.trellis_port = 4242;
    options.runtime.nats_server_url = "nats://nats.example.test:4222".to_string();
    options.runtime.nats_websocket_url = "wss://nats.example.test/ws".to_string();
    options.runtime.public_origin = "https://trellis.example.test/".to_string();
    options.runtime.extra_origins = vec!["http://localhost:5174".to_string()];
    let config = render_trellis_config(&options);

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
    assert!(matches!(
        parsed.platform_storage_backend().expect("platform storage"),
        StorageBackend::Sqlite(storage)
            if storage.path == temp.path().join("platform.sqlite")
    ));
}

#[test]
fn nats_config_render_round_trips_custom_ports() {
    let config = render_nats_config("trellis", 4322, 8322, 8180);

    assert_eq!(
        parse_config(&config).expect("parse rendered config"),
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        }
    );
}

#[test]
fn local_nats_config_render_round_trips_custom_ports() {
    let config = render_local_nats_config(
        "trellis",
        "/tmp/trellis/nats/data",
        "/tmp/trellis/nats/jwt.local.conf",
        4322,
        8180,
        8322,
    );

    assert_eq!(
        parse_config(&config).expect("parse local rendered config"),
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        }
    );
}

#[test]
fn managed_listener_parser_accepts_comments_whitespace_quotes_and_detached_separators() {
    let config = r#"
server_name: "quoted { # // /* structure is not structure"
# native listener
listen = "127.0.0.1:4322" // trailing comment
/* monitoring listener */
http : '127.0.0.1:8322'
websocket = {
  listen: `[::1]:8180` # bracketed IPv6 loopback
  no_tls = true
}
"#;

    assert_eq!(
        parse_config(config).expect("parse commented config"),
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        }
    );
}

#[test]
fn managed_listener_parser_accepts_inline_stream_and_colon_values() {
    let config = "listen:127.0.0.1:4322 http:127.0.0.1:8322 websocket { listen:127.0.0.1:8180 }";

    assert_eq!(
        parse_config(config).expect("parse inline listeners"),
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        }
    );
}

#[test]
fn managed_listener_parser_ignores_nested_unrelated_listeners() {
    let config = r#"
listen: 127.0.0.1:4322
http: 127.0.0.1:8322
websocket {
  listen: 127.0.0.1:8180
}
cluster {
  listen: 127.0.0.1:6222
}
gateway {
  listen: 127.0.0.1:7222
}
leafnodes {
  listen: 127.0.0.1:7422
}
tls {
  cert_file: "./cert.pem"
  listen: 127.0.0.1:7522
}
"#;

    assert_eq!(
        parse_config(config).expect("parse nested listeners"),
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        }
    );
}

#[test]
fn managed_listener_parser_rejects_duplicate_listeners() {
    let error = parse_config("listen: 127.0.0.1:4222\nlisten: 127.0.0.1:4322\nhttp: 127.0.0.1:8222\nwebsocket {\nlisten: 127.0.0.1:8080\n}\n")
        .expect_err("duplicate native listener must fail");
    assert!(matches!(
        error,
        NatsConfigError::DuplicateListener {
            listener: "listen",
            first: 1,
            second: 2,
            ..
        }
    ));

    let error =
        parse_config("listen: 4222\nhttp: 8222\nwebsocket {\nlisten: 8080\nlisten: 8180\n}\n")
            .expect_err("duplicate websocket listener must fail");
    assert!(matches!(
        error,
        NatsConfigError::DuplicateListener {
            listener: "websocket listen",
            first: 4,
            second: 5,
            ..
        }
    ));
}

#[test]
fn managed_listener_parser_requires_every_listener() {
    for (config, listener) in [
        ("http: 8222\nwebsocket { listen: 8080 }\n", "listen"),
        ("listen: 4222\nwebsocket { listen: 8080 }\n", "http"),
        ("listen: 4222\nhttp: 8222\n", "websocket listen"),
    ] {
        let error = parse_config(config).expect_err("missing listener must fail");
        assert!(
            matches!(error, NatsConfigError::MissingListener { listener: actual, .. } if actual == listener),
            "expected missing {listener}, got {error}"
        );
    }
}

#[test]
fn managed_listener_parser_rejects_include_only_configuration() {
    let error =
        parse_config("include ./other-listeners.conf\n").expect_err("indirect listeners must fail");
    assert!(matches!(
        error,
        NatsConfigError::MissingListener {
            listener: "listen",
            ..
        }
    ));
}

#[test]
fn managed_listener_parser_rejects_unsupported_expressions_and_malformed_values() {
    for config in [
        "listen: $NATS_PORT\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: 127.0.0.1:\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: 0\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: [::1]\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: [not-an-address]:4222\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: ::1:4222\nhttp: 8222\nwebsocket { listen: 8080 }\n",
    ] {
        assert!(
            matches!(
                parse_config(config),
                Err(NatsConfigError::InvalidListener { .. })
            ),
            "{config:?} must be rejected as an invalid listener"
        );
    }
}

#[test]
fn managed_listener_parser_rejects_malformed_blocks() {
    for config in [
        "listen: 4222\nhttp: 8222\nwebsocket { listen: 8080\n",
        "}\n",
        "listen: 4222\nhttp: 8222\nwebsocket { listen: 8080 }\nlisten:\n",
        "listen: \"4222\nhttp: 8222\nwebsocket { listen: 8080 }\n",
        "listen: 4222\n/* unterminated\n",
    ] {
        assert!(
            matches!(parse_config(config), Err(NatsConfigError::Malformed { .. })),
            "{config:?} must be rejected as malformed"
        );
    }
}

#[test]
fn managed_listener_parse_failure_does_not_select_defaults() {
    let error = parse_config("server_name: trellis\n")
        .expect_err("a config without listeners must not fall back to defaults");

    assert!(matches!(
        error,
        NatsConfigError::MissingListener {
            listener: "listen",
            ..
        }
    ));
}

#[test]
fn managed_listener_read_reports_missing_file() {
    let temp = tempfile::tempdir().expect("temp dir");
    let error = read_nats_listen_ports(&temp.path().join("missing.conf"))
        .expect_err("missing config must fail");

    assert!(matches!(error, NatsConfigError::Read { .. }));
}

#[test]
fn generated_bundle_listeners_round_trip_through_managed_parser() {
    let temp = tempfile::tempdir().expect("temp dir");
    let out = temp.path().join("out");
    let mut options = trellis_options(&out);
    options.runtime.trellis_port = 3444;
    options.nats.nats_port = 4333;
    options.nats.monitor_port = 8333;
    options.nats.websocket_port = 8183;

    generate_trellis_bootstrap(&options).expect("generate Trellis");

    assert_eq!(
        read_nats_listen_ports(&out.join("nats/nats.conf")).expect("read listeners"),
        NatsListeners {
            native: 4333,
            monitor: 8333,
            websocket: 8183,
        }
    );
}

#[test]
fn nats_bootstrap_rejects_zero_and_duplicate_ports_before_writing() {
    let temp = tempfile::tempdir().expect("temp dir");
    let zero_out = temp.path().join("zero");
    let mut options = nats_options(&zero_out);
    options.config.nats_port = 0;

    assert!(matches!(
        generate_nats_bootstrap(&options),
        Err(BootstrapError::InvalidListenerPort { listener: "native" })
    ));
    assert!(!zero_out.exists(), "invalid ports must not create output");

    let duplicate_out = temp.path().join("duplicate");
    let mut options = nats_options(&duplicate_out);
    options.config.websocket_port = options.config.monitor_port;

    assert!(matches!(
        generate_nats_bootstrap(&options),
        Err(BootstrapError::DuplicateListenerPort {
            first: "monitor",
            second: "websocket",
            ..
        })
    ));
    assert!(
        !duplicate_out.exists(),
        "invalid ports must not create output"
    );
}

#[test]
fn trellis_bootstrap_rejects_http_collision_before_replacing_output() {
    let temp = tempfile::tempdir().expect("temp dir");
    let out = temp.path().join("out");
    fs::create_dir_all(&out).expect("create output dir");
    fs::write(out.join("keep"), "keep").expect("write sentinel");
    let mut options = trellis_options(&out);
    options.force = true;
    options.nats.nats_port = options.runtime.trellis_port;

    assert!(matches!(
        generate_trellis_bootstrap(&options),
        Err(BootstrapError::TrellisListenerPortCollision {
            listener: "native",
            ..
        })
    ));
    assert!(
        out.join("keep").is_file(),
        "validation failure must not replace existing output"
    );
}

#[test]
fn trellis_bootstrap_rejects_each_trellis_http_collision() {
    let temp = tempfile::tempdir().expect("temp dir");
    for (listener, port) in [("monitor", 3000), ("websocket", 3000)] {
        let out = temp.path().join(listener);
        let mut options = trellis_options(&out);
        match listener {
            "monitor" => options.nats.monitor_port = port,
            _ => options.nats.websocket_port = port,
        }

        assert!(matches!(
            generate_trellis_bootstrap(&options),
            Err(BootstrapError::TrellisListenerPortCollision {
                listener: actual,
                port: 3000,
            }) if actual == listener
        ));
        assert!(!out.exists());
    }
}

#[test]
fn generated_session_and_authorization_seeds_are_decodable_and_private() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    let temp = tempfile::tempdir().expect("temp dir");
    generate_trellis_bootstrap(&trellis_options(temp.path())).expect("generate Trellis");

    for name in ["session.seed", "auth/authorization-issuer.seed"] {
        let seed_path = temp.path().join(name);
        let contents = fs::read_to_string(&seed_path).expect("read generated seed");
        let decoded = URL_SAFE_NO_PAD
            .decode(contents.trim())
            .expect("decode generated seed");
        assert_eq!(decoded.len(), 32, "{name} must contain a 32-byte seed");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&seed_path)
                    .expect("seed metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{name} must be private"
            );
        }
    }
}

fn parse_config(config: &str) -> Result<NatsListeners, NatsConfigError> {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("nats.conf");
    fs::write(&path, config).expect("write nats config");
    read_nats_listen_ports(&path)
}

fn nats_options(out: impl Into<PathBuf>) -> NatsBootstrapOptions {
    NatsBootstrapOptions::new(out)
}

fn trellis_options(out: impl Into<PathBuf>) -> TrellisBootstrapOptions {
    TrellisBootstrapOptions::new(out)
}

#[test]
fn local_nats_config_quotes_host_paths_with_spaces() {
    let config = render_local_nats_config(
        "trellis",
        "/tmp/trellis data/store dir",
        "/tmp/trellis data/jwt config.conf",
        4222,
        8080,
        8222,
    );
    assert!(config.contains("store_dir: \"/tmp/trellis data/store dir\""));
    assert!(config.contains("include \"/tmp/trellis data/jwt config.conf\""));

    let rendered = render_local_jwt_config("dir: ./resolver\n", "a\"b\\c");
    assert_eq!(rendered, r#"dir: "a\"b\\c""#);
}
