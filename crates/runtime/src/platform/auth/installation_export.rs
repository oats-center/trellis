//! Moving a whole installation's configuration to another installation:
//! `trellis-server export` and `trellis-server import`.
//!
//! See `design/auth/installation-export.md`. An export reads the platform
//! database and the installation's NATS, and writes plain records, one entry
//! per record kind, each with its own format number, followed by one service
//! bundle per service and device deployment. Secrets are opened with this
//! installation's storage key and carried in the clear inside the
//! passphrase-sealed file; an import seals them with its own key.
//!
//! An import runs while the target's server is stopped and its NATS runs. It
//! recreates every record it can in one transaction, skipping and reporting
//! one that existing data is in the way of, then loads each imported
//! deployment's data as a resumed service import does. `--check` runs the
//! same transaction and rolls it back.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rusqlite::types::ValueRef;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};

use super::access_records::{self, AccessDocument, GrantRecord, RoleRecord, SignIn, UserRef};
use super::policy::{apply_policy_in_transaction, PolicyMutation};
use super::service_bundles;
use super::sqlite::{decode, id, json, Connection};
use super::{AuthError, Mutation, OAuthStorage};

/// The first bytes of the content, before the container's format number.
const MAGIC: &[u8; 16] = b"TRELLIS-INSTALL\n";
/// The container format this build writes and reads.
const FORMAT: u32 = 1;
/// The largest content chunk accepted, and the size this build writes.
const MAX_CHUNK: usize = 1 << 20;
const CHUNK: usize = 64 * 1024;
/// The largest record entry an import holds in memory.
const MAX_RECORDS: usize = 256 << 20;
/// Clients each installation registers for itself; only their sign-up
/// settings move.
const BUILTIN_CLIENTS: [&str; 2] = ["trellis.console", "trellis.cli"];

/// Every record kind with the format this build writes. An import converts
/// an older format of a kind to this one before it reads the records.
const KINDS: [(&str, u32); 10] = [
    ("signInSettings", 1),
    ("oidcProviders", 1),
    ("packageRegistries", 1),
    ("users", 1),
    ("roles", 1),
    ("grants", 1),
    ("oidcRoleMappings", 1),
    ("oauthClients", 1),
    ("consents", 1),
    ("portalThemes", 1),
];

/// What an installation export leaves out, in plain words.
const NOT_CARRIED: [&str; 9] = [
    "sign-in sessions and app sessions: everyone signs in again",
    "the NATS operator, accounts and credentials",
    "the installation's TLS certificates",
    "the issuer signing keys",
    "the security audit, and job, event, health and webhook request history",
    "earlier settings revisions",
    "password reset and setup links that were not used",
    "device activation reviews, and the values of State resources",
    "deleted users, and APIs no deployment provides",
];

fn damaged(detail: impl std::fmt::Display) -> AuthError {
    AuthError::Invalid(format!("the installation export is damaged: {detail}"))
}

fn io(error: std::io::Error) -> AuthError {
    AuthError::Invalid(format!("the installation export cannot be read: {error}"))
}

// ---------------------------------------------------------------------------
// Rows as plain JSON records.

/// Each row `sql` returns, as a JSON object of its columns. Blobs are
/// `{"base64": ...}`.
fn rows(
    connection: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<Vec<Map<String, Value>>, AuthError> {
    let mut statement = connection.prepare(sql)?;
    let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let mut found = statement.query(values)?;
    let mut out = Vec::new();
    while let Some(row) = found.next()? {
        let mut record = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => Value::from(value),
                ValueRef::Real(value) => Value::from(value),
                ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).into_owned()),
                ValueRef::Blob(value) => json!({ "base64": STANDARD.encode(value) }),
            };
            record.insert(name.clone(), value);
        }
        out.push(record);
    }
    Ok(out)
}

fn sql_value(value: &Value) -> Result<rusqlite::types::Value, AuthError> {
    use rusqlite::types::Value as Sql;
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(value) => Sql::Integer(i64::from(*value)),
        Value::Number(number) => match number.as_i64() {
            Some(value) => Sql::Integer(value),
            None => Sql::Real(number.as_f64().unwrap_or_default()),
        },
        Value::String(text) => Sql::Text(text.clone()),
        Value::Object(object) => match object.get("base64").and_then(Value::as_str) {
            Some(text) => Sql::Blob(
                STANDARD
                    .decode(text)
                    .map_err(|_| damaged("a binary value is not base64"))?,
            ),
            None => return Err(damaged("a value is an object")),
        },
        Value::Array(_) => return Err(damaged("a value is a list")),
    })
}

/// Insert `record` into `table`, one column per field.
fn insert(
    connection: &Connection,
    table: &str,
    record: &Map<String, Value>,
) -> Result<(), AuthError> {
    if record.keys().any(|column| {
        column.is_empty() || !column.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    }) {
        return Err(damaged(format!(
            "a record of {table} names an unknown column"
        )));
    }
    let columns: Vec<&str> = record.keys().map(String::as_str).collect();
    let marks: Vec<String> = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect();
    let values = record
        .values()
        .map(sql_value)
        .collect::<Result<Vec<_>, _>>()?;
    connection.execute(
        &format!(
            "INSERT INTO {table}({}) VALUES({})",
            columns.join(","),
            marks.join(",")
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

fn text(record: &Map<String, Value>, field: &str) -> String {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------
// The container: named entries of length-prefixed chunks.

fn write_entry(out: &mut dyn Write, name: &str, data: &mut dyn Read) -> Result<(), AuthError> {
    let length = u16::try_from(name.len()).map_err(|_| damaged("an entry name is too long"))?;
    out.write_all(&length.to_be_bytes()).map_err(io)?;
    out.write_all(name.as_bytes()).map_err(io)?;
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = data.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        out.write_all(&u32::try_from(read).unwrap_or_default().to_be_bytes())
            .map_err(io)?;
        out.write_all(&buffer[..read]).map_err(io)?;
    }
    out.write_all(&0u32.to_be_bytes()).map_err(io)
}

/// The next entry's name, or `None` at the end of the content.
fn read_name(input: &mut dyn Read) -> Result<Option<String>, AuthError> {
    let mut length = [0; 2];
    match input.read(&mut length[..1]).map_err(io)? {
        0 => return Ok(None),
        _ => input.read_exact(&mut length[1..]).map_err(io)?,
    }
    let mut name = vec![0; usize::from(u16::from_be_bytes(length))];
    input.read_exact(&mut name).map_err(io)?;
    String::from_utf8(name)
        .map(Some)
        .map_err(|_| damaged("an entry name is not text"))
}

/// Copy the entry's chunks into `out`, refusing more than `limit` bytes.
fn read_entry(input: &mut dyn Read, out: &mut dyn Write, limit: usize) -> Result<(), AuthError> {
    let mut total = 0usize;
    loop {
        let mut length = [0; 4];
        input.read_exact(&mut length).map_err(io)?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 {
            return Ok(());
        }
        if length > MAX_CHUNK {
            return Err(damaged("a chunk is too long"));
        }
        total += length;
        if total > limit {
            return Err(damaged("an entry is too large"));
        }
        let mut chunk = vec![0; length];
        input.read_exact(&mut chunk).map_err(io)?;
        out.write_all(&chunk).map_err(io)?;
    }
}

// ---------------------------------------------------------------------------
// Export.

/// The records of each kind, read in one transaction.
fn read_records(
    connection: &Connection,
    storage: &OAuthStorage,
) -> Result<BTreeMap<&'static str, Vec<Value>>, AuthError> {
    let mut records: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let wrap = |rows: Vec<Map<String, Value>>| rows.into_iter().map(Value::Object).collect();
    records.insert(
        "signInSettings",
        wrap(rows(
            connection,
            "SELECT local_users,local_sign_up,password_min_length FROM auth_sign_in_settings",
            [],
        )?),
    );
    let mut providers = Vec::new();
    for mut provider in rows(
        connection,
        "SELECT * FROM auth_oidc_providers WHERE state!='deleted' ORDER BY provider_id",
        [],
    )? {
        let sealed: Option<Vec<u8>> = connection.query_row(
            "SELECT sealed_client_secret FROM auth_oidc_providers WHERE provider_id=?1",
            [text(&provider, "provider_id")],
            |row| row.get(0),
        )?;
        provider.remove("sealed_client_secret");
        let secret = sealed
            .map(|sealed| {
                storage
                    .open_upstream_client_secret(&text(&provider, "provider_id"), &sealed)
                    .map_err(|_| AuthError::Unavailable)
            })
            .transpose()?;
        provider.insert(
            "client_secret".into(),
            secret.map_or(Value::Null, Value::String),
        );
        providers.push(Value::Object(provider));
    }
    records.insert("oidcProviders", providers);
    let mut registries = Vec::new();
    for mut registry in rows(
        connection,
        "SELECT name,prefix,username,sealed_secret FROM auth_package_registries ORDER BY name",
        [],
    )? {
        let sealed: Option<Vec<u8>> = connection.query_row(
            "SELECT sealed_secret FROM auth_package_registries WHERE name=?1",
            [text(&registry, "name")],
            |row| row.get(0),
        )?;
        registry.remove("sealed_secret");
        let secret = sealed
            .map(|sealed| {
                storage
                    .decode::<String>(super::registries::SEAL, &text(&registry, "name"), &sealed)
                    .map_err(|_| AuthError::Unavailable)
            })
            .transpose()?;
        registry.insert("secret".into(), secret.map_or(Value::Null, Value::String));
        registries.push(Value::Object(registry));
    }
    records.insert("packageRegistries", registries);
    let administrator: Option<String> = connection
        .query_row(
            "SELECT principal_id FROM auth_bootstrap_administrator",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mut users = Vec::new();
    for principal in rows(
        connection,
        "SELECT principal_id,state,created_at FROM auth_principals \
        WHERE kind='user' AND state!='revoked' ORDER BY principal_id",
        [],
    )? {
        let user_id = text(&principal, "principal_id");
        // The default administrator has a password without a local sign-in
        // record; every other local account has both.
        let username: Option<String> = connection
            .query_row(
                "SELECT provider_subject FROM auth_provider_identities \
                WHERE principal_id=?1 AND provider='local' \
                UNION ALL SELECT normalized_username FROM auth_local_credentials \
                WHERE principal_id=?1 LIMIT 1",
                [&user_id],
                |row| row.get(0),
            )
            .optional()?;
        let credentials = rows(
            connection,
            "SELECT password_hash,hash_profile,password_changed_at FROM auth_local_credentials \
            WHERE principal_id=?1",
            [&user_id],
        )?;
        let identities = rows(
            connection,
            "SELECT i.provider,o.issuer,i.provider_subject,i.linked_at,i.last_seen_at,\
            i.profile_json FROM auth_provider_identities i \
            LEFT JOIN auth_oidc_providers o ON o.provider_id=i.provider \
            WHERE i.principal_id=?1 AND i.provider!='local' ORDER BY i.provider,i.provider_subject",
            [&user_id],
        )?;
        let mut profile = rows(
            connection,
            "SELECT * FROM auth_user_profiles WHERE principal_id=?1",
            [&user_id],
        )?;
        if let Some(profile) = profile.first_mut() {
            profile.remove("principal_id");
        }
        users.push(json!({
            "userId": user_id,
            "state": principal["state"],
            "createdAt": principal["created_at"],
            "defaultAdministrator": administrator.as_deref() == Some(user_id.as_str()),
            "username": username,
            "localSignIn": connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_provider_identities \
                WHERE principal_id=?1 AND provider='local')",
                [&user_id],
                |row| row.get::<_, bool>(0),
            )?,
            "credentials": credentials.into_iter().next(),
            "identities": identities,
            "profile": profile.into_iter().next(),
        }));
    }
    records.insert("users", users);
    let access = access_records::read_access(connection, None)?;
    records.insert(
        "roles",
        access
            .roles
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?,
    );
    records.insert(
        "grants",
        access
            .grants
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?,
    );
    let mut mappings = Vec::new();
    for mut mapping in rows(
        connection,
        "SELECT mapping_id,provider_id,claim_name,claim_value FROM auth_oidc_role_mappings \
        ORDER BY mapping_id",
        [],
    )? {
        let roles: Vec<String> = connection
            .prepare(
                "SELECT m.role_id FROM auth_oidc_mapping_roles m JOIN auth_roles r \
                ON r.role_id=m.role_id AND r.identity_generation=m.role_generation \
                WHERE m.mapping_id=?1 AND r.state='active' ORDER BY m.role_id",
            )?
            .query_map([text(&mapping, "mapping_id")], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        mapping.insert("roles".into(), json!(roles));
        mappings.push(Value::Object(mapping));
    }
    records.insert("oidcRoleMappings", mappings);
    records.insert(
        "oauthClients",
        wrap(rows(
            connection,
            "SELECT * FROM auth_oauth_clients WHERE state!='deleted' ORDER BY client_id",
            [],
        )?),
    );
    // Only consent given to the capability's current wording still applies.
    records.insert(
        "consents",
        wrap(rows(
            connection,
            "SELECT c.principal_id,c.client_id,c.binding_kind,c.binding_value,c.capability_id,\
            c.decision,c.expires_at FROM auth_remembered_consent c \
            JOIN auth_capability_identities ci ON ci.capability_id=c.capability_id \
            AND ci.identity_generation=c.capability_generation AND ci.state='active' \
            AND ci.consent_revision=c.consent_revision \
            JOIN auth_principals p ON p.principal_id=c.principal_id \
            WHERE p.kind='user' AND p.state!='revoked' \
            ORDER BY c.principal_id,c.client_id,c.capability_id",
            [],
        )?),
    );
    records.insert(
        "portalThemes",
        wrap(rows(
            connection,
            "SELECT client_id,theme_json,logo_format,logo,stylesheet FROM auth_portal_themes \
            ORDER BY client_id",
            [],
        )?),
    );
    Ok(records)
}

/// Write the installation behind `storage` and `nats` into `out` as content
/// of an installation export, and return its report.
pub(crate) async fn export(
    storage: &OAuthStorage,
    nats: &async_nats::Client,
    directory: &Path,
    out: &mut dyn Write,
) -> Result<Value, AuthError> {
    let (records, deployments, administrator) = {
        let storage = storage.clone();
        super::blocking(move || {
            storage.authority.transaction(|connection| {
                let administrator: String = connection
                    .query_row(
                        "SELECT principal_id FROM auth_bootstrap_administrator",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        AuthError::Invalid(
                            "the installation has no administrator yet; finish its setup first"
                                .into(),
                        )
                    })?;
                let deployments: Vec<(String, String)> = connection
                    .prepare(
                        "SELECT deployment_id,title FROM auth_deployments \
                        WHERE state!='revoked' AND participant_kind IN ('service','device') \
                        AND participant_id NOT LIKE 'trellis.%' ORDER BY deployment_id",
                    )?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok((
                    read_records(connection, &storage)?,
                    deployments,
                    administrator,
                ))
            })
        })
        .await?
    };
    let settings = storage.authority.settings.clone();
    let exported_at = crate::clock::unix_now();
    let kinds: BTreeMap<&str, u32> = KINDS.into_iter().collect();
    let meta = json!({
        "format": FORMAT,
        "trellisVersion": env!("CARGO_PKG_VERSION"),
        "exportedAt": exported_at,
        "sourceInstallationId": settings.instance_id,
        "sourceOrigin": settings.oauth_origin(),
        "kinds": kinds,
    });
    out.write_all(MAGIC).map_err(io)?;
    out.write_all(&FORMAT.to_be_bytes()).map_err(io)?;
    write_entry(out, "meta.json", &mut serde_json::to_vec(&meta)?.as_slice())?;
    let mut counts = Map::new();
    for (kind, format) in KINDS {
        let records = records.get(kind).cloned().unwrap_or_default();
        counts.insert(kind.to_owned(), json!(records.len()));
        let entry = json!({ "kind": kind, "format": format, "records": records });
        write_entry(
            out,
            &format!("records/{kind}.json"),
            &mut serde_json::to_vec(&entry)?.as_slice(),
        )?;
    }
    let mut exported = Vec::new();
    for (deployment_id, title) in deployments {
        let quiet = |_: &str, _: u64| std::future::ready(Ok::<(), AuthError>(()));
        let content = service_bundles::export_content(
            storage,
            nats,
            directory,
            &administrator,
            &deployment_id,
            true,
            &quiet,
        )
        .await?;
        let (size, report) = (content.size, content.report);
        write_entry(
            out,
            &format!("deployments/{deployment_id}.bundle"),
            &mut content.spool.reader()?,
        )?;
        exported.push(json!({
            "deploymentId": deployment_id,
            "title": title,
            "size": size,
            "resources": report["resources"],
            "leftBehind": report["leftBehind"],
        }));
    }
    Ok(json!({
        "sourceInstallationId": meta["sourceInstallationId"],
        "sourceOrigin": meta["sourceOrigin"],
        "exportedAt": exported_at,
        "records": counts,
        "deployments": exported,
        "notCarried": NOT_CARRIED,
    }))
}

// ---------------------------------------------------------------------------
// Import.

/// The records of one kind in the format this build reads. Every kind is at
/// its first format, so there is nothing older to convert yet; a later
/// format adds the conversion from each earlier one here.
fn current_records(kind: &str, format: u64, records: Vec<Value>) -> Result<Vec<Value>, AuthError> {
    let current = KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, format)| u64::from(*format))
        .ok_or_else(|| damaged(format!("it has records of an unknown kind {kind}")))?;
    match format {
        format if format == current => Ok(records),
        format => Err(AuthError::Invalid(format!(
            "its {kind} records have format {format}, which this Trellis does not read; \
            import it with a release that does"
        ))),
    }
}

/// What a read installation export holds: records by kind, and each
/// deployment's bundle content in a file of its own.
struct Received {
    meta: Value,
    records: BTreeMap<String, Vec<Value>>,
    bundles: Vec<(String, Arc<service_bundles::Spool>)>,
}

fn receive(input: &mut dyn Read, directory: &Path) -> Result<Received, AuthError> {
    let mut header = [0; 20];
    input.read_exact(&mut header).map_err(io)?;
    if &header[..16] != MAGIC {
        return Err(damaged("it is not a Trellis installation export"));
    }
    let format = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    if format != FORMAT {
        return Err(AuthError::Invalid(format!(
            "the export has container format {format}, and this Trellis reads format {FORMAT}"
        )));
    }
    let mut received = Received {
        meta: Value::Null,
        records: BTreeMap::new(),
        bundles: Vec::new(),
    };
    while let Some(name) = read_name(input)? {
        if let Some(deployment) = name
            .strip_prefix("deployments/")
            .and_then(|rest| rest.strip_suffix(".bundle"))
        {
            if ulid::Ulid::from_string(deployment).is_err() {
                return Err(damaged(format!("entry {name}")));
            }
            // Bundle content waits in a spool encrypted with a key held
            // only in memory, as a service import keeps it.
            let (spool, mut writer) = service_bundles::Spool::create(directory)?;
            read_entry(input, &mut writer, usize::MAX)?;
            writer.finish()?;
            received.bundles.push((deployment.to_owned(), spool));
            continue;
        }
        let mut bytes = Vec::new();
        read_entry(input, &mut bytes, MAX_RECORDS)?;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|error| damaged(format!("{name}: {error}")))?;
        if name == "meta.json" {
            received.meta = document;
        } else if let Some(kind) = name
            .strip_prefix("records/")
            .and_then(|rest| rest.strip_suffix(".json"))
        {
            let records = document["records"]
                .as_array()
                .cloned()
                .ok_or_else(|| damaged(format!("{name} has no records")))?;
            let format = document["format"]
                .as_u64()
                .ok_or_else(|| damaged(format!("{name} has no format")))?;
            received
                .records
                .insert(kind.to_owned(), current_records(kind, format, records)?);
        } else {
            return Err(damaged(format!("entry {name:?}")));
        }
    }
    if received.meta.is_null() {
        return Err(damaged("it has no meta.json"));
    }
    Ok(received)
}

/// What this installation already holds that an import adds to.
fn existing(connection: &Connection) -> Result<Map<String, Value>, AuthError> {
    let count = |sql: &str| -> Result<u64, AuthError> {
        Ok(connection.query_row(sql, [], |row| row.get(0))?)
    };
    let mut found = Map::new();
    for (name, sql) in [
        (
            "users",
            "SELECT count(*) FROM auth_principals WHERE kind='user' AND state!='revoked' \
            AND principal_id NOT IN (SELECT principal_id FROM auth_bootstrap_administrator)",
        ),
        (
            "oidcProviders",
            "SELECT count(*) FROM auth_oidc_providers WHERE state!='deleted'",
        ),
        // The role setup gives the default administrator alone does not count.
        (
            "roles",
            "SELECT count(*) FROM auth_roles r WHERE r.state='active' AND (NOT EXISTS(\
            SELECT 1 FROM auth_role_assignments a WHERE a.role_id=r.role_id \
            AND a.role_generation=r.identity_generation) OR EXISTS(SELECT 1 \
            FROM auth_role_assignments a WHERE a.role_id=r.role_id \
            AND a.role_generation=r.identity_generation AND a.principal_id NOT IN \
            (SELECT principal_id FROM auth_bootstrap_administrator)))",
        ),
        (
            "oauthClients",
            "SELECT count(*) FROM auth_oauth_clients WHERE state!='deleted' \
            AND client_id NOT IN ('trellis.console','trellis.cli')",
        ),
        (
            "deployments",
            "SELECT count(*) FROM auth_deployments WHERE state!='revoked' \
            AND participant_id NOT LIKE 'trellis.%'",
        ),
        ("portalThemes", "SELECT count(*) FROM auth_portal_themes"),
        (
            "packageRegistries",
            "SELECT count(*) FROM auth_package_registries",
        ),
    ] {
        let value = count(sql)?;
        if value > 0 {
            found.insert(name.to_owned(), json!(value));
        }
    }
    Ok(found)
}

/// The outcome of one import transaction.
#[derive(Default)]
struct Outcome {
    created: Vec<Value>,
    skipped: Vec<Value>,
    access: Value,
    /// Deployments recorded by this import, whose data loads afterwards.
    deployments: Vec<String>,
    target: Value,
}

impl Outcome {
    fn created(&mut self, kind: &str, id: &str) {
        self.created.push(json!({ "kind": kind, "id": id }));
    }

    fn skipped(&mut self, kind: &str, id: &str, reason: impl Into<String>) {
        self.skipped
            .push(json!({ "kind": kind, "id": id, "reason": reason.into() }));
    }
}

/// Run `step` in a savepoint: a refusal rolls back what it wrote and is
/// returned as the reason it was skipped; a storage failure stops the import.
fn attempt(
    connection: &Connection,
    step: impl FnOnce() -> Result<(), AuthError>,
) -> Result<Result<(), String>, AuthError> {
    connection.execute_batch("SAVEPOINT installation_record")?;
    match step() {
        Ok(()) => {
            connection.execute_batch("RELEASE installation_record")?;
            Ok(Ok(()))
        }
        Err(error) => {
            connection
                .execute_batch("ROLLBACK TO installation_record; RELEASE installation_record")?;
            match error {
                AuthError::Storage(_) | AuthError::Json(_) | AuthError::Unavailable => Err(error),
                error => Ok(Err(error.to_string())),
            }
        }
    }
}

fn records<'a>(received: &'a Received, kind: &str) -> &'a [Value] {
    received
        .records
        .get(kind)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Make `user` here as the export recorded it, or say what is in the way.
fn create_user(
    connection: &Connection,
    user: &Value,
    providers: &BTreeMap<String, String>,
    now: i64,
) -> Result<(), AuthError> {
    let user_id = user["userId"].as_str().unwrap_or_default();
    if ulid::Ulid::from_string(user_id).is_err() {
        return Err(damaged("a user ID is not a ULID"));
    }
    let taken: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_principals WHERE principal_id=?1)",
        [user_id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(AuthError::InUse(format!(
            "user ID {user_id} is already in use here"
        )));
    }
    let username = user["username"].as_str();
    if let Some(username) = username {
        let owner: Option<String> = connection
            .query_row(
                "SELECT principal_id FROM auth_provider_identities \
                WHERE provider='local' AND provider_subject=?1 \
                UNION ALL SELECT principal_id FROM auth_local_credentials \
                WHERE normalized_username=?1 LIMIT 1",
                [username],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            return Err(AuthError::InUse(format!(
                "username {username} belongs to user {owner} here"
            )));
        }
    }
    let state = user["state"].as_str().unwrap_or("active");
    connection.execute(
        "INSERT INTO auth_principals(principal_id,kind,state,created_at,updated_at,version,\
        disabled_at) VALUES(?1,'user',?2,?3,?4,1,?5)",
        params![
            user_id,
            state,
            user["createdAt"].as_i64().unwrap_or(now),
            now,
            (state == "disabled").then_some(now)
        ],
    )?;
    if let Some(username) = username {
        if user["localSignIn"].as_bool() == Some(true) {
            connection.execute(
                "INSERT INTO auth_provider_identities(identity_id,provider,provider_subject,\
                principal_id,linked_at,last_seen_at) VALUES(?1,'local',?2,?3,?4,?4)",
                params![id(), username, user_id, now],
            )?;
        }
        if let Some(credentials) = user["credentials"].as_object() {
            connection.execute(
                "INSERT INTO auth_local_credentials(principal_id,normalized_username,\
                password_hash,hash_profile,password_changed_at,updated_at,version) \
                VALUES(?1,?2,?3,?4,?5,?6,1)",
                params![
                    user_id,
                    username,
                    credentials.get("password_hash").and_then(Value::as_str),
                    credentials
                        .get("hash_profile")
                        .and_then(Value::as_i64)
                        .unwrap_or(1),
                    credentials
                        .get("password_changed_at")
                        .and_then(Value::as_i64)
                        .unwrap_or(now),
                    now
                ],
            )?;
        }
    }
    for identity in user["identities"].as_array().into_iter().flatten() {
        let subject = identity["provider_subject"].as_str().unwrap_or_default();
        let provider = providers
            .get(identity["provider"].as_str().unwrap_or_default())
            .ok_or_else(|| {
                AuthError::NotFound(format!(
                    "its sign-in {subject} is through a provider this import did not bring"
                ))
            })?;
        let owner: Option<String> = connection
            .query_row(
                "SELECT principal_id FROM auth_provider_identities \
                WHERE provider=?1 AND provider_subject=?2",
                params![provider, subject],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            return Err(AuthError::InUse(format!(
                "its sign-in {subject} belongs to user {owner} here"
            )));
        }
        connection.execute(
            "INSERT INTO auth_provider_identities(identity_id,provider,provider_subject,\
            principal_id,linked_at,last_seen_at,profile_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                id(),
                provider,
                subject,
                user_id,
                identity["linked_at"].as_i64().unwrap_or(now),
                identity["last_seen_at"].as_i64().unwrap_or(now),
                identity["profile_json"].as_str()
            ],
        )?;
    }
    if let Some(profile) = user["profile"].as_object() {
        let mut profile = profile.clone();
        profile.insert("principal_id".into(), Value::String(user_id.to_owned()));
        // A provider the import did not bring cannot vouch for the email.
        for field in ["email_provider_id", "picture_provider_id"] {
            let mapped = profile
                .get(field)
                .and_then(Value::as_str)
                .and_then(|provider| providers.get(provider))
                .cloned();
            profile.insert(field.into(), mapped.map_or(Value::Null, Value::String));
        }
        if profile.get("email_source").and_then(Value::as_str) == Some("oidc")
            && profile.get("email_provider_id").is_some_and(Value::is_null)
        {
            profile.insert("email_source".into(), json!("manual"));
            profile.insert("email_verified".into(), json!(0));
        }
        insert(connection, "auth_user_profiles", &profile)?;
    }
    Ok(())
}

/// Delete `principal`'s account as deleting a user does, once `successor`
/// has taken its place as the default administrator.
fn retire_administrator(
    connection: &Connection,
    principal: &str,
    successor: &str,
    now: i64,
) -> Result<(), AuthError> {
    let version: u64 = connection.query_row(
        "SELECT version FROM auth_principals WHERE principal_id=?1",
        [principal],
        |row| row.get(0),
    )?;
    let identity = Mutation {
        actor: successor.to_owned(),
        request_id: id(),
        request_digest: id(),
        session: None,
    };
    // The role setup made for it alone goes with it; the export brings its
    // own administrator's roles.
    let own: Vec<(String, u64)> = connection
        .prepare(
            "SELECT r.role_id,r.revision FROM auth_roles r WHERE r.state='active' \
            AND EXISTS(SELECT 1 FROM auth_role_assignments a WHERE a.role_id=r.role_id \
            AND a.role_generation=r.identity_generation AND a.principal_id=?1) \
            AND NOT EXISTS(SELECT 1 FROM auth_role_assignments a WHERE a.role_id=r.role_id \
            AND a.role_generation=r.identity_generation AND a.principal_id!=?1)",
        )?
        .query_map([principal], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (role_id, revision) in own {
        apply_policy_in_transaction(
            connection,
            &identity,
            &PolicyMutation::RoleDelete {
                role_id,
                expected_revision: revision,
            },
            now,
        )?;
    }
    apply_policy_in_transaction(
        connection,
        &identity,
        &PolicyMutation::PrincipalState {
            principal_id: principal.to_owned(),
            expected_revision: version,
            state: "revoked".into(),
        },
        now,
    )?;
    for table in [
        "auth_local_credentials",
        "auth_provider_identities",
        "auth_user_profiles",
        "auth_avatar_cache",
        "auth_role_assignments",
        "auth_direct_capabilities",
        "auth_remembered_consent",
    ] {
        connection.execute(
            &format!("DELETE FROM {table} WHERE principal_id=?1"),
            [principal],
        )?;
    }
    connection.execute(
        "UPDATE auth_account_flows SET state='revoked',version=version+1 \
        WHERE target_principal_id=?1 AND state='pending'",
        [principal],
    )?;
    Ok(())
}

/// Everything an import writes to the Auth database, in one transaction.
fn apply(
    connection: &Connection,
    storage: &OAuthStorage,
    received: &Received,
    bundles: &[(String, service_bundles::Bundle)],
    now: i64,
) -> Result<Outcome, AuthError> {
    let mut outcome = Outcome::default();
    let found = existing(connection)?;
    let empty = found.is_empty();
    outcome.target = json!({ "empty": empty, "existing": found });

    // Sign-in settings: the export's, unless this installation set its own.
    if let Some(settings) = records(received, "signInSettings").first() {
        let (local_users, local_sign_up, minimum, revision): (bool, bool, i64, u64) = connection
            .query_row(
                "SELECT local_users,local_sign_up,password_min_length,revision \
                FROM auth_sign_in_settings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let wanted = (
            settings["local_users"].as_i64() == Some(1),
            settings["local_sign_up"].as_i64() == Some(1),
            settings["password_min_length"].as_i64().unwrap_or(minimum),
        );
        if wanted != (local_users, local_sign_up, minimum) {
            if empty || revision == 1 {
                connection.execute(
                    "UPDATE auth_sign_in_settings SET local_users=?1,local_sign_up=?2,\
                    password_min_length=?3,revision=revision+1,updated_at=?4",
                    params![wanted.0, wanted.1, wanted.2, now],
                )?;
                outcome.created("signInSettings", "");
            } else {
                outcome.skipped(
                    "signInSettings",
                    "",
                    "this installation has sign-in settings of its own; they were left as they are",
                );
            }
        }
    }

    // OIDC providers, and which provider here each exported one is.
    let mut providers = BTreeMap::new();
    for provider in records(received, "oidcProviders") {
        let Some(record) = provider.as_object() else {
            return Err(damaged("an OIDC provider is not a record"));
        };
        let provider_id = text(record, "provider_id");
        let issuer = text(record, "issuer");
        let same_id: Option<String> = connection
            .query_row(
                "SELECT issuer FROM auth_oidc_providers WHERE provider_id=?1",
                [&provider_id],
                |row| row.get(0),
            )
            .optional()?;
        let same_issuer: Option<String> = connection
            .query_row(
                "SELECT provider_id FROM auth_oidc_providers WHERE issuer=?1 AND state!='deleted'",
                [&issuer],
                |row| row.get(0),
            )
            .optional()?;
        match (same_id, same_issuer) {
            (Some(here), _) if here == issuer => {
                providers.insert(provider_id.clone(), provider_id.clone());
            }
            (Some(_), _) => outcome.skipped(
                "oidcProvider",
                &provider_id,
                "a provider with this ID and another issuer exists here; its sign-ins were not \
                brought",
            ),
            (None, Some(here)) => {
                outcome.skipped(
                    "oidcProvider",
                    &provider_id,
                    format!("provider {here} here has the same issuer; its sign-ins use that one"),
                );
                providers.insert(provider_id.clone(), here);
            }
            (None, None) => {
                let mut row = record.clone();
                let secret = row.remove("client_secret");
                let sealed = match secret.as_ref().and_then(Value::as_str) {
                    Some(secret) => Some(
                        storage
                            .seal_upstream_client_secret(&provider_id, secret)
                            .map_err(|_| AuthError::Unavailable)?,
                    ),
                    None => None,
                };
                insert(connection, "auth_oidc_providers", &row)?;
                connection.execute(
                    "UPDATE auth_oidc_providers SET sealed_client_secret=?2 WHERE provider_id=?1",
                    params![provider_id, sealed],
                )?;
                providers.insert(provider_id.clone(), provider_id.clone());
                outcome.created("oidcProvider", &provider_id);
            }
        }
    }

    for registry in records(received, "packageRegistries") {
        let name = registry["name"].as_str().unwrap_or_default().to_owned();
        let result = attempt(connection, || {
            let taken: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_package_registries WHERE name=?1)",
                [&name],
                |row| row.get(0),
            )?;
            if taken {
                return Err(AuthError::InUse(format!(
                    "a package registry named {name} exists here"
                )));
            }
            let sealed = registry["secret"]
                .as_str()
                .map(|secret| {
                    storage
                        .encode(super::registries::SEAL, &name, &secret)
                        .map_err(|_| AuthError::Unavailable)
                })
                .transpose()?;
            connection.execute(
                "INSERT INTO auth_package_registries(name,prefix,username,sealed_secret,\
                updated_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    name,
                    registry["prefix"].as_str(),
                    registry["username"].as_str(),
                    sealed,
                    now
                ],
            )?;
            Ok(())
        })?;
        match result {
            Ok(()) => outcome.created("packageRegistry", &name),
            Err(reason) => outcome.skipped("packageRegistry", &name, reason),
        }
    }

    // Users, and who is the default administrator.
    let administrator: Option<String> = connection
        .query_row(
            "SELECT principal_id FROM auth_bootstrap_administrator",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mut acting = administrator.clone();
    let mut user_refs = Vec::new();
    for user in records(received, "users") {
        let user_id = user["userId"].as_str().unwrap_or_default().to_owned();
        let mut identities: Vec<SignIn> = user["identities"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|identity| SignIn {
                provider: identity["provider"].as_str().unwrap_or_default().to_owned(),
                issuer: identity["issuer"].as_str().map(str::to_owned),
                subject: identity["provider_subject"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect();
        if let Some(username) = user["username"].as_str() {
            identities.push(SignIn {
                provider: "local".into(),
                issuer: None,
                subject: username.to_owned(),
            });
        }
        user_refs.push(UserRef {
            user_id: user_id.clone(),
            identities,
        });
        let leading = user["defaultAdministrator"].as_bool() == Some(true);
        // An installation with nothing in it yet takes the exported
        // administrator in place of its own; one in use keeps its own.
        let replaces = leading
            && empty
            && administrator
                .as_deref()
                .is_some_and(|current| current != user_id);
        let result = attempt(connection, || {
            // Its sign-ins go first, so the exported administrator may have
            // the same username.
            if let (true, Some(current)) = (replaces, &administrator) {
                for table in ["auth_local_credentials", "auth_provider_identities"] {
                    connection.execute(
                        &format!("DELETE FROM {table} WHERE principal_id=?1"),
                        [current],
                    )?;
                }
            }
            create_user(connection, user, &providers, now)?;
            if leading && (replaces || administrator.is_none()) {
                connection.execute(
                    "INSERT INTO auth_bootstrap_administrator(singleton,principal_id,created_at) \
                    VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET \
                    principal_id=excluded.principal_id",
                    params![user_id, now],
                )?;
            }
            if let (true, Some(current)) = (replaces, &administrator) {
                retire_administrator(connection, current, &user_id, now)?;
            }
            Ok(())
        })?;
        match result {
            Ok(()) => {
                outcome.created("user", &user_id);
                if leading && (replaces || administrator.is_none()) {
                    if let Some(previous) = &administrator {
                        outcome.created("defaultAdministrator", &user_id);
                        outcome.skipped(
                            "user",
                            previous,
                            "this installation's own administrator was removed: the exported \
                            administrator took its place",
                        );
                    } else {
                        outcome.created("defaultAdministrator", &user_id);
                    }
                    acting = Some(user_id.clone());
                } else if leading {
                    outcome.skipped(
                        "defaultAdministrator",
                        &user_id,
                        "this installation is in use and keeps its own administrator; the \
                        exported one is an ordinary user here",
                    );
                }
            }
            Err(reason) => outcome.skipped("user", &user_id, reason),
        }
    }
    let actor = acting.ok_or_else(|| {
        AuthError::Invalid(
            "neither the export nor this installation has a default administrator".into(),
        )
    })?;
    let identity = Mutation {
        actor: actor.clone(),
        request_id: id(),
        request_digest: id(),
        session: None,
    };

    // Deployments, through the service import, without their access: the
    // roles and grants below cover every API at once.
    for (deployment_id, bundle) in bundles {
        let result = attempt(connection, || {
            service_bundles::apply_import(connection, storage, bundle, &actor, now)
        })?;
        match result {
            Ok(()) => {
                outcome.created("deployment", deployment_id);
                outcome.deployments.push(deployment_id.clone());
            }
            Err(reason) => outcome.skipped("deployment", deployment_id, reason),
        }
    }

    // Roles and grants, as a service import recreates them.
    let access = AccessDocument {
        users: user_refs,
        roles: records(received, "roles")
            .iter()
            .map(|record| serde_json::from_value::<RoleRecord>(record.clone()))
            .collect::<Result<_, _>>()
            .map_err(|error| damaged(format!("a role: {error}")))?,
        grants: records(received, "grants")
            .iter()
            .map(|record| serde_json::from_value::<GrantRecord>(record.clone()))
            .collect::<Result<_, _>>()
            .map_err(|error| damaged(format!("a grant: {error}")))?,
        apps: Vec::new(),
    };
    outcome.access = serde_json::to_value(access_records::recreate(
        connection, &identity, &access, now,
    )?)?;

    for mapping in records(received, "oidcRoleMappings") {
        let mapping_id = mapping["mapping_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let result = attempt(connection, || {
            let provider = providers
                .get(mapping["provider_id"].as_str().unwrap_or_default())
                .ok_or_else(|| AuthError::NotFound("its provider was not brought".into()))?;
            connection
                .execute(
                    "INSERT INTO auth_oidc_role_mappings(mapping_id,provider_id,claim_name,\
                    claim_value,revision) VALUES(?1,?2,?3,?4,1)",
                    params![
                        mapping_id,
                        provider,
                        mapping["claim_name"].as_str(),
                        mapping["claim_value"].as_str()
                    ],
                )
                .map_err(|_| {
                    AuthError::InUse("a mapping with this ID or claim exists here".into())
                })?;
            for role in mapping["roles"].as_array().into_iter().flatten() {
                connection.execute(
                    "INSERT INTO auth_oidc_mapping_roles(mapping_id,role_id,role_generation) \
                    SELECT ?1,role_id,identity_generation FROM auth_roles \
                    WHERE role_id=?2 AND state='active'",
                    params![mapping_id, role.as_str()],
                )?;
            }
            Ok(())
        })?;
        match result {
            Ok(()) => outcome.created("oidcRoleMapping", &mapping_id),
            Err(reason) => outcome.skipped("oidcRoleMapping", &mapping_id, reason),
        }
    }

    for client in records(received, "oauthClients") {
        let Some(record) = client.as_object() else {
            return Err(damaged("an OAuth client is not a record"));
        };
        let client_id = text(record, "client_id");
        let here: Option<String> = connection
            .query_row(
                "SELECT metadata_json FROM auth_oauth_clients WHERE client_id=?1",
                [&client_id],
                |row| row.get(0),
            )
            .optional()?;
        if BUILTIN_CLIENTS.contains(&client_id.as_str()) {
            // Each installation registers its own Console and CLI, for its
            // own origin. Who may sign up through them moves.
            let wanted = decode::<Value>(&text(record, "metadata_json"))?
                .get("sign_up")
                .cloned();
            let Some(mut metadata) = here.map(|here| decode::<Value>(&here)).transpose()? else {
                continue;
            };
            if metadata.get("sign_up") == wanted.as_ref() {
                continue;
            }
            if empty || metadata.get("sign_up").is_none() {
                match &wanted {
                    Some(wanted) => metadata["sign_up"] = wanted.clone(),
                    None => {
                        if let Some(fields) = metadata.as_object_mut() {
                            fields.remove("sign_up");
                        }
                    }
                }
                connection.execute(
                    "UPDATE auth_oauth_clients SET metadata_json=?2,revision=revision+1,\
                    updated_at=?3 WHERE client_id=?1",
                    params![client_id, json(&metadata)?, now],
                )?;
                outcome.created("clientSignUp", &client_id);
            } else {
                outcome.skipped(
                    "clientSignUp",
                    &client_id,
                    "this installation set who may sign up through it; that was left as it is",
                );
            }
            continue;
        }
        if here.is_some() {
            outcome.skipped(
                "oauthClient",
                &client_id,
                "an app with this client ID exists here; it was left as it is",
            );
            continue;
        }
        match attempt(connection, || {
            insert(connection, "auth_oauth_clients", record)
        })? {
            Ok(()) => outcome.created("oauthClient", &client_id),
            Err(reason) => outcome.skipped("oauthClient", &client_id, reason),
        }
    }

    // Consent, for the users this import made, to apps and capabilities here.
    let made: BTreeSet<String> = outcome
        .created
        .iter()
        .filter(|item| item["kind"] == "user")
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect();
    let mut consents = 0;
    for consent in records(received, "consents") {
        let principal = consent["principal_id"].as_str().unwrap_or_default();
        if !made.contains(principal) {
            continue;
        }
        consents += connection.execute(
            "INSERT INTO auth_remembered_consent(principal_id,client_id,binding_kind,\
            binding_value,capability_id,capability_generation,consent_revision,decision,\
            expires_at,revision,updated_at) \
            SELECT ?1,?2,?3,?4,ci.capability_id,ci.identity_generation,ci.consent_revision,?6,\
            ?7,1,?8 FROM auth_capability_identities ci \
            WHERE ci.capability_id=?5 AND ci.state='active' \
            AND EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?2 AND state!='deleted') \
            ON CONFLICT DO NOTHING",
            params![
                principal,
                consent["client_id"].as_str(),
                consent["binding_kind"].as_str(),
                consent["binding_value"].as_str(),
                consent["capability_id"].as_str(),
                consent["decision"].as_str(),
                consent["expires_at"].as_i64(),
                now
            ],
        )?;
    }
    if consents > 0 {
        outcome.created("consents", &consents.to_string());
    }

    for theme in records(received, "portalThemes") {
        let Some(record) = theme.as_object() else {
            return Err(damaged("a Portal theme is not a record"));
        };
        let client_id = text(record, "client_id");
        let label = if client_id.is_empty() {
            "the installation's"
        } else {
            client_id.as_str()
        };
        let here = rows(
            connection,
            "SELECT client_id,theme_json,logo_format,logo,stylesheet FROM auth_portal_themes \
            WHERE client_id=?1",
            [&client_id],
        )?;
        match here.first() {
            Some(here) if here == record => {}
            Some(_) if !empty => outcome.skipped(
                "portalTheme",
                label,
                "this installation has a theme of its own; it was left as it is",
            ),
            _ => {
                connection.execute(
                    "DELETE FROM auth_portal_themes WHERE client_id=?1",
                    [&client_id],
                )?;
                let mut row = record.clone();
                row.insert("revision".into(), json!(1));
                row.insert("updated_at".into(), json!(now));
                insert(connection, "auth_portal_themes", &row)?;
                outcome.created("portalTheme", label);
            }
        }
    }
    Ok(outcome)
}

/// Import the installation export `input` into the installation behind
/// `storage` and `nats`, or with `check` report what it would do and change
/// nothing.
pub(crate) async fn import(
    storage: &OAuthStorage,
    nats: &async_nats::Client,
    directory: &Path,
    input: &mut (dyn Read + Send),
    check: bool,
) -> Result<Value, AuthError> {
    let received = receive(input, directory)?;
    // Each deployment's bundle, read and verified as a service import does
    // before anything changes.
    let mut bundles = Vec::new();
    let mut refused = Vec::new();
    for (deployment_id, spool) in &received.bundles {
        let reader = service_bundles::spool_reader(Arc::clone(spool));
        match service_bundles::prepare_import(nats, directory, reader).await {
            Ok(bundle) => bundles.push((deployment_id.clone(), bundle)),
            Err(error) => refused.push(json!({
                "kind": "deployment",
                "id": deployment_id,
                "reason": error.to_string(),
            })),
        }
    }
    let now = crate::clock::unix_now();
    let received = Arc::new(received);
    let mut outcome = {
        let (storage, received, bundles) = (storage.clone(), Arc::clone(&received), bundles);
        super::blocking(move || {
            let run =
                |connection: &Connection| apply(connection, &storage, &received, &bundles, now);
            if check {
                storage.authority.rehearse(run)
            } else {
                storage.authority.transaction(run)
            }
        })
        .await?
    };
    outcome.skipped.extend(refused);
    // The data of each deployment whose records are committed and whose data
    // has not loaded yet, loaded as a service import that resumes. Running
    // the same import again finishes one that stopped here.
    let mut deployments = Vec::new();
    if !check {
        for (deployment_id, spool) in &received.bundles {
            let reader = service_bundles::spool_reader(Arc::clone(spool));
            match service_bundles::finish_import(storage, nats, directory, reader).await {
                Ok(Some(report)) => {
                    deployments.push(json!({ "deploymentId": deployment_id, "report": report }));
                }
                Ok(None) => {}
                Err(error) => deployments.push(json!({
                    "deploymentId": deployment_id,
                    "error": error.to_string(),
                    "retry": "run the same import again to finish loading its data",
                })),
            }
        }
    }
    Ok(json!({
        "applied": !check,
        "source": {
            "installationId": received.meta["sourceInstallationId"],
            "origin": received.meta["sourceOrigin"],
            "trellisVersion": received.meta["trellisVersion"],
            "exportedAt": received.meta["exportedAt"],
        },
        "target": outcome.target,
        "created": outcome.created,
        "skipped": outcome.skipped,
        "access": outcome.access,
        "deployments": deployments,
        "notCarried": NOT_CARRIED,
    }))
}
