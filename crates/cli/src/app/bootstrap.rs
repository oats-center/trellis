use crate::cli::*;
use crate::output;
use miette::{miette, IntoDiagnostic};
use serde_json::json;
use trellis_bootstrap::{generate_trellis_bootstrap, BootstrapError, TrellisBootstrapOptions};

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
    options.runtime.nats_server_url = args
        .nats_server_url
        .clone()
        .unwrap_or_else(|| format!("nats://127.0.0.1:{}", args.nats_port));
    options.runtime.nats_websocket_url = args
        .nats_websocket_url
        .clone()
        .unwrap_or_else(|| format!("ws://localhost:{}", args.nats_ws_port));
    options.runtime.public_origin = args
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("http://localhost:{}", args.trellis_port));
    options.runtime.extra_origins = args.extra_origin.clone();
    options.runtime.bind_address = args.bind_address;
    options.runtime.rate_limit_max = args.rate_limit_max;
    options.runtime.rate_limit_window_ms = args.rate_limit_window_ms;
    for (target, directory, proxy) in [
        (
            &mut options.runtime.web_source,
            &args.web_directory,
            &args.web_proxy,
        ),
        (
            &mut options.runtime.portal_source,
            &args.portal_directory,
            &args.portal_proxy,
        ),
        (
            &mut options.runtime.console_source,
            &args.console_directory,
            &args.console_proxy,
        ),
    ] {
        *target = directory
            .as_ref()
            .map(|path| {
                path.canonicalize()
                    .map(trellis_runtime::WebSourceConfig::Directory)
            })
            .transpose()
            .into_diagnostic()?
            .or_else(|| {
                proxy
                    .as_ref()
                    .map(|url| trellis_runtime::WebSourceConfig::Proxy(url.clone()))
            });
    }
    options.runtime.authorization = trellis_bootstrap::BootstrapAuthorizationPolicy {
        context_lifetime_seconds: args.auth_context_lifetime_seconds,
        refresh_lead_seconds: args.auth_refresh_lead_seconds,
        refresh_jitter_seconds: args.auth_refresh_jitter_seconds,
        minimum_context_lifetime_seconds: args.auth_minimum_context_lifetime_seconds,
    };
    for path in &args.oauth_providers_file {
        for (id, provider) in
            trellis_bootstrap::read_oauth_providers(path).map_err(bootstrap_report)?
        {
            if options
                .runtime
                .oauth_providers
                .insert(id, provider)
                .is_some()
            {
                return Err(miette!("duplicate OAuth provider id in input files"));
            }
        }
    }
    options.nats.nats_port = args.nats_port;
    options.nats.monitor_port = args.nats_monitor_port;
    options.nats.websocket_port = args.nats_ws_port;
    options.nats.names.operator_name = args.operator_name.clone();
    options.nats.names.system_account = args.system_account.clone();
    options.nats.names.auth_account = args.auth_account.clone();
    options.nats.names.trellis_account = args.trellis_account.clone();
    options.nats.names.server_name = args.server_name.clone();

    generate_trellis_bootstrap(&options).map_err(bootstrap_report)?;
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
    Ok(())
}

fn bootstrap_report(error: BootstrapError) -> miette::Report {
    miette::Report::new(error)
}

#[cfg(test)]
mod tests {
    use super::init_config_command;
    use crate::cli::{Cli, InitConfigArgs, InitSubcommand, OutputFormat, TopLevelCommand};
    use clap::Parser as _;
    use std::fs;
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
                "--rate-limit-max",
                "37",
                "--rate-limit-window-ms",
                "2300",
                "--auth-context-lifetime-seconds",
                "90",
                "--auth-refresh-lead-seconds",
                "10",
                "--auth-refresh-jitter-seconds",
                "2",
                "--auth-minimum-context-lifetime-seconds",
                "43",
            ],
        );
        let http = config.http.as_ref().expect("http");
        assert_eq!(http.public_origin.as_deref(), Some("http://localhost:3444"));
        assert_eq!(http.bind_address, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(http.rate_limit_max, Some(37));
        assert_eq!(http.rate_limit_window_ms, Some(2300));
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "nats://127.0.0.1:4333"
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
            &["ws://localhost:8183"]
        );
        let auth = config.resolve_authorization().unwrap();
        assert_eq!(
            (
                auth.context_lifetime_seconds,
                auth.refresh_lead_seconds,
                auth.refresh_jitter_seconds,
                auth.minimum_context_lifetime_seconds
            ),
            (90, 10, 2, 43)
        );
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
                "nats://nats.example.test:4999",
                "--nats-websocket-url",
                "wss://nats.example.test/ws",
                "--public-origin",
                "https://trellis.example.test",
            ],
        );
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "nats://nats.example.test:4999"
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
        assert!(http
            .allow_insecure_origins
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

    #[test]
    fn oauth_provider_files_round_trip_and_rejected_input_preserves_existing_bundle() {
        let temp = tempfile::tempdir().expect("temp dir");
        let input = temp.path().join("providers.json");
        fs::write(&input, r#"{"identity":{"type":"oidc","issuer":"https://identity.example.test","clientId":"consumer","clientSecret":"private-provider-secret","scopes":["openid","email"],"roleClaims":["/roles"]}}"#).unwrap();
        let out = temp.path().join("bundle");
        let input_path = input.to_str().unwrap();
        let (config, _) = generate_bundle(&out, &["--oauth-providers-file", input_path]);
        let provider = &config.oauth.as_ref().unwrap().providers["identity"];
        assert_eq!(
            provider.issuer.as_deref(),
            Some("https://identity.example.test")
        );
        assert_eq!(provider.client_id.as_deref(), Some("consumer"));
        assert_eq!(
            provider.client_secret.as_deref(),
            Some("private-provider-secret")
        );
        assert_eq!(provider.scopes.as_ref().unwrap(), &["openid", "email"]);
        assert_eq!(provider.role_claims, ["/roles"]);
        let before = fs::read(out.join("config.toml")).unwrap();
        let duplicate = init_config_args(
            &out,
            &[
                "--force",
                "--oauth-providers-file",
                input_path,
                "--oauth-providers-file",
                input_path,
            ],
        );
        init_config_command(OutputFormat::Json, &duplicate).expect_err("duplicate providers");
        assert_eq!(fs::read(out.join("config.toml")).unwrap(), before);
        fs::write(&input, r#"{"identity":{"type":"oidc","clientId":"consumer","clientSecret":{"value":"do-not-echo-this-secret"}}}"#).unwrap();
        let invalid = init_config_args(&out, &["--force", "--oauth-providers-file", input_path]);
        let error =
            init_config_command(OutputFormat::Json, &invalid).expect_err("invalid private input");
        assert!(!format!("{error:?}").contains("do-not-echo-this-secret"));
        assert_eq!(fs::read(out.join("config.toml")).unwrap(), before);
    }

    #[test]
    fn web_source_flags_round_trip_and_invalid_source_preserves_output() {
        let temp = tempfile::tempdir().unwrap();
        let web = temp.path().join("web");
        fs::create_dir(&web).unwrap();
        let out = temp.path().join("bundle");
        let (config, _) = generate_bundle(
            &out,
            &[
                "--web-directory",
                web.to_str().unwrap(),
                "--portal-proxy",
                "http://127.0.0.1:5199",
            ],
        );
        let http = config.http.unwrap();
        assert_eq!(
            http.web_source,
            Some(trellis_runtime::WebSourceConfig::Directory(
                web.canonicalize().unwrap()
            ))
        );
        assert_eq!(
            http.portal_source,
            Some(trellis_runtime::WebSourceConfig::Proxy(
                "http://127.0.0.1:5199".into()
            ))
        );
        let before = fs::read(out.join("config.toml")).unwrap();
        let args = init_config_args(&out, &["--force", "--web-proxy", "ftp://example.test"]);
        init_config_command(OutputFormat::Json, &args).expect_err("invalid proxy scheme");
        assert_eq!(fs::read(out.join("config.toml")).unwrap(), before);
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
