//! Portal auth primitives and headless completion of a local-password login flow.
//!
//! Mirrors the TypeScript `@oatscenter/trellis/auth/browser` portal surface so a Rust caller can
//! complete the same login the browser portal performs, without a browser: mint a per-flow
//! binding, submit the local password, then approve the pending consent when the flow asks.

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::TrellisAuthError;
use crate::client::{canonical_trellis_origin, decode_trellis_http_error};

/// Header carrying the portal binding secret on portal-scoped requests.
pub const PORTAL_BINDING_HEADER: &str = "trellis-portal-binding";

/// A portal's per-flow binding: the secret carried as a header and its digest submitted on login.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalBinding {
    /// Base64url secret carried by the `trellis-portal-binding` header.
    pub secret: String,
    /// Base64url SHA-256 of the raw secret bytes, submitted with the login request.
    pub digest: String,
}

/// Creates a fresh portal binding from 32 random bytes.
#[must_use]
pub fn create_portal_binding() -> PortalBinding {
    let bytes: [u8; 32] = rand::random();
    PortalBinding {
        secret: URL_SAFE_NO_PAD.encode(bytes),
        digest: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
    }
}

/// Reads the `flowId` query parameter out of a login URL.
///
/// # Errors
///
/// Returns an invalid-argument error when the URL is malformed or carries no `flowId`.
pub fn flow_id_from_url(url: &str) -> Result<String, TrellisAuthError> {
    let parsed = url::Url::parse(url)?;
    parsed
        .query_pairs()
        .find(|(key, _)| key == "flowId")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| {
            TrellisAuthError::InvalidArgument(format!("auth URL is missing flowId: {url}"))
        })
}

#[derive(Debug, Deserialize)]
struct FlowWire {
    state: String,
    #[serde(rename = "consentView", default)]
    consent_view: Option<Value>,
    #[serde(rename = "decisionDigest", default)]
    decision_digest: Option<String>,
}

fn http_client() -> Result<HttpClient, TrellisAuthError> {
    Ok(HttpClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

fn base_url(trellis_url: &str) -> Result<String, TrellisAuthError> {
    Ok(canonical_trellis_origin(trellis_url)?.trim_end_matches('/').to_owned())
}

async fn get_flow(base: &str, flow_id: &str) -> Result<FlowWire, TrellisAuthError> {
    let response = http_client()?
        .get(format!("{base}/auth/flow/{flow_id}"))
        .send()
        .await?;
    if !response.status().is_success() {
        let error = decode_trellis_http_error(response).await;
        return Err(TrellisAuthError::AuthRequestHttpFailure(
            error.status,
            error.code,
        ));
    }
    Ok(response.json::<FlowWire>().await?)
}

async fn post_portal_flow(
    base: &str,
    flow_id: &str,
    binding: &PortalBinding,
) -> Result<FlowWire, TrellisAuthError> {
    let response = http_client()?
        .post(format!("{base}/auth/flow/{flow_id}/portal"))
        .header("origin", base)
        .header(PORTAL_BINDING_HEADER, &binding.secret)
        .send()
        .await?;
    if !response.status().is_success() {
        let error = decode_trellis_http_error(response).await;
        return Err(TrellisAuthError::AuthRequestHttpFailure(
            error.status,
            error.code,
        ));
    }
    Ok(response.json::<FlowWire>().await?)
}

fn approval_body(consent: &Value, decision_digest: &str, approve: bool) -> Value {
    let eligible = |item: &Value| {
        item.get("required").and_then(Value::as_bool) == Some(true)
            && item.get("eligible").and_then(Value::as_bool) == Some(true)
    };
    let approved_capabilities = consent
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| eligible(item))
                .map(|item| {
                    json!({
                        "id": item.get("id"),
                        "consentDigest": item.get("consentDigest"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let approved_resources = consent
        .get("resources")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| eligible(item))
                .map(|item| {
                    json!({
                        "kind": item.get("kind"),
                        "name": item.get("name"),
                        "commitment": item.get("requestedCommitment"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "decision": if approve { "approve" } else { "reject" },
        "approval": if approve {
            json!({
                "mode": "capabilities",
                "installedRevision": consent.get("installedRevision"),
                "expectedGrantRevision": consent.get("expectedGrantRevision"),
                "decisionDigest": decision_digest,
                "approvedCapabilities": approved_capabilities,
                "approvedResources": approved_resources,
                "companionApproved": consent.get("companion").is_some_and(|value| !value.is_null()),
            })
        } else {
            Value::Null
        },
    })
}

/// Performs the local-password login step of a portal flow.
///
/// # Errors
///
/// Returns an auth-request HTTP failure when the login is rejected.
pub async fn perform_local_login(
    trellis_url: &str,
    flow_id: &str,
    username: &str,
    password: &str,
    binding: &PortalBinding,
) -> Result<(), TrellisAuthError> {
    let base = base_url(trellis_url)?;
    let response = http_client()?
        .post(format!("{base}/auth/login/local"))
        .header("origin", &base)
        .json(&json!({
            "flowId": flow_id,
            "username": username,
            "password": password,
            "portalBindingDigest": binding.digest,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        let error = decode_trellis_http_error(response).await;
        return Err(TrellisAuthError::AuthRequestHttpFailure(
            error.status,
            error.code,
        ));
    }
    Ok(())
}

/// Consent the flow is asking for, handed to a caller that must prepare policy before approving.
pub struct PortalConsentSummary {
    /// Participant the pending consent belongs to, when the flow names one.
    pub participant_id: Option<String>,
    /// Capability ids the flow is asking to grant.
    pub capabilities: Vec<String>,
}

/// Outcome of starting a local-password login.
pub enum LocalLoginStep {
    /// The flow is waiting for consent; finish it with [`approve_local_login`].
    ConsentRequired {
        /// Flow identifier.
        flow_id: String,
        /// Binding secret that must accompany the approval.
        binding: PortalBinding,
        /// What the flow is asking to grant.
        summary: PortalConsentSummary,
    },
    /// The flow already completed.
    Completed {
        /// Flow identifier.
        flow_id: String,
    },
}

fn consent_summary(consent: &Value) -> PortalConsentSummary {
    let participant_id = consent
        .get("participantId")
        .or_else(|| consent.get("contractId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let capabilities = consent
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    PortalConsentSummary {
        participant_id,
        capabilities,
    }
}

/// Starts a local-password login flow and reports whether it needs consent.
///
/// Callers that must configure policy before consenting use this instead of
/// [`complete_local_login`]: run [`approve_local_login`] once the policy is in place.
///
/// # Errors
///
/// Returns an auth-request HTTP failure when a step is rejected, and an auth-flow failure when
/// the flow reaches a terminal state that is neither approval nor redirect.
pub async fn begin_local_login(
    trellis_url: &str,
    login_url: &str,
    username: &str,
    password: &str,
) -> Result<LocalLoginStep, TrellisAuthError> {
    let base = base_url(trellis_url)?;
    let flow_id = flow_id_from_url(login_url)?;
    let binding = create_portal_binding();
    perform_local_login(&base, &flow_id, username, password, &binding).await?;

    let browser = get_flow(&base, &flow_id).await?;
    let portal = match browser.state.as_str() {
        "authenticated" | "approval_required" => {
            post_portal_flow(&base, &flow_id, &binding).await?
        }
        "approved" | "consumed" => return Ok(LocalLoginStep::Completed { flow_id }),
        other => {
            return Err(TrellisAuthError::AuthFlowFailed(format!(
                "local login did not reach approval; portal state is '{other}'"
            )));
        }
    };

    match portal.state.as_str() {
        "approved" | "consumed" => Ok(LocalLoginStep::Completed { flow_id }),
        "approval_required" => {
            let consent = portal.consent_view.clone().ok_or_else(|| {
                TrellisAuthError::AuthFlowFailed("flow omitted its consent view".to_owned())
            })?;
            let summary = consent_summary(&consent);
            Ok(LocalLoginStep::ConsentRequired {
                flow_id,
                binding,
                summary,
            })
        }
        other => Err(TrellisAuthError::AuthFlowFailed(format!(
            "local login did not reach approval; portal state is '{other}'"
        ))),
    }
}

/// Approves the consent a started flow is waiting on, completing the login.
///
/// # Errors
///
/// Returns an auth-request HTTP failure when the approval is rejected, and an auth-flow failure
/// when the flow does not reach a terminal state.
pub async fn approve_local_login(
    trellis_url: &str,
    flow_id: &str,
    binding: &PortalBinding,
) -> Result<String, TrellisAuthError> {
    let base = base_url(trellis_url)?;
    let portal = post_portal_flow(&base, flow_id, binding).await?;
    let consent = portal.consent_view.clone().ok_or_else(|| {
        TrellisAuthError::AuthFlowFailed("flow omitted its consent view".to_owned())
    })?;
    let decision_digest = consent
        .get("decisionDigest")
        .and_then(Value::as_str)
        .or(portal.decision_digest.as_deref())
        .ok_or_else(|| {
            TrellisAuthError::AuthFlowFailed("flow omitted its decision digest".to_owned())
        })?
        .to_owned();
    let body = approval_body(&consent, &decision_digest, true);
    let response = http_client()?
        .post(format!("{base}/auth/flow/{flow_id}/approval"))
        .header("content-type", "application/json")
        .header("origin", &base)
        .header(PORTAL_BINDING_HEADER, &binding.secret)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let error = decode_trellis_http_error(response).await;
        return Err(TrellisAuthError::AuthRequestHttpFailure(
            error.status,
            error.code,
        ));
    }
    let approved = response.json::<FlowWire>().await?;
    if approved.state == "approved" || approved.state == "consumed" {
        Ok(flow_id.to_owned())
    } else {
        Err(TrellisAuthError::AuthFlowFailed(format!(
            "auth approval did not complete; portal state is '{}'",
            approved.state
        )))
    }
}

/// Completes a local-password login flow: log in, then approve the pending consent when asked.
///
/// Returns the completed flow id. This is the Rust counterpart of the browser portal's
/// `completeLocalAuthFlow`; the flow ends bound in the same way whether a browser or this
/// helper approved it.
///
/// # Errors
///
/// Returns an auth-request HTTP failure when a step is rejected, and an auth-flow failure when
/// the flow reaches a terminal state that is neither approval nor redirect.
pub async fn complete_local_login(
    trellis_url: &str,
    login_url: &str,
    username: &str,
    password: &str,
) -> Result<String, TrellisAuthError> {
    match begin_local_login(trellis_url, login_url, username, password).await? {
        LocalLoginStep::Completed { flow_id } => Ok(flow_id),
        LocalLoginStep::ConsentRequired {
            flow_id, binding, ..
        } => approve_local_login(trellis_url, &flow_id, &binding).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_secret_hashes_to_its_digest() {
        let binding = create_portal_binding();
        let secret = URL_SAFE_NO_PAD
            .decode(&binding.secret)
            .expect("secret is base64url");
        assert_eq!(secret.len(), 32);
        assert_eq!(binding.digest, URL_SAFE_NO_PAD.encode(Sha256::digest(&secret)));
    }

    #[test]
    fn consent_summary_reads_the_participant_and_capability_ids() {
        let consent = serde_json::json!({
            "participantId": "trellis.cli",
            "capabilities": [
                { "id": "record", "required": true, "eligible": true },
                { "id": "configure", "required": false, "eligible": true }
            ]
        });
        let summary = consent_summary(&consent);
        assert_eq!(summary.participant_id.as_deref(), Some("trellis.cli"));
        assert_eq!(summary.capabilities, vec!["record", "configure"]);
    }

    #[test]
    fn consent_summary_falls_back_to_the_contract_id() {
        let consent = serde_json::json!({ "contractId": "tsd-operator.Operator" });
        let summary = consent_summary(&consent);
        assert_eq!(summary.participant_id.as_deref(), Some("tsd-operator.Operator"));
        assert!(summary.capabilities.is_empty());
    }

    #[test]
    fn flow_id_is_read_from_a_login_url() {
        assert_eq!(
            flow_id_from_url("http://localhost:3000/login?flowId=abc").expect("flow id"),
            "abc"
        );
        assert!(flow_id_from_url("http://localhost:3000/login").is_err());
    }

    #[test]
    fn approval_body_selects_only_required_eligible_capabilities() {
        let consent = json!({
            "installedRevision": 3,
            "expectedGrantRevision": 1,
            "decisionDigest": "digest",
            "capabilities": [
                {"id": "a", "consentDigest": "ca", "required": true, "eligible": true},
                {"id": "b", "consentDigest": "cb", "required": false, "eligible": true},
                {"id": "c", "consentDigest": "cc", "required": true, "eligible": false}
            ],
            "resources": [
                {"kind": "kv", "name": "n", "requestedCommitment": {"k": 1}, "required": true, "eligible": true}
            ],
            "companion": null
        });
        let body = approval_body(&consent, "digest", true);
        assert_eq!(body["decision"], "approve");
        assert_eq!(body["approval"]["installedRevision"], 3);
        assert_eq!(
            body["approval"]["approvedCapabilities"]
                .as_array()
                .expect("capabilities")
                .len(),
            1
        );
        assert_eq!(body["approval"]["approvedCapabilities"][0]["id"], "a");
        assert_eq!(
            body["approval"]["approvedResources"]
                .as_array()
                .expect("resources")
                .len(),
            1
        );
        assert_eq!(body["approval"]["companionApproved"], false);
    }
}
