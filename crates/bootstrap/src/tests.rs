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
    options.runtime.nats_server_url = "tls://nats.example.test:4222".to_string();
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
    assert!(matches!(
        parsed.platform_storage_backend().expect("platform storage"),
        StorageBackend::Sqlite(storage)
            if storage.path == temp.path().join("data/platform.sqlite")
    ));
}

#[test]
fn nats_config_render_round_trips_custom_ports() {
    let config = render_nats_config(
        "trellis",
        4322,
        8322,
        8180,
        Some(&NatsTlsFiles {
            certificate: "/etc/nats/tls/nats.crt".into(),
            private_key: "/etc/nats/tls/nats.key".into(),
            websocket_behind_proxy: false,
        }),
    );

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
        Some(&NatsTlsFiles {
            certificate: "/tmp/trellis/nats/tls/nats.crt".into(),
            private_key: "/tmp/trellis/nats/tls/nats.key".into(),
            websocket_behind_proxy: true,
        }),
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
    let error = parse_config(
        "listen: 127.0.0.1:4222\nlisten: 127.0.0.1:4322\nhttp: \
        127.0.0.1:8222\nwebsocket {\nlisten: 127.0.0.1:8080\n}\n",
    )
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
            matches!(
                error,
                NatsConfigError::MissingListener { listener: actual, .. } if actual == listener
            ),
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
    let out = temp.path().join("bundle");
    generate_trellis_bootstrap(&trellis_options(&out)).expect("generate Trellis");

    for name in ["session.seed", "auth/authorization-issuer.seed"] {
        let seed_path = out.join(name);
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
        None,
    );
    assert!(config.contains("store_dir: \"/tmp/trellis data/store dir\""));
    assert!(config.contains("include \"/tmp/trellis data/jwt config.conf\""));

    let rendered = render_local_jwt_config("dir: ./resolver\n", "a\"b\\c");
    assert_eq!(rendered, r#"dir: "a\"b\\c""#);
}

/// Verifies `certificate` for `host` with only the bundle CA trusted.
fn verify_with_installation_ca(
    ca: &std::path::Path,
    certificate: &[u8],
    host: &str,
) -> Result<(), rustls::Error> {
    use rustls::client::danger::ServerCertVerifier as _;
    use rustls::pki_types::{pem::PemObject as _, CertificateDer, ServerName, UnixTime};
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_file(ca).expect("read CA"))
        .expect("add CA");
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(roots.into(), provider)
            .build()
            .expect("build verifier");
    verifier
        .verify_server_cert(
            &CertificateDer::from_pem_slice(certificate).expect("parse leaf"),
            &[],
            &ServerName::try_from(host.to_owned()).expect("server name"),
            &[],
            UnixTime::now(),
        )
        .map(|_| ())
}

#[test]
fn generated_tls_trusts_only_installation_hosts_and_renews_without_changing_trust() {
    let temp = tempfile::tempdir().expect("temp dir");
    let out = temp.path().join("bundle");
    let ca_directory = temp.path().join("ca");
    let mut options = trellis_options(&out);
    options.runtime.public_origin = "https://eta.oats:3000".into();
    options.runtime.oauth_origin = "https://auth.eta.oats:3001".into();
    options.runtime.nats_server_url = "tls://192.0.2.10:4222".into();
    options.runtime.nats_websocket_url = "wss://nats.eta.oats:8080".into();
    options.runtime.tls = BootstrapTls::Generated {
        ca_directory: Some(ca_directory.clone()),
    };
    let summary = generate_trellis_bootstrap(&options)
        .expect("generate bundle")
        .expect("generated TLS summary");
    let ca = out.join("tls/ca.crt");
    let runtime_leaf = fs::read(out.join("tls/runtime.crt")).expect("runtime leaf");
    let nats_leaf = fs::read(out.join("nats/tls/nats.crt")).expect("NATS leaf");

    // Every role-mounted path lacks the CA private key; only the CA directory has it.
    assert!(ca_directory.join("ca.key").is_file());
    for entry in walk(&out) {
        assert!(
            !fs::read_to_string(&entry)
                .unwrap_or_default()
                .contains("PRIVATE KEY")
                || entry.ends_with("tls/runtime.key")
                || entry.ends_with("nats/tls/nats.key"),
            "{} must not hold the CA key",
            entry.display()
        );
    }
    assert_eq!(
        trellis_runtime::tls::inspect_certificate(&ca)
            .expect("inspect CA")
            .sha256_fingerprint,
        summary.ca_sha256_fingerprint
    );
    verify_with_installation_ca(&ca, &runtime_leaf, "eta.oats").expect("Console host");
    verify_with_installation_ca(&ca, &runtime_leaf, "auth.eta.oats").expect("OAuth host");
    verify_with_installation_ca(&ca, &nats_leaf, "nats.eta.oats").expect("WebSocket host");
    verify_with_installation_ca(&ca, &nats_leaf, "192.0.2.10").expect("native broker IP");
    verify_with_installation_ca(&ca, &runtime_leaf, "other.example")
        .expect_err("a leaf must not be accepted for another host");

    // Name constraints stop even the CA key from minting a trusted certificate elsewhere.
    let ca_key =
        rcgen::KeyPair::from_pem(&fs::read_to_string(ca_directory.join("ca.key")).expect("CA key"))
            .expect("parse CA key");
    let issuer = rcgen::Issuer::from_ca_cert_pem(&fs::read_to_string(&ca).unwrap(), ca_key)
        .expect("CA issuer");
    let rogue_key = rcgen::KeyPair::generate().unwrap();
    let rogue = rcgen::CertificateParams::new(vec!["bank.example".to_owned()])
        .unwrap()
        .signed_by(&rogue_key, &issuer)
        .unwrap();
    verify_with_installation_ca(&ca, rogue.pem().as_bytes(), "bank.example")
        .expect_err("the installation CA must not be trusted outside its hosts");

    let renewed = renew_generated_certificates(&out, &ca_directory).expect("renew");
    assert_eq!(renewed.len(), 2);
    let renewed_leaf = fs::read(out.join("tls/runtime.crt")).expect("renewed leaf");
    assert_ne!(renewed_leaf, runtime_leaf, "renewal must replace the leaf");
    verify_with_installation_ca(&ca, &renewed_leaf, "auth.eta.oats")
        .expect("renewed leaf keeps its names under the unchanged CA");
    assert_eq!(
        fs::read(&ca).unwrap(),
        fs::read(ca_directory.join("ca.crt")).unwrap()
    );

    let other = temp.path().join("other-ca");
    let mut unrelated = trellis_options(temp.path().join("unrelated"));
    unrelated.runtime.tls = BootstrapTls::Generated {
        ca_directory: Some(other.clone()),
    };
    generate_trellis_bootstrap(&unrelated).expect("unrelated bundle");
    assert!(matches!(
        renew_generated_certificates(&out, &other),
        Err(BootstrapError::InvalidTls(_))
    ));
}

#[test]
fn generated_ca_directory_must_not_overlap_the_bundle() {
    let temp = tempfile::tempdir().expect("temp dir");
    let bundle = temp.path().join("bundle");
    fs::create_dir(&bundle).expect("empty bundle directory");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&bundle, temp.path().join("link")).expect("symlink");
    let mut cases = vec![
        (bundle.clone(), bundle.join("ca")),
        (bundle.clone(), bundle.clone()),
        (bundle.clone(), temp.path().join("elsewhere/../bundle/tls")),
        (temp.path().join("ca/bundle"), temp.path().join("ca")),
    ];
    #[cfg(unix)]
    cases.push((bundle.clone(), temp.path().join("link/ca")));
    for (out, ca_directory) in cases {
        let mut options = trellis_options(&out);
        options.runtime.tls = BootstrapTls::Generated {
            ca_directory: Some(ca_directory.clone()),
        };
        let result = generate_trellis_bootstrap(&options);
        assert!(
            matches!(&result, Err(BootstrapError::InvalidTls(message)) if message.contains("overlap")),
            "{} with CA {}: {result:?}",
            out.display(),
            ca_directory.display()
        );
        for directory in [&out, &ca_directory] {
            assert!(
                !directory.exists() || fs::read_dir(directory).unwrap().next().is_none(),
                "{} must stay empty",
                directory.display()
            );
        }
    }
}

#[test]
fn a_bundle_keeps_its_ca_in_its_own_folder_and_renews_from_it() {
    let temp = tempfile::tempdir().expect("temp dir");
    let out = temp.path().join("bundle");
    let mut options = trellis_options(&out);
    options.runtime.tls = BootstrapTls::Generated {
        ca_directory: Some(out.join(crate::BUNDLE_CA_DIRECTORY)),
    };
    generate_trellis_bootstrap(&options).expect("bundle with its CA inside");
    let runtime_leaf = fs::read(out.join("tls/runtime.crt")).expect("runtime leaf");

    let ca_directory = crate::bundle_ca_directory(&out);
    assert_eq!(ca_directory, out.join("tls-ca"));
    renew_generated_certificates(&out, &ca_directory).expect("renew from the bundle's CA");
    let renewed_leaf = fs::read(out.join("tls/runtime.crt")).expect("renewed leaf");
    assert_ne!(renewed_leaf, runtime_leaf);
    verify_with_installation_ca(&out.join("tls/ca.crt"), &renewed_leaf, "localhost")
        .expect("renewed leaf is trusted by the unchanged CA");
}

#[test]
fn bundle_tls_modes_reject_endpoints_they_cannot_protect() {
    let temp = tempfile::tempdir().expect("temp dir");
    let mut plaintext_broker = trellis_options(temp.path().join("plaintext-broker"));
    plaintext_broker.runtime.nats_server_url = "nats://localhost:4222".into();
    assert!(matches!(
        generate_trellis_bootstrap(&plaintext_broker),
        Err(BootstrapError::InvalidTls(_))
    ));

    let mut remote_loopback = trellis_options(temp.path().join("remote-loopback"));
    remote_loopback.runtime.tls = BootstrapTls::LoopbackDevelopment;
    remote_loopback.runtime.public_origin = "http://eta.oats:3000".into();
    remote_loopback.runtime.oauth_origin = "http://localhost:3001".into();
    remote_loopback.runtime.nats_server_url = "nats://localhost:4222".into();
    remote_loopback.runtime.nats_websocket_url = "ws://localhost:8080".into();
    assert!(matches!(
        generate_trellis_bootstrap(&remote_loopback),
        Err(BootstrapError::InvalidTls(_))
    ));
    assert!(!temp.path().join("remote-loopback").exists());
}

fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
