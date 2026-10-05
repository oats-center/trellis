use std::fs;
use std::path::PathBuf;

use crate::cli::*;
use crate::output;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use miette::{miette, IntoDiagnostic};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use time::OffsetDateTime;
use trellis_bootstrap::{generate_trellis_bootstrap, BootstrapError, TrellisBootstrapOptions};
use ulid::Ulid;

pub(super) async fn init(format: OutputFormat, command: InitCommand) -> miette::Result<()> {
    match command.command {
        InitSubcommand::Config(args) => init_config_command(format, &args),
        InitSubcommand::Admin(args) => init_admin_command(format, &args).await,
    }
}

async fn init_admin_command(_format: OutputFormat, args: &InitAdminArgs) -> miette::Result<()> {
    let Some((provider, subject)) = args.identity.split_once(':') else {
        return Err(miette!("--identity must use PROVIDER:SUBJECT"));
    };
    bootstrap_admin_command(&args.db_path, provider, subject).await
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
    options.runtime.ttl_ms = trellis_runtime::PlatformTtlConfig {
        sessions: args.platform_sessions_ttl_ms,
        oauth: args.platform_oauth_ttl_ms,
        device_flow: args.platform_device_flow_ttl_ms,
        pending_auth: args.platform_pending_auth_ttl_ms,
    };
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

async fn bootstrap_admin_command(
    db_path: &PathBuf,
    provider: &str,
    subject: &str,
) -> miette::Result<()> {
    let capabilities = Vec::<String>::new();
    let capability_groups = vec!["admin".to_string()];

    let seed = seed_admin_user(
        db_path,
        provider,
        subject,
        &capabilities,
        &capability_groups,
    )?;

    output::print_success("bootstrapped admin user");
    output::print_info(&format!("dbPath={}", db_path.display()));
    output::print_info(&format!("userId={}", seed.user_id));
    output::print_info(&format!("identityId={}", seed.identity_id));
    output::print_info(&format!(
        "payload={}",
        json!({
            "userId": seed.user_id,
            "identity": {
                "identityId": seed.identity_id,
                "provider": provider,
                "subject": subject,
            },
            "active": true,
            "capabilities": capabilities,
            "capabilityGroups": capability_groups,
        })
    ));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeededAdminUser {
    user_id: String,
    identity_id: String,
}

fn seed_admin_user(
    db_path: &PathBuf,
    provider: &str,
    subject: &str,
    capabilities: &[String],
    capability_groups: &[String],
) -> miette::Result<SeededAdminUser> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).into_diagnostic()?;
    }

    let conn = Connection::open(db_path).into_diagnostic()?;
    seed_admin_user_in_connection(&conn, provider, subject, capabilities, capability_groups)
}

fn seed_admin_user_in_connection(
    conn: &Connection,
    provider: &str,
    subject: &str,
    capabilities: &[String],
    capability_groups: &[String],
) -> miette::Result<SeededAdminUser> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users (
          id TEXT PRIMARY KEY,
          user_id TEXT NOT NULL UNIQUE,
          name TEXT,
          email TEXT,
          active INTEGER NOT NULL,
          capabilities TEXT NOT NULL,
          capability_groups TEXT NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        )",
        [],
    )
    .into_diagnostic()?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS user_identities (
          id TEXT PRIMARY KEY,
          identity_id TEXT NOT NULL UNIQUE,
          user_id TEXT NOT NULL,
          provider TEXT NOT NULL,
          subject TEXT NOT NULL,
          display_name TEXT,
          email TEXT,
          email_verified INTEGER NOT NULL,
          linked_at TEXT NOT NULL,
          last_login_at TEXT,
          UNIQUE(provider, subject)
        )",
        [],
    )
    .into_diagnostic()?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS users_active_idx ON users(active)",
        [],
    )
    .into_diagnostic()?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS user_identities_user_id_idx ON user_identities(user_id)",
        [],
    )
    .into_diagnostic()?;

    let existing_user_id: Option<String> = conn
        .query_row(
            "SELECT user_id FROM user_identities WHERE provider = ?1 AND subject = ?2",
            params![provider, subject],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    let user_id = existing_user_id.unwrap_or_else(|| format!("usr_{}", Ulid::new()));
    let identity_id = identity_id_for_provider_subject(provider, subject);
    let now = OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .into_diagnostic()?;
    let capabilities_json = serde_json::to_string(capabilities).into_diagnostic()?;
    let capability_groups_json = serde_json::to_string(capability_groups).into_diagnostic()?;

    conn.execute(
        "INSERT INTO users (id, user_id, name, email, active, capabilities, capability_groups, created_at, updated_at)
         VALUES (?1, ?2, NULL, NULL, 1, ?3, ?4, ?5, ?5)
         ON CONFLICT(user_id) DO UPDATE SET
           active = excluded.active,
           capabilities = excluded.capabilities,
           capability_groups = excluded.capability_groups,
           updated_at = excluded.updated_at",
        params![
            Ulid::new().to_string(),
            &user_id,
            capabilities_json,
            capability_groups_json,
            now
        ],
    )
    .into_diagnostic()?;

    conn.execute(
        "INSERT INTO user_identities (id, identity_id, user_id, provider, subject, display_name, email, email_verified, linked_at, last_login_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, 0, ?6, NULL)
         ON CONFLICT(provider, subject) DO UPDATE SET
           identity_id = excluded.identity_id,
           user_id = excluded.user_id",
        params![
            Ulid::new().to_string(),
            &identity_id,
            &user_id,
            provider,
            subject,
            now
        ],
    )
    .into_diagnostic()?;

    Ok(SeededAdminUser {
        user_id,
        identity_id,
    })
}

fn identity_id_for_provider_subject(provider: &str, subject: &str) -> String {
    format!(
        "idn_{}",
        URL_SAFE_NO_PAD.encode(format!("{provider}:{subject}").as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::{
        identity_id_for_provider_subject, init_config_command, seed_admin_user_in_connection,
    };
    use crate::cli::{Cli, InitConfigArgs, InitSubcommand, OutputFormat, TopLevelCommand};
    use clap::Parser as _;
    use rusqlite::{params, Connection};
    use std::fs;
    use std::path::Path;

    #[test]
    fn seed_admin_user_uses_account_first_storage_shape() {
        let conn = Connection::open_in_memory().expect("open db");
        let seeded =
            seed_admin_user_in_connection(&conn, "github", "ada", &[], &["admin".to_string()])
                .expect("seed admin");

        assert!(seeded.user_id.starts_with("usr_"));
        assert_eq!(
            seeded.identity_id,
            identity_id_for_provider_subject("github", "ada")
        );

        let user_row: (String, String, i64, String, String) = conn
            .query_row(
                "SELECT user_id, capabilities, active, capability_groups, created_at FROM users",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("select user");
        assert_eq!(user_row.0, seeded.user_id);
        assert_eq!(user_row.1, "[]");
        assert_eq!(user_row.2, 1);
        assert_eq!(user_row.3, r#"["admin"]"#);
        assert!(!user_row.4.is_empty());

        let identity_row: (String, String, String, String, i64) = conn
            .query_row(
                "SELECT identity_id, user_id, provider, subject, email_verified FROM user_identities",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .expect("select identity");
        assert_eq!(identity_row.0, seeded.identity_id);
        assert_eq!(identity_row.1, seeded.user_id);
        assert_eq!(identity_row.2, "github");
        assert_eq!(identity_row.3, "ada");
        assert_eq!(identity_row.4, 0);
    }

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
                "--platform-sessions-ttl-ms",
                "45678",
                "--platform-oauth-ttl-ms",
                "23456",
                "--platform-device-flow-ttl-ms",
                "34567",
                "--platform-pending-auth-ttl-ms",
                "12345",
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
        let ttl = config.platform.as_ref().unwrap().ttl_ms.as_ref().unwrap();
        assert_eq!(
            (ttl.sessions, ttl.oauth, ttl.device_flow, ttl.pending_auth),
            (Some(45678), Some(23456), Some(34567), Some(12345))
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

    #[test]
    fn seed_admin_user_updates_existing_provider_subject() {
        let conn = Connection::open_in_memory().expect("open db");
        let first =
            seed_admin_user_in_connection(&conn, "github", "ada", &["admin".to_string()], &[])
                .expect("first seed");
        let second = seed_admin_user_in_connection(
            &conn,
            "github",
            "ada",
            &["trellis.core::contract.read".to_string()],
            &["admin".to_string()],
        )
        .expect("second seed");

        assert_eq!(second, first);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count users");
        assert_eq!(count, 1);
        let capabilities: String = conn
            .query_row(
                "SELECT capabilities FROM users WHERE user_id = ?1",
                params![second.user_id],
                |row| row.get(0),
            )
            .expect("select capabilities");
        assert_eq!(capabilities, r#"["trellis.core::contract.read"]"#);
        let capability_groups: String = conn
            .query_row(
                "SELECT capability_groups FROM users WHERE user_id = ?1",
                params![second.user_id],
                |row| row.get(0),
            )
            .expect("select capability groups");
        assert_eq!(capability_groups, r#"["admin"]"#);
    }
}
