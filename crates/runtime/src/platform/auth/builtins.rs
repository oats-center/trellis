use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest as _, Sha256};
use trellis_rs::generated::ParticipantDescriptor;

use super::{AuthorizationStateError, ParticipantBindingRecord};

pub(crate) const AUTH_RUNTIME_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_platform::PARTICIPANT_ID;
pub(crate) const CLI_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_cli::PARTICIPANT_ID;
pub(crate) const CONSOLE_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_console::PARTICIPANT_ID;
pub(crate) const PORTAL_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_portal::PARTICIPANT_ID;
pub(crate) const EVENTS_RUNTIME_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_events_runtime::PARTICIPANT_ID;
pub(crate) const HEALTH_RUNTIME_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_health_runtime::PARTICIPANT_ID;
pub(crate) const JOBS_RUNTIME_PARTICIPANT_ID: &str =
    trellis_runtime_apis::participants::trellis_jobs_runtime::PARTICIPANT_ID;

pub(crate) fn validate_binding_namespace(
    binding: &ParticipantBindingRecord,
    platform_trusted: bool,
) -> Result<(), AuthorizationStateError> {
    let trusted_package = platform_trusted || is_trusted_package_digest(&binding.package_digest);
    if binding.participant_id.starts_with("trellis.") {
        match binding.participant_id.as_str() {
            AUTH_RUNTIME_PARTICIPANT_ID
            | CLI_PARTICIPANT_ID
            | CONSOLE_PARTICIPANT_ID
            | PORTAL_PARTICIPANT_ID
            | EVENTS_RUNTIME_PARTICIPANT_ID
            | HEALTH_RUNTIME_PARTICIPANT_ID
            | JOBS_RUNTIME_PARTICIPANT_ID => {}
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(format!(
                    "participant id '{}' uses the reserved 'trellis.' namespace",
                    binding.participant_id
                )))
            }
        }
        if !trusted_package {
            return Err(AuthorizationStateError::InvalidRecord(format!(
                "participant '{}' does not match the trusted Trellis package",
                binding.participant_id
            )));
        }
    }

    for (api_id, api) in &binding.projection.referenced_apis {
        if api_id.starts_with("trellis.")
            && !trusted_package
            && !is_platform_api(api_id, &api.digest)
        {
            return Err(AuthorizationStateError::InvalidRecord(format!(
                "API '{api_id}' uses the reserved 'trellis.' namespace"
            )));
        }
    }
    Ok(())
}

pub(crate) fn is_platform_api(api_id: &str, api_digest: &str) -> bool {
    [
        (
            trellis_runtime_apis::apis::trellis_auth_v1::API_ID,
            trellis_runtime_apis::apis::trellis_auth_v1::API_DIGEST,
        ),
        (
            trellis_runtime_apis::apis::trellis_core_v1::API_ID,
            trellis_runtime_apis::apis::trellis_core_v1::API_DIGEST,
        ),
        (
            trellis_runtime_apis::apis::trellis_events_v1::API_ID,
            trellis_runtime_apis::apis::trellis_events_v1::API_DIGEST,
        ),
        (
            trellis_runtime_apis::apis::trellis_health_v1::API_ID,
            trellis_runtime_apis::apis::trellis_health_v1::API_DIGEST,
        ),
        (
            trellis_runtime_apis::apis::trellis_jobs_v1::API_ID,
            trellis_runtime_apis::apis::trellis_jobs_v1::API_DIGEST,
        ),
        (
            trellis_runtime_apis::apis::trellis_state_v1::API_ID,
            trellis_runtime_apis::apis::trellis_state_v1::API_DIGEST,
        ),
    ]
    .contains(&(api_id, api_digest))
}

pub(crate) fn cli_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<trellis_runtime_apis::participants::trellis_cli::Participant>(
        trellis_runtime_apis::participants::trellis_cli::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn auth_runtime_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<trellis_runtime_apis::participants::trellis_platform::Participant>(
        trellis_runtime_apis::participants::trellis_platform::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn events_runtime_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<
        trellis_runtime_apis::participants::trellis_events_runtime::Participant,
    >(
        trellis_runtime_apis::participants::trellis_events_runtime::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn health_runtime_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<
        trellis_runtime_apis::participants::trellis_health_runtime::Participant,
    >(
        trellis_runtime_apis::participants::trellis_health_runtime::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn jobs_runtime_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<
        trellis_runtime_apis::participants::trellis_jobs_runtime::Participant,
    >(
        trellis_runtime_apis::participants::trellis_jobs_runtime::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn console_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<trellis_runtime_apis::participants::trellis_console::Participant>(
        trellis_runtime_apis::participants::trellis_console::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn portal_participant_binding(
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    builtin_participant_binding::<trellis_runtime_apis::participants::trellis_portal::Participant>(
        trellis_runtime_apis::participants::trellis_portal::PARTICIPANT_DIGEST,
        resolved_at,
    )
}

pub(crate) fn trusted_package_evidence_json(
    package_digest: &str,
) -> Result<Option<String>, AuthorizationStateError> {
    let generated = trellis_runtime_apis::participants::trellis_platform::PACKAGE_EVIDENCE;
    if generated.root_digest() != package_digest {
        return Ok(None);
    }
    let value = serde_json::to_value(generated)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    trellis_protocol::canonicalize_json(&value)
        .map(Some)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))
}

pub(crate) fn is_trusted_package_evidence(package_digest: &str, evidence_json: &str) -> bool {
    trusted_package_evidence_json(package_digest)
        .ok()
        .flatten()
        .is_some_and(|trusted| trusted == evidence_json)
}

fn is_trusted_package_digest(package_digest: &str) -> bool {
    package_digest == super::builtin_semantics::PACKAGE_DIGEST
}

fn builtin_participant_binding<D: ParticipantDescriptor>(
    expected_digest: &str,
    resolved_at: i64,
) -> Result<ParticipantBindingRecord, AuthorizationStateError> {
    let native = super::builtin_semantics::participant(D::ID).ok_or_else(|| {
        AuthorizationStateError::InvalidRecord(format!(
            "generated participant '{}' has no native semantics",
            D::ID
        ))
    })?;
    let expected_kind = match D::KIND {
        trellis_rs::generated::ParticipantKind::Service => {
            trellis_protocol::ParticipantKind::Service
        }
        trellis_rs::generated::ParticipantKind::Device => trellis_protocol::ParticipantKind::Device,
        trellis_rs::generated::ParticipantKind::App => trellis_protocol::ParticipantKind::App,
        trellis_rs::generated::ParticipantKind::Agent => trellis_protocol::ParticipantKind::Agent,
    };
    if native.participant_digest != expected_digest
        || native.projection.participant_id != D::ID
        || native.projection.participant_kind != expected_kind
    {
        return Err(AuthorizationStateError::ParticipantDigestMismatch);
    }
    Ok(ParticipantBindingRecord {
        participant_id: D::ID.to_owned(),
        participant_kind: native.projection.participant_kind,
        participant_digest: native.participant_digest.to_owned(),
        needs_digest: native.needs_digest.to_owned(),
        package_digest: native.package_digest.to_owned(),
        evidence_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(
            trusted_package_evidence_json(native.package_digest)?
                .ok_or(AuthorizationStateError::ParticipantMissing)?
                .as_bytes(),
        )),
        participant_path: D::PATH.to_owned(),
        projection: native.projection,
        resolved_at,
        state: super::ParticipantBindingState::Resolved,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use trellis_rs::generated::ParticipantDescriptor;

    fn assert_matches_source(
        generated: trellis_rs::generated::PackageEvidence,
        participant_id: &str,
        native: super::ParticipantBindingRecord,
    ) {
        let evidence = super::super::evidence::PackageEvidenceInput::from_generated_descriptor(
            generated,
            participant_id,
        )
        .expect("generated evidence");
        let (source, _) = super::ParticipantBindingRecord::from_package_evidence(&evidence, 0)
            .expect("source semantics");
        assert_eq!(native.participant_digest, source.participant_digest);
        assert_eq!(native.projection, source.projection);
    }

    #[test]
    fn generated_builtins_use_exact_trusted_package_evidence() {
        for binding in [
            super::cli_participant_binding(0).expect("CLI binding"),
            super::auth_runtime_participant_binding(0).expect("platform binding"),
            super::console_participant_binding(0).expect("Console binding"),
            super::portal_participant_binding(0).expect("Portal binding"),
        ] {
            super::validate_binding_namespace(&binding, true).expect("trusted binding");
            assert!(
                super::trusted_package_evidence_json(&binding.package_digest)
                    .expect("trusted evidence")
                    .is_some()
            );
        }

        assert_matches_source(
            trellis_runtime_apis::participants::trellis_cli::Participant::package_evidence(),
            trellis_runtime_apis::participants::trellis_cli::Participant::PATH,
            super::cli_participant_binding(0).expect("CLI binding"),
        );
        assert_matches_source(
            trellis_runtime_apis::participants::trellis_platform::Participant::package_evidence(),
            trellis_runtime_apis::participants::trellis_platform::Participant::PATH,
            super::auth_runtime_participant_binding(0).expect("platform binding"),
        );
        assert_matches_source(
            trellis_runtime_apis::participants::trellis_console::Participant::package_evidence(),
            trellis_runtime_apis::participants::trellis_console::Participant::PATH,
            super::console_participant_binding(0).expect("Console binding"),
        );
        assert_matches_source(
            trellis_runtime_apis::participants::trellis_portal::Participant::package_evidence(),
            trellis_runtime_apis::participants::trellis_portal::Participant::PATH,
            super::portal_participant_binding(0).expect("Portal binding"),
        );
    }

    #[test]
    fn reserved_namespace_rejects_tampered_participant_and_api_digests() {
        let mut participant = super::auth_runtime_participant_binding(0).expect("binding");
        participant.package_digest = "x".repeat(43);
        assert!(super::validate_binding_namespace(&participant, false).is_err());

        let mut api = super::auth_runtime_participant_binding(0).expect("binding");
        api.package_digest = "x".repeat(43);
        api.projection
            .referenced_apis
            .get_mut(trellis_runtime_apis::apis::trellis_auth_v1::API_ID)
            .expect("Auth API")
            .digest = "x".repeat(43);
        assert!(super::validate_binding_namespace(&api, false).is_err());
    }
}
