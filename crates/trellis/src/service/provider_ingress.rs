//! Per-generation generic provider intake.
//!
//! One ingress owns a single generation lease, the queue-group subscriptions on
//! that generation's physical connection, and the request loop that feeds the
//! shared logical service core. The generation manager adopts this owner during
//! its preactivation barrier, so a candidate's intake is broker-ready **before**
//! it becomes the default generation; retiring an ingress stops accepting new
//! work and drains accepted handler futures without cancelling them, releasing
//! its lease only once the loop has drained.

use std::sync::Arc;
use std::time::Duration;

use super::request_loop::{run_nats_request_loop_until, RequestHandler};
use super::router::GenerationPin;
use super::runtime::subscribe_subject;
use super::runtime_facade::ServiceRuntimeError;
use super::ServerError;
use crate::client::{
    GenerationIntake, GenerationIntakeHandle, GenerationIntakeRetireReason, LogicalTerminalCause,
    TransportGeneration, TransportGenerationManager, TransportLease, TrellisClientError,
};
use futures_util::future::BoxFuture;

/// How long the broker-readiness round trip may take before the barrier fails.
const BROKER_READINESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Broker-owned admission-identity request, granted to every admitted client
/// (`$SYS.REQ.USER.INFO` publish is compiled for all participants). The server
/// answers it on the caller's inbox, so it is a genuine broker round trip that
/// needs no extra grants and no self-publish to the subscribe-only inbox.
const BROKER_READINESS_SUBJECT: &str = "$SYS.REQ.USER.INFO";

/// Confirm the broker has processed this connection's pending protocol commands.
///
/// [`async_nats::Client::flush`] only drains the client's local write buffer; it
/// is **not** a server round trip. The broker's own `$SYS.REQ.USER.INFO` request
/// is, and because the broker processes one connection's commands in order, a
/// completed round trip proves the intake `SUB`s sent before it are registered.
/// This is the readiness the preactivation barrier needs.
async fn confirm_broker_processed(nats: &async_nats::Client) -> Result<(), ServerError> {
    let confirmed = tokio::time::timeout(
        BROKER_READINESS_TIMEOUT,
        nats.request(BROKER_READINESS_SUBJECT, bytes::Bytes::new()),
    )
    .await;
    match confirmed {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ServerError::Nats(format!(
            "broker readiness probe was not answered: {error}"
        ))),
        Err(_) => Err(ServerError::Nats(
            "broker readiness confirmation timed out".into(),
        )),
    }
}

/// How long to wait before re-attaching provider intake after a transient
/// generation change. Bounded pacing, never a terminal authority.
const PROVIDER_INTAKE_ATTACH_RETRY_MS: u64 = 100;

/// One generation's generic provider intake.
pub(crate) struct ProviderIngress {
    retire: tokio::sync::watch::Sender<bool>,
    /// Set once the request loop has returned: intake stopped and every accepted
    /// handler has drained, so the generation lease has released.
    stopped: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderIngress {
    /// Subscribe the logical service core's subjects on one generation and start
    /// the request loop.
    ///
    /// Returns only once every subscription is broker-ready, so a candidate's
    /// intake is installed before it becomes the default and the superseded
    /// generation's intake can be retired without an intake gap; the stable route
    /// queue group makes an overlapping request deliver to exactly one ingress.
    async fn start<H>(
        lease: TransportLease,
        subjects: Arc<[String]>,
        handler: Arc<H>,
        owner_stop: tokio::sync::watch::Receiver<bool>,
        admission: Arc<super::admission::RequestAdmission>,
    ) -> Result<Self, ServerError>
    where
        H: RequestHandler + 'static,
    {
        let nats = lease.nats().clone();
        let mut subscribers = Vec::with_capacity(subjects.len());
        for subject in subjects.iter() {
            subscribers.push(subscribe_subject(&nats, subject).await?);
        }
        // Local write-buffer flush only: it does not prove the broker processed
        // the SUBs. A failure rolls the candidate's subscriptions back.
        if let Err(error) = nats.flush().await {
            for mut subscriber in subscribers {
                let _ = subscriber.unsubscribe().await;
            }
            return Err(ServerError::Nats(format!(
                "failed to flush provider subscriptions: {error}"
            )));
        }
        // Real broker round trip: only after the broker has processed the ordered
        // intake SUBs may readiness be published, so the preactivation barrier is
        // genuinely broker-ready (not merely written to the local socket).
        if let Err(error) = confirm_broker_processed(&nats).await {
            for mut subscriber in subscribers {
                let _ = subscriber.unsubscribe().await;
            }
            return Err(error);
        }
        let (retire, mut retire_rx) = tokio::sync::watch::channel(false);
        let (stopped, _) = tokio::sync::watch::channel(false);
        let stopped_tx = stopped.clone();
        let pin = GenerationPin(Some(lease.clone()));
        let task = tokio::spawn(async move {
            // The lease pins this generation until the loop has drained every
            // accepted handler and returned. It is released *before* the stopped
            // signal, so an observer that sees `intake_stopped` also observes the
            // accepted-work lease at zero.
            let lease = lease;
            let mut owner_stop = owner_stop;
            // Retirement is the union of this generation's own retire signal and
            // the logical owner's stop signal: a supersession retires only this
            // generation, while ending the owner retires every ingress it
            // installed on every receiving generation. Both paths are graceful -
            // the loop stops accepting and drains accepted callbacks - so neither
            // ever aborts accepted work.
            let retire = async move {
                loop {
                    // Resolve for a signal already latched before this task ran
                    // (the owner may have stopped during the attach await).
                    if *retire_rx.borrow() || *owner_stop.borrow() {
                        return;
                    }
                    tokio::select! {
                        result = retire_rx.changed() => {
                            if result.is_err() {
                                return;
                            }
                        }
                        result = owner_stop.changed() => {
                            if result.is_err() {
                                return;
                            }
                        }
                    }
                }
            };
            if let Err(error) =
                run_nats_request_loop_until(nats, subscribers, handler, pin, retire, admission)
                    .await
            {
                tracing::warn!(%error, "service provider ingress loop ended");
            }
            drop(lease);
            // `watch::Sender::send` does not store the value when there is no
            // receiver yet (the ingress holds a `Sender`, and a disposal waiter
            // only subscribes later), so the completion signal would be lost and
            // `intake_stopped` would wait forever. `send_replace` always stores it.
            stopped_tx.send_replace(true);
        });
        Ok(Self {
            retire,
            stopped,
            task,
        })
    }
}

impl GenerationIntakeHandle for ProviderIngress {
    fn retire(&self, _reason: GenerationIntakeRetireReason) {
        let _ = self.retire.send(true);
    }

    fn intake_stopped(&self) -> BoxFuture<'_, ()> {
        let mut stopped = self.stopped.subscribe();
        Box::pin(async move {
            // Resolve for a waiter that arrives after the loop already returned as
            // well as one waiting on the signal. The task stores completion with
            // `send_replace` (a plain `send` drops the value when no receiver
            // exists yet, which would strand this wait forever), so `subscribe`
            // observes the stored `true` immediately, or `wait_for` resolves on
            // the change.
            if *stopped.borrow_and_update() {
                return;
            }
            let _ = stopped.wait_for(|value| *value).await;
        })
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // The request loop owns the broker subscriptions and the generation
            // lease and releases them when it drains; `dispose` owns nothing else,
            // so a forced termination never waits for accepted work here.
        })
    }
}

impl Drop for ProviderIngress {
    fn drop(&mut self) {
        // Safety net: dropping a retired generation's handle stops new intake;
        // the task drains accepted work and releases the lease on its own.
        let _ = self.retire.send(true);
        let _ = &self.task;
    }
}

/// The logical service core's provider intake owner.
struct ProviderIntake<H> {
    admission: Arc<super::admission::RequestAdmission>,
    subjects: Arc<[String]>,
    handler: Arc<H>,
    /// Owner-scoped stop, latched once this logical provider's registration
    /// ends. Every ingress this owner installs carries a receiver, so ending the
    /// owner retires its intake on every receiving generation and an adopted
    /// clone can never resurrect a stopped provider.
    stop: tokio::sync::watch::Sender<bool>,
}

impl<H> GenerationIntake for ProviderIntake<H>
where
    H: RequestHandler + 'static,
{
    fn adopt<'a>(
        &'a self,
        generation: Arc<TransportGeneration>,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<Box<dyn GenerationIntakeHandle>, TrellisClientError>,
    > {
        Box::pin(async move {
            // A stopped owner must never install intake, even when a candidate
            // still held this owner's Arc from before the stop.
            if *self.stop.borrow() {
                return Err(TrellisClientError::TransportUnavailable(
                    "provider intake owner has stopped".into(),
                ));
            }
            let lease = generation.lease().ok_or_else(|| {
                TrellisClientError::TransportUnavailable(
                    "transport generation is closed; provider intake was not installed".into(),
                )
            })?;
            // Race intake preparation against the owning registration ending.
            // Readiness is a real broker round trip bounded well above the
            // cancellation budget, so a manager adoption already awaiting it
            // would otherwise hold this owner - and through it the
            // owner->handler->client strong reference and the generation lease -
            // until that bound elapsed. Biasing the shared owner stop lets a
            // stop preempt the in-flight setup: dropping the losing `start`
            // future releases the lease and any partially created subscriptions
            // through their own ownership. An already installed ingress is
            // unaffected: its own retire signal, per-generation retire, and
            // graceful accepted-work drain still own retirement.
            let mut stop = self.stop.subscribe();
            let start = ProviderIngress::start(
                lease,
                self.subjects.clone(),
                self.handler.clone(),
                stop.clone(),
                self.admission.clone(),
            );
            tokio::pin!(start);
            let result = tokio::select! {
                biased;
                stopped = stop.wait_for(|stopped| *stopped) => {
                    // `Ok` is the stop latching; `Err` is every owner dropped.
                    // Either way this owner must not install or keep intake.
                    let _ = stopped;
                    return Err(TrellisClientError::TransportUnavailable(
                        "provider intake owner has stopped".into(),
                    ));
                }
                result = &mut start => result,
            };
            result
                .map(|ingress| Box::new(ingress) as Box<dyn GenerationIntakeHandle>)
                .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))
        })
    }
}

/// RAII guard for one logical provider's intake registration.
///
/// Installed before the first attach await and held until
/// [`run_provider_intake`] returns or its serve future is cancelled. Its
/// synchronous `Drop` latches the owner's shared stop - retiring every ingress
/// the owner installed on every receiving generation - and then unregisters the
/// owner so no later candidate adopts it. Ending the registration this way
/// breaks the strong `manager -> owner -> handler -> client` cycle that would
/// otherwise keep the logical client (and its provisional transport resources)
/// alive after its ordinary owners were dropped.
struct ProviderRegistrationGuard<H>
where
    H: RequestHandler + 'static,
{
    manager: TransportGenerationManager,
    /// Retained through [`TransportGenerationManager::detach_intake`]: removing
    /// the manager's Arc must never drop the last owner (and therefore the
    /// service client) while the manager's owner lock is held inside the removal.
    owner: Arc<ProviderIntake<H>>,
}

impl<H> Drop for ProviderRegistrationGuard<H>
where
    H: RequestHandler + 'static,
{
    fn drop(&mut self) {
        // Latch the owner stop first, so an adoption already in flight observes
        // it and every installed ingress retires, then unregister so a later
        // candidate's barrier cannot adopt this stopped owner.
        self.owner.stop.send_replace(true);
        let owner: Arc<dyn GenerationIntake> = self.owner.clone();
        self.manager.detach_intake(&owner);
    }
}

/// Register the logical service core's provider intake with the generation
/// manager and serve until the connection closes.
///
/// The manager installs broker-ready intake on every new candidate before it
/// becomes the default, and promptly retires the superseded generation's intake
/// while its accepted callbacks finish on it. The registration is owned by this
/// call for its whole life: ending it - including cancelling this future -
/// retires the owner's intake and releases the client reference cycle.
pub(crate) async fn run_provider_intake<H>(
    manager: TransportGenerationManager,
    subjects: Arc<[String]>,
    handler: Arc<H>,
    admission: Arc<super::admission::RequestAdmission>,
) -> Result<(), ServiceRuntimeError>
where
    H: RequestHandler + 'static,
{
    if subjects.is_empty() {
        // No intake to own: only observe the authoritative outcome.
        return terminal_result(manager.wait_terminal().await);
    }
    let (stop, _) = tokio::sync::watch::channel(false);
    let owner = Arc::new(ProviderIntake {
        admission,
        subjects,
        handler,
        stop,
    });
    // Installed before the first attach await and held across every retry and
    // through `wait_terminal`, so any exit - including cancelling this future
    // while an attach is pending - unregisters this owner and stops its intake.
    let _registration = ProviderRegistrationGuard {
        manager: manager.clone(),
        owner: Arc::clone(&owner),
    };
    let owner: Arc<dyn GenerationIntake> = owner;
    // Attach until the current generation accepts broker-ready intake, or the
    // logical connection reaches an authoritative terminal state. A generation
    // that changed under the install is transient, not a service failure.
    loop {
        if let Some(cause) = manager.terminal() {
            return terminal_result(cause);
        }
        match manager.attach_intake(Arc::clone(&owner)).await {
            Ok(()) => break,
            Err(error) => {
                if let Some(cause) = manager.terminal() {
                    return terminal_result(cause);
                }
                tracing::warn!(%error, "provider intake attach will retry");
                tokio::time::sleep(Duration::from_millis(PROVIDER_INTAKE_ATTACH_RETRY_MS)).await;
            }
        }
    }
    // Serve until the logical connection is terminally finished. The installed
    // ingress serves intake on every adopted generation; this task only observes
    // the authoritative outcome, so it never retries a dead authority forever.
    terminal_result(manager.wait_terminal().await)
}

/// The service runtime outcome of one latched logical terminal cause: an explicit
/// close is a clean stop, an authoritative refusal is a reported terminal error.
fn terminal_result(cause: LogicalTerminalCause) -> Result<(), ServiceRuntimeError> {
    match cause {
        LogicalTerminalCause::Closed => Ok(()),
        cause => Err(ServiceRuntimeError::from(cause.client_error())),
    }
}
