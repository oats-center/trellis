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
    options.runtime.nats_server_url = args.nats_server_url.clone();
    options.runtime.nats_websocket_url = args.nats_websocket_url.clone();
    options.runtime.public_origin = args.public_origin.clone();
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
    use super::{identity_id_for_provider_subject, seed_admin_user_in_connection};
    use rusqlite::{params, Connection};

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
