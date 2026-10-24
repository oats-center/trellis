use crate::cli::*;
use crate::output;
use miette::{miette, IntoDiagnostic};
use serde_json::json;
use trellis_bootstrap::{
    generate_trellis_bootstrap, BootstrapError, BootstrapTls, SuppliedTls, TrellisBootstrapOptions,
};

pub(super) async fn init(format: OutputFormat, command: InitCommand) -> miette::Result<()> {
    match command.command {
        InitSubcommand::Config(args) => init_config_command(format, &args),
        InitSubcommand::Admin(args) => init_admin_command(format, &args).await,
    }
}

async fn init_admin_command(format: OutputFormat, args: &InitAdminArgs) -> miette::Result<()> {
    let defaults = if args.system {
        trellis_runtime::RuntimePathDefaults::system_profile()
    } else {
        trellis_runtime::RuntimePathDefaults::user_profile(|name| std::env::var_os(name))
    }
    .into_diagnostic()?;
    let (config, _) =
        trellis_runtime::RuntimeConfig::load_from_path_with_defaults(&args.config, defaults)
            .into_diagnostic()?;
    let password = if args.password_stdin {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).into_diagnostic()?;
        let input = input.trim_end_matches(['\n', '\r']).to_owned();
        if input.is_empty() {
            return Err(miette!("--password-stdin received an empty password"));
        }
        Some(input)
    } else {
        args.password.clone()
    };
    let (principal, account_url) = trellis_runtime::platform::initialize_admin_credentials(
        &config,
        &args.username,
        password.as_deref(),
    )
    .await
    .into_diagnostic()?;
    if matches!(format, OutputFormat::Json) {
        output::print_json(&json!({"principalId":principal,"adminAccountUrl":account_url}))?;
    } else {
        output::print_success(&format!("initialized administrator '{principal}'"));
        if let Some(url) = account_url {
            output::print_info(&format!("adminAccountUrl={url}"));
        }
    }
    Ok(())
}

fn init_config_command(format: OutputFormat, args: &InitConfigArgs) -> miette::Result<()> {
    let mut options = TrellisBootstrapOptions::new(args.out.clone());
    options.force = args.force;
    options.runtime.name = args.name.clone();
    options.runtime.trellis_port = args.trellis_port;
    let loopback = args.tls == InitTlsMode::LoopbackDevelopment;
    let (http, native, websocket) = if loopback {
        ("http", "nats", "ws")
    } else {
        ("https", "tls", "wss")
    };
    options.runtime.nats_server_url = args
        .nats_server_url
        .clone()
        .unwrap_or_else(|| format!("{native}://localhost:{}", args.nats_port));
    options.runtime.nats_websocket_url = args
        .nats_websocket_url
        .clone()
        .unwrap_or_else(|| format!("{websocket}://localhost:{}", args.nats_ws_port));
    options.runtime.public_origin = args
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://localhost:{}", args.trellis_port));
    options.runtime.oauth_origin = args
        .oauth_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://localhost:{}", args.oauth_port));
    options.runtime.oauth_port = args.oauth_port;
    options.runtime.tls = match args.tls {
        InitTlsMode::Generated => BootstrapTls::Generated {
            ca_directory: args.tls_ca_dir.clone(),
        },
        InitTlsMode::Supplied => BootstrapTls::Supplied(SuppliedTls {
            certificate_chain: required_path(
                &args.tls_certificate_chain,
                "--tls-certificate-chain",
            )?,
            private_key: required_path(&args.tls_private_key, "--tls-private-key")?,
            nats_certificate_chain: required_path(
                &args.nats_tls_certificate_chain,
                "--nats-tls-certificate-chain",
            )?,
            nats_private_key: required_path(&args.nats_tls_private_key, "--nats-tls-private-key")?,
            ca_certificate: args.tls_ca_certificate.clone(),
        }),
        InitTlsMode::LoopbackDevelopment => BootstrapTls::LoopbackDevelopment,
    };
    options.runtime.trusted_proxies = args.trusted_proxy.clone();
    options.runtime.nats_websocket_behind_proxy = args.nats_websocket_behind_proxy;
    options.runtime.extra_origins = args.extra_origin.clone();
    options.runtime.bind_address = args.bind_address;
    options.runtime.authorization = trellis_bootstrap::BootstrapAuthorizationPolicy {
        context_lifetime_seconds: args.auth_context_lifetime_seconds,
        oauth_admission_lifetime_seconds: args.auth_oauth_admission_lifetime_seconds,
        allowed_clock_skew_seconds: args.auth_allowed_clock_skew_seconds,
    };
    options.nats.nats_port = args.nats_port;
    options.nats.monitor_port = args.nats_monitor_port;
    options.nats.websocket_port = args.nats_ws_port;
    options.nats.names.operator_name = args.operator_name.clone();
    options.nats.names.system_account = args.system_account.clone();
    options.nats.names.auth_account = args.auth_account.clone();
    options.nats.names.trellis_account = args.trellis_account.clone();
    options.nats.names.server_name = args.server_name.clone();

    if args
        .nats_data_dir
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(miette!("--nats-data-dir must be absolute"));
    }
    let generated = generate_trellis_bootstrap(&options).map_err(bootstrap_report)?;
    if let Some(data) = &args.nats_data_dir {
        let nats = args.out.join("nats");
        std::fs::create_dir_all(data.join("jwt")).into_diagnostic()?;
        let jwt = std::fs::read_to_string(nats.join("jwt.conf")).into_diagnostic()?;
        std::fs::write(
            nats.join("jwt.conf"),
            trellis_bootstrap::render_local_jwt_config(
                &jwt,
                &data.join("jwt").display().to_string(),
            ),
        )
        .into_diagnostic()?;
        std::fs::write(
            nats.join("nats.conf"),
            trellis_bootstrap::render_local_nats_config(
                args.server_name.as_deref().unwrap_or("trellis"),
                &data.display().to_string(),
                "./jwt.conf",
                args.nats_port,
                args.nats_ws_port,
                args.nats_monitor_port,
                (!loopback)
                    .then(|| trellis_bootstrap::NatsTlsFiles {
                        certificate: nats.join("tls/nats.crt").display().to_string(),
                        private_key: nats.join("tls/nats.key").display().to_string(),
                        websocket_behind_proxy: args.nats_websocket_behind_proxy,
                    })
                    .as_ref(),
            ),
        )
        .into_diagnostic()?;
    }
    let trellis_config = args.out.join("config.toml");
    let nats_config = args.out.join("nats/nats.conf");
    if output::is_json(format) {
        output::print_json(&json!({
            "generated": true,
            "out": args.out.display().to_string(),
            "trellisConfig": trellis_config.display().to_string(),
            "natsConfig": nats_config.display().to_string(),
            "trellisRuntimeCreds": args.out.join("nats/creds/trellis-auth.creds").display().to_string(),
            "publicOrigin": options.runtime.public_origin,
            "natsServer": options.runtime.nats_server_url,
            "natsWebsocket": options.runtime.nats_websocket_url,
            "oauthOrigin": options.runtime.oauth_origin,
            "generatedTls": generated.as_ref().map(|tls| json!({
                "caCertificate": tls.ca_certificate.display().to_string(),
                "caSha256Fingerprint": tls.ca_sha256_fingerprint,
                "leafNotAfter": rfc3339(tls.leaf_not_after),
            })),
        }))?;
        return Ok(());
    }

    output::print_success("generated Trellis bootstrap files");
    output::print_info(&format!("out={}", args.out.display()));
    output::print_info(&format!("trellisConfig={}", trellis_config.display()));
    output::print_info(&format!("natsConfig={}", nats_config.display()));
    output::print_info(&format!("publicOrigin={}", options.runtime.public_origin));
    output::print_info(&format!("natsServer={}", options.runtime.nats_server_url));
    output::print_info(&format!(
        "natsWebsocket={}",
        options.runtime.nats_websocket_url
    ));
    output::print_info(&format!("oauthOrigin={}", options.runtime.oauth_origin));
    if let Some(tls) = &generated {
        warn_generated_tls(
            &tls.ca_certificate,
            &tls.ca_sha256_fingerprint,
            tls.leaf_not_after,
        );
    }
    Ok(())
}

fn required_path(
    value: &Option<std::path::PathBuf>,
    flag: &str,
) -> miette::Result<std::path::PathBuf> {
    value
        .clone()
        .ok_or_else(|| miette!("{flag} is required with --tls supplied"))
}

fn rfc3339(value: time::OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| value.to_string())
}

/// Always-on warning for installations serving their generated CA.
fn warn_generated_tls(
    ca: &std::path::Path,
    fingerprint: &str,
    leaf_not_after: time::OffsetDateTime,
) {
    output::print_warning(&format!(
        "this installation uses Trellis-generated TLS from its own private \
        CA.\n  Browsers and native clients must trust {} explicitly.\n  Verify \
        this SHA-256 fingerprint out of band before trusting it:\n  \
        {fingerprint}\n  Leaf certificates expire {}; renew them with `trellis \
        tls renew`.",
        ca.display(),
        rfc3339(leaf_not_after),
    ));
}

pub(super) fn tls(format: OutputFormat, command: TlsCommand) -> miette::Result<()> {
    match command.command {
        TlsSubcommand::Renew(args) => {
            let ca_dir = args.ca_dir.clone().unwrap_or_else(|| {
                let mut name = args
                    .bundle
                    .file_name()
                    .map(std::ffi::OsStr::to_os_string)
                    .unwrap_or_else(|| "trellis".into());
                name.push("-tls-ca");
                args.bundle.with_file_name(name)
            });
            let renewed = trellis_bootstrap::renew_generated_certificates(&args.bundle, &ca_dir)
                .map_err(bootstrap_report)?;
            if output::is_json(format) {
                output::print_json(&json!({
                    "renewed": renewed.iter().map(|certificate| json!({
                        "certificate": certificate.certificate.display().to_string(),
                        "notAfter": rfc3339(certificate.not_after),
                    })).collect::<Vec<_>>(),
                }))?;
                return Ok(());
            }
            for certificate in &renewed {
                output::print_success(&format!(
                    "renewed {} (expires {})",
                    certificate.certificate.display(),
                    rfc3339(certificate.not_after)
                ));
            }
            output::print_info(
                "the runtime reloads replaced certificates automatically; reload NATS \
                (`nats-server --signal reload` or restart its unit) to serve its \
                replacement",
            );
            Ok(())
        }
        TlsSubcommand::Status(args) => {
            let config =
                trellis_runtime::RuntimeConfig::load_from_path(&args.config).into_diagnostic()?;
            let notice = trellis_runtime::tls::GeneratedTlsNotice::from_config(
                &config,
                config.http_transport(),
            )
            .into_diagnostic()?;
            if output::is_json(format) {
                output::print_json(&json!({
                    "generatedTls": notice.as_ref().map(|notice| json!({
                        "caSha256Fingerprint": notice.ca.sha256_fingerprint,
                        "caNotAfter": rfc3339(notice.ca.not_after),
                        "leafNotAfter": notice.leaf.as_ref().map(|leaf| rfc3339(leaf.not_after)),
                    })),
                }))?;
                return Ok(());
            }
            match notice {
                Some(notice) => {
                    let ca = config
                        .tls
                        .as_ref()
                        .and_then(|tls| tls.generated_ca_certificate.clone())
                        .unwrap_or_default();
                    warn_generated_tls(
                        &ca,
                        &notice.ca.sha256_fingerprint,
                        notice
                            .leaf
                            .as_ref()
                            .map_or(notice.ca.not_after, |leaf| leaf.not_after),
                    );
                }
                None => output::print_info("this installation does not use Trellis-generated TLS"),
            }
            Ok(())
        }
    }
}

fn bootstrap_report(error: BootstrapError) -> miette::Report {
    miette::Report::new(error)
}

#[cfg(test)]
mod tests {
    use super::init_config_command;
    use crate::cli::{Cli, InitConfigArgs, InitSubcommand, OutputFormat, TopLevelCommand};
    use clap::Parser as _;
    use std::path::Path;

    #[test]
    fn init_config_maps_bootstrap_policies_into_effective_runtime_configuration() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, listeners) = generate_bundle(
            &temp.path().join("custom"),
            &[
                "--trellis-port",
                "3444",
                "--nats-port",
                "4333",
                "--nats-monitor-port",
                "8333",
                "--nats-ws-port",
                "8183",
                "--bind-address",
                "127.0.0.1",
                "--auth-context-lifetime-seconds",
                "90",
                "--auth-oauth-admission-lifetime-seconds",
                "12",
                "--auth-allowed-clock-skew-seconds",
                "2",
            ],
        );
        let http = config.http.as_ref().expect("http");
        assert_eq!(
            http.public_origin.as_deref(),
            Some("https://localhost:3444")
        );
        assert_eq!(http.bind_address, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "tls://localhost:4333"
        );
        assert_eq!(
            (listeners.native, listeners.monitor, listeners.websocket),
            (4333, 8333, 8183)
        );
        assert_eq!(
            config
                .client
                .as_ref()
                .unwrap()
                .ws_nats_servers
                .as_ref()
                .unwrap(),
            &["wss://localhost:8183"]
        );
        let auth = config.resolve_authorization().unwrap();
        assert_eq!(auth.context_lifetime_seconds, 90);
        assert_eq!(auth.oauth_admission_lifetime_seconds, 12);
        assert_eq!(auth.allowed_clock_skew_seconds, 2);
    }

    #[test]
    fn init_config_explicit_advertised_urls_override_derived_values() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, _) = generate_bundle(
            &temp.path().join("explicit"),
            &[
                "--nats-port",
                "4333",
                "--nats-server-url",
                "tls://nats.example.test:4999",
                "--nats-websocket-url",
                "wss://nats.example.test/ws",
                "--public-origin",
                "https://trellis.example.test",
            ],
        );
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "tls://nats.example.test:4999"
        );
        assert_eq!(
            config
                .client
                .as_ref()
                .unwrap()
                .ws_nats_servers
                .as_ref()
                .unwrap(),
            &["wss://nats.example.test/ws"]
        );
        assert_eq!(
            config.http.as_ref().unwrap().public_origin.as_deref(),
            Some("https://trellis.example.test")
        );
    }

    #[test]
    fn init_config_adds_extra_origins() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, _) = generate_bundle(
            &temp.path().join("extra-origin"),
            &["--extra-origin", "http://localhost:5174"],
        );
        let http = config.http.unwrap();
        assert!(http
            .origins
            .unwrap()
            .iter()
            .any(|origin| origin == "http://localhost:5174"));
    }

    #[test]
    fn init_config_rejects_zero_duplicate_and_trellis_collisions_without_writing() {
        let temp = tempfile::tempdir().expect("temp dir");
        for (name, flags) in [
            ("zero", vec!["--nats-port", "0"]),
            (
                "duplicate",
                vec!["--nats-port", "4333", "--nats-monitor-port", "4333"],
            ),
            ("collision", vec!["--nats-port", "3000"]),
        ] {
            let out = temp.path().join(name);
            let args = init_config_args(&out, &flags);
            init_config_command(OutputFormat::Json, &args)
                .expect_err("invalid listener ports must be rejected");
            assert!(
                !out.exists(),
                "{name}: rejected configuration must not write output"
            );
        }
    }

    fn init_config_args(out: &Path, flags: &[&str]) -> InitConfigArgs {
        let out = out.to_str().expect("UTF-8 output path").to_string();
        let mut arguments = vec![
            "trellis".to_string(),
            "init".to_string(),
            "config".to_string(),
            "--out".to_string(),
            out,
        ];
        arguments.extend(flags.iter().map(|flag| (*flag).to_string()));
        let cli = Cli::parse_from(&arguments);
        match cli.command {
            TopLevelCommand::Init(command) => match command.command {
                InitSubcommand::Config(args) => *args,
                other => panic!("unexpected init command: {other:?}"),
            },
            other => panic!("unexpected top-level command: {other:?}"),
        }
    }

    fn generate_bundle(
        out: &Path,
        flags: &[&str],
    ) -> (
        trellis_runtime::RuntimeConfig,
        trellis_bootstrap::NatsListeners,
    ) {
        let args = init_config_args(out, flags);
        init_config_command(OutputFormat::Json, &args).expect("generate bundle");
        (
            trellis_runtime::RuntimeConfig::load_from_path(out.join("config.toml"))
                .expect("load production runtime config"),
            trellis_bootstrap::read_nats_listen_ports(&out.join("nats/nats.conf"))
                .expect("resolve native listener configuration"),
        )
    }
}
