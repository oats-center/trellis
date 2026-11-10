//! A deferred operation stays owned by the replica that ran its handler: only
//! that replica's `control` reaches it, and any replica's `reconcile` enters
//! the handler again on the owner.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_trellis::apps::runtime_trellis_caller::{Client as CallerClient, Request};
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use trellis_rs::client::OperationState as CallerState;
use trellis_rs::service::{OperationState, ServerError};
use trellis_testkit::TrellisTestRuntime;

fn value(value: &str) -> Value {
    Value {
        value: value.into(),
        extra: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_owner_controls_a_deferred_operation_and_any_replica_reconciles_it() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let first = runtime
        .register_service::<Participant>("reconciled work")
        .await
        .unwrap();
    let second = runtime.add_service_instance(&first).await.unwrap();
    // Handler entries per input value, and which replica took each first entry.
    let entries = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let (deferred, mut deferrals) = tokio::sync::mpsc::unbounded_channel();
    let mut registrations = Vec::new();
    let mut runs = Vec::new();
    for (replica, identity) in [&first, &second].into_iter().enumerate() {
        let mut service = Participant::connect(identity.connect_options())
            .await
            .unwrap();
        let entries = Arc::clone(&entries);
        let deferred = deferred.clone();
        registrations.push(
            Provider::new(&mut service)
                .runtime_trellis_runtime_v1()
                .register_work(move |_context, input, operation| {
                    let entries = Arc::clone(&entries);
                    let deferred = deferred.clone();
                    async move {
                        let entry = {
                            let mut entries = entries.lock().unwrap();
                            let entry = entries.entry(input.value.clone()).or_default();
                            *entry += 1;
                            *entry
                        };
                        if entry == 1 {
                            operation.started().await?;
                            operation.defer();
                            let _ = deferred.send((replica, operation.operation_ref().id.clone()));
                            return Ok::<_, ServerError>(());
                        }
                        operation.complete(input).await?;
                        Ok(())
                    }
                }),
        );
        runs.push(tokio::spawn(service.run()));
    }

    let caller = runtime.register_client::<Request>("caller").await.unwrap();
    let caller = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller.connect_options())
            .await
            .unwrap(),
    );
    let api = caller.runtime_trellis_runtime_v1();
    let work = api.work();
    let deferral = Duration::from_secs(30);

    // The owner finishes a deferred operation from outside its handler; the
    // other replica cannot borrow its fence.
    let controlled = work.start(&value("controlled")).await.unwrap();
    let (owner, id) = tokio::time::timeout(deferral, deferrals.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, controlled.id());
    assert!(matches!(
        registrations[1 - owner].control(&id),
        Err(ServerError::OperationNotFound { .. })
    ));
    registrations[owner]
        .control(&id)
        .unwrap()
        .complete(value("finished by control"))
        .await
        .unwrap();
    let finished = controlled.wait().await.unwrap();
    assert_eq!(finished.state, CallerState::Completed);
    assert_eq!(finished.output.unwrap().value, "finished by control");

    // Either replica's reconciliation reaches the owner, which enters the
    // handler again with the stored input.
    let reconciled = work.start(&value("reconciled")).await.unwrap();
    let (owner, id) = tokio::time::timeout(deferral, deferrals.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        registrations[1 - owner]
            .reconcile("missing-operation")
            .await,
        Err(ServerError::OperationNotFound { .. })
    ));
    let snapshot = registrations[1 - owner].reconcile(&id).await.unwrap();
    assert_eq!(snapshot.state, OperationState::Completed);
    assert_eq!(snapshot.output.unwrap().value, "reconciled");
    assert_eq!(entries.lock().unwrap()["reconciled"], 2);
    assert_eq!(
        reconciled.wait().await.unwrap().state,
        CallerState::Completed
    );

    for run in runs {
        run.abort();
    }
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_deferred_operation_enters_its_handler_again_to_clean_up() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("cancelled deferral")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (deferred, mut deferrals) = tokio::sync::mpsc::unbounded_channel();
    let (cleaned, mut cleanups) = tokio::sync::mpsc::unbounded_channel();
    let _registration = Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_work(move |_context, input, operation| {
            let deferred = deferred.clone();
            let cleaned = cleaned.clone();
            async move {
                if operation.cancellation().is_cancelled() {
                    let _ = cleaned.send(input.value);
                    return Ok::<_, ServerError>(());
                }
                operation.started().await?;
                operation.defer();
                let _ = deferred.send(());
                Ok(())
            }
        });
    let run = tokio::spawn(service.run());
    let caller = runtime.register_client::<Request>("caller").await.unwrap();
    let caller = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller.connect_options())
            .await
            .unwrap(),
    );
    let api = caller.runtime_trellis_runtime_v1();
    let work = api.work();
    let limit = Duration::from_secs(30);

    let operation = work.start(&value("to undo")).await.unwrap();
    tokio::time::timeout(limit, deferrals.recv())
        .await
        .unwrap()
        .unwrap();
    operation.cancel().await.unwrap();
    assert_eq!(
        tokio::time::timeout(limit, cleanups.recv())
            .await
            .unwrap()
            .unwrap(),
        "to undo"
    );
    let finished = tokio::time::timeout(limit, operation.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, CallerState::Cancelled);

    run.abort();
    runtime.shutdown().await.unwrap();
}
