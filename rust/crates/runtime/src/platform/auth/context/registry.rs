use std::time::Duration;

use async_nats::jetstream::{self, kv};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use trellis_protocol::{canonicalize_json, parse_authorization_context};

use super::AuthorizationContextRecord;
use crate::{config::AuthorizationConfig, platform::auth::AuthorizationStateError};

const REVOCATION_VALUE_BYTES: usize = 4_096;
pub(super) const REVOCATION_PREFIX: &str = "revocation.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthorizationContextRevocation {
    pub(crate) revoked_at: i64,
}

impl AuthorizationContextRevocation {
    fn validate(&self) -> Result<(), AuthorizationStateError> {
        if self.revoked_at <= 0 {
            return Err(storage("authorization context revocation is invalid"));
        }
        Ok(())
    }
}

/// Server-owned NATS context and revocation registry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuthorizationRegistryBinding {
    /// KV bucket holding canonical signed contexts and revocations.
    pub(crate) context_bucket: String,
}

impl AuthorizationRegistryBinding {
    pub(crate) fn from_config(config: &AuthorizationConfig) -> Self {
        Self {
            context_bucket: config.context_bucket.clone(),
        }
    }
}

pub(crate) use trellis_rs::client::AuthorizationContextBundle;

#[derive(Clone)]
pub(crate) struct AuthorizationContextRegistry {
    contexts: kv::Store,
}

impl AuthorizationContextRegistry {
    pub(crate) async fn check(
        client: async_nats::Client,
        config: &AuthorizationConfig,
    ) -> Result<(), AuthorizationStateError> {
        let jetstream = jetstream::new(client);
        let context_value_bytes = context_registry_value_bytes(config.maximum_context_bytes)?;
        let contexts = open_existing(&jetstream, &config.context_bucket)
            .await?
            .ok_or_else(|| storage("authorization context registry is missing"))?;
        check_policy(
            &contexts,
            &config.context_bucket,
            Duration::ZERO,
            context_value_bytes,
            config.registry_replicas,
        )
        .await
    }

    pub(crate) async fn ensure(
        client: async_nats::Client,
        config: &AuthorizationConfig,
    ) -> Result<Self, AuthorizationStateError> {
        let jetstream = jetstream::new(client);
        let context_value_bytes = context_registry_value_bytes(config.maximum_context_bytes)?;
        let contexts = open_or_create(
            &jetstream,
            &config.context_bucket,
            Duration::ZERO,
            context_value_bytes,
            config.registry_replicas,
        )
        .await?;
        Ok(Self { contexts })
    }

    pub(crate) async fn publish_context(
        &self,
        context: &AuthorizationContextRecord,
    ) -> Result<(), AuthorizationStateError> {
        let value: serde_json::Value = serde_json::from_str(&context.signed_context_json)
            .map_err(|error| storage(format!("cannot parse authorization context: {error}")))?;
        let signed = parse_authorization_context(&value)
            .map_err(|error| storage(format!("cannot parse authorization context: {error}")))?;
        let canonical = canonicalize_json(&value).map_err(|error| {
            storage(format!(
                "cannot canonicalize authorization context: {error}"
            ))
        })?;
        let digest = signed
            .digest()
            .map_err(|error| storage(format!("cannot digest authorization context: {error}")))?;
        if canonical != context.signed_context_json || digest != context.context_digest {
            return Err(storage(
                "authorization context key or canonical signed JSON does not match",
            ));
        }
        publish_immutable(&self.contexts, &digest, canonical.as_bytes()).await
    }

    pub(crate) async fn publish_revocation(
        &self,
        context: &AuthorizationContextRecord,
    ) -> Result<(), AuthorizationStateError> {
        let record = AuthorizationContextRevocation {
            revoked_at: context
                .revoked_at
                .ok_or_else(|| storage("revoked context has no revocation time"))?,
        };
        record.validate()?;
        let payload = canonicalize_json(
            &serde_json::to_value(record)
                .map_err(|error| storage(format!("cannot encode context revocation: {error}")))?,
        )
        .map_err(|error| storage(format!("cannot encode context revocation: {error}")))?;
        publish_immutable(
            &self.contexts,
            &format!("{REVOCATION_PREFIX}{}", context.context_digest),
            payload.as_bytes(),
        )
        .await
    }
}

fn context_registry_value_bytes(
    maximum_context_bytes: usize,
) -> Result<i32, AuthorizationStateError> {
    i32::try_from(maximum_context_bytes.max(REVOCATION_VALUE_BYTES))
        .map_err(|_| storage("maximum context bytes exceed NATS KV bounds"))
}

async fn open_existing(
    jetstream: &jetstream::Context,
    bucket: &str,
) -> Result<Option<jetstream::kv::Store>, AuthorizationStateError> {
    use async_nats::jetstream::{context::GetStreamErrorKind, ErrorCode};

    match jetstream.get_stream(format!("KV_{bucket}")).await {
        Ok(_) => jetstream
            .get_key_value(bucket)
            .await
            .map(Some)
            .map_err(|error| storage(format!("cannot open {bucket}: {error}"))),
        Err(error)
            if matches!(
                error.kind(),
                GetStreamErrorKind::JetStream(error)
                    if error.error_code() == ErrorCode::STREAM_NOT_FOUND
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(storage(format!("cannot inspect {bucket}: {error}"))),
    }
}

async fn open_or_create(
    jetstream: &jetstream::Context,
    bucket: &str,
    max_age: Duration,
    max_value_size: i32,
    replicas: usize,
) -> Result<kv::Store, AuthorizationStateError> {
    let config = kv::Config {
        bucket: bucket.to_owned(),
        history: 1,
        max_age,
        max_value_size,
        num_replicas: replicas,
        ..Default::default()
    };
    let store = match jetstream.get_key_value(bucket).await {
        Ok(store) => store,
        Err(open_error) => match jetstream.create_key_value(config).await {
            Ok(store) => store,
            Err(create_error) => jetstream.get_key_value(bucket).await.map_err(|error| {
                storage(format!(
                    "cannot open {bucket} ({open_error}), create it ({create_error}), or reopen it ({error})"
                ))
            })?,
        },
    };
    check_policy(&store, bucket, max_age, max_value_size, replicas).await?;
    Ok(store)
}

async fn check_policy(
    store: &kv::Store,
    bucket: &str,
    max_age: Duration,
    max_value_size: i32,
    replicas: usize,
) -> Result<(), AuthorizationStateError> {
    let status = store
        .status()
        .await
        .map_err(|error| storage(format!("cannot inspect {bucket}: {error}")))?;
    if status.max_age() != max_age
        || status.info.config.max_message_size != max_value_size
        || status.info.config.max_bytes != -1
        || status.history() != 1
        || status.info.config.num_replicas != replicas
    {
        return Err(storage(format!(
            "{bucket} policy does not match configuration"
        )));
    }
    Ok(())
}

async fn publish_immutable(
    store: &kv::Store,
    key: &str,
    bytes: &[u8],
) -> Result<(), AuthorizationStateError> {
    match store.create(key, Bytes::copy_from_slice(bytes)).await {
        Ok(_) => confirm_exact(store, key, bytes).await,
        Err(create_error) => {
            match store.get(key).await.map_err(|error| {
                storage(format!("cannot read {key} after create failure: {error}"))
            })? {
                Some(existing) if existing.as_ref() == bytes => {
                    confirm_exact(store, key, bytes).await
                }
                Some(_) => Err(storage(format!("immutable registry key {key} changed"))),
                None => Err(storage(format!(
                    "cannot create registry key {key}: {create_error}"
                ))),
            }
        }
    }
}

async fn confirm_exact(
    store: &kv::Store,
    key: &str,
    expected: &[u8],
) -> Result<(), AuthorizationStateError> {
    for attempt in 0..3 {
        match store
            .get(key)
            .await
            .map_err(|error| storage(format!("cannot confirm registry key {key}: {error}")))?
        {
            Some(actual) if actual.as_ref() == expected => return Ok(()),
            Some(_) => return Err(storage(format!("registry key {key} readback changed"))),
            None if attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
            }
            None => break,
        }
    }
    Err(storage(format!(
        "registry key {key} was not visible after publication"
    )))
}

fn storage(message: impl Into<String>) -> AuthorizationStateError {
    AuthorizationStateError::Storage(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_registry_limit_uses_canonical_json_bytes() {
        assert_eq!(context_registry_value_bytes(16_384).unwrap(), 16_384);
    }

    #[test]
    fn registry_binding_matches_published_key_layout() {
        let binding = AuthorizationRegistryBinding {
            context_bucket: "trellis_authorization_contexts".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(binding).unwrap(),
            json!({
                "contextBucket": "trellis_authorization_contexts"
            })
        );
    }

    #[test]
    fn revocations_are_additively_tolerant() {
        assert_eq!(
            canonicalize_json(
                &serde_json::to_value(AuthorizationContextRevocation { revoked_at: 42 }).unwrap()
            )
            .unwrap(),
            r#"{"revokedAt":42}"#
        );
        assert!(
            serde_json::from_value::<AuthorizationContextRevocation>(json!({
                "revokedAt": 42,
                "extra": true
            }))
            .is_ok()
        );
    }
}
