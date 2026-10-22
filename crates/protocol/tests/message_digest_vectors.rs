//! Per-message proof digests match the vectors the TypeScript SDK's native
//! implementations are pinned to, so the two implementations sign and verify
//! identical bytes.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;
use trellis_protocol::transfer::{
    transfer_frame_digest, transfer_server_proof_digest, TransferFrameDescriptor,
};
use trellis_protocol::{
    live_server_proof_digest, session_event_signing_digest, session_request_signing_digest,
    session_response_signing_digest, SessionEvent, SessionRequest, SessionResponse,
};

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect()
}

fn field<T: serde::de::DeserializeOwned>(vector: &Value, name: &str) -> T {
    serde_json::from_value(vector[name].clone()).unwrap()
}

#[test]
fn per_message_digests_match_the_pinned_cross_language_vectors() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../integration/fixtures/protocol/message-digests/vectors.json");
    let vectors: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(!vectors.is_empty());
    for vector in vectors {
        let payload = bytes(vector["payloadHex"].as_str().unwrap());
        let text = |name: &str| vector[name].as_str().unwrap().to_owned();
        let actual = match vector["kind"].as_str().unwrap() {
            "transferFrame" => URL_SAFE_NO_PAD.encode(
                transfer_frame_digest(
                    &field::<TransferFrameDescriptor>(&vector, "descriptor"),
                    &payload,
                )
                .unwrap(),
            ),
            "transferServerProof" => URL_SAFE_NO_PAD.encode(
                transfer_server_proof_digest(
                    &text("contextDigest"),
                    &text("subject"),
                    &field::<TransferFrameDescriptor>(&vector, "descriptor"),
                    &payload,
                )
                .unwrap(),
            ),
            "liveServerProof" => {
                live_server_proof_digest(&text("contextDigest"), &text("subject"), &payload)
                    .unwrap()
            }
            "sessionRequest" => URL_SAFE_NO_PAD.encode(
                session_request_signing_digest(
                    &field::<SessionRequest>(&vector, "request"),
                    &payload,
                )
                .unwrap(),
            ),
            "sessionEvent" => URL_SAFE_NO_PAD.encode(
                session_event_signing_digest(&field::<SessionEvent>(&vector, "event"), &payload)
                    .unwrap(),
            ),
            "sessionResponse" => URL_SAFE_NO_PAD.encode(
                session_response_signing_digest(
                    &field::<SessionResponse>(&vector, "response"),
                    &payload,
                )
                .unwrap(),
            ),
            other => panic!("unknown vector kind {other}"),
        };
        assert_eq!(actual, text("digest"), "{}", vector["kind"]);
    }
}
