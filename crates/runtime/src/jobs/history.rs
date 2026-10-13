use std::path::PathBuf;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};
use trellis_rs::service::{LiveProviderOwner, Router, ServerError, ServiceResourceBindings};
use trellis_runtime_apis::apis::trellis_jobs_v1::rpc::History;

pub(super) fn register(
    router: &mut Router,
    path: PathBuf,
    nats: async_nats::Client,
    owner: LiveProviderOwner,
) {
    router.register_rpc::<History, _, _>(move |request, input| {
        let path = path.clone();
        let nats = nats.clone();
        let owner = owner.clone();
        async move {
            let session_key = request.session_key.clone().unwrap_or_default();
            let forbidden = || forbidden(&session_key);
            let verified = request.verified_request().ok_or_else(forbidden)?;
            let authority = verified.authority();
            let trellis_protocol::SessionBinding::Service { deployment_id, participant_id, .. } =
                &authority.authority().binding else { return Err(forbidden()); };
            if input.job_id.is_empty() || input.job_id.bytes().any(|c| !c.is_ascii_alphanumeric() && c != b'-' && c != b'_') {
                return Err(forbidden());
            }
            let deployment_id = deployment_id.clone();
            let participant_id = participant_id.clone();
            let queue = input.queue.clone();
            let expected = authority.authority().resource_bindings_digest.clone();
            let resource_session_key = session_key.clone();
            let prefix = tokio::task::spawn_blocking(move || -> Result<String, ServerError> {
                let connection = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|error|ServerError::Nats(error.to_string()))?;
                let json: String = connection.query_row(
                    "SELECT b.resource_commitments_json FROM auth_deployment_bindings b JOIN auth_resources r ON r.owner_id=b.deployment_id AND r.binding_revision=b.revision JOIN auth_deployments d ON d.deployment_id=b.deployment_id WHERE b.deployment_id=?1 AND r.owner_kind='deployment' AND r.participant_id=?2 AND r.kind='job' AND r.local_name=?3 AND r.state='ready' AND d.state='active'",
                    rusqlite::params![deployment_id,participant_id,queue], |row|row.get(0)).map_err(|_|crate::jobs::history::forbidden(&resource_session_key))?;
                let value: serde_json::Value = serde_json::from_str(&json).map_err(|error|ServerError::Nats(error.to_string()))?;
                let canonical = trellis_protocol::canonicalize_json(&value).map_err(|error|ServerError::Nats(error.to_string()))?;
                if expected.as_deref() != Some(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())).as_str()) {
                    return Err(crate::jobs::history::forbidden(&resource_session_key));
                }
                let bindings: ServiceResourceBindings = serde_json::from_value(value).map_err(|error|ServerError::Nats(error.to_string()))?;
                bindings.jobs.and_then(|jobs|jobs.queues.get(&queue).map(|queue|queue.publish_prefix.clone()))
                    .ok_or_else(||crate::jobs::history::forbidden(&resource_session_key))
            }).await.map_err(|error|ServerError::Nats(error.to_string()))??;
            owner.runtime_authorization_owner().assert_current(authority).map_err(|_|forbidden())?;
            request.authorize_handler_request()?;
            let js = async_nats::jetstream::new(nats);
            // Leader reads preserve publication order. Every continuation keeps the
            // first page's watermark while reauthorizing the current resource binding.
            let stream = js.get_stream("JOBS").await.map_err(|error|ServerError::Nats(error.to_string()))?;
            let through = input.through_sequence.map_or(stream.cached_info().state.last_sequence, |value|value.0.min(stream.cached_info().state.last_sequence));
            let subject = format!("{prefix}.{}.*",input.job_id);
            let mut next = input.from_sequence.0.max(1);
            let mut entries = Vec::new();
            let mut bytes = 0;
            while next <= through && entries.len() < 128 && bytes < 128 * 1024 {
                let message = match stream.get_first_raw_message_by_subject(&subject,next).await {
                    Ok(message) if message.sequence <= through => message,
                    Ok(_) => { next = through.saturating_add(1); break; }
                    Err(error) if error.kind() == async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound => { next = through.saturating_add(1); break; }
                    Err(error) => return Err(ServerError::Nats(error.to_string())),
                };
                bytes += message.payload.len();
                next = message.sequence.saturating_add(1);
                entries.push(trellis_runtime_apis::types::JobsHistoryEntry { sequence:message.sequence.into(),payload:message.payload.to_vec().into(),extra:Default::default() });
            }
            owner.runtime_authorization_owner().assert_current(authority).map_err(|_|forbidden())?;
            Ok(trellis_runtime_apis::types::JobsHistoryResponse { entries,through_sequence:through.into(),next_sequence:(next<=through).then_some(next.into()),extra:Default::default() })
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
