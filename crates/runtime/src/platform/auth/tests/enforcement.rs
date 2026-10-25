use super::*;

#[tokio::test]
async fn real_broker_publishes_signed_cutoff_and_closes_only_the_matching_marker() {
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let attachment = id();
    let marker = format!(
        "{}{attachment}",
        super::super::broker::ATTACHMENT_MARKER_PREFIX
    );
    let directory = tempfile::tempdir().unwrap();
    // Held until the broker's listeners are up; see ReservedPort.
    let mut reservations =
        std::array::from_fn::<_, 3, _>(|_| trellis_local_nats::ReservedPort::reserve().unwrap());
    let [port, http, websocket] = reservations.each_ref().map(|reserved| reserved.port());
    let config = directory.path().join("nats.conf");
    std::fs::write(
        &config,
        format!(
            "port: {port}\nhttp_port: {http}\nwebsocket {{ port: {websocket}, \
            no_tls: true }}\njetstream {{ store_dir: '{}' }}\naccounts {{ SYS {{ \
            users: [{{user: sys,password: pw}}] }}, APP {{ jetstream: enabled, \
            users: [{{user: '{marker}',password: pw}},{{user: control,password: \
            pw}}] }} }}\nsystem_account: SYS\n",
            directory.path().join("jetstream").display()
        ),
    )
    .unwrap();
    for reservation in &mut reservations {
        reservation.release_for_bind();
    }
    // CI supplies its producer-resolved executable. There is no rebuilt runtime,
    // fake broker, test-support feature, readiness bypass, or hidden skip.
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut server = tokio::process::Command::new(binary)
        .arg("-c")
        .arg(&config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("set TRELLIS_NATS_SERVER_BIN to the producer-resolved NATS executable");
    let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            eprintln!("{line}");
            if line.contains("Server is ready") {
                return;
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .expect("NATS readiness deadline");
    let url = format!("nats://127.0.0.1:{port}");
    let system = async_nats::ConnectOptions::with_user_and_password("sys".into(), "pw".into())
        .connect(&url)
        .await
        .unwrap();
    let closed = std::sync::Arc::new(tokio::sync::Notify::new());
    let notified = closed.clone();
    let app = async_nats::ConnectOptions::with_user_and_password(marker.clone(), "pw".into())
        .max_reconnects(0)
        .event_callback(move |event| {
            let notified = notified.clone();
            async move {
                if matches!(event, async_nats::Event::Disconnected) {
                    notified.notify_one();
                }
            }
        })
        .connect(&url)
        .await
        .unwrap();
    let publisher =
        async_nats::ConnectOptions::with_user_and_password("control".into(), "pw".into())
            .connect(&url)
            .await
            .unwrap();
    let jetstream = async_nats::jetstream::new(publisher.clone());
    let material = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: format!("material_{}", id()),
            ..Default::default()
        })
        .await
        .unwrap();
    let revocations = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: format!("revocations_{}", id()),
            limit_markers: Some(Duration::from_secs(10)),
            ..Default::default()
        })
        .await
        .unwrap();
    let server_id = app.server_info().server_id;
    let inventory = system
        .request(
            format!("$SYS.REQ.SERVER.{server_id}.CONNZ"),
            serde_json::to_vec(&serde_json::json!({"auth":true}))
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.payload).unwrap();
    let cid = inventory["data"]["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| connection["authorized_user"] == marker)
        .unwrap()["cid"]
        .as_u64()
        .unwrap();
    super::super::broker::close_attachment(&system, &server_id, cid, &id())
        .await
        .unwrap();
    app.flush().await.unwrap(); // A reused CID is not authority to kick this user.
    assert!(
        super::super::broker::close_attachment(&system, "UNKNOWN_SERVER", cid, &attachment)
            .await
            .is_err()
    );
    drain(&f.store, NOW);
    f.store
        .process_effects(&material, &revocations, &system, NOW)
        .await
        .unwrap();
    let published = material
        .get(format!("context.{}", issued.digest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        trellis_protocol::parse_session_authority(&published, 65536).unwrap(),
        issued.authority
    );
    // Drain ordinary provisioning before putting an effect-free policy page
    // ahead of a provisioned identity's retirement. Reporting quiescence after
    // that first page would leave its already-issued authority active.
    let provider = f
        .store
        .initialize_builtin_provider::<
            trellis_runtime_apis::participants::trellis_health_runtime::Participant,
        >(&URL_SAFE_NO_PAD.encode([47; 32]), NOW)
        .unwrap();
    let provisioned_admission = AdmissionIdentity {
        credential: Credential::ProvisionedIdentity(provider.identity_key_id),
        refresh_family_id: None,
        ..f.admission.clone()
    };
    let provider_authority = issue_authority(&f.store, &provisioned_admission, None, NOW).unwrap();
    while f
        .store
        .process_effects(&material, &revocations, &system, NOW)
        .await
        .unwrap()
        != 0
    {}
    let unused_role = PolicyMutation::RolePut {
        role_id: id(),
        expected_revision: 0,
        title: "Unused role".into(),
        description: "No assignments".into(),
        capabilities: vec![],
    };
    apply_policy(
        &f.store,
        &mutation(&f.admin, &unused_role),
        &unused_role,
        NOW + 1,
    )
    .unwrap();
    let disable_provider = PolicyMutation::PrincipalState {
        principal_id: provider.principal_id,
        expected_revision: 1,
        state: "disabled".into(),
    };
    apply_policy(
        &f.store,
        &mutation(&f.admin, &disable_provider),
        &disable_provider,
        NOW + 2,
    )
    .unwrap();
    while f
        .store
        .process_effects(&material, &revocations, &system, NOW + 2)
        .await
        .unwrap()
        != 0
    {}
    let provider_cutoff = revocations
        .get(
            &provider_authority
                .authority
                .unsigned
                .authorization_session_id,
        )
        .await
        .unwrap()
        .expect("effect-free work must not strand provider retirement");
    let provider_cutoff: trellis_protocol::SignedSessionRevocation =
        serde_json::from_slice(&provider_cutoff).unwrap();
    provider_cutoff
        .verify(
            &f.store.issuer().unwrap(),
            &f.store.settings.instance_id,
            &f.store.settings.account,
        )
        .unwrap();
    assert_eq!(
        resolve_authority(&f.store, &provider_authority.digest).unwrap(),
        provider_authority.authority
    );
    let session = issued.authority.unsigned.authorization_session_id.clone();
    let mut watcher = revocations.watch_with_history(&session).await.unwrap();
    let scope = EnforcementScope::Session(session.clone());
    revoke_root(&f.store, &mutation(&f.admin, &scope), &scope, NOW + 1).unwrap();
    f.store
        .process_effects(&material, &revocations, &system, NOW + 1)
        .await
        .unwrap();
    use futures_util::StreamExt;
    let update = tokio::time::timeout(Duration::from_secs(5), watcher.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let signed: trellis_protocol::SignedSessionRevocation =
        serde_json::from_slice(&update.value).unwrap();
    signed
        .verify(
            &f.store.issuer().unwrap(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
        )
        .unwrap();
    assert_eq!(signed.effective_cutoff, NOW + 1);
    // The outbox dependency test owns claim ordering. Here the real adapters
    // must retain the signed publication and close only its exact broker marker.
    app.flush().await.unwrap();
    super::super::broker::close_attachment(&system, &server_id, cid, &attachment)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), closed.notified())
        .await
        .unwrap();
    let inventory = system
        .request(
            format!("$SYS.REQ.SERVER.{server_id}.CONNZ"),
            serde_json::to_vec(&serde_json::json!({"auth":true,"cid":cid}))
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.payload).unwrap();
    assert!(inventory["data"]["connections"]
        .as_array()
        .unwrap()
        .is_empty());
    publisher.flush().await.unwrap();
    assert_eq!(
        revocations.get(&session).await.unwrap().unwrap().as_ref(),
        update.value.as_ref()
    );
    assert!(
        super::super::broker::close_attachment(&system, &server_id, cid, &attachment)
            .await
            .is_ok()
    );
    server.kill().await.unwrap();
}

#[test]
fn hard_grant_revocation_blocks_offline_reissue_and_preserves_signed_cutoff() {
    let f = fixture();
    let issued = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let scope = EnforcementScope::Grant(f.grant.clone());
    let identity = mutation(&f.admin, &scope);
    let work = revoke_root(&f.store, &identity, &scope, NOW + 1).unwrap();
    assert_eq!(
        revoke_root(&f.store, &identity, &scope, NOW + 2).unwrap(),
        work
    );
    assert!(matches!(
        issue_authority(&f.store, &f.admission, None, NOW + 1),
        Err(AuthError::Denied)
    ));
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    drain(&restarted, NOW + 1);
    let revocation = restarted
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .unwrap();
    revocation
        .verify(
            &restarted.issuer().unwrap(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            account_key(),
        )
        .unwrap();
    assert_eq!(revocation.effective_cutoff, NOW + 1);
    restarted.prune_expired_artifacts(NOW + 10000).unwrap();
    assert!(restarted
        .resolve_revocation(&issued.authority.unsigned.authorization_session_id)
        .unwrap()
        .is_some());
}

#[test]
fn targeted_enforcement_preempts_a_queued_broad_scan_and_wakes_workers() {
    let f = fixture();
    drain(&f.store, NOW);
    let mut queued = f.store.work_signal();
    queued.mark_unchanged();
    let broad = f
        .store
        .transaction(|connection| {
            super::super::revocation::enqueue_scope(connection, &EnforcementScope::All, NOW)
        })
        .unwrap();
    assert!(
        queued.has_changed().unwrap(),
        "a commit that queues work wakes workers"
    );
    queued.mark_unchanged();
    let targeted = f
        .store
        .transaction(|connection| {
            super::super::revocation::enqueue_scope(
                connection,
                &EnforcementScope::Session(id()),
                NOW,
            )
        })
        .unwrap();
    assert!(queued.has_changed().unwrap());
    assert_eq!(f.store.enforce_next_page(NOW).unwrap(), Some(targeted));
    assert_eq!(f.store.enforce_next_page(NOW).unwrap(), Some(broad));
    queued.mark_unchanged();
    f.store.read(|_| Ok(())).unwrap();
    assert!(!queued.has_changed().unwrap(), "reads never wake workers");
}

#[test]
fn identical_effects_behind_different_predecessors_keep_both_edges() {
    let f = fixture();
    let payload = serde_json::json!({"kind":"authority","digest":"same"});
    let actions = f
        .store
        .transaction(|connection| {
            let first = super::super::revocation::enqueue_effect(
                connection,
                "authority_publish",
                &payload,
                Some("first-predecessor"),
                NOW,
            )?;
            let second = super::super::revocation::enqueue_effect(
                connection,
                "authority_publish",
                &payload,
                Some("second-predecessor"),
                NOW,
            )?;
            let edges: i64 = connection.query_row(
                "SELECT count(*) FROM auth_post_commit_actions \
                WHERE action_id IN (?1,?2) AND predecessor_action_id IS NOT NULL",
                [&first, &second],
                |row| row.get(0),
            )?;
            Ok((first, second, edges))
        })
        .unwrap();
    assert_ne!(actions.0, actions.1);
    assert_eq!(actions.2, 2);
}

#[test]
fn outbox_orders_revocation_before_kicks_and_fences_expired_claims() {
    let f = fixture();
    {
        let nkey = nkeys::KeyPair::new_from_raw(nkeys::KeyPairType::User, [9; 32])
            .unwrap()
            .public_key();
        f.store.admit_connection(
            &fixture_admission(&f, &nkey, NOW),
            &nkey,
            &nkey,
            "offline-broker",
            8,
            NOW,
        )
    }
    .unwrap();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityRevoke {
            principal_id: f.principal.clone(),
            capability_id: f.write.clone(),
            expected_revision: 1,
        },
        NOW + 1,
    );
    drain(&f.store, NOW + 1);
    let effects = f.store.claim_effects(256, NOW + 1, 2, false).unwrap();
    assert!(!effects.iter().any(|effect| effect.kind == "kick"));
    let revoke = effects
        .iter()
        .find(|effect| effect.kind == "session_revoke")
        .unwrap();
    assert!(!f.store.complete_effect(revoke, NOW + 3).unwrap());
    assert!(!f.store.retry_effect(revoke, NOW + 3).unwrap());
    let stale_revoke = revoke.clone();
    drop(f.store);
    let restarted = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let effects = restarted.claim_effects(256, NOW + 3, 10, false).unwrap();
    let revoke = effects
        .iter()
        .find(|effect| effect.kind == "session_revoke")
        .unwrap();
    assert!(!restarted.complete_effect(&stale_revoke, NOW + 3).unwrap());
    assert!(!restarted.retry_effect(&stale_revoke, NOW + 3).unwrap());
    assert!(restarted.complete_effect(revoke, NOW + 3).unwrap());
    let effects = restarted.claim_effects(256, NOW + 3, 10, false).unwrap();
    let kick = effects.iter().find(|effect| effect.kind == "kick").unwrap();
    assert!(restarted.retry_effect(kick, NOW + 3).unwrap());
    assert!(restarted
        .claim_effects(256, NOW + 3, 10, false)
        .unwrap()
        .is_empty());
    let retried = restarted.claim_effects(256, NOW + 4, 10, false).unwrap();
    assert!(restarted
        .complete_effect(
            retried.iter().find(|effect| effect.kind == "kick").unwrap(),
            NOW + 4,
        )
        .unwrap());
    assert!(restarted
        .claim_effects(256, NOW + 4, 10, false)
        .unwrap()
        .is_empty());
}
