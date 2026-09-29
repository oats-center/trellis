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

use super::request_loop::{run_nats_request_loop_until, RequestHandler};
use super::router::GenerationPin;
use super::runtime::subscribe_subject;
use super::runtime_facade::ServiceRuntimeError;
use super::ServerError;
use crate::client::{
    GenerationIntake, GenerationIntakeHandle, TransportGeneration, TransportGenerationManager,
    TransportLease, TrellisClientError,
};

/// One generation's generic provider intake.
pub(crate) struct ProviderIngress {
    retire: tokio::sync::watch::Sender<bool>,
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
    ) -> Result<Self, ServerError>
    where
        H: RequestHandler + 'static,
    {
        let nats = lease.nats().clone();
        let mut subscribers = Vec::with_capacity(subjects.len());
        for subject in subjects.iter() {
            subscribers.push(subscribe_subject(&nats, subject).await?);
        }
        // Broker-ready barrier: the server must have acknowledged every SUB
        // before the candidate can become the default generation. A flush
        // failure rolls the candidate's subscriptions back.
        if let Err(error) = nats.flush().await {
            for mut subscriber in subscribers {
                let _ = subscriber.unsubscribe().await;
            }
            return Err(ServerError::Nats(format!(
                "failed to flush provider subscriptions: {error}"
            )));
        }
        let (retire, mut retire_rx) = tokio::sync::watch::channel(false);
        let pin = GenerationPin(Some(lease.clone()));
        let task = tokio::spawn(async move {
            // The lease pins this generation until the loop has drained every
            // accepted handler and returned.
            let _lease = lease;
            let retire = async move {
                while retire_rx.changed().await.is_ok() {
                    if *retire_rx.borrow() {
                        return;
                    }
                }
            };
            if let Err(error) =
                run_nats_request_loop_until(nats, subscribers, handler, pin, retire).await
            {
                tracing::warn!(%error, "service provider ingress loop ended");
            }
        });
        Ok(Self { retire, task })
    }
}

impl GenerationIntakeHandle for ProviderIngress {
    fn retire(&self) {
        let _ = self.retire.send(true);
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
    subjects: Arc<[String]>,
    handler: Arc<H>,
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
            let ingress = ProviderIngress::start(
                generation.lease(),
                self.subjects.clone(),
                self.handler.clone(),
            )
            .await
            .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
            Ok(Box::new(ingress) as Box<dyn GenerationIntakeHandle>)
        })
    }
}

/// Register the logical service core's provider intake with the generation
/// manager and serve until the connection closes.
///
/// The manager installs broker-ready intake on every new candidate before it
/// becomes the default, and promptly retires the superseded generation's intake
/// while its accepted callbacks finish on it.
pub(crate) async fn run_provider_intake<H>(
    manager: TransportGenerationManager,
    subjects: Arc<[String]>,
    handler: Arc<H>,
) -> Result<(), ServiceRuntimeError>
where
    H: RequestHandler + 'static,
{
    if subjects.is_empty() {
        std::future::pending::<()>().await;
    }
    let owner: Arc<dyn GenerationIntake> = Arc::new(ProviderIntake { subjects, handler });
    manager
        .attach_intake(owner)
        .await
        .map_err(ServiceRuntimeError::from)?;
    std::future::pending::<()>().await;
    Ok(())
}
