use super::*;
use std::fs;

#[test]
fn slug_from_name_slugs_trellis_name() {
    assert_eq!(slug_from_name("Trellis"), "trellis");
    assert_eq!(slug_from_name("Acme Trellis"), "acme-trellis");
    assert_eq!(slug_from_name("  Acme__Trellis!!  "), "acme-trellis");
    assert_eq!(slug_from_name("!!!"), "trellis");
}

#[test]
fn nats_config_render_round_trips_custom_ports() {
    let config = render_nats_config(
        "trellis",
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        },
        Some(&NatsTlsFiles {
            certificate: "/etc/nats/tls/nats.crt".into(),
            private_key: "/etc/nats/tls/nats.key".into(),
        }),
        NatsWebsocket::Tls,
        false,
        "",
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
        "",
        NatsListeners {
            native: 4322,
            monitor: 8322,
            websocket: 8180,
        },
        Some(&NatsTlsFiles {
            certificate: "/tmp/trellis/nats/tls/nats.crt".into(),
            private_key: "/tmp/trellis/nats/tls/nats.key".into(),
        }),
        NatsWebsocket::BehindProxy,
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

fn parse_config(config: &str) -> Result<NatsListeners, NatsConfigError> {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("nats.conf");
    fs::write(&path, config).expect("write nats config");
    read_nats_listen_ports(&path)
}

#[test]
fn local_nats_config_quotes_host_paths_with_spaces() {
    let config = render_local_nats_config(
        "trellis",
        "/tmp/trellis data/store dir",
        "",
        NatsListeners {
            native: 4222,
            monitor: 8222,
            websocket: 8080,
        },
        None,
        NatsWebsocket::Loopback,
    );
    assert!(config.contains("store_dir: \"/tmp/trellis data/store dir\""));

    let rendered = nats_accounts(
        "listen: 1\noperator: x\nresolver: {\n  dir: /data/jwt\n}\n",
        "a\"b\\c",
    );
    assert_eq!(
        rendered,
        "operator: x\nresolver: {\n  dir: \"a\\\"b\\\\c\"\n}\n"
    );
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
fn generated_tls_names_only_its_hosts_and_keeps_its_ca_across_renewals() {
    let temp = tempfile::tempdir().expect("temp dir");
    let directory = temp.path().join("tls");
    let hosts = vec!["eta.oats".to_owned(), "console.eta.oats".to_owned()];
    let renew_within = time::Duration::days(30);
    let first = ensure_generated_tls(&directory, "Test CA", &hosts, renew_within).expect("create");
    let ca = fs::read(&first.ca_certificate).expect("CA");
    let leaf = fs::read(&first.certificate_chain).expect("leaf");
    verify_with_installation_ca(&first.ca_certificate, &leaf, "eta.oats").expect("Trellis host");
    verify_with_installation_ca(&first.ca_certificate, &leaf, "console.eta.oats")
        .expect("Console host");
    verify_with_installation_ca(&first.ca_certificate, &leaf, "other.example")
        .expect_err("a leaf must not be accepted for another host");

    // Name constraints stop even the CA key from minting a trusted certificate elsewhere.
    let ca_key =
        rcgen::KeyPair::from_pem(&fs::read_to_string(directory.join("ca.key")).expect("CA key"))
            .expect("parse CA key");
    let issuer = rcgen::Issuer::from_ca_cert_pem(
        &fs::read_to_string(&first.ca_certificate).unwrap(),
        ca_key,
    )
    .expect("CA issuer");
    let rogue_key = rcgen::KeyPair::generate().unwrap();
    let rogue = rcgen::CertificateParams::new(vec!["bank.example".to_owned()])
        .unwrap()
        .signed_by(&rogue_key, &issuer)
        .unwrap();
    verify_with_installation_ca(
        &first.ca_certificate,
        rogue.pem().as_bytes(),
        "bank.example",
    )
    .expect_err("the CA must not be trusted outside its hosts");

    // A current certificate is kept; one close to expiry is reissued from the
    // same CA, so clients that trust it keep working.
    ensure_generated_tls(&directory, "Test CA", &hosts, renew_within).expect("keep");
    assert_eq!(fs::read(&first.certificate_chain).unwrap(), leaf);
    let renewed = ensure_generated_tls(&directory, "Test CA", &hosts, time::Duration::days(365))
        .expect("renew");
    let renewed_leaf = fs::read(&renewed.certificate_chain).unwrap();
    assert_ne!(renewed_leaf, leaf);
    assert_eq!(fs::read(&renewed.ca_certificate).unwrap(), ca);
    verify_with_installation_ca(&renewed.ca_certificate, &renewed_leaf, "console.eta.oats")
        .expect("renewed leaf under the unchanged CA");

    // New hosts need a CA that may issue for them.
    let moved = ensure_generated_tls(
        &directory,
        "Test CA",
        &["new.oats".to_owned()],
        renew_within,
    )
    .expect("new hosts");
    assert_ne!(fs::read(&moved.ca_certificate).unwrap(), ca);
    verify_with_installation_ca(
        &moved.ca_certificate,
        &fs::read(&moved.certificate_chain).unwrap(),
        "new.oats",
    )
    .expect("new host");
}
