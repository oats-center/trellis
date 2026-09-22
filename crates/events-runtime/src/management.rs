use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trellis_rs::service::{DeclaredRpcError, RequestContext, Router, ServerError};
use trellis_runtime_apis::apis::trellis_events_v1::rpc;

use crate::dead_letters::{
    DeadLetterJournal, DeadLetterTransition, DeliveryReport, DeliveryReportError, DeliveryReporter,
    JournalError,
};
use crate::router::map_query_error;
use crate::storage::{EventsStore, EventsStoreError};
use crate::wire::{generated_input, generated_output};
use crate::EventsQuery;

/// Exact resource action selected by an Events management method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementAction {
    /// Read Consumer lifecycle data.
    Read,
    /// Replay or dismiss a dead letter.
    Control,
    /// Report Consumer delivery.
    Consume,
}

/// Runtime callback implementing Resource authority or exact API-call-plus-Admin authority.
pub type ManagementAuthorizer = Arc<
    dyn Fn(
            RequestContext,
            String,
            ManagementAction,
            &'static str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ServerError>> + Send>>
        + Send
        + Sync,
>;

/// Replace Consumer query routes with payload-bound resource authorization.
pub fn secure_consumer_routes(
    router: &mut Router,
    query: EventsQuery,
    authorize: ManagementAuthorizer,
) {
    router.register_rpc::<rpc::ConsumersQuery, _, _>({
        let query = query.clone();
        let authorize = Arc::clone(&authorize);
        move |context, input| {
            let query = query.clone();
            let authorize = Arc::clone(&authorize);
            async move {
                let input = generated_input(input, &["limit"])?;
                let resource_id = input
                    .get("resourceId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                authorize(
                    context,
                    resource_id,
                    ManagementAction::Read,
                    "Consumers.Query",
                )
                .await?;
                generated_output(
                    query
                        .query_consumers(&input)
                        .await
                        .map_err(map_query_error)?,
                    &[
                        "ackPending",
                        "ackWaitMs",
                        "limit",
                        "maxDeliver",
                        "pending",
                        "redelivered",
                        "waitingPulls",
                    ],
                )
            }
        }
    });
    router.rpc_handler_authorizes::<rpc::ConsumersQuery>();

    router.register_rpc::<rpc::ConsumersInspect, _, _>({
        let query = query.clone();
        move |context, input| {
            let query = query.clone();
            let authorize = Arc::clone(&authorize);
            async move {
                let input = generated_input(input, &[])?;
                let resource_id = required_string(&input, "resourceId")?.to_owned();
                authorize(
                    context,
                    resource_id,
                    ManagementAction::Read,
                    "Consumers.Inspect",
                )
                .await?;
                generated_output(
                    query
                        .inspect_consumer(&input)
                        .await
                        .map_err(map_query_error)?,
                    &[
                        "ackPending",
                        "ackWaitMs",
                        "maxDeliver",
                        "pending",
                        "redelivered",
                        "waitingPulls",
                    ],
                )
            }
        }
    });
    router.rpc_handler_authorizes::<rpc::ConsumersInspect>();
}

/// Final generated Events management RPC backend.
#[derive(Clone)]
pub struct EventsManagement {
    store: EventsStore,
    journal: DeadLetterJournal,
    delivery: DeliveryReporter,
    authorize: ManagementAuthorizer,
}

impl EventsManagement {
    /// Construct the management backend from runtime-owned dependencies.
    pub fn new(
        store: EventsStore,
        journal: DeadLetterJournal,
        delivery: DeliveryReporter,
        authorize: ManagementAuthorizer,
    ) -> Self {
        Self {
            store,
            journal,
            delivery,
            authorize,
        }
    }

    /// Register management handlers against final generated descriptors.
    pub fn register(self, router: &mut Router) {
        router.register_rpc::<rpc::ConsumersReportDelivery, _, _>({
            let backend = self.clone();
            move |context, input| {
                let backend = backend.clone();
                async move {
                    let value = generated_input(
                        input,
                        &["deliveryCount", "replayGeneration", "sourceSequence"],
                    )?;
                    let report = serde_json::from_value::<DeliveryReport>(value)
                        .map_err(|error| validation("request", error.to_string()))?;
                    (backend.authorize)(
                        context,
                        report.resource_id.clone(),
                        ManagementAction::Consume,
                        "Consumers.ReportDelivery",
                    )
                    .await?;
                    let resource_id = report.resource_id.clone();
                    let result = backend
                        .delivery
                        .report(&resource_id, report)
                        .await
                        .map_err(delivery_error)?;
                    generated_output(
                        json!({
                            "deadLetter": backend.summary(&result.transition).await?,
                            "stale": result.stale,
                        }),
                        &SUMMARY_INTEGERS,
                    )
                }
            }
        });
        router.rpc_handler_authorizes::<rpc::ConsumersReportDelivery>();

        router.register_rpc::<rpc::DeadLettersQuery, _, _>({
            let backend = self.clone();
            move |context, input| {
                let backend = backend.clone();
                async move {
                    let input = generated_input(input, &["limit"])?;
                    let resource_id = input
                        .get("resourceId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    (backend.authorize)(
                        context,
                        resource_id.clone(),
                        ManagementAction::Read,
                        "DeadLetters.Query",
                    )
                    .await?;
                    let states = input
                        .get("state")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let page = input.get("page").unwrap_or(&Value::Null);
                    let output = backend
                        .store
                        .query_dead_letters(
                            (!resource_id.is_empty()).then_some(resource_id.as_str()),
                            &states,
                            page.get("cursor").and_then(Value::as_str),
                            page.get("limit").and_then(Value::as_u64).unwrap_or(100),
                        )
                        .map_err(store_error)?;
                    generated_output(output, &SUMMARY_INTEGERS)
                }
            }
        });
        router.rpc_handler_authorizes::<rpc::DeadLettersQuery>();

        router.register_rpc::<rpc::DeadLettersInspect, _, _>({
            let backend = self.clone();
            move |context, input| {
                let backend = backend.clone();
                async move {
                    let input = generated_input(input, &[])?;
                    let resource_id = required_string(&input, "resourceId")?;
                    (backend.authorize)(
                        context,
                        resource_id.to_owned(),
                        ManagementAction::Read,
                        "DeadLetters.Inspect",
                    )
                    .await?;
                    let id = required_string(&input, "deadLetterId")?;
                    let mut detail = backend
                        .store
                        .inspect_dead_letter(id)
                        .map_err(store_error)?
                        .filter(|detail| detail["deadLetter"]["resourceId"] == resource_id)
                        .ok_or_else(|| not_found(id))?;
                    if let Some(payload) = detail["originalPayload"].as_array() {
                        let bytes = payload
                            .iter()
                            .filter_map(Value::as_u64)
                            .map(|byte| byte as u8)
                            .collect::<Vec<_>>();
                        detail["originalPayload"] = Value::String(STANDARD.encode(bytes));
                    }
                    generated_output(
                        json!({ "deadLetter": detail }),
                        &["originalSequence", "revision", "transitionReferences"],
                    )
                }
            }
        });
        router.rpc_handler_authorizes::<rpc::DeadLettersInspect>();

        self.register_transition::<rpc::DeadLettersReplay>(router, true);
        self.register_transition::<rpc::DeadLettersDismiss>(router, false);
    }

    fn register_transition<D>(&self, router: &mut Router, replay: bool)
    where
        D: trellis_rs::generated::RpcDescriptor + 'static,
        D::Input: serde::Serialize + Send,
        D::Output: serde::de::DeserializeOwned,
    {
        let backend = self.clone();
        router.register_rpc::<D, _, _>(move |context, input| {
            let backend = backend.clone();
            async move {
                let input = generated_input(input, &["expectedRevision"])?;
                let resource_id = required_string(&input, "resourceId")?.to_owned();
                (backend.authorize)(
                    context,
                    resource_id.clone(),
                    ManagementAction::Control,
                    if replay {
                        "DeadLetters.Replay"
                    } else {
                        "DeadLetters.Dismiss"
                    },
                )
                .await?;
                let id = required_string(&input, "deadLetterId")?;
                backend.ensure_resource(id, &resource_id).await?;
                let request_id = required_string(&input, "requestId")?.to_owned();
                let revision = input
                    .get("expectedRevision")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| validation("expectedRevision", "must be uint64"))?;
                let digest = command_digest(
                    if replay { "replay" } else { "dismiss" },
                    id,
                    &resource_id,
                    revision,
                );
                let transition = if replay {
                    backend
                        .journal
                        .request_replay(id, revision, request_id, digest)
                        .await
                } else {
                    backend
                        .journal
                        .dismiss(id, revision, request_id, digest)
                        .await
                }
                .map_err(journal_error)?
                .0;
                generated_output(
                    json!({ "deadLetter": backend.summary(&transition).await? }),
                    &SUMMARY_INTEGERS,
                )
            }
        });
        router.rpc_handler_authorizes::<D>();
    }

    async fn ensure_resource(&self, id: &str, resource_id: &str) -> Result<(), ServerError> {
        let transition = self
            .journal
            .latest(id)
            .await
            .map_err(journal_error)?
            .ok_or_else(|| not_found(id))?
            .0;
        (transition.resource_id == resource_id)
            .then_some(())
            .ok_or_else(|| not_found(id))
    }

    async fn summary(&self, transition: &DeadLetterTransition) -> Result<Value, ServerError> {
        let original = self
            .journal
            .original(transition)
            .await
            .map_err(journal_error)?
            .ok_or_else(|| {
                ServerError::Nats("dead-letter original evidence is missing".to_owned())
            })?;
        Ok(json!({
            "deadLetterId": transition.id,
            "deliveries": transition.deliveries,
            "generation": transition.generation,
            "lastError": transition.last_error,
            "originalSequence": original.sequence,
            "originalStream": original.stream,
            "resourceId": transition.resource_id,
            "revision": transition.revision,
            "state": transition.state,
            "updatedAt": transition.occurred_at,
        }))
    }
}

const SUMMARY_INTEGERS: [&str; 2] = ["originalSequence", "revision"];

fn command_digest(action: &str, id: &str, resource_id: &str, revision: u64) -> String {
    let mut digest = Sha256::new();
    for part in [action.as_bytes(), id.as_bytes(), resource_id.as_bytes()] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    digest.update(revision.to_be_bytes());
    STANDARD.encode(digest.finalize())
}

fn required_string<'a>(input: &'a Value, field: &'static str) -> Result<&'a str, ServerError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation(field, "is required"))
}

fn validation(field: &'static str, details: impl Into<String>) -> ServerError {
    let details = details.into();
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        "ValidationError",
        format!("Invalid {field}: {details}"),
        [("field", json!(field)), ("details", json!(details))],
    ))
}

fn not_found(id: &str) -> ServerError {
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        "NotFoundError",
        "Dead letter not found",
        [
            ("type", json!("NotFoundError")),
            ("id", json!(id)),
            ("message", json!("Dead letter not found")),
        ],
    ))
}

fn journal_error(error: JournalError) -> ServerError {
    match error {
        JournalError::NotFound(id) => not_found(&id),
        JournalError::Conflict(details) => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "Conflict",
            details,
            [("code", json!("dead_letter_conflict"))],
        )),
        JournalError::UnreplayableOriginal => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "UnreplayableOriginal",
            "Original event evidence is no longer retained",
            [("code", json!("unreplayable_original"))],
        )),
        other => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "Unavailable",
            other.to_string(),
            [("code", json!("dead_letter_journal_unavailable"))],
        )),
    }
}

fn store_error(error: EventsStoreError) -> ServerError {
    match error {
        EventsStoreError::InvalidCursor => validation("page.cursor", "invalid cursor"),
        other => ServerError::Nats(other.to_string()),
    }
}

fn delivery_error(error: DeliveryReportError) -> ServerError {
    match error {
        DeliveryReportError::NotFound(id) => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "NotFoundError",
            "Delivery evidence not found",
            [
                ("type", json!("NotFoundError")),
                ("id", json!(id)),
                ("message", json!("Delivery evidence not found")),
            ],
        )),
        DeliveryReportError::Validation(details) => validation("request", details),
        DeliveryReportError::Conflict(details) => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "Conflict",
            details,
            [("code", json!("delivery_report_conflict"))],
        )),
        DeliveryReportError::Unavailable(details) => {
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "Unavailable",
                details,
                [("code", json!("delivery_reporting_unavailable"))],
            ))
        }
    }
}
