use async_nats::jetstream::stream::RawMessageErrorKind;
use trellis_rs::__internal::LiveProviderOwner;
use trellis_rs::__internal::RuntimeOwner as _;
use trellis_rs::service::{Router, ServerError};
use trellis_runtime_apis::apis::trellis_jobs_v1::rpc::History;

pub(super) fn register(
    router: &mut Router,
    evidence: crate::resource_evidence::ResourceEvidenceReader,
    nats: async_nats::Client,
    owner: LiveProviderOwner,
) {
    router.register_rpc::<History, _, _>(move |request, input| {
        let evidence = evidence.clone();
        let nats = nats.clone();
        let owner = owner.clone();
        async move {
            let session_key = request.session_key.clone().unwrap_or_default();
            let forbidden = || forbidden(&session_key);
            let verified = request.verified_request().ok_or_else(forbidden)?;
            let authority = verified.authority();
            let trellis_protocol::SessionBinding::Service {
                deployment_id,
                participant_id,
                instance_id,
            } = &authority.authority().binding
            else {
                return Err(forbidden());
            };
            if input.job_id.is_empty()
                || input
                    .job_id
                    .bytes()
                    .any(|c| !c.is_ascii_alphanumeric() && c != b'-' && c != b'_')
            {
                return Err(forbidden());
            }
            use trellis_runtime_apis::types::{
                AuthResourceBindingsQueryRequest, AuthResourceEvidenceKind,
            };
            let rows = evidence
                .query(AuthResourceBindingsQueryRequest {
                    kind: AuthResourceEvidenceKind::JobQueue,
                    instance_id: Some(instance_id.parse().map_err(|_| forbidden())?),
                    owner_id: Some(deployment_id.parse().map_err(|_| forbidden())?),
                    participant_id: Some(participant_id.clone()),
                    local_name: Some(input.queue.clone()),
                    resource_id: None,
                    page: None,
                    extra: Default::default(),
                })
                .await
                .map_err(ServerError::Nats)?;
            let expected = authority
                .authority()
                .resource_bindings_digest
                .as_deref()
                .ok_or_else(forbidden)?;
            let binding = rows
                .into_iter()
                .find(|row| {
                    row.resource_bindings_digest
                        .as_ref()
                        .is_some_and(|digest| digest.as_ref() == expected)
                })
                .and_then(|row| row.job)
                .ok_or_else(forbidden)?;
            let prefix = binding.publish_prefix;
            owner
                .runtime_authorization_owner()
                .assert_current(authority)
                .map_err(|_| forbidden())?;
            request.authorize_handler_request()?;
            let js = async_nats::jetstream::new(nats);
            // Leader reads preserve publication order. Every continuation keeps the
            // first page's watermark while reauthorizing the current resource binding.
            let stream = js
                .get_stream("JOBS")
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            let through = input
                .through_sequence
                .map_or(stream.cached_info().state.last_sequence, |value| {
                    value.0.min(stream.cached_info().state.last_sequence)
                });
            let subject = format!("{prefix}.{}.*", input.job_id);
            let mut next = input.from_sequence.0.max(1);
            let mut entries = Vec::new();
            let mut bytes = 0;
            while next <= through && entries.len() < 128 && bytes < 128 * 1024 {
                let message = match stream
                    .get_first_raw_message_by_subject(&subject, next)
                    .await
                {
                    Ok(message) if message.sequence <= through => message,
                    Ok(_) => {
                        next = through.saturating_add(1);
                        break;
                    }
                    Err(error) if error.kind() == RawMessageErrorKind::NoMessageFound => {
                        next = through.saturating_add(1);
                        break;
                    }
                    Err(error) => return Err(ServerError::Nats(error.to_string())),
                };
                bytes += message.payload.len();
                next = message.sequence.saturating_add(1);
                entries.push(trellis_runtime_apis::types::JobsHistoryEntry {
                    sequence: message.sequence.into(),
                    payload: message.payload.to_vec().into(),
                    extra: Default::default(),
                });
            }
            owner
                .runtime_authorization_owner()
                .assert_current(authority)
                .map_err(|_| forbidden())?;
            Ok(trellis_runtime_apis::types::JobsHistoryResponse {
                entries,
                through_sequence: through.into(),
                next_sequence: (next <= through).then_some(next.into()),
                extra: Default::default(),
            })
        }
    });
    router.rpc_handler_authorizes::<History>();
}

fn forbidden(session_key: &str) -> ServerError {
    ServerError::RequestDenied {
        subject: "rpc.v1.jobs.History".into(),
        session_key: session_key.into(),
    }
}
