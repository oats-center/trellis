use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::client::proof::base64url_encode;
use crate::client::{verify_event_proof, VerifyEventProofInput};
use crate::client::{SessionAuth, TrellisClientError};
use trellis_protocol::{
    build_authorization_event_proof_input, build_authorization_request_proof_input,
    AuthorizationEventProof, AuthorizationRequestProof, AuthorizationRequestProofInput,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationChainFixture {
    session_seed: String,
    session_public_key: String,
    context_digest: String,
    request_proof_input_hex: String,
    request_proof_digest: String,
    request_proof: String,
    event_proof_input_hex: String,
    event_proof_digest: String,
    event_proof: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationVectorDefaults {
    request: RequestProofFixture,
    event: EventProofFixture,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestProofFixture {
    subject: String,
    reply: String,
    payload: String,
    iat: i64,
    request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventProofFixture {
    descriptor_identity: String,
    subject: String,
    payload: String,
    event_id: String,
    event_time: String,
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", byte).unwrap();
    }
    out
}

fn chain_fixture() -> AuthorizationChainFixture {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/authorization-context/vectors.json");
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture_path).unwrap()).unwrap();
    let complete: AuthorizationChainFixture =
        serde_json::from_value(value["completeChain"].clone()).unwrap();
    complete
}

fn vector_defaults() -> AuthorizationVectorDefaults {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/authorization-context/vectors.json");
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture_path).unwrap()).unwrap();
    serde_json::from_value(value["defaults"].clone()).unwrap()
}

#[test]
fn request_proof_matches_language_neutral_conformance_vector() {
    let chain = chain_fixture();
    let defaults = vector_defaults();
    let auth = SessionAuth::from_seed_base64url(&chain.session_seed).unwrap();
    assert_eq!(auth.session_key, chain.session_public_key);

    let payload = defaults.request.payload.as_bytes();
    let proof: AuthorizationRequestProof = auth
        .create_request_proof(
            &chain.context_digest,
            &defaults.request.subject,
            &defaults.request.reply,
            payload,
            defaults.request.iat,
            &defaults.request.request_id,
        )
        .unwrap();
    assert_eq!(proof.as_str(), chain.request_proof);

    let context_digest = crate::client::proof::base64url_decode(&chain.context_digest).unwrap();
    let context_digest: [u8; 32] = context_digest.try_into().unwrap();
    let input = build_authorization_request_proof_input(
        &context_digest,
        &defaults.request.subject,
        Some(&defaults.request.reply),
        payload,
        defaults.request.iat,
        &defaults.request.request_id,
    )
    .unwrap();
    assert_eq!(
        bytes_to_hex(input.as_bytes()),
        chain.request_proof_input_hex
    );
    assert_eq!(base64url_encode(input.digest()), chain.request_proof_digest);
    assert!(verify_request_proof(&auth.session_key, &input, proof.as_str(),).unwrap());
    // A different reply subject breaks verification: the proof is bound to the
    // exact inbox the response arrives on.
    let altered_input = build_authorization_request_proof_input(
        &context_digest,
        &defaults.request.subject,
        Some("_INBOX.other.reply"),
        payload,
        defaults.request.iat,
        &defaults.request.request_id,
    )
    .unwrap();
    assert!(!verify_request_proof(&auth.session_key, &altered_input, proof.as_str(),).unwrap());
}

fn verify_request_proof(
    public_session_key: &str,
    input: &AuthorizationRequestProofInput,
    proof: &str,
) -> Result<bool, TrellisClientError> {
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    let public_key = VerifyingKey::from_bytes(
        &crate::client::proof::base64url_decode(public_session_key)?
            .try_into()
            .map_err(|_| {
                TrellisClientError::Bootstrap("session public key must encode 32 bytes".into())
            })?,
    )
    .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    let signature = Signature::from_bytes(
        &crate::client::proof::base64url_decode(proof)?
            .try_into()
            .map_err(|_| TrellisClientError::Bootstrap("proof must encode 64 bytes".into()))?,
    );
    Ok(public_key.verify(input.digest(), &signature).is_ok())
}

#[test]
fn event_proof_matches_language_neutral_conformance_vector() {
    let chain = chain_fixture();
    let defaults = vector_defaults();
    let auth = SessionAuth::from_seed_base64url(&chain.session_seed).unwrap();
    assert_eq!(auth.session_key, chain.session_public_key);

    let payload = defaults.event.payload.as_bytes();
    let proof: AuthorizationEventProof = auth
        .create_event_proof(
            &chain.context_digest,
            &defaults.event.descriptor_identity,
            &defaults.event.subject,
            payload,
            &defaults.event.event_id,
            &defaults.event.event_time,
        )
        .unwrap();
    assert_eq!(proof.as_str(), chain.event_proof);

    let context_digest = crate::client::proof::base64url_decode(&chain.context_digest).unwrap();
    let context_digest: [u8; 32] = context_digest.try_into().unwrap();
    let input = build_authorization_event_proof_input(
        &context_digest,
        &defaults.event.descriptor_identity,
        &defaults.event.subject,
        payload,
        &defaults.event.event_id,
        &defaults.event.event_time,
    )
    .unwrap();
    assert_eq!(bytes_to_hex(input.as_bytes()), chain.event_proof_input_hex);
    assert_eq!(base64url_encode(input.digest()), chain.event_proof_digest);
    assert!(verify_event_proof(VerifyEventProofInput {
        public_session_key: &auth.session_key,
        context_digest: &chain.context_digest,
        descriptor_identity: &defaults.event.descriptor_identity,
        subject: &defaults.event.subject,
        payload,
        event_id: &defaults.event.event_id,
        event_time: &defaults.event.event_time,
        proof_base64url: proof.as_str(),
    })
    .expect("event proof verifies"));
    assert!(!verify_event_proof(VerifyEventProofInput {
        public_session_key: &auth.session_key,
        context_digest: &chain.context_digest,
        descriptor_identity: &defaults.event.descriptor_identity,
        subject: &defaults.event.subject,
        payload,
        event_id: "evt_other",
        event_time: &defaults.event.event_time,
        proof_base64url: proof.as_str(),
    })
    .expect("changed event id rejects"));
}
