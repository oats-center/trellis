//! Generate pinned cross-language live proof vectors.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use serde_json::json;
use trellis_protocol::{sign_live_server_proof, LiveServerProof};

fn main() {
    let key = SigningKey::from_bytes(&[42u8; 32]);
    let context_digest = URL_SAFE_NO_PAD.encode([9u8; 32]);
    let cases = [
        (
            "data",
            "live.v1.data.cHJvdmlkZXI.Y29uc3VtZXI.AAAAAAAAAAAAAAAAAAAAAA",
            br#"{"format":"trellis.live.v1","type":"data","sessionId":"AAAAAAAAAAAAAAAAAAAAAA","seq":"1","value":{"x":1}}"#.as_slice(),
        ),
        (
            "challenge",
            "live.v1.data.cHJvdmlkZXI.Y29uc3VtZXI.AAAAAAAAAAAAAAAAAAAAAA",
            br#"{"format":"trellis.live.v1","type":"challenge","sessionId":"AAAAAAAAAAAAAAAAAAAAAA","challengeId":"AAAAAAAAAAAAAAAAAAAAAA","lastSentSeq":"1"}"#.as_slice(),
        ),
        ("empty-body", "rpc.v1.X.Y", b"".as_slice()),
    ];
    let mut vectors = Vec::new();
    for (name, subject, body) in cases {
        let proof = sign_live_server_proof(&context_digest, subject, body, &key).unwrap();
        let parsed = LiveServerProof::parse(proof.as_str()).unwrap();
        assert_eq!(parsed.as_str(), proof.as_str());
        vectors.push(json!({
            "name": name,
            "keySeedHex": "2a".repeat(32),
            "contextDigest": context_digest,
            "subject": subject,
            "bodyUtf8": String::from_utf8_lossy(body),
            "proof": proof.as_str(),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&vectors).expect("serializable vectors")
    );
}
