//! Platform-owned Trellis State RPC runtime.

use crate::platform::auth::blocking;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, kv, stream::LastRawMessageErrorKind};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::service::{
    ConnectedServiceRuntime, DeclaredRpcError, RequestContext, ServerError, ValidationIssue,
};
use trellis_runtime_apis::apis::trellis_state_v1::lives::Watch as StateWatch;
use trellis_runtime_apis::apis::trellis_state_v1::rpc::{
    Delete as StateDeleteRpc, Get as StateGetRpc, Put as StatePutRpc, Register as StateRegisterRpc,
};
use trellis_runtime_apis::types::{Bytes as WireBytes, StateDeleteResponse, StateGetResponse};

use super::auth::{SqliteAuthorizationStore, StateDeclaration as Declaration};
use crate::supervisor::RuntimeError;

const BUCKET: &str = "trellis_state";
const STREAM: &str = "KV_trellis_state";
const SUBJECT_PREFIX: &str = "$KV.trellis_state.";
pub(crate) const OWNERSHIP_MARKER: &str = "Trellis shared State storage";

#[derive(Clone)]
pub(crate) struct StateRuntime {
    repository: Arc<SqliteAuthorizationStore>,
    store: kv::Store,
    jetstream: jetstream::Context,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredEnvelope {
    value: WireBytes,
    representation_version: u32,
    created_at: String,
    updated_at: String,
}

struct PhysicalEntry {
    revision: u64,
    envelope: StoredEnvelope,
}

impl StateRuntime {
    pub(crate) async fn start(
        nats: async_nats::Client,
        repository: Arc<SqliteAuthorizationStore>,
    ) -> Result<Self, RuntimeError> {
        let jetstream = jetstream::new(nats);
        let config = kv::Config {
            bucket: BUCKET.to_owned(),
            description: OWNERSHIP_MARKER.to_owned(),
            history: 1,
            max_age: Duration::ZERO,
            storage: jetstream::stream::StorageType::File,
            ..Default::default()
        };
        let store = match jetstream.get_key_value(BUCKET).await {
            Ok(store) => store,
            Err(open_error) => match jetstream.create_key_value(config).await {
                Ok(store) => store,
                Err(create_error) => {
                    jetstream
                        .get_key_value(BUCKET)
                        .await
                        .map_err(|retry_error| {
                            RuntimeError::Platform(format!(
                        "failed to open {BUCKET} ({open_error}), create it ({create_error}), or \
                        reopen it after a possible concurrent create ({retry_error})"
                    ))
                        })?
                }
            },
        };
        let status = store.status().await.map_err(|error| {
            RuntimeError::Platform(format!("failed to inspect {BUCKET}: {error}"))
        })?;
        // State reads the latest revision; retaining older revisions is harmless.
        // Expiration would violate State's promise to retain values until deletion.
        if status.max_age() != Duration::ZERO {
            return Err(RuntimeError::Platform(format!(
                "{BUCKET} must not expire State values"
            )));
        }
        Ok(Self {
            repository,
            store,
            jetstream,
        })
    }

    pub(crate) fn register<P: trellis_rs::generated::ParticipantDescriptor>(
        &self,
        router: &mut ConnectedServiceRuntime<P>,
    ) {
        let state = self.clone();
        router.register_rpc::<StateRegisterRpc, _, _>(move |context, input| {
            let state = state.clone();
            async move {
                let caller = context
                    .request()
                    .verified_request()
                    .ok_or_else(auth_denied)?
                    .caller()
                    .clone();
                blocking(move || {
                    Ok::<_, super::auth::AuthError>(
                        state
                            .repository
                            .register_state(
                                &caller.principal_id,
                                &caller.binding,
                                &input,
                                crate::clock::unix_now(),
                            )
                            .map_err(|error| match error {
                                super::auth::AuthError::Denied => auth_denied(),
                                error => unexpected(error),
                            }),
                    )
                })
                .await
                .map_err(unexpected)??
                .map_err(|conflict| {
                    ServerError::DeclaredRpc(DeclaredRpcError::new(
                        "trellis.state@v1::RegistrationConflict",
                        "State registration revision changed",
                        [("currentRevision", json!(conflict.current_revision.0))],
                    ))
                })
            }
        });
        let state = self.clone();
        router.register_rpc::<StateGetRpc, _, _>(move |context, input| {
            let state = state.clone();
            async move {
                let declaration = state
                    .normal_declaration(context.request(), &input.resource_name.0)
                    .await?;
                let entry = state
                    .authoritative_entry(&declaration)
                    .await?
                    .map(|entry| project_entry(&declaration, &entry))
                    .transpose()?;
                serde_json::from_value(json!({ "entry": entry })).map_err(ServerError::from)
            }
        });
        let state = self.clone();
        router.register_live::<StateWatch, _, _>(move |context, input| {
            let state = state.clone();
            futures_util::stream::once(async move {
                let declaration = state
                    .normal_declaration(context.context.request(), &input.resource_name.0)
                    .await?;
                // Establish ordered JetStream delivery before reading the baseline.
                // Entries already covered by that baseline are discarded by revision.
                let updates = state
                    .store
                    .watch(&declaration.resource_id)
                    .await
                    .map_err(kv_error)?;
                let raw = state.authoritative_raw(&declaration.resource_id).await?;
                let baseline = raw.as_ref().map_or(0, |(_, revision, _)| *revision);
                let initial = raw
                    .filter(|(put, _, _)| *put)
                    .map(|(_, revision, payload)| {
                        let entry = PhysicalEntry {
                            revision,
                            envelope: serde_json::from_slice(&payload)?,
                        };
                        project_entry(&declaration, &entry)
                    })
                    .transpose()?;
                let initial: StateGetResponse = serde_json::from_value(json!({"entry":initial}))?;
                let updates = updates.filter_map(move |entry| {
                    let value = match entry {
                        Ok(entry) if entry.revision <= baseline => None,
                        Err(error) => Some(Err(kv_error(error))),
                        Ok(entry) => Some((|| {
                            let current = if entry.operation == kv::Operation::Put {
                                Some(project_entry(
                                    &declaration,
                                    &PhysicalEntry {
                                        revision: entry.revision,
                                        envelope: serde_json::from_slice(&entry.value)?,
                                    },
                                )?)
                            } else {
                                None
                            };
                            serde_json::from_value::<StateGetResponse>(json!({"entry":current}))
                                .map_err(ServerError::from)
                        })()),
                    };
                    futures_util::future::ready(value)
                });
                Ok::<_, ServerError>(
                    futures_util::stream::once(futures_util::future::ready(Ok(initial)))
                        .chain(updates),
                )
            })
            .try_flatten()
        });
        let state = self.clone();
        router.register_rpc::<StatePutRpc, _, _>(move |context, input| {
            let state = state.clone();
            async move {
                let declaration = state
                    .normal_declaration(context.request(), &input.resource_name.0)
                    .await?;
                let revision = input
                    .revision
                    .as_ref()
                    .map(|value| parse_revision(&value.0))
                    .transpose()?;
                let mode = input.mode.as_str();
                if (mode == "replace") != revision.is_some() {
                    return Err(validation(
                        "/revision",
                        "replace requires revision; create and set forbid it",
                    ));
                }
                if !matches!(mode, "create" | "set" | "replace") {
                    return Err(validation("/mode", "mode is invalid"));
                }
                validate_representation(
                    &declaration,
                    input.representation_version.0,
                    &input.value,
                    false,
                )?;
                let current = if mode == "set" {
                    None
                } else {
                    state.authoritative_entry(&declaration).await?
                };
                if (mode == "create" && current.is_some())
                    || (mode == "replace"
                        && current.as_ref().map(|entry| entry.revision) != revision)
                {
                    return Err(conflict(current.as_ref(), &declaration)?);
                }
                let now = timestamp(OffsetDateTime::now_utc())?;
                let envelope = StoredEnvelope {
                    value: input.value,
                    representation_version: input.representation_version.0,
                    created_at: current
                        .as_ref()
                        .map_or_else(|| now.clone(), |entry| entry.envelope.created_at.clone()),
                    updated_at: now,
                };
                let key = &declaration.resource_id;
                let result: Result<u64, ServerError> = match mode {
                    "set" => state
                        .store
                        .put(key, encode(&envelope)?)
                        .await
                        .map_err(kv_error),
                    "replace" => state
                        .store
                        .update(
                            key,
                            encode(&envelope)?,
                            revision.ok_or_else(|| {
                                validation("/revision", "replace requires revision")
                            })?,
                        )
                        .await
                        .map_err(kv_error),
                    "create" => match current {
                        Some(_) => return Err(conflict(current.as_ref(), &declaration)?),
                        None => match state.authoritative_raw(key).await? {
                            Some((false, tombstone_revision, _)) => state
                                .store
                                .update(key, encode(&envelope)?, tombstone_revision)
                                .await
                                .map_err(kv_error),
                            Some((true, _, _)) => {
                                let latest = state.authoritative_entry(&declaration).await?;
                                return Err(conflict(latest.as_ref(), &declaration)?);
                            }
                            None => state
                                .store
                                .create(key, encode(&envelope)?)
                                .await
                                .map_err(kv_error),
                        },
                    },
                    _ => return Err(validation("/mode", "mode is invalid")),
                };
                match result {
                    Ok(revision) => serde_json::from_value(json!({
                        "entry": public_entry(revision, &envelope)
                    }))
                    .map_err(ServerError::from),
                    Err(error) if mode != "set" => {
                        let current = state.authoritative_entry(&declaration).await?;
                        if current.as_ref().map(|entry| entry.revision) != revision {
                            Err(conflict(current.as_ref(), &declaration)?)
                        } else {
                            Err(error)
                        }
                    }
                    Err(error) => Err(error),
                }
            }
        });
        let state = self.clone();
        router.register_rpc::<StateDeleteRpc, _, _>(move |context, input| {
            let state = state.clone();
            async move {
                let declaration = state
                    .normal_declaration(context.request(), &input.resource_name.0)
                    .await?;
                let expected = input
                    .revision
                    .as_ref()
                    .map(|value| parse_revision(&value.0))
                    .transpose()?;
                if expected.is_none() {
                    let deleted = state.authoritative_entry(&declaration).await?.is_some();
                    state
                        .store
                        .delete(&declaration.resource_id)
                        .await
                        .map_err(kv_error)?;
                    return Ok(StateDeleteResponse {
                        deleted,
                        extra: Default::default(),
                    });
                }
                let Some(current) = state.authoritative_entry(&declaration).await? else {
                    return Err(conflict(None, &declaration)?);
                };
                if expected.is_some_and(|expected| expected != current.revision) {
                    return Err(conflict(Some(&current), &declaration)?);
                }
                match state
                    .store
                    .delete_expect_revision(&declaration.resource_id, expected)
                    .await
                {
                    Ok(()) => Ok(StateDeleteResponse {
                        deleted: true,
                        extra: Default::default(),
                    }),
                    Err(error) => {
                        let latest = state.authoritative_entry(&declaration).await?;
                        if latest.as_ref().map(|entry| entry.revision) != Some(current.revision) {
                            Err(conflict(latest.as_ref(), &declaration)?)
                        } else {
                            Err(kv_error(error))
                        }
                    }
                }
            }
        });
    }

    async fn normal_declaration(
        &self,
        context: &RequestContext,
        resource_name: &str,
    ) -> Result<Declaration, ServerError> {
        let verified = context.verified_request().ok_or_else(auth_denied)?;
        let caller = verified.caller();
        let repository = self.repository.clone();
        if matches!(
            caller.binding,
            trellis_protocol::SessionBinding::Device { .. }
        ) {
            let (principal, binding, name) = (
                caller.principal_id.clone(),
                caller.binding.clone(),
                resource_name.to_owned(),
            );
            let digest = verified
                .authority()
                .authority()
                .resource_bindings_digest
                .clone();
            return blocking(move || {
                repository.device_state_declaration(&principal, &binding, digest.as_deref(), &name)
            })
            .await
            .map_err(|_| auth_denied());
        }
        let (resource_id, _) = SqliteAuthorizationStore::state_namespace(
            &caller.principal_id,
            &caller.binding,
            resource_name,
        )
        .map_err(|_| auth_denied())?;
        blocking(move || repository.state_declaration(&resource_id))
            .await
            .map_err(unexpected)
    }

    async fn authoritative_entry(
        &self,
        declaration: &Declaration,
    ) -> Result<Option<PhysicalEntry>, ServerError> {
        let Some((put, revision, payload)) =
            self.authoritative_raw(&declaration.resource_id).await?
        else {
            return Ok(None);
        };
        if !put {
            return Ok(None);
        }
        let envelope: StoredEnvelope = serde_json::from_slice(&payload)
            .map_err(|_| representation_error("CorruptRepresentation", declaration, 0))?;
        validate_representation(
            declaration,
            envelope.representation_version,
            &envelope.value,
            true,
        )?;
        parse_timestamp(&envelope.created_at)?;
        parse_timestamp(&envelope.updated_at)?;
        Ok(Some(PhysicalEntry { revision, envelope }))
    }

    async fn authoritative_raw(
        &self,
        key: &str,
    ) -> Result<Option<(bool, u64, Bytes)>, ServerError> {
        let stream = self.jetstream.get_stream(STREAM).await.map_err(kv_error)?;
        let subject = format!("{SUBJECT_PREFIX}{key}");
        match stream.get_last_raw_message_by_subject(&subject).await {
            Ok(message) => {
                let put = match message
                    .headers
                    .get("KV-Operation")
                    .map(|value| value.as_str())
                {
                    Some("DEL" | "PURGE") => false,
                    Some("PUT") | None => true,
                    Some(_) => return Err(unexpected("stored State operation is invalid")),
                };
                Ok(Some((put, message.sequence, message.payload)))
            }
            Err(error) if error.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
            Err(error) => Err(kv_error(error)),
        }
    }
}

fn validate_representation(
    declaration: &Declaration,
    version: u32,
    value: &WireBytes,
    stored: bool,
) -> Result<(), ServerError> {
    if version == 0 {
        return Err(if stored {
            representation_error("CorruptRepresentation", declaration, version)
        } else {
            validation(
                "/representationVersion",
                "representationVersion must be positive",
            )
        });
    }
    let schema = if version == declaration.representation_version {
        &declaration.schema
    } else if let Some(schema) = declaration.accepted_versions.get(&version) {
        schema
    } else {
        return Err(representation_error(
            "UnsupportedRepresentation",
            declaration,
            version,
        ));
    };
    let value: Value = serde_json::from_slice(&value.0).map_err(|_| {
        if stored {
            representation_error("CorruptRepresentation", declaration, version)
        } else {
            validation("/value", "value must be UTF-8 JSON")
        }
    })?;
    let validator = jsonschema::validator_for(schema).map_err(unexpected)?;
    if validator.is_valid(&value) {
        Ok(())
    } else if stored {
        Err(representation_error(
            "CorruptRepresentation",
            declaration,
            version,
        ))
    } else {
        Err(validation(
            "/value",
            "value fails the declared State schema",
        ))
    }
}

fn encode(envelope: &StoredEnvelope) -> Result<Bytes, ServerError> {
    trellis_protocol::canonicalize_json(&serde_json::to_value(envelope)?)
        .map(Bytes::from)
        .map_err(unexpected)
}

fn public_entry(revision: u64, envelope: &StoredEnvelope) -> Value {
    json!({
        "value": envelope.value,
        "representationVersion": envelope.representation_version,
        "revision": revision.to_string(),
        "createdAt": envelope.created_at,
        "updatedAt": envelope.updated_at,
    })
}

fn project_entry(declaration: &Declaration, entry: &PhysicalEntry) -> Result<Value, ServerError> {
    validate_representation(
        declaration,
        entry.envelope.representation_version,
        &entry.envelope.value,
        true,
    )?;
    Ok(public_entry(entry.revision, &entry.envelope))
}

fn conflict(
    current: Option<&PhysicalEntry>,
    declaration: &Declaration,
) -> Result<ServerError, ServerError> {
    let current = current
        .map(|entry| project_entry(declaration, entry))
        .transpose()?;
    Ok(ServerError::DeclaredRpc(DeclaredRpcError::new(
        "trellis.state@v1::Conflict",
        "State revision conflict",
        current.map(|value| ("current", value)),
    )))
}

fn representation_error(error: &str, declaration: &Declaration, version: u32) -> ServerError {
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        format!("trellis.state@v1::{error}"),
        "stored State representation is unavailable",
        [
            ("resourceName", json!(declaration.resource_name)),
            ("representationVersion", json!(version)),
        ],
    ))
}

fn parse_revision(value: &str) -> Result<u64, ServerError> {
    if value.is_empty()
        || value == "0"
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(validation(
            "/revision",
            "revision must be a canonical positive integer",
        ));
    }
    value
        .parse()
        .map_err(|_| validation("/revision", "revision exceeds the supported range"))
}

fn timestamp(value: OffsetDateTime) -> Result<String, ServerError> {
    value
        .replace_nanosecond(value.nanosecond() / 1_000_000 * 1_000_000)
        .map_err(unexpected)?
        .format(&Rfc3339)
        .map_err(unexpected)
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, ServerError> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).map_err(unexpected)?;
    if parsed.format(&Rfc3339).map_err(unexpected)? != value {
        return Err(unexpected("stored State timestamp is not canonical"));
    }
    Ok(parsed)
}

fn validation(path: &str, message: &str) -> ServerError {
    ServerError::Validation {
        issues: Box::new(vec![ValidationIssue {
            path: path.to_owned(),
            message: message.to_owned(),
        }]),
    }
}

fn auth_denied() -> ServerError {
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        "AuthError",
        "request is not granted by the active authority",
        [("reason", json!("not_authorized"))],
    ))
}

fn unexpected(error: impl std::fmt::Display) -> ServerError {
    tracing::error!(error = %error, "State runtime failure");
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        "UnexpectedError",
        "State is temporarily unavailable",
        std::iter::empty::<(&str, Value)>(),
    ))
}

fn kv_error(error: impl std::fmt::Display) -> ServerError {
    unexpected(error)
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};

    use super::*;

    struct Server(Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn revision_tokens_are_canonical_positive_u64_values() {
        assert_eq!(parse_revision("1").expect("valid revision"), 1);
        assert_eq!(
            parse_revision(&u64::MAX.to_string()).expect("valid revision"),
            u64::MAX
        );
        for invalid in ["", "0", "01", "-1", "18446744073709551616"] {
            assert!(parse_revision(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn stored_envelope_keeps_bytes_and_positive_u32_version() {
        let envelope = StoredEnvelope {
            value: WireBytes(br#"{"enabled":true}"#.to_vec()),
            representation_version: u32::MAX,
            created_at: "2026-09-10T12:00:00Z".to_owned(),
            updated_at: "2026-09-10T12:00:00Z".to_owned(),
        };
        let decoded: StoredEnvelope =
            serde_json::from_slice(&encode(&envelope).expect("encode")).expect("decode");
        assert_eq!(decoded.value.0, envelope.value.0);
        assert_eq!(decoded.representation_version, u32::MAX);
    }

    #[test]
    fn state_timestamps_use_generated_canonical_spelling() {
        let value = OffsetDateTime::parse("2026-09-10T12:00:00.120456Z", &Rfc3339).unwrap();
        let rendered = timestamp(value).unwrap();
        assert_eq!(rendered, "2026-09-10T12:00:00.12Z");
        assert_eq!(
            parse_timestamp(&rendered).unwrap(),
            value.replace_nanosecond(120_000_000).unwrap()
        );
        assert!(parse_timestamp("2026-09-10T12:00:00.120Z").is_err());
    }

    #[tokio::test]
    async fn nats_component_covers_state_write_modes_conflicts_tombstones_and_read() {
        // Held until the broker's listener is up; see ReservedPort.
        let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
        let port = reservation.port();
        let directory = tempfile::tempdir().unwrap();
        let cache = std::env::var_os("TRELLIS_CACHE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| directory.path().join("cache"));
        let binary = trellis_local_nats::NatsServerBinary::resolve(
            &trellis_local_nats::NatsBinarySource::DownloadPinned,
            Some(&cache),
        )
        .unwrap();
        let _server = Server(
            Command::new(binary)
                .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js"])
                .arg("-sd")
                .arg(directory.path().join("data"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let url = format!("nats://127.0.0.1:{port}");
        let mut client = None;
        for _ in 0..100 {
            if let Ok(connected) = async_nats::connect(&url).await {
                client = Some(connected);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let store = jetstream::new(client.expect("connect to local NATS"))
            .create_key_value(kv::Config {
                bucket: "state_test".to_owned(),
                history: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        let value = |text: &str| {
            encode(&StoredEnvelope {
                value: WireBytes(text.as_bytes().to_vec()),
                representation_version: 1,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .unwrap()
        };

        let created = store.create("resource", value("create")).await.unwrap();
        assert!(store.create("resource", value("conflict")).await.is_err());
        assert_eq!(
            store.entry("resource").await.unwrap().unwrap().revision,
            created
        );
        assert_eq!(
            store.entry("resource").await.unwrap().unwrap().revision,
            created,
            "reading must not write"
        );
        let set = store.put("resource", value("set")).await.unwrap();
        assert!(store
            .update("resource", value("stale"), created)
            .await
            .is_err());
        let replaced = store
            .update("resource", value("replace"), set)
            .await
            .unwrap();
        store
            .delete_expect_revision("resource", Some(replaced))
            .await
            .unwrap();
        let tombstone = store.entry("resource").await.unwrap().unwrap();
        assert_eq!(tombstone.operation, kv::Operation::Delete);
        assert!(store.get("resource").await.unwrap().is_none());
        let (first, second) = tokio::join!(
            store.update("resource", value("first"), tombstone.revision),
            store.update("resource", value("second"), tombstone.revision),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert!(store.entry("resource").await.unwrap().unwrap().revision > tombstone.revision);
        store.delete("resource").await.unwrap();
        assert!(store.get("resource").await.unwrap().is_none());
        store.delete("missing").await.unwrap();
    }
}
