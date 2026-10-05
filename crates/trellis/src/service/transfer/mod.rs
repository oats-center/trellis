mod download;
mod ingress;
mod protocol;
pub(crate) mod telemetry;
mod upload;
pub(crate) mod wire;

use super::{
    OperationTransferProgress, ServerError, ServiceResourceBindings, StoreResourceBinding,
    StoreResourceClient,
};
use crate::{
    client::{TransportLease, TrellisClient},
    data_plane::credit::{CreditPolicy, CreditScheduler},
};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use bytes::Bytes;
pub use download::run_download_transfer_endpoint;
use futures_util::StreamExt;
pub use protocol::*;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::oneshot;
use trellis_protocol::transfer::*;
pub use upload::UploadTransferSession;
use wire::{error, publish_signal, TransferPeer};

pub(crate) fn credit_scheduler() -> CreditScheduler {
    CreditScheduler::new(CreditPolicy {
        frame_step: TRANSFER_CREDIT_FRAME_STEP,
        byte_step: Some(TRANSFER_CREDIT_BYTE_STEP),
        max_delay: Duration::from_millis(TRANSFER_CREDIT_MAX_DELAY_MS),
    })
}

pub(crate) async fn credit_due(scheduler: &CreditScheduler) {
    match scheduler.next_due() {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

pub(crate) fn transfer_expiry_delay(expires_at: &str) -> Result<Duration, ServerError> {
    let expiry = OffsetDateTime::parse(expires_at, &Rfc3339).map_err(error)?;
    let remaining = (expiry - OffsetDateTime::now_utc()).whole_milliseconds();
    if remaining <= 0 {
        return Err(error("transfer grant expired"));
    }
    Ok(Duration::from_millis(remaining.min(u64::MAX as i128) as u64))
}

/// Subscribe exact upload subjects on the admitting generation before exposing its grant.
pub async fn spawn_upload_transfer_endpoint_with_progress_and_completion<C, F>(
    client: Arc<TrellisClient>,
    session: UploadTransferSession,
    store: C,
    lease: TransportLease,
    on_progress: F,
) -> Result<UploadTransferCompletion, ServerError>
where
    C: StoreResourceClient,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    let data = lease
        .nats()
        .subscribe(session.plan.grant.data_subject.clone())
        .await
        .map_err(error)?;
    let control = lease
        .nats()
        .subscribe(session.plan.grant.control_subject.clone())
        .await
        .map_err(error)?;
    lease.nats().flush().await.map_err(error)?;
    let (sender, receiver) = oneshot::channel();
    let task = tokio::spawn(run_upload(
        client,
        lease,
        data,
        control,
        session,
        store,
        on_progress,
        sender,
    ));
    Ok(UploadTransferCompletion {
        receiver,
        task: Some(task),
    })
}

/// Prepare an owned upload endpoint without transient progress callbacks.
pub async fn spawn_upload_transfer_endpoint_with_completion<C>(
    client: Arc<TrellisClient>,
    session: UploadTransferSession,
    store: C,
    lease: TransportLease,
) -> Result<UploadTransferCompletion, ServerError>
where
    C: StoreResourceClient,
{
    spawn_upload_transfer_endpoint_with_progress_and_completion(
        client,
        session,
        store,
        lease,
        |_| {},
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "the endpoint consumes its exact subscriptions, pinned lease, backend and completion sender"
)]
async fn run_upload<C, F>(
    client: Arc<TrellisClient>,
    lease: TransportLease,
    mut data: async_nats::Subscriber,
    mut control: async_nats::Subscriber,
    mut session: UploadTransferSession,
    store: C,
    on_progress: F,
    sender: oneshot::Sender<protocol::UploadTransferCompletionResult>,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    let grant = session.plan.grant.clone();
    let observation = telemetry::observation(TransferDirection::Send, "provider");
    let admission = &session.plan.admission;
    let digest = admission
        .authorization_context
        .as_deref()
        .ok_or_else(|| error("upload admission context missing"))?;
    let permission = admission
        .required_permission
        .as_ref()
        .ok_or_else(|| error("upload admission permission missing"))?
        .permission_atom()
        .map_err(error)?;
    let peer = TransferPeer::retain(
        client.authorization_provider(),
        digest,
        &grant.consumer,
        Some(permission),
    )
    .await?;
    let local = TransferPeer::retain(
        client.authorization_provider(),
        &client.authorization_context_digest().map_err(error)?,
        &grant.provider,
        None,
    )
    .await?;
    let mut changes = peer.guard.subscribe_changes();
    let mut local_changes = local.guard.subscribe_changes();
    let expiry = tokio::time::sleep(transfer_expiry_delay(&grant.expires_at)?);
    tokio::pin!(expiry);
    let mut store = Some(store);
    let mut activated = false;
    let mut activate_body: Option<Bytes> = None;
    let mut sender = Some(sender);
    let mut scheduler = credit_scheduler();
    let mut reported_seq = 0;
    let mut reported_bytes = 0;
    let mut progress_seq = 0;
    let mut control_seq = 0;
    let mut control_body: Option<Bytes> = None;
    let outcome: Result<(), ServerError> = async {
        loop {
            tokio::select! {
                _ = &mut expiry => return Err(error("upload grant expired")),
                _ = lease.wait_lost() => return Err(error("upload pinned transport lost")),
                failure = wire::authority_failure(&local, &peer) => return Err(failure),
                _ = changes.recv() => { peer.guard.check_now().map_err(|e| error(format!("upload authority lost: {e:?}")))?; },
                _ = local_changes.recv() => { local.guard.reconcile().await.map_err(|e| error(format!("upload local authority lost: {e:?}")))?; },
                backend = async { session.upload_future.as_mut().ok_or_else(|| error("upload backend missing"))?.await }, if activated => {
                    session.upload_future.take();
                    let failure = match backend { Err(failure) => failure, Ok(_) => error("upload backend returned before validated completion") };
                    if let Some(sender) = sender.take() { let _ = sender.send((Err(failure), None)); }
                    return Err(error("upload backend ended before completion"));
                },
                changed = session.consumed.changed(), if activated => {
                    if changed.is_err() {
                        let failure = session.early_backend_failure().await;
                        if let Some(sender) = sender.take() { let _ = sender.send((Err(failure), None)); }
                        return Err(error("upload backend ended before completion"));
                    }
                    let consumed = session.consumption();
                    if consumed.seq > progress_seq { on_progress(session.progress()); progress_seq = consumed.seq; }
                    scheduler.note_pending(tokio::time::Instant::now(), consumed.seq - reported_seq, consumed.bytes - reported_bytes);
                },
                _ = credit_due(&scheduler), if activated => {
                    let consumed = session.consumption();
                    publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Send, TransferSignal::Credit { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone(), received_seq: U64s::new(session.received.highest_sent.load(Ordering::Acquire)), consumed_seq: U64s::new(consumed.seq), consumed_bytes: U64s::new(consumed.bytes) }).await?;
                    reported_seq = consumed.seq; reported_bytes = consumed.bytes; scheduler.clear();
                },
                message = control.next() => {
                    let message = message.ok_or_else(|| error("upload control subscription closed"))?;
                    let descriptor = match wire::descriptor(&message, &grant.transfer_id, TransferDirection::Send) { Ok(value) => value, Err(_) => continue };
                    if descriptor.kind != TransferFrameKind::Control || peer.verify_caller(&message, &descriptor, &grant.provider.connection_id, &grant.signal_subject).await.is_err() { continue; }
                    let body = parse_transfer_control(&message.payload).map_err(error)?;
                    let seq = body.control_seq().get();
                    if seq == control_seq {
                        if control_body.as_ref() != Some(&message.payload) { return Err(error("upload control conflict")); }
                    } else if seq != control_seq + 1 { return Err(error("upload control gap")); }
                    control_seq = seq; control_body = Some(message.payload.clone());
                    match body {
                        TransferControl::Activate { receive_max_frame_bytes, .. } => {
                            if activated && activate_body.as_ref() != Some(&message.payload) { return Err(error("upload activation conflict")); }
                            if !activated {
                                session.plan.grant.max_frame_bytes = session.plan.grant.max_frame_bytes.min(receive_max_frame_bytes);
                                session.start(store.take().ok_or_else(|| error("upload store already started"))?)?;
                                activated = true; activate_body = Some(message.payload.clone());
                            }
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Send, TransferSignal::Activated { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(1), request_id: wire::header(message.headers.as_ref().ok_or_else(|| error("headers missing"))?, "request-id")?, max_frame_bytes: session.plan.grant.max_frame_bytes, window_frames: grant.window_frames, window_bytes: grant.window_bytes }).await?;
                        },
                        TransferControl::Cancel { .. } => {
                            session.abort().await;
                            if let Some(sender) = sender.take() { let _ = sender.send((Err(ServerError::TransferCancelled { transfer_id: grant.transfer_id.clone() }), None)); }
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Send, TransferSignal::Cancelled { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone() }).await?;
                            return Ok(());
                        },
                        _ => return Err(error("unsupported upload control")),
                    }
                },
                message = data.next() => {
                    let message = message.ok_or_else(|| error("upload data subscription closed"))?;
                    let descriptor = match wire::descriptor(&message, &grant.transfer_id, TransferDirection::Send) { Ok(value) => value, Err(_) => continue };
                    if peer.verify_caller(&message, &descriptor, &grant.provider.connection_id, &grant.signal_subject).await.is_err() { continue; }
                    if !activated { return Err(error("upload DATA before activation")); }
                    match descriptor.kind {
                        TransferFrameKind::Data => session.admit(descriptor.sequence.get(), message.payload)?,
                        TransferFrameKind::Complete => {
                            let complete = parse_transfer_complete(&message.payload).map_err(error)?;
                            let commit_key = session.plan.key.clone();
                            let indeterminate = |message: &str| ServerError::StoreCommitIndeterminate { key: commit_key.clone(), message: message.into() };
                            let result = {
                                let finish = session.finish(complete.final_seq.get(), complete.size, &complete.digest);
                                tokio::pin!(finish);
                                loop {
                                    tokio::select! {
                                        biased;
                                        result = &mut finish => break result,
                                        _ = lease.wait_lost() => break Err(indeterminate("upload pinned transport lost during commit")),
                                        _ = &mut expiry => break Err(indeterminate("upload grant expired during commit")),
                                        _ = wire::authority_failure(&local, &peer) => break Err(indeterminate("upload authority expired during commit")),
                                        _ = changes.recv() => { if peer.guard.check_now().is_err() { break Err(indeterminate("upload authority lost during commit")); } },
                                        _ = local_changes.recv() => { if local.guard.reconcile().await.is_err() { break Err(indeterminate("upload local authority lost during commit")); } },
                                        message = control.next() => {
                                            let Some(message) = message else { break Err(indeterminate("upload control subscription closed during commit")); };
                                            let descriptor = match wire::descriptor(&message, &grant.transfer_id, TransferDirection::Send) { Ok(value) => value, Err(_) => continue };
                                            if descriptor.kind != TransferFrameKind::Control || peer.verify_caller(&message, &descriptor, &grant.provider.connection_id, &grant.signal_subject).await.is_err() { continue; }
                                            let body = parse_transfer_control(&message.payload).map_err(error)?;
                                            if body.control_seq().get() != control_seq + 1 { break Err(indeterminate("upload control sequence conflict during commit")); }
                                            if matches!(body, TransferControl::Cancel { .. }) { break Err(indeterminate("upload cancellation raced backend commit")); }
                                            break Err(indeterminate("unexpected upload control during commit"));
                                        },
                                    }
                                }
                            };
                             let info = match result { Ok(info) => info, Err(error) => { tracing::warn!(%error, "transfer upload backend commit failed"); if let Some(sender) = sender.take() { let _ = sender.send((Err(error), None)); } return Err(wire::error("upload backend commit failed")); } };
                            let consumed = session.consumption();
                            on_progress(session.progress());
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Send, TransferSignal::Credit { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone(), received_seq: complete.final_seq, consumed_seq: complete.final_seq, consumed_bytes: U64s::new(consumed.bytes) }).await?;
                            let (persisted, done) = oneshot::channel();
                            sender.take().ok_or_else(|| error("upload completion owner missing"))?.send((Ok(info.clone()), Some(persisted))).map_err(|_| error("upload completion owner closed"))?;
                            tokio::select! {
                                result = done => result.map_err(|_| error("Operation completion barrier closed"))??,
                                _ = lease.wait_lost() => return Err(error("upload pinned transport lost during Operation commit")),
                                _ = &mut expiry => return Err(error("upload expired during Operation commit")),
                                failure = wire::authority_failure(&local, &peer) => return Err(failure),
                            }
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Send, TransferSignal::Committed { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone(), final_seq: complete.final_seq, info }).await?;
                            return Ok(());
                        },
                        _ => return Err(error("unexpected upload frame")),
                    }
                },
            }
        }
    }.await;
    session.abort().await;
    if let Err(failure) = &outcome {
        tracing::warn!(error = %failure, "transfer upload endpoint failed");
        if let Some(sender) = sender {
            let _ = sender.send((Err(error(failure)), None));
        }
        let _ = publish_signal(
            lease.nats(),
            &client,
            &local.guard,
            &grant.signal_subject,
            TransferDirection::Send,
            TransferSignal::Error {
                format: TransferFormat::V2,
                transfer_id: grant.transfer_id.clone(),
                code: TransferErrorCode::StorageFailed,
            },
        )
        .await;
    }
    observation.finish(if outcome.is_ok() { "ok" } else { "error" });
    outcome
}

/// Start one owned push download on the accepting generation.
pub async fn spawn_download_transfer_endpoint<C>(
    client: Arc<TrellisClient>,
    plan: DownloadTransferGrantPlan,
    store: C,
    lease: TransportLease,
) -> Result<tokio::task::JoinHandle<Result<(), ServerError>>, ServerError>
where
    C: StoreResourceClient,
{
    let control = lease
        .nats()
        .subscribe(plan.grant.control_subject.clone())
        .await
        .map_err(error)?;
    lease.nats().flush().await.map_err(error)?;
    Ok(tokio::spawn(run_download_transfer_endpoint(
        client, lease, control, plan, store,
    )))
}

/// Plan upload transport authority without exposing its physical storage mapping.
pub fn plan_upload_transfer_grant(
    args: TransferUploadGrantArgs<'_>,
) -> Result<UploadTransferGrantPlan, ServerError> {
    let store = store_binding(args.service_name, args.resources, args.store)?;
    let max_frame_bytes =
        negotiate_transfer_max_frame_bytes(args.max_payload, args.max_payload).map_err(error)?;
    let store_max = store
        .max_object_bytes
        .and_then(|value| u64::try_from(value).ok());
    let max_bytes = match (args.max_bytes, store_max) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let provider = TransferIdentity {
        connection_id: args.provider_connection_id.into(),
        session_key: args.service_session_key.into(),
    };
    let consumer = TransferIdentity {
        connection_id: args.consumer_connection_id.into(),
        session_key: args.session_key.into(),
    };
    let subject = |kind| {
        derive_transfer_subject(
            kind,
            args.provider_connection_id,
            args.consumer_connection_id,
            args.transfer_id,
        )
        .map_err(error)
    };
    transfer_expiry_delay(args.expires_at)?;
    Ok(UploadTransferGrantPlan {
        admission: args.admission.clone(),
        store_alias: args.store.into(),
        store: store.name.clone(),
        key: args.key.into(),
        grant: UploadTransferGrant {
            format: TRANSFER_VERSION.into(),
            type_name: "TransferGrant".into(),
            direction: "send".into(),
            service: args.service_name.into(),
            transfer_id: args.transfer_id.into(),
            expires_at: args.expires_at.into(),
            provider,
            consumer,
            data_subject: subject(TransferSubjectKind::UploadData)?,
            control_subject: subject(TransferSubjectKind::Control)?,
            signal_subject: subject(TransferSubjectKind::Signal)?,
            max_frame_bytes,
            window_frames: TRANSFER_WINDOW_FRAMES,
            window_bytes: TRANSFER_WINDOW_BYTES,
            max_bytes,
            content_type: args.content_type.map(Into::into),
            metadata: args.metadata,
        },
    })
}

/// Plan a logical download; the backend store identifier never leaves this runtime.
pub fn plan_download_transfer_grant(
    mut args: TransferDownloadGrantArgs<'_>,
) -> Result<DownloadTransferGrantPlan, ServerError> {
    let store = store_binding(args.service_name, args.resources, args.store)?;
    let max_object_bytes = store
        .max_object_bytes
        .and_then(|value| u64::try_from(value).ok());
    if max_object_bytes.is_some_and(|max| args.info.size > max) {
        return Err(error("download exceeds store object limit"));
    }
    // NATS object metadata uses padded base64url. Adapt that local backend
    // representation before admitting the storage-neutral canonical wire grant.
    if let Some(encoded) = args
        .info
        .digest
        .strip_prefix("SHA-256=")
        .filter(|encoded| encoded.ends_with('='))
    {
        let digest = URL_SAFE.decode(encoded).map_err(error)?;
        args.info.digest = format!("SHA-256={}", URL_SAFE_NO_PAD.encode(digest));
    }
    validate_transfer_digest(&args.info.digest).map_err(|failure| {
        tracing::warn!(error = %failure, digest = %args.info.digest, "download grant object digest rejected");
        error(failure)
    })?;
    let max_frame_bytes =
        negotiate_transfer_max_frame_bytes(args.max_payload, args.max_payload).map_err(error)?;
    let subject = |kind| {
        derive_transfer_subject(
            kind,
            args.provider_connection_id,
            args.consumer_connection_id,
            args.transfer_id,
        )
        .map_err(error)
    };
    transfer_expiry_delay(args.expires_at)?;
    Ok(DownloadTransferGrantPlan {
        admission: args.admission.clone(),
        store_alias: args.store.into(),
        store: store.name.clone(),
        max_object_bytes,
        grant: DownloadTransferGrant {
            format: TRANSFER_VERSION.into(),
            type_name: "TransferGrant".into(),
            direction: "receive".into(),
            service: args.service_name.into(),
            transfer_id: args.transfer_id.into(),
            expires_at: args.expires_at.into(),
            provider: TransferIdentity {
                connection_id: args.provider_connection_id.into(),
                session_key: args.service_session_key.into(),
            },
            consumer: TransferIdentity {
                connection_id: args.consumer_connection_id.into(),
                session_key: args.session_key.into(),
            },
            data_subject: subject(TransferSubjectKind::DownloadData)?,
            control_subject: subject(TransferSubjectKind::Control)?,
            signal_subject: subject(TransferSubjectKind::Signal)?,
            max_frame_bytes,
            window_frames: TRANSFER_WINDOW_FRAMES,
            window_bytes: TRANSFER_WINDOW_BYTES,
            info: args.info,
        },
    })
}

fn store_binding<'a>(
    service_name: &str,
    resources: &'a ServiceResourceBindings,
    store: &str,
) -> Result<&'a StoreResourceBinding, ServerError> {
    resources
        .store
        .get(store)
        .ok_or_else(|| ServerError::MissingResourceBinding {
            service_name: service_name.into(),
            resource_kind: "store".into(),
            resource_name: store.into(),
        })
}

fn enforce_upload_max_bytes(plan: &UploadTransferGrantPlan, size: u64) -> Result<(), ServerError> {
    if plan.grant.max_bytes.is_some_and(|max| size > max) {
        return Err(ServerError::TransferObjectTooLarge {
            service_name: plan.grant.service.clone(),
            store: plan.store_alias.clone(),
            key: plan.key.clone(),
            size,
            max_bytes: plan.grant.max_bytes.unwrap_or_default(),
        });
    }
    Ok(())
}

pub(crate) fn transfer_digests_match(left: &str, right: &str) -> bool {
    left.trim_end_matches('=') == right.trim_end_matches('=')
}

type TransferTask = tokio::task::JoinHandle<Result<(), ServerError>>;

#[derive(Default)]
pub(crate) struct TransferTasks(std::sync::Mutex<(bool, Vec<TransferTask>)>);
impl TransferTasks {
    pub(crate) async fn own(&self, task: TransferTask) -> Result<(), ServerError> {
        let rejected = {
            let mut tasks = self.0.lock().unwrap_or_else(|error| error.into_inner());
            tasks.1.retain(|task| !task.is_finished());
            if tasks.0 {
                Some(task)
            } else {
                tasks.1.push(task);
                None
            }
        };
        if let Some(task) = rejected {
            task.abort();
            let _ = task.await;
            return Err(error("service transfer owner closed"));
        }
        Ok(())
    }
    pub(crate) async fn shutdown(&self) {
        let tasks = {
            let mut tasks = self.0.lock().unwrap_or_else(|error| error.into_inner());
            tasks.0 = true;
            std::mem::take(&mut tasks.1)
        };
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}
pub(crate) struct TransferTasksCleanup(pub(crate) Arc<TransferTasks>);
impl Drop for TransferTasksCleanup {
    fn drop(&mut self) {
        let mut tasks = self.0 .0.lock().unwrap_or_else(|error| error.into_inner());
        tasks.0 = true;
        for task in &tasks.1 {
            task.abort();
        }
    }
}
