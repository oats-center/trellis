//! Store-backed NATS admission. Application authority is never a CONNECT token.

use std::{fs, path::Path, sync::Arc, time::Duration};

use async_nats::HeaderMap;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use futures_util::StreamExt as _;
use nats_jwt_rs::{
    authorization::{AuthRequest, AuthResponse},
    types::{Permission, Permissions, ResponsePermission},
    user::User,
    Claims,
};
use nkeys::{KeyPair, KeyPairType, XKey};
use serde::{Deserialize, Serialize};
use trellis_protocol::TransportAuthorizationV1;

use super::auth::{AuthError, SqliteAuthorizationStore};
use crate::{shutdown::StopHandle, supervisor::RuntimeError};

const AUTH_CALLOUT_SUBJECT: &str = "$SYS.REQ.USER.AUTH";
const DISCONNECT_SUBJECT: &str = "$SYS.ACCOUNT.*.DISCONNECT";
const MAX_CONCURRENT_REQUESTS: usize = 32;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);

// TLS client certificates are optional NATS telemetry, not Trellis admission
// evidence. The upstream ClientTLS model incorrectly requires them. Consume
// only that field here while Claims::decode still verifies the original JWT.
#[derive(Clone, Deserialize, Serialize)]
struct CalloutRequest {
    #[serde(default, rename = "client_tls", skip_serializing)]
    _client_tls: Option<serde::de::IgnoredAny>,
    #[serde(flatten)]
    request: AuthRequest,
}

impl nats_jwt_rs::Claim for CalloutRequest {
    fn validate() {
        <AuthRequest as nats_jwt_rs::Claim>::validate();
    }
}

#[derive(Deserialize)]
struct DisconnectEvent {
    server: Option<DisconnectedServer>,
    client: Option<DisconnectedClient>,
}
#[derive(Deserialize)]
struct DisconnectedServer {
    id: Option<String>,
}
#[derive(Deserialize)]
struct DisconnectedClient {
    id: Option<u64>,
    #[serde(alias = "user", alias = "userNkey")]
    user_nkey: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CalloutKeys {
    auth_signing_key: Arc<KeyPair>,
    target_signing_key: Arc<KeyPair>,
    xkey: XKey,
    auth_account: String,
    target_account: String,
    sentinel_public_key: String,
}

impl CalloutKeys {
    pub(crate) fn from_files(
        auth_signing_seed_file: &Path,
        target_signing_seed_file: &Path,
        xkey_seed_file: &Path,
        sentinel_public_key_file: &Path,
        auth_user_creds_file: &Path,
        target_user_creds_file: &Path,
    ) -> Result<Self, AuthError> {
        let auth_signing_key = account_key(auth_signing_seed_file)?;
        let target_signing_key = account_key(target_signing_seed_file)?;
        let auth_account = issuer_account(&user_claims(auth_user_creds_file)?);
        let target_account = issuer_account(&user_claims(target_user_creds_file)?);
        let sentinel_public_key = read_secret(sentinel_public_key_file)?;
        if !is_nkey(&sentinel_public_key, KeyPairType::User) {
            return Err(AuthError::Denied);
        }
        let xkey = XKey::from_seed(&read_secret(xkey_seed_file)?).map_err(|_| AuthError::Denied)?;
        Ok(Self {
            auth_signing_key: Arc::new(auth_signing_key),
            target_signing_key: Arc::new(target_signing_key),
            xkey,
            auth_account,
            target_account,
            sentinel_public_key,
        })
    }

    pub(crate) fn target_account(&self) -> &str {
        &self.target_account
    }

    fn validate_sentinel(&self, jwt: &str) -> Result<(), AuthError> {
        let claims = Claims::<User>::decode(jwt).map_err(|_| AuthError::Denied)?;
        let permissions = &claims.payload().permissions.permissions;
        let issued_by_account =
            claims.iss == self.auth_account && claims.payload().issuer_account.is_none();
        let issued_by_signer = claims.iss == self.auth_signing_key.public_key()
            && claims.payload().issuer_account.as_deref() == Some(self.auth_account.as_str());
        if !(issued_by_account || issued_by_signer)
            || claims.sub != self.sentinel_public_key
            || claims.exp.is_some()
            || claims.payload().permissions.bearer_token != Some(true)
            || !permissions.publish.allow.is_empty()
            || permissions.publish.deny != [">"]
            || !permissions.subscribe.allow.is_empty()
            || permissions.subscribe.deny != [">"]
            || permissions.resp.is_some()
        {
            return Err(AuthError::Denied);
        }
        Ok(())
    }

    fn authorized_user_jwt(
        &self,
        user_nkey: &str,
        policy: &TransportAuthorizationV1,
        attachment: &str,
    ) -> Result<String, AuthError> {
        policy.validate()?;
        if policy.account != self.target_account {
            return Err(AuthError::Denied);
        }
        let mut claims =
            User::new_claims(format!("trellis.auth.v1:{attachment}"), user_nkey.into());
        claims.aud = Some(self.target_account.clone());
        claims.exp = policy.hard_expires_at;
        let payload = claims.payload_mut();
        payload.issuer_account = Some(self.target_account.clone());
        payload.permissions.permissions = Permissions {
            publish: Permission {
                allow: policy.publish_allow.clone(),
                deny: Vec::new(),
            },
            subscribe: Permission {
                allow: policy.subscribe_allow.clone(),
                deny: Vec::new(),
            },
            resp: policy
                .response
                .as_ref()
                .map(|response| {
                    Ok::<_, AuthError>(ResponsePermission {
                        max_messages: i64::try_from(response.max_messages)
                            .map_err(|_| AuthError::Denied)?,
                        ttl: Duration::from_millis(response.ttl_ms),
                    })
                })
                .transpose()?,
        };
        claims
            .encode(&self.target_signing_key)
            .map_err(|_| AuthError::Unavailable)
    }

    fn response(
        &self,
        request: &AuthRequest,
        result: Result<String, &str>,
    ) -> Result<Vec<u8>, AuthError> {
        let mut claims = AuthResponse::generic_claim(request.user_nkey.clone());
        claims.aud = Some(request.server.id.clone());
        let response = claims.payload_mut();
        response.issuer_account = Some(self.auth_account.clone());
        match result {
            Ok(jwt) => response.jwt = jwt,
            Err(code) => response.error = code.into(),
        }
        Ok(claims
            .encode(&self.auth_signing_key)
            .map_err(|_| AuthError::Unavailable)?
            .into_bytes())
    }
}

pub(crate) struct AuthCallout {
    subscriber: async_nats::Subscriber,
    disconnects: async_nats::Subscriber,
    client: async_nats::Client,
    store: Arc<SqliteAuthorizationStore>,
    keys: CalloutKeys,
}

impl AuthCallout {
    pub(crate) async fn start(
        client: async_nats::Client,
        system: async_nats::Client,
        store: Arc<SqliteAuthorizationStore>,
        keys: CalloutKeys,
    ) -> Result<Self, AuthError> {
        let subscriber = client
            .queue_subscribe(AUTH_CALLOUT_SUBJECT, "trellis".into())
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let disconnects = system
            .subscribe(DISCONNECT_SUBJECT)
            .await
            .map_err(|_| AuthError::Unavailable)?;
        client.flush().await.map_err(|_| AuthError::Unavailable)?;
        system.flush().await.map_err(|_| AuthError::Unavailable)?;
        Ok(Self {
            subscriber,
            disconnects,
            client,
            store,
            keys,
        })
    }

    pub(crate) async fn run(mut self, stop: StopHandle) -> Result<(), RuntimeError> {
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                ()=stop.stopped()=>break,
                completed=requests.join_next(),if !requests.is_empty()=> {
                    if let Some(result) = completed {
                        if let Err(error) =
                            result.map_err(|error| RuntimeError::Platform(error.to_string()))?
                        {
                            tracing::warn!(%error,"callout request rejected or response unavailable");
                        }
                    }
                }
                message=self.subscriber.next(),if requests.len()<MAX_CONCURRENT_REQUESTS=> {
                    let Some(message) = message else {
                        return Err(RuntimeError::Platform(
                            "authorization callout subscription closed".into(),
                        ));
                    };
                    let client = self.client.clone();
                    let store = self.store.clone();
                    let keys = self.keys.clone();
                    requests.spawn(async move { process(client, store, keys, message).await });
                }
                message=self.disconnects.next()=> {
                    let Some(message) = message else {
                        return Err(RuntimeError::Platform(
                            "authorization disconnect subscription closed".into(),
                        ));
                    };
                    let Ok(event) = serde_json::from_slice::<DisconnectEvent>(&message.payload)
                    else {
                        continue;
                    };
                    if let (
                        Some(DisconnectedServer { id: Some(server) }),
                        Some(DisconnectedClient {
                            id: Some(cid),
                            user_nkey: Some(key),
                        }),
                    ) = (event.server, event.client)
                    {
                        if message.subject.as_str()
                            != format!("$SYS.ACCOUNT.{}.DISCONNECT", self.keys.target_account)
                        {
                            continue;
                        }
                        let store = self.store.clone();
                        let now = crate::clock::unix_now();
                        tokio::task::spawn_blocking(move || {
                            store.disconnect_attachment(&server, cid, &key, now)
                        })
                        .await
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?
                        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
                    }
                }
            }
        }
        self.subscriber
            .unsubscribe()
            .await
            .map_err(|_| RuntimeError::Platform("callout unsubscribe failed".into()))?;
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(completed) = requests.join_next().await {
                completed
                    .map_err(|error| RuntimeError::Platform(error.to_string()))?
                    .map_err(|error| RuntimeError::Platform(error.to_string()))?;
            }
            Ok::<(), RuntimeError>(())
        })
        .await;
        match drained {
            Ok(result) => result,
            Err(_) => {
                requests.abort_all();
                Ok(())
            }
        }
    }
}

async fn process(
    client: async_nats::Client,
    store: Arc<SqliteAuthorizationStore>,
    keys: CalloutKeys,
    message: async_nats::Message,
) -> Result<(), AuthError> {
    let Some(reply) = message.reply else {
        return Ok(());
    };
    let xkey = server_xkey(message.headers.as_ref())?;
    let decrypted = keys
        .xkey
        .open(&message.payload, &xkey)
        .map_err(|_| AuthError::Denied)?;
    let claims = Claims::<CalloutRequest>::decode(
        std::str::from_utf8(&decrypted).map_err(|_| AuthError::Denied)?,
    )
    .map_err(|_| AuthError::Denied)?;
    let request = &claims.payload().request;
    let now = crate::clock::unix_now();
    if claims.iss != request.server.id
        || claims.sub != keys.auth_account
        || claims.aud.as_deref() != Some("nats-authorization-request")
        || claims.exp.is_none_or(|expiry| expiry <= now)
        || claims.nbf.is_some_and(|time| time > now)
        || claims.iat > now.saturating_add(i64::from(store.settings.clock_skew_seconds)) as u64
        || request.server.xkey.as_deref() != Some(xkey.public_key().as_str())
        || request.client_info.id == 0
        || request.client_info.host.is_empty()
        || request.client_info.nonce.is_empty()
        || !is_nkey(&request.server.id, KeyPairType::Server)
        || !is_nkey(&request.user_nkey, KeyPairType::User)
    {
        return Err(AuthError::Denied);
    }
    let request = request.clone();
    let signing = keys.clone();
    let admission = tokio::task::spawn_blocking(move || {
        let connect = &request.connect_opts;
        let ephemeral = connect.nkey.as_deref().ok_or(AuthError::Denied)?;
        signing.validate_sentinel(connect.jwt.as_deref().ok_or(AuthError::Denied)?)?;
        verify_nats_nonce_signature(
            ephemeral,
            &request.client_info.nonce,
            connect.sig.as_deref().ok_or(AuthError::Denied)?,
        )?;
        let token = connect.auth_token.as_deref().ok_or(AuthError::Denied)?;
        let (attachment, policy) = store.admit_connection(
            token.as_bytes(),
            ephemeral,
            &request.user_nkey,
            &request.server.id,
            request.client_info.id,
            crate::clock::unix_now(),
        )?;
        signing.authorized_user_jwt(&request.user_nkey, &policy, &attachment.attachment_id)
    });
    let result = match tokio::time::timeout(ADMISSION_TIMEOUT, admission).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => Err(AuthError::Unavailable),
    };
    let response = match result {
        Ok(jwt) => keys.response(&claims.payload().request, Ok(jwt))?,
        Err(error) => {
            tracing::debug!(%error,"NATS admission denied");
            keys.response(&claims.payload().request, Err(denial_code(&error)))?
        }
    };
    let encrypted = keys
        .xkey
        .seal(&response, &xkey)
        .map_err(|_| AuthError::Unavailable)?;
    client
        .publish(reply, encrypted.into())
        .await
        .map_err(|_| AuthError::Unavailable)
}

fn denial_code(error: &AuthError) -> &'static str {
    match error {
        AuthError::Denied
        | AuthError::NotFound
        | AuthError::Retired
        | AuthError::RequiredMissing
        | AuthError::Protocol(_)
        | AuthError::Json(_) => "not_authorized",
        _ => "unavailable",
    }
}
fn server_xkey(headers: Option<&HeaderMap>) -> Result<XKey, AuthError> {
    let value = headers
        .and_then(|headers| headers.get("Nats-Server-Xkey"))
        .ok_or(AuthError::Denied)?;
    XKey::from_public_key(value.as_ref()).map_err(|_| AuthError::Denied)
}
fn verify_nats_nonce_signature(nkey: &str, nonce: &str, signature: &str) -> Result<(), AuthError> {
    let key = KeyPair::from_public_key(nkey).map_err(|_| AuthError::Denied)?;
    if key.key_pair_type() != KeyPairType::User {
        return Err(AuthError::Denied);
    }
    let signature = STANDARD
        .decode(signature)
        .or_else(|_| URL_SAFE_NO_PAD.decode(signature))
        .map_err(|_| AuthError::Denied)?;
    key.verify(nonce.as_bytes(), &signature)
        .map_err(|_| AuthError::Denied)
}
fn is_nkey(value: &str, kind: KeyPairType) -> bool {
    KeyPair::from_public_key(value).is_ok_and(|key| key.key_pair_type() == kind)
}
fn account_key(path: &Path) -> Result<KeyPair, AuthError> {
    let key = KeyPair::from_seed(&read_secret(path)?).map_err(|_| AuthError::Denied)?;
    if key.key_pair_type() != KeyPairType::Account {
        return Err(AuthError::Denied);
    }
    Ok(key)
}
fn read_secret(path: &Path) -> Result<String, AuthError> {
    fs::read_to_string(path)
        .map(|value| value.trim().into())
        .map_err(|_| AuthError::Unavailable)
}
fn user_claims(path: &Path) -> Result<Claims<User>, AuthError> {
    let contents = read_secret(path)?;
    let jwt = contents
        .lines()
        .skip_while(|line| *line != "-----BEGIN NATS USER JWT-----")
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .ok_or(AuthError::Denied)?;
    Claims::<User>::decode(jwt).map_err(|_| AuthError::Denied)
}
fn issuer_account(claims: &Claims<User>) -> String {
    claims
        .payload()
        .issuer_account
        .clone()
        .unwrap_or_else(|| claims.iss.clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    use trellis_protocol::TRANSPORT_AUTHORIZATION_FORMAT_V1;

    #[test]
    fn decodes_go_tls_callouts_without_client_certificates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Official github.com/nats-io/jwt/v2 v2.8.2 signed this request with
        // ClientTLS{Version: "1.3", Cipher: "TLS_AES_128_GCM_SHA256"} and no
        // mutual-TLS certificates. No private key or usable credential is retained.
        let token = include_str!("fixtures/nats_tls_callout.jwt").trim();
        let claims = Claims::<CalloutRequest>::decode(token)?;
        assert_eq!(
            claims.payload().request.connect_opts.auth_token.as_deref(),
            Some("fixture-not-a-credential")
        );

        let parts: Vec<_> = token.split('.').collect();
        let mut payload: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1])?)?;
        payload["nats"]["client_tls"]["cipher"] = "modified TLS telemetry".into();
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?),
            parts[2]
        );
        assert!(Claims::<CalloutRequest>::decode(&tampered).is_err());
        Ok(())
    }

    #[test]
    fn nonce_and_account_signing_boundaries() -> Result<(), Box<dyn std::error::Error>> {
        let keys = CalloutKeys {
            auth_signing_key: Arc::new(KeyPair::new_account()),
            target_signing_key: Arc::new(KeyPair::new_account()),
            xkey: XKey::new(),
            auth_account: KeyPair::new_account().public_key(),
            target_account: KeyPair::new_account().public_key(),
            sentinel_public_key: KeyPair::new_user().public_key(),
        };
        let session = KeyPair::new_user();
        let nonce = "actual-server-nonce";
        let signature = URL_SAFE_NO_PAD.encode(session.sign(nonce.as_bytes())?);
        verify_nats_nonce_signature(&session.public_key(), nonce, &signature)?;
        assert!(verify_nats_nonce_signature(
            &session.public_key(),
            "another-server-nonce",
            &signature
        )
        .is_err());
        assert!(
            verify_nats_nonce_signature(&KeyPair::new_user().public_key(), nonce, &signature)
                .is_err()
        );
        let user = KeyPair::new_user().public_key();
        let policy = TransportAuthorizationV1 {
            format: TRANSPORT_AUTHORIZATION_FORMAT_V1.into(),
            account: keys.target_account.clone(),
            publish_allow: vec!["rpc.v1.example".into()],
            subscribe_allow: vec!["_INBOX.example.>".into()],
            response: None,
            hard_expires_at: Some(200),
        };
        let jwt = keys.authorized_user_jwt(&user, &policy, "01ARZ3NDEKTSV4RRFFQ69G5FAV")?;
        let decoded = Claims::<User>::decode(&jwt)?;
        assert_eq!(decoded.iss, keys.target_signing_key.public_key());
        assert_eq!(
            decoded.payload().issuer_account.as_deref(),
            Some(keys.target_account.as_str())
        );
        assert_eq!(
            decoded.payload().permissions.permissions.publish.allow,
            policy.publish_allow
        );
        let mut request = AuthRequest {
            user_nkey: user,
            ..Default::default()
        };
        request.server.id = KeyPair::new_server().public_key();
        let response = keys.response(&request, Ok(jwt))?;
        let response = std::str::from_utf8(&response)?;
        let (signed, signature) = response.rsplit_once('.').unwrap();
        KeyPair::from_public_key(&keys.auth_signing_key.public_key())?
            .verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(signature)?)?;
        let (_, payload) = signed.split_once('.').unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        assert_eq!(decoded["iss"], keys.auth_signing_key.public_key());
        assert_eq!(decoded["aud"], request.server.id);
        assert_eq!(decoded["nats"]["issuer_account"], keys.auth_account);
        Ok(())
    }
}
