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

use super::access_records::{
    self, AccessDocument, GrantRecord, Match, RoleRecord, SignIn, UserRef,
};
use super::service_bundles;
use super::sqlite::{decode, digest, id, json, Connection};
use super::{AuthError, Mutation, OAuthStorage, SqliteAuthorizationStore};

/// The first bytes of the content, before the container's format number.
const MAGIC: &[u8; 16] = b"TRELLIS-INSTALL\n";
/// The container format this build writes and reads.
const FORMAT: u32 = 1;
/// The largest content chunk accepted, and the size this build writes.
const MAX_CHUNK: usize = 1 << 20;
const CHUNK: usize = 64 * 1024;
/// The largest record entry an import holds in memory, and the largest an
/// export writes.
const MAX_RECORDS: usize = 256 << 20;
/// Clients each installation registers for itself; only their sign-up
/// settings move.
const BUILTIN_CLIENTS: [&str; 2] = ["trellis.console", "trellis.cli"];

/// Every record kind with the format this build writes. An import converts
/// an older format of a kind to this one before it reads the records.
const KINDS: [(&str, u32); 11] = [
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
    ("state", 1),
];

/// What an installation export leaves out, in plain words.
const NOT_CARRIED: [&str; 10] = [
    "sign-in sessions and app sessions: everyone signs in again",
    "the NATS operator, accounts and credentials",
    "the installation's TLS certificates",
    "the issuer signing keys",
    "the security audit, and job, event, health and webhook request history",
    "earlier settings revisions",
    "password reset and setup links that were not used",
    "device activation reviews",
    "deleted users, and APIs no deployment provides",
    "the exported administrator's account: its roles and grants go to this \
    installation's administrator",
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

/// Write each kind's records as entries an import reads: one or more
/// `records/<kind>.json` entries, each at most `limit` bytes, so a kind with
/// more records than one entry holds is split across several. A single
/// record larger than `limit` fails the export, which then writes no file.
fn write_records(
    out: &mut dyn Write,
    records: &BTreeMap<&'static str, Vec<Value>>,
    limit: usize,
) -> Result<(), AuthError> {
    for (kind, format) in KINDS {
        let name = format!("records/{kind}.json");
        let empty = serde_json::to_vec(&json!({ "kind": kind, "format": format, "records": [] }))?;
        let mut batch: Vec<Vec<u8>> = Vec::new();
        let mut size = empty.len();
        let mut flush = |batch: &mut Vec<Vec<u8>>| -> Result<(), AuthError> {
            // The empty entry ends in `[]}`: the records go between.
            let mut entry = empty[..empty.len() - 2].to_vec();
            for (index, record) in batch.iter().enumerate() {
                if index > 0 {
                    entry.push(b',');
                }
                entry.extend_from_slice(record);
            }
            entry.extend_from_slice(b"]}");
            batch.clear();
            write_entry(out, &name, &mut entry.as_slice())
        };
        let kind_records = records.get(kind).map(Vec::as_slice).unwrap_or_default();
        for record in kind_records {
            let record = serde_json::to_vec(record)?;
            if empty.len() + record.len() > limit {
                return Err(AuthError::Invalid(format!(
                    "a {kind} record takes {} bytes, more than the {limit} an import reads in \
                    one entry",
                    record.len()
                )));
            }
            let added = record.len() + usize::from(!batch.is_empty());
            if size + added > limit {
                flush(&mut batch)?;
                size = empty.len();
            }
            size += record.len() + usize::from(!batch.is_empty());
            batch.push(record);
        }
        if !batch.is_empty() || kind_records.is_empty() {
            flush(&mut batch)?;
        }
    }
    Ok(())
}

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

/// A State resource of a user or device, read in the export's transaction,
/// whose values are read from NATS afterwards.
enum StateResource {
    /// A user's value, at `key`.
    User { record: Value, key: String },
    /// A device deployment's values, one per device: the key prefix, and
    /// each device's principal by the key suffix its values have.
    Device {
        record: Value,
        prefix: String,
        devices: BTreeMap<String, String>,
    },
}

/// The ready State resources of live users and of carried device
/// deployments.
fn read_state(connection: &Connection) -> Result<Vec<StateResource>, AuthError> {
    let mut found = Vec::new();
    for row in rows(
        connection,
        "SELECT r.resource_id,r.owner_id,r.namespace_json,r.local_name,r.commitment_json,\
        r.physical_id,r.revision,r.created_at,r.updated_at FROM auth_resources r \
        JOIN auth_principals p ON p.principal_id=r.owner_id \
        WHERE r.owner_kind='user' AND r.kind='state' AND r.state='ready' \
        AND p.kind='user' AND p.state!='revoked' ORDER BY r.owner_id,r.resource_id",
        [],
    )? {
        let history = rows(
            connection,
            "SELECT revision,commitment_json,state,changed_at FROM auth_resource_history \
            WHERE resource_id=?1 ORDER BY revision",
            [text(&row, "resource_id")],
        )?;
        found.push(StateResource::User {
            key: text(&row, "physical_id"),
            record: json!({
                "owner": "user",
                "userId": row["owner_id"],
                "namespace": decode::<Value>(&text(&row, "namespace_json"))?,
                "name": row["local_name"],
                "declaration": decode::<Value>(&text(&row, "commitment_json"))?,
                "revision": row["revision"],
                "createdAt": row["created_at"],
                "updatedAt": row["updated_at"],
                "history": history,
            }),
        });
    }
    for row in rows(
        connection,
        "SELECT r.owner_id,r.participant_id,r.local_name,r.physical_id FROM auth_resources r \
        JOIN auth_deployments d ON d.deployment_id=r.owner_id \
        WHERE r.owner_kind='deployment' AND r.kind='state' AND r.state='ready' \
        AND d.state!='revoked' AND d.participant_kind='device' \
        AND d.participant_id NOT LIKE 'trellis.%' ORDER BY r.owner_id,r.local_name",
        [],
    )? {
        let (deployment_id, participant_id, name) = (
            text(&row, "owner_id"),
            text(&row, "participant_id"),
            text(&row, "local_name"),
        );
        // The devices a deployment's bundle carries: each value is keyed by
        // its device's principal, which keeps its ID.
        let mut devices = BTreeMap::new();
        for device in rows(
            connection,
            "SELECT i.principal_id,i.instance_id FROM auth_instances i \
            JOIN auth_provisioned_identities p ON p.instance_id=i.instance_id \
            AND p.state='active' WHERE i.deployment_id=?1 AND i.state IN ('active','disabled')",
            [&deployment_id],
        )? {
            let principal = text(&device, "principal_id");
            let (suffix, _) = SqliteAuthorizationStore::state_namespace(
                &principal,
                &trellis_protocol::SessionBinding::Device {
                    deployment_id: deployment_id.clone(),
                    instance_id: text(&device, "instance_id"),
                    participant_id: participant_id.clone(),
                },
                &name,
            )?;
            devices.insert(suffix, principal);
        }
        found.push(StateResource::Device {
            prefix: format!("{}.", text(&row, "physical_id")),
            devices,
            record: json!({
                "owner": "device",
                "deploymentId": deployment_id,
                "participantId": participant_id,
                "name": name,
            }),
        });
    }
    Ok(found)
}

/// The `state` records of `resources`, with their values read from the
/// State bucket. A value no carried device owns stays behind.
async fn state_records(
    nats: &async_nats::Client,
    resources: Vec<StateResource>,
) -> Result<Vec<Value>, AuthError> {
    if resources.is_empty() {
        return Ok(Vec::new());
    }
    // An export only reads: a missing bucket is State this installation
    // lost, which an export must not hide or paper over by making one.
    let store = crate::platform::state::existing_store(&async_nats::jetstream::new(nats.clone()))
        .await
        .map_err(|error| AuthError::Broker(error.to_string()))?
        .ok_or_else(|| {
            AuthError::Broker(format!(
                "{} State resources are registered, and this installation's NATS has no \
                State bucket to read their values from; restore its JetStream, for example \
                from a backup, before exporting",
                resources.len()
            ))
        })?;
    let value = |entry: Option<async_nats::jetstream::kv::Entry>| {
        entry
            .filter(|entry| entry.operation == async_nats::jetstream::kv::Operation::Put)
            .map(|entry| json!({ "base64": STANDARD.encode(&entry.value) }))
    };
    let mut keys = None;
    let mut records = Vec::new();
    for resource in resources {
        match resource {
            StateResource::User { mut record, key } => {
                let entry = store
                    .entry(&key)
                    .await
                    .map_err(|error| AuthError::Broker(error.to_string()))?;
                record["value"] = value(entry).unwrap_or(Value::Null);
                records.push(record);
            }
            StateResource::Device {
                mut record,
                prefix,
                devices,
            } => {
                if keys.is_none() {
                    let mut listed = Vec::new();
                    let mut stream = store
                        .keys()
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                    while let Some(key) = futures_util::StreamExt::next(&mut stream).await {
                        listed.push(key.map_err(|error| AuthError::Broker(error.to_string()))?);
                    }
                    keys = Some(listed);
                }
                let mut values = Vec::new();
                for key in keys.iter().flatten() {
                    let Some(principal) = key
                        .strip_prefix(&prefix)
                        .and_then(|suffix| devices.get(suffix))
                    else {
                        continue;
                    };
                    let entry = store
                        .entry(key)
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                    if let Some(value) = value(entry) {
                        values.push(json!({ "principalId": principal, "value": value }));
                    }
                }
                record["values"] = Value::Array(values);
                records.push(record);
            }
        }
    }
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
    let (mut records, deployments, administrator, state) = {
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
                    read_state(connection)?,
                ))
            })
        })
        .await?
    };
    records.insert("state", state_records(nats, state).await?);
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
    for (kind, _) in KINDS {
        counts.insert(
            kind.to_owned(),
            json!(records.get(kind).map_or(0, Vec::len)),
        );
    }
    write_records(out, &records, MAX_RECORDS)?;
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

/// Read the content `input` holds, refusing a record entry larger than
/// `limit` bytes, and spool each bundle into `directory`.
fn receive(input: &mut dyn Read, directory: &Path, limit: usize) -> Result<Received, AuthError> {
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
        read_entry(input, &mut bytes, limit)?;
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
            // A kind's records may come in several entries.
            received
                .records
                .entry(kind.to_owned())
                .or_default()
                .extend(current_records(kind, format, records)?);
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

/// A State value to write once the import's transaction commits.
struct StateWrite {
    key: String,
    value: Vec<u8>,
    /// The report entry naming it.
    item: Value,
}

/// The outcome of one import transaction.
#[derive(Default)]
struct Outcome {
    created: Vec<Value>,
    matched: Vec<Value>,
    changed: Vec<Value>,
    skipped: Vec<Value>,
    access: Value,
    administrator: Value,
    /// Deployments recorded by this import, whose data loads afterwards.
    deployments: Vec<String>,
    /// User State values, written after the transaction commits.
    state: Vec<StateWrite>,
    /// Who each carried user of the export is here: the user this import
    /// created or found as the same person, by the exported user ID.
    people: BTreeMap<String, String>,
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

    /// An installation-wide setting the export replaced, with what it was.
    fn changed(&mut self, kind: &str, id: &str, old: Value, new: Value) {
        self.changed
            .push(json!({ "kind": kind, "id": id, "old": old, "new": new }));
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

/// What an OIDC provider record sets, as a report shows it. The client
/// secret is never shown.
fn provider_settings(record: &Map<String, Value>) -> Value {
    json!({
        "displayName": record.get("display_name"),
        "clientId": record.get("client_id"),
        "config": record
            .get("config_json")
            .and_then(Value::as_str)
            .and_then(|config| serde_json::from_str::<Value>(config).ok()),
        "claimFreshnessSeconds": record.get("claim_freshness_seconds"),
        "state": record.get("state"),
        "pinnedCertificate": record.get("issuer_certificate").is_some_and(|value| !value.is_null()),
    })
}

/// What a Portal theme record sets, as a report shows it.
fn theme_settings(record: &Map<String, Value>) -> Value {
    json!({
        "theme": record
            .get("theme_json")
            .and_then(Value::as_str)
            .and_then(|theme| serde_json::from_str::<Value>(theme).ok()),
        "logo": record.get("logo").is_some_and(|value| !value.is_null()),
        "stylesheet": record.get("stylesheet").is_some_and(|value| !value.is_null()),
    })
}

/// An app record in a form two installations can compare.
fn app_settings(record: &Map<String, Value>) -> Result<Value, AuthError> {
    let field = |name: &str| decode::<Value>(&text(record, name));
    let mut implied = field("implied_capabilities_json")?
        .as_array()
        .cloned()
        .unwrap_or_default();
    implied.sort_by_key(Value::to_string);
    Ok(json!({
        "kind": record.get("kind"),
        "displayName": record.get("display_name"),
        "redirectUris": field("redirect_uris_json")?,
        "development": record.get("development"),
        "impliedConsent": implied,
        "metadata": field("metadata_json")?,
        "state": record.get("state"),
    }))
}

/// Everything an import writes to the Auth database, in one transaction.
/// Settings take the export's values; an entity that something here is in
/// the way of is skipped with its reason, and so is everything that would
/// attach to it.
fn apply(
    connection: &Connection,
    storage: &OAuthStorage,
    received: &Received,
    bundles: &[(String, service_bundles::Bundle)],
    now: i64,
) -> Result<Outcome, AuthError> {
    let mut outcome = Outcome::default();
    let found = existing(connection)?;
    outcome.target = json!({ "empty": found.is_empty(), "existing": found });
    let administrator: String = connection
        .query_row(
            "SELECT principal_id FROM auth_bootstrap_administrator",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            AuthError::Invalid(
                "this installation has no administrator yet; finish its setup first".into(),
            )
        })?;
    let identity = Mutation {
        actor: administrator.clone(),
        request_id: id(),
        request_digest: id(),
        session: None,
    };

    // Sign-in settings: the export's, whatever this installation set.
    if let Some(settings) = records(received, "signInSettings").first() {
        let current: (bool, bool, i64) = connection.query_row(
            "SELECT local_users,local_sign_up,password_min_length FROM auth_sign_in_settings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let wanted = (
            settings["local_users"].as_i64() == Some(1),
            settings["local_sign_up"].as_i64() == Some(1),
            settings["password_min_length"]
                .as_i64()
                .unwrap_or(current.2),
        );
        if wanted != current {
            connection.execute(
                "UPDATE auth_sign_in_settings SET local_users=?1,local_sign_up=?2,\
                password_min_length=?3,revision=revision+1,updated_at=?4",
                params![wanted.0, wanted.1, wanted.2, now],
            )?;
            let shown = |(local_users, local_sign_up, minimum): (bool, bool, i64)| {
                json!({
                    "localUsers": local_users,
                    "localSignUp": local_sign_up,
                    "passwordMinLength": minimum,
                })
            };
            outcome.changed("signInSettings", "", shown(current), shown(wanted));
        }
    }

    // OIDC providers, and which provider here each exported one is. The
    // same issuer is the same provider, and takes the export's settings.
    let mut providers = BTreeMap::new();
    for provider in records(received, "oidcProviders") {
        let Some(record) = provider.as_object() else {
            return Err(damaged("an OIDC provider is not a record"));
        };
        let provider_id = text(record, "provider_id");
        let issuer = text(record, "issuer");
        let same_id: Option<(String, String)> = connection
            .query_row(
                "SELECT issuer,state FROM auth_oidc_providers WHERE provider_id=?1",
                [&provider_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let same_issuer: Option<String> = connection
            .query_row(
                "SELECT provider_id FROM auth_oidc_providers WHERE issuer=?1 AND state!='deleted'",
                [&issuer],
                |row| row.get(0),
            )
            .optional()?;
        let here = match (same_id, same_issuer) {
            (Some((here, state)), _) if here == issuer && state != "deleted" => provider_id.clone(),
            (Some(_), _) => {
                outcome.skipped(
                    "oidcProvider",
                    &provider_id,
                    "a provider with this ID and another issuer, or a deleted one, exists here; \
                    it was left as it is, and sign-ins through it were not brought",
                );
                continue;
            }
            (None, Some(here)) => here,
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
                continue;
            }
        };
        providers.insert(provider_id.clone(), here.clone());
        let current = rows(
            connection,
            "SELECT display_name,client_id,issuer_certificate,config_json,\
            claim_freshness_seconds,state FROM auth_oidc_providers WHERE provider_id=?1",
            [&here],
        )?
        .into_iter()
        .next()
        .ok_or(AuthError::Unavailable)?;
        let sealed: Option<Vec<u8>> = connection.query_row(
            "SELECT sealed_client_secret FROM auth_oidc_providers WHERE provider_id=?1",
            [&here],
            |row| row.get(0),
        )?;
        let secret_here = sealed
            .map(|sealed| {
                storage
                    .open_upstream_client_secret(&here, &sealed)
                    .map_err(|_| AuthError::Unavailable)
            })
            .transpose()?;
        let secret = record.get("client_secret").and_then(Value::as_str);
        let fields = [
            "display_name",
            "client_id",
            "issuer_certificate",
            "config_json",
            "claim_freshness_seconds",
            "state",
        ];
        let secret_changed = secret_here.as_deref() != secret;
        if !secret_changed
            && fields
                .iter()
                .all(|field| current.get(*field) == record.get(*field))
        {
            continue;
        }
        let sealed = secret
            .map(|secret| {
                storage
                    .seal_upstream_client_secret(&here, secret)
                    .map_err(|_| AuthError::Unavailable)
            })
            .transpose()?;
        let value = |field: &str| sql_value(record.get(field).unwrap_or(&Value::Null));
        connection.execute(
            "UPDATE auth_oidc_providers SET display_name=?2,client_id=?3,issuer_certificate=?4,\
            config_json=?5,claim_freshness_seconds=?6,state=?7,sealed_client_secret=?8,\
            revision=revision+1,updated_at=?9 WHERE provider_id=?1",
            params![
                here,
                value("display_name")?,
                value("client_id")?,
                value("issuer_certificate")?,
                value("config_json")?,
                value("claim_freshness_seconds")?,
                value("state")?,
                sealed,
                now
            ],
        )?;
        let mut new = provider_settings(record);
        new["clientSecretChanged"] = json!(secret_changed);
        outcome.changed("oidcProvider", &here, provider_settings(&current), new);
    }

    for registry in records(received, "packageRegistries") {
        let name = registry["name"].as_str().unwrap_or_default().to_owned();
        let here: Option<(String, Option<String>, Option<Vec<u8>>)> = connection
            .query_row(
                "SELECT prefix,username,sealed_secret FROM auth_package_registries WHERE name=?1",
                [&name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((prefix, username, sealed)) = here {
            let secret = sealed
                .map(|sealed| {
                    storage
                        .decode::<String>(super::registries::SEAL, &name, &sealed)
                        .map_err(|_| AuthError::Unavailable)
                })
                .transpose()?;
            if registry["prefix"].as_str() != Some(prefix.as_str())
                || registry["username"].as_str() != username.as_deref()
                || registry["secret"].as_str() != secret.as_deref()
            {
                outcome.skipped(
                    "packageRegistry",
                    &name,
                    "a package registry with this name and other settings exists here; it was \
                    left as it is",
                );
            }
            continue;
        }
        let result = attempt(connection, || {
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

    // Users. The exported administrator is not one of them: what it held
    // goes to this installation's administrator. Every other user is
    // created, or found here as the same person, or skipped because someone
    // else is in the way, and then nothing names them.
    let mut known = BTreeMap::new();
    let mut user_refs = Vec::new();
    let mut exported_administrator = None;
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
        let user_ref = UserRef {
            user_id: user_id.clone(),
            identities,
        };
        user_refs.push(user_ref.clone());
        if user["defaultAdministrator"].as_bool() == Some(true) {
            known.insert(
                user_id.clone(),
                Match::Found {
                    principal: administrator.clone(),
                    by: "administrator",
                },
            );
            exported_administrator = Some(user_id);
            continue;
        }
        let found = match access_records::find_user(connection, &user_ref)? {
            Match::Missing => match attempt(connection, || {
                create_user(connection, user, &providers, now)
            })? {
                Ok(()) => {
                    outcome.created("user", &user_id);
                    Match::Found {
                        principal: user_id.clone(),
                        by: "id",
                    }
                }
                Err(reason) => {
                    outcome.skipped("user", &user_id, reason.clone());
                    Match::Conflict(reason)
                }
            },
            Match::Found { principal, by } => {
                outcome.matched.push(json!({
                    "kind": "user",
                    "id": user_id,
                    "userId": principal,
                    "matchedBy": by,
                }));
                Match::Found { principal, by }
            }
            Match::Conflict(reason) => {
                outcome.skipped(
                    "user",
                    &user_id,
                    format!("{reason}; nothing was given to that user"),
                );
                Match::Conflict(reason)
            }
        };
        if let Match::Found { principal, .. } = &found {
            outcome.people.insert(user_id.clone(), principal.clone());
        }
        known.insert(user_id, found);
    }
    // Why a record naming the exported user `user_id` is skipped.
    let unplaced = |user_id: &str| match known.get(user_id) {
        Some(Match::Found {
            by: "administrator",
            ..
        }) => "it names the exported administrator, which is not imported as a user".to_owned(),
        Some(Match::Conflict(reason)) => format!("its user was skipped: {reason}"),
        _ => format!("its user {user_id} is not in the export"),
    };

    // Deployments, through the service import, without their access: the
    // roles and grants below cover every API at once.
    for (deployment_id, bundle) in bundles {
        let result = attempt(connection, || {
            service_bundles::apply_import(connection, storage, bundle, &administrator, now)
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
    let recreated = access_records::recreate(connection, &identity, &access, &known, now)?;
    // Roles left as they are here, which nothing of the export joins.
    let skipped_roles: BTreeSet<String> = recreated
        .skipped
        .iter()
        .filter(|item| item["kind"] == "role")
        .filter_map(|item| item["roleId"].as_str().map(str::to_owned))
        .collect();
    if let Some(exported) = exported_administrator {
        outcome.administrator = json!({
            "exportedUserId": exported,
            "userId": administrator,
            "detail": "the exported administrator is not imported as a user; this \
                installation's administrator received the roles and grants it held",
            "received": recreated
                .recreated
                .iter()
                .filter(|item| item["matchedBy"] == "administrator")
                .collect::<Vec<_>>(),
        });
    }
    outcome.access = serde_json::to_value(recreated)?;

    for mapping in records(received, "oidcRoleMappings") {
        let mapping_id = mapping["mapping_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let Some(provider) = providers.get(mapping["provider_id"].as_str().unwrap_or_default())
        else {
            outcome.skipped(
                "oidcRoleMapping",
                &mapping_id,
                "its provider was not brought",
            );
            continue;
        };
        let mut roles = BTreeSet::new();
        for role in mapping["roles"].as_array().into_iter().flatten() {
            let role = role.as_str().unwrap_or_default().to_owned();
            if skipped_roles.contains(&role) {
                outcome.skipped(
                    "oidcRoleMappingRole",
                    &mapping_id,
                    format!("role {role} was skipped, so the mapping does not give it"),
                );
            } else {
                roles.insert(role);
            }
        }
        let claim = (
            mapping["claim_name"].as_str().unwrap_or_default(),
            mapping["claim_value"].as_str().unwrap_or_default(),
        );
        let here: Option<(String, String, String)> = connection
            .query_row(
                "SELECT provider_id,claim_name,claim_value FROM auth_oidc_role_mappings \
                WHERE mapping_id=?1",
                [&mapping_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((provider_here, name, value)) = here {
            let held: BTreeSet<String> = connection
                .prepare(
                    "SELECT m.role_id FROM auth_oidc_mapping_roles m JOIN auth_roles r \
                    ON r.role_id=m.role_id AND r.identity_generation=m.role_generation \
                    WHERE m.mapping_id=?1 AND r.state='active'",
                )?
                .query_map([&mapping_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            // The roles of the record this installation has.
            let mut wanted = BTreeSet::new();
            for role in &roles {
                if connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_roles WHERE role_id=?1 AND state='active')",
                    [role],
                    |row| row.get::<_, bool>(0),
                )? {
                    wanted.insert(role.clone());
                }
            }
            if provider_here != *provider
                || (name.as_str(), value.as_str()) != claim
                || wanted != held
            {
                outcome.skipped(
                    "oidcRoleMapping",
                    &mapping_id,
                    "a mapping with this ID and other settings exists here; it was left as it is",
                );
            }
            continue;
        }
        let result = attempt(connection, || {
            connection
                .execute(
                    "INSERT INTO auth_oidc_role_mappings(mapping_id,provider_id,claim_name,\
                    claim_value,revision) VALUES(?1,?2,?3,?4,1)",
                    params![mapping_id, provider, claim.0, claim.1],
                )
                .map_err(|_| {
                    AuthError::InUse(
                        "a mapping of the same claim exists here; it was left as it is".into(),
                    )
                })?;
            for role in &roles {
                connection.execute(
                    "INSERT INTO auth_oidc_mapping_roles(mapping_id,role_id,role_generation) \
                    SELECT ?1,role_id,identity_generation FROM auth_roles \
                    WHERE role_id=?2 AND state='active'",
                    params![mapping_id, role],
                )?;
            }
            Ok(())
        })?;
        match result {
            Ok(()) => outcome.created("oidcRoleMapping", &mapping_id),
            Err(reason) => outcome.skipped("oidcRoleMapping", &mapping_id, reason),
        }
    }

    // Apps. One with the same client ID and other settings is someone
    // else's app, which nothing of the export attaches to.
    let mut skipped_apps = BTreeSet::new();
    for client in records(received, "oauthClients") {
        let Some(record) = client.as_object() else {
            return Err(damaged("an OAuth client is not a record"));
        };
        let client_id = text(record, "client_id");
        let here = rows(
            connection,
            "SELECT * FROM auth_oauth_clients WHERE client_id=?1",
            [&client_id],
        )?
        .into_iter()
        .next();
        if BUILTIN_CLIENTS.contains(&client_id.as_str()) {
            // Each installation registers its own Console and CLI, for its
            // own origin. Who may sign up through them moves.
            let wanted = decode::<Value>(&text(record, "metadata_json"))?
                .get("sign_up")
                .cloned();
            let Some(mut metadata) = here
                .map(|here| decode::<Value>(&text(&here, "metadata_json")))
                .transpose()?
            else {
                continue;
            };
            let old = metadata.get("sign_up").cloned();
            if old == wanted {
                continue;
            }
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
            outcome.changed(
                "clientSignUp",
                &client_id,
                old.unwrap_or(Value::Null),
                wanted.unwrap_or(Value::Null),
            );
            continue;
        }
        if let Some(here) = here {
            if text(&here, "state") != "deleted" && app_settings(&here)? == app_settings(record)? {
                outcome
                    .matched
                    .push(json!({ "kind": "oauthClient", "id": client_id }));
            } else {
                outcome.skipped(
                    "oauthClient",
                    &client_id,
                    "an app with this client ID and other settings exists here; it was left \
                    as it is, and nothing of the export attaches to it",
                );
                skipped_apps.insert(client_id);
            }
            continue;
        }
        match attempt(connection, || {
            insert(connection, "auth_oauth_clients", record)
        })? {
            Ok(()) => outcome.created("oauthClient", &client_id),
            Err(reason) => {
                outcome.skipped("oauthClient", &client_id, reason);
                skipped_apps.insert(client_id);
            }
        }
    }

    // Consent, for every user carried, to apps and capabilities here.
    let mut consents = 0;
    let mut left = BTreeMap::new();
    for consent in records(received, "consents") {
        let principal = consent["principal_id"].as_str().unwrap_or_default();
        let client_id = consent["client_id"].as_str().unwrap_or_default();
        let Some(here) = outcome.people.get(principal) else {
            left.entry(("user", principal.to_owned()))
                .or_insert_with(|| unplaced(principal));
            continue;
        };
        if skipped_apps.contains(client_id) {
            left.entry(("app", client_id.to_owned()))
                .or_insert_with(|| "its app was skipped".to_owned());
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
                here,
                client_id,
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
    for ((kind, id), reason) in left {
        outcome.skipped(
            if kind == "user" {
                "consentsOfUser"
            } else {
                "consentsToApp"
            },
            &id,
            reason,
        );
    }

    // Themes: the installation's, and each carried app's, as the export
    // has them.
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
        if !client_id.is_empty() {
            let here: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients \
                WHERE client_id=?1 AND state!='deleted')",
                [&client_id],
                |row| row.get(0),
            )?;
            if skipped_apps.contains(&client_id) || !here {
                outcome.skipped("portalTheme", label, "its app was skipped or is not here");
                continue;
            }
        }
        let here = rows(
            connection,
            "SELECT client_id,theme_json,logo_format,logo,stylesheet FROM auth_portal_themes \
            WHERE client_id=?1",
            [&client_id],
        )?;
        if here.first() == Some(record) {
            continue;
        }
        connection.execute(
            "DELETE FROM auth_portal_themes WHERE client_id=?1",
            [&client_id],
        )?;
        let mut row = record.clone();
        row.insert("revision".into(), json!(1));
        row.insert("updated_at".into(), json!(now));
        insert(connection, "auth_portal_themes", &row)?;
        match here.first() {
            Some(here) => {
                outcome.changed(
                    "portalTheme",
                    label,
                    theme_settings(here),
                    theme_settings(record),
                );
            }
            None => outcome.created("portalTheme", label),
        }
    }

    // Users' State, under each carried user and app here. Values are written
    // once the transaction commits.
    for record in records(received, "state") {
        if record["owner"] != "user" {
            continue;
        }
        let user_id = record["userId"].as_str().unwrap_or_default();
        let name = record["name"].as_str().unwrap_or_default();
        let label = format!("{user_id}/{name}");
        let Some(principal) = outcome.people.get(user_id).cloned() else {
            outcome.skipped("state", &label, unplaced(user_id));
            continue;
        };
        let namespace = record["namespace"].clone();
        if let Some(client) = namespace["clientId"]
            .as_str()
            .filter(|client| skipped_apps.contains(*client))
        {
            outcome.skipped("state", &label, format!("its app {client} was skipped"));
            continue;
        }
        let resource_id = digest(&("trellis.state", &principal, &namespace, name))?;
        // The declaration names its resource, which the user it is under
        // here decides.
        let named = |declaration: &Value| {
            let mut declaration = declaration.clone();
            declaration["resource_id"] = Value::String(resource_id.clone());
            declaration
        };
        let declaration = named(&record["declaration"]);
        let here: Option<(String, String)> = connection
            .query_row(
                "SELECT commitment_json,physical_id FROM auth_resources WHERE resource_id=?1",
                [&resource_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let key = match here {
            Some((commitment, physical)) => {
                if decode::<Value>(&commitment)? != declaration {
                    outcome.skipped(
                        "state",
                        &label,
                        "the user has this State here with another representation; it was \
                        left as it is",
                    );
                    continue;
                }
                physical
            }
            None => {
                let allocation = id();
                let physical = format!("{resource_id}.{allocation}");
                let created = record["createdAt"].as_i64().unwrap_or(now);
                connection.execute(
                    "INSERT INTO auth_resources(resource_id,allocation_id,owner_kind,owner_id,\
                    namespace_json,kind,local_name,commitment_json,physical_id,state,\
                    binding_revision,revision,created_at,updated_at) \
                    VALUES(?1,?2,'user',?3,?4,'state',?5,?6,?7,'ready',1,?8,?9,?10)",
                    params![
                        resource_id,
                        allocation,
                        principal,
                        json(&namespace)?,
                        name,
                        json(&declaration)?,
                        physical,
                        record["revision"].as_i64().unwrap_or(1),
                        created,
                        record["updatedAt"].as_i64().unwrap_or(now).max(created)
                    ],
                )?;
                for revision in record["history"].as_array().into_iter().flatten() {
                    connection.execute(
                        "INSERT INTO auth_resource_history(resource_id,revision,\
                        commitment_json,state,changed_at) VALUES(?1,?2,?3,?4,?5)",
                        params![
                            resource_id,
                            revision["revision"].as_i64(),
                            json(&named(&decode::<Value>(
                                revision["commitment_json"].as_str().unwrap_or("{}")
                            )?))?,
                            revision["state"].as_str(),
                            revision["changed_at"].as_i64()
                        ],
                    )?;
                }
                physical
            }
        };
        let item = json!({ "kind": "state", "id": label, "userId": principal });
        match record["value"]["base64"].as_str() {
            Some(value) => outcome.state.push(StateWrite {
                key,
                value: STANDARD
                    .decode(value)
                    .map_err(|_| damaged("a State value is not base64"))?,
                item,
            }),
            None => outcome.created.push(item),
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
    let received = receive(input, directory, MAX_RECORDS)?;
    // State is kept until it is deleted, so its values go only into a bucket
    // that keeps them, which is checked before anything changes. A preview
    // opens the bucket without making one.
    let carries_state = records(&received, "state").iter().any(|record| {
        !record["value"].is_null()
            || record["values"]
                .as_array()
                .is_some_and(|values| !values.is_empty())
    });
    let state_store = {
        let jetstream = async_nats::jetstream::new(nats.clone());
        match (carries_state, check) {
            (false, _) => Ok(None),
            (true, true) => crate::platform::state::existing_store(&jetstream).await,
            (true, false) => crate::platform::state::open_store(&jetstream)
                .await
                .map(Some),
        }
        .map_err(|error| AuthError::Broker(error.to_string()))?
    };
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
    // Devices' State goes where each device deployment's State resource is
    // here, which exists once its data has loaded.
    let mut writes = std::mem::take(&mut outcome.state);
    let devices: Vec<Value> = records(&received, "state")
        .iter()
        .filter(|record| record["owner"] == "device")
        .cloned()
        .collect();
    let placed = storage
        .authority
        .transaction_blocking(move |connection| {
            let mut placed = Vec::new();
            for record in &devices {
                let deployment_id = record["deploymentId"].as_str().unwrap_or_default();
                let participant_id = record["participantId"].as_str().unwrap_or_default();
                let name = record["name"].as_str().unwrap_or_default();
                let physical: Option<String> = connection
                    .query_row(
                        "SELECT physical_id FROM auth_resources WHERE owner_kind='deployment' \
                        AND owner_id=?1 AND participant_id=?2 AND kind='state' \
                        AND local_name=?3 AND state='ready'",
                        params![deployment_id, participant_id, name],
                        |row| row.get(0),
                    )
                    .optional()?;
                for value in record["values"].as_array().into_iter().flatten() {
                    let principal = value["principalId"].as_str().unwrap_or_default();
                    let item = json!({
                        "kind": "deviceState",
                        "id": format!("{deployment_id}/{name}"),
                        "deviceId": principal,
                    });
                    let instance: Option<String> = connection
                        .query_row(
                            "SELECT instance_id FROM auth_instances \
                            WHERE deployment_id=?1 AND principal_id=?2",
                            params![deployment_id, principal],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let bytes = STANDARD
                        .decode(value["value"]["base64"].as_str().unwrap_or_default())
                        .map_err(|_| damaged("a State value is not base64"))?;
                    placed.push(match (&physical, instance) {
                        _ if check => Ok((None, bytes, item)),
                        (Some(physical), Some(instance_id)) => {
                            let (suffix, _) = SqliteAuthorizationStore::state_namespace(
                                principal,
                                &trellis_protocol::SessionBinding::Device {
                                    deployment_id: deployment_id.to_owned(),
                                    instance_id,
                                    participant_id: participant_id.to_owned(),
                                },
                                name,
                            )?;
                            Ok((Some(format!("{physical}.{suffix}")), bytes, item))
                        }
                        _ => Err(item),
                    });
                }
            }
            Ok(placed)
        })
        .await?;
    for placed in placed {
        match placed {
            Ok((Some(key), value, item)) => writes.push(StateWrite { key, value, item }),
            Ok((None, _, item)) => outcome.created.push(item),
            Err(item) => outcome.skipped.push({
                let mut item = item;
                item["reason"] = json!(
                    "its device or its deployment's State is not here yet; run the same import \
                    again once the deployment's data has loaded"
                );
                item
            }),
        }
    }
    // State values, each written where none is yet. One that differs from
    // the value here is left as it is.
    if check {
        outcome
            .created
            .extend(writes.into_iter().map(|write| write.item));
    } else if let Some(store) = state_store.filter(|_| !writes.is_empty()) {
        let broker = |error: &dyn std::fmt::Display| AuthError::Broker(error.to_string());
        for write in writes {
            let here = store
                .entry(&write.key)
                .await
                .map_err(|error| broker(&error))?
                .filter(|entry| entry.operation == async_nats::jetstream::kv::Operation::Put);
            match here {
                Some(here) if here.value.as_ref() == write.value.as_slice() => {}
                Some(_) => outcome.skipped.push({
                    let mut item = write.item;
                    item["reason"] = json!("another value is stored here; it was left as it is");
                    item
                }),
                None => {
                    store
                        .put(&write.key, write.value.into())
                        .await
                        .map_err(|error| broker(&error))?;
                    outcome.created.push(write.item);
                }
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
        "matched": outcome.matched,
        "changed": outcome.changed,
        "skipped": outcome.skipped,
        "administrator": outcome.administrator,
        "access": outcome.access,
        "deployments": deployments,
        "notCarried": NOT_CARRIED,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The content of an export holding `records`, with entries of at most
    /// `limit` bytes, as an export writes it.
    fn content(
        records: &BTreeMap<&'static str, Vec<Value>>,
        limit: usize,
    ) -> Result<Vec<u8>, AuthError> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&FORMAT.to_be_bytes());
        write_entry(&mut out, "meta.json", &mut b"{}".as_slice())?;
        write_records(&mut out, records, limit)?;
        Ok(out)
    }

    /// A kind with more records than one entry an import reads is split
    /// across entries that each stay within it, and an import with the
    /// same bound reads every record back in order. A record that alone is
    /// larger fails the export instead of writing a file no import reads.
    #[test]
    fn an_export_writes_only_record_entries_its_import_reads() {
        let limit = 1024;
        let users: Vec<Value> = (0..40)
            .map(|index| json!({ "userId": index, "username": "x".repeat(100) }))
            .collect();
        let grants = vec![json!({ "userId": 1, "capability": "c" })];
        let records = BTreeMap::from([("users", users.clone()), ("grants", grants.clone())]);
        let written = content(&records, limit).unwrap();
        assert!(written.len() > 4 * limit);
        let directory = tempfile::tempdir().unwrap();
        let received = receive(&mut written.as_slice(), directory.path(), limit).unwrap();
        assert_eq!(received.records["users"], users);
        assert_eq!(received.records["grants"], grants);
        assert_eq!(received.records["roles"], Vec::<Value>::new());

        let oversized = BTreeMap::from([("users", vec![json!({ "username": "x".repeat(limit) })])]);
        assert!(matches!(
            content(&oversized, limit),
            Err(AuthError::Invalid(_))
        ));
    }
}
