use std::{sync::Arc, time::SystemTime};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::jwt::{EcdsaP256Key, JwsSigner, JwsVerifiers, P256Verifier};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::TokenBinding;

const TOKEN_URI: &str = "https://auth.example/token";

async fn proof(key: &EcdsaP256Key, payload: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "ES256", "typ": "dpop+jwt", "jwk": key.public_jwk()
        }))
        .unwrap(),
    );
    let input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(payload));
    let signature = key.sign(input.as_bytes()).await.unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_bytes()))
}

#[tokio::test]
async fn durable_proof_authenticates_only_its_signed_runtime_and_user_nkey() {
    let key = EcdsaP256Key::generate("durable-client");
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime = URL_SAFE_NO_PAD.encode([7; 16]);
    let now = SystemTime::now();
    let (_, bytes) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let thumbprint = URL_SAFE_NO_PAD.encode(Sha256::digest(
        serde_json::to_vec(&json!({
            "crv": "Ed25519", "kty": "OKP", "x": URL_SAFE_NO_PAD.encode(bytes)
        }))
        .unwrap(),
    ));
    let claims = json!({
        "htm": "POST", "htu": TOKEN_URI,
        "iat": now.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(),
        "jti": ulid::Ulid::new().to_string(),
        "nonce": URL_SAFE_NO_PAD.encode([9; 32]), "ath": "ignored-at-token-endpoint",
        "trellis_runtime_id": runtime, "trellis_nats_jkt": thumbprint
    });
    let mut verifiers = JwsVerifiers::new();
    verifiers.install(Arc::new(P256Verifier));
    let signed = proof(&key, &claims.to_string()).await;
    let binding = TokenBinding::verify(
        &verifiers,
        &signed,
        TOKEN_URI,
        &runtime,
        &ephemeral.public_key(),
        now,
    )
    .unwrap();
    assert_eq!(binding.ephemeral_public_key, URL_SAFE_NO_PAD.encode(bytes));
    assert_eq!(binding.ephemeral_nkey, ephemeral.public_key());
    assert_eq!(binding.runtime_id, runtime);
    assert_eq!(binding.persistent_dpop_jkt, key.public_jwk().thumbprint());
    assert_eq!(binding.nonce.as_deref(), claims["nonce"].as_str());
    for (uri, nonce, nkey) in [
        (
            "https://attacker.example/token",
            runtime.clone(),
            ephemeral.public_key(),
        ),
        (
            TOKEN_URI,
            URL_SAFE_NO_PAD.encode([8; 16]),
            ephemeral.public_key(),
        ),
        (
            TOKEN_URI,
            runtime.clone(),
            nkeys::KeyPair::new_user().public_key(),
        ),
        (
            TOKEN_URI,
            runtime.clone(),
            nkeys::KeyPair::new_account().public_key(),
        ),
    ] {
        assert!(TokenBinding::verify(&verifiers, &signed, uri, &nonce, &nkey, now).is_err());
    }
    // Re-signing is not the attack: changing signed content without its durable
    // private key must fail at the library's signature boundary.
    let parts: Vec<_> = signed.split('.').collect();
    for (claim, replacement) in [
        ("trellis_runtime_id", URL_SAFE_NO_PAD.encode([8; 16])),
        ("nonce", URL_SAFE_NO_PAD.encode([10; 32])),
    ] {
        let mut tampered = claims.clone();
        tampered[claim] = json!(replacement);
        let forged = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(tampered.to_string()),
            parts[2]
        );
        assert!(
            TokenBinding::verify(
                &verifiers,
                &forged,
                TOKEN_URI,
                &runtime,
                &ephemeral.public_key(),
                now
            )
            .is_err(),
            "accepted unsigned change to {claim}"
        );
    }
    // Even a genuinely signed proof cannot introduce ambiguous security claims.
    for claim in [
        "htm",
        "htu",
        "iat",
        "jti",
        "nonce",
        "ath",
        "trellis_runtime_id",
        "trellis_nats_jkt",
    ] {
        let duplicate =
            claims
                .to_string()
                .replacen("{", &format!("{{\"{claim}\":{},", claims[claim]), 1);
        let duplicate = proof(&key, &duplicate).await;
        assert!(
            TokenBinding::verify(
                &verifiers,
                &duplicate,
                TOKEN_URI,
                &runtime,
                &ephemeral.public_key(),
                now
            )
            .is_err(),
            "accepted duplicate {claim}"
        );
    }
}
