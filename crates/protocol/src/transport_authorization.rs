//! Pure signed transport-authorization policy protocol.
//!
//! [`TransportAuthorizationV1`] is the exact allow-only NATS transport policy the
//! runtime compiles from installed participant evidence and binds into a signed
//! authorization context. Both SDKs use the same inclusion and classification
//! implementation exposed here; the runtime remains the only owner of compiling
//! policy from server-owned bindings.
//!
//! Admission policy `A` is compared with currently allowed policy `D` for the
//! same account. `A <= D` means every subject `A` permits is also permitted by
//! `D`, `A`'s response allowance is absent or covered with at least the same
//! count and duration, and `A`'s hard deadline is no later than `D`'s (`null` is
//! infinity). A policy that is mutually inclusive with the current one is
//! `current`; one that is strictly narrower is a passive `upgrade_available`;
//! anything else requires reduction of the physical attachment.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    authorization::{authorization_error, is_utf16_strictly_sorted, validate_text},
    digest_json, AuthorizationErrorCode, ProtocolError,
};

/// Signed transport-authorization wire format.
pub const TRANSPORT_AUTHORIZATION_FORMAT_V1: &str = "trellis.transport-authorization.v1";

const MAXIMUM_SAFE_JSON_INTEGER: i64 = 9_007_199_254_740_991;
const WITNESS_TOKEN_BASE: &str = "trellis-other-witness";

/// Bounded response allowance installed for services and API-implementing devices.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransportResponseAuthorizationV1 {
    /// Maximum outstanding responses.
    pub max_messages: u64,
    /// Response expiry in milliseconds.
    pub ttl_ms: u64,
}

/// Exact allow-only NATS transport policy admitted for one physical attachment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransportAuthorizationV1 {
    /// Wire format.
    pub format: String,
    /// Target NATS account public id.
    pub account: String,
    /// Sorted, deduplicated publish subject patterns.
    pub publish_allow: Vec<String>,
    /// Sorted, deduplicated subscribe subject patterns.
    pub subscribe_allow: Vec<String>,
    /// Optional bounded response allowance.
    pub response: Option<TransportResponseAuthorizationV1>,
    /// Exclusive Unix-seconds hard deadline of the underlying authorization.
    pub hard_expires_at: Option<i64>,
}

/// Result of comparing admitted policy `A` with currently allowed policy `D`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPolicyClass {
    /// `A` and `D` are mutually inclusive.
    Current,
    /// `D` safely covers `A` and offers additional transport capability.
    UpgradeAvailable,
    /// `D` no longer covers some capability in `A`.
    ReductionRequired,
}

impl TransportAuthorizationV1 {
    /// Validate the format, account, subject grammar, numeric bounds, and expiry.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Authorization`] when any field is malformed or a
    /// subject list is not canonically sorted and unique.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.format != TRANSPORT_AUTHORIZATION_FORMAT_V1 {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["format"],
                "unsupported transport-authorization format",
            ));
        }
        validate_text(&self.account, &["account"])?;
        validate_subject_pattern_set(&self.publish_allow, "publishAllow")?;
        validate_subject_pattern_set(&self.subscribe_allow, "subscribeAllow")?;
        if let Some(response) = &self.response {
            if response.max_messages == 0 {
                return Err(authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    ["response", "maxMessages"],
                    "response message count must be positive",
                ));
            }
            if response.max_messages > MAXIMUM_SAFE_JSON_INTEGER as u64 {
                return Err(authorization_error(
                    AuthorizationErrorCode::UnsafeJsonInteger,
                    ["response", "maxMessages"],
                    "integer must be within the interoperable JSON safe-integer range",
                ));
            }
            if response.ttl_ms == 0 {
                return Err(authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    ["response", "ttlMs"],
                    "response expiry must be positive",
                ));
            }
            if response.ttl_ms > MAXIMUM_SAFE_JSON_INTEGER as u64 {
                return Err(authorization_error(
                    AuthorizationErrorCode::UnsafeJsonInteger,
                    ["response", "ttlMs"],
                    "integer must be within the interoperable JSON safe-integer range",
                ));
            }
        }
        if let Some(deadline) = self.hard_expires_at {
            if !(1..=MAXIMUM_SAFE_JSON_INTEGER).contains(&deadline) {
                return Err(authorization_error(
                    AuthorizationErrorCode::UnsafeJsonInteger,
                    ["hardExpiresAt"],
                    "hard deadline must be a positive interoperable JSON integer",
                ));
            }
        }
        Ok(())
    }

    /// Return the canonical digest of the policy value.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the policy is invalid or cannot be
    /// canonicalized.
    pub fn digest(&self) -> Result<String, ProtocolError> {
        self.validate()?;
        digest_json(&serde_json::to_value(self)?)
    }

    /// Return whether this admitted policy `A` is included in `allowed` policy `D`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when either policy is invalid.
    pub fn is_covered_by(&self, allowed: &Self) -> Result<bool, ProtocolError> {
        self.validate()?;
        allowed.validate()?;
        if self.account != allowed.account {
            return Ok(false);
        }
        if !subject_union_covered_by(&self.publish_allow, &allowed.publish_allow)? {
            return Ok(false);
        }
        if !subject_union_covered_by(&self.subscribe_allow, &allowed.subscribe_allow)? {
            return Ok(false);
        }
        if !response_covered(self.response.as_ref(), allowed.response.as_ref()) {
            return Ok(false);
        }
        if !deadline_covered(self.hard_expires_at, allowed.hard_expires_at) {
            return Ok(false);
        }
        Ok(true)
    }

    /// Classify admitted policy `A` against currently allowed policy `D`.
    ///
    /// A hard deadline that has already elapsed is a required reduction because
    /// the attachment can no longer outlive authority actually granted.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when either policy is invalid.
    pub fn classify(
        &self,
        allowed: &Self,
        now_unix_seconds: i64,
    ) -> Result<TransportPolicyClass, ProtocolError> {
        let identical = self == allowed;
        if identical {
            self.validate()?;
        }
        if let Some(deadline) = self.hard_expires_at {
            if deadline <= now_unix_seconds {
                return Ok(TransportPolicyClass::ReductionRequired);
            }
        }
        if identical {
            return Ok(TransportPolicyClass::Current);
        }
        if !self.is_covered_by(allowed)? {
            return Ok(TransportPolicyClass::ReductionRequired);
        }
        if allowed.is_covered_by(self)? {
            Ok(TransportPolicyClass::Current)
        } else {
            Ok(TransportPolicyClass::UpgradeAvailable)
        }
    }
}

/// Return whether the union of `destination` subject patterns covers every
/// subject permitted by the `source` union.
///
/// # Errors
///
/// Returns [`ProtocolError`] when any pattern is malformed.
pub fn subject_union_covered_by(
    source: &[String],
    destination: &[String],
) -> Result<bool, ProtocolError> {
    let destination_patterns = destination
        .iter()
        .map(|pattern| parse_pattern(pattern))
        .collect::<Result<Vec<_>, _>>()
        .map_err(pattern_error)?;
    let literals = source
        .iter()
        .chain(destination.iter())
        .flat_map(|pattern| pattern.split('.'))
        .filter(|token| *token != "*" && *token != ">")
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let witness = unique_witness_token(&literals);
    let maximum_length = source
        .iter()
        .chain(destination.iter())
        .map(|pattern| pattern.split('.').count())
        .max()
        .unwrap_or(0);
    for pattern in source {
        let tokens = parse_pattern(pattern).map_err(pattern_error)?;
        if !pattern_covered_by_union(&tokens, &destination_patterns, &witness, maximum_length) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Literal(String),
    Star,
    Greater,
}

fn pattern_error(reason: &'static str) -> ProtocolError {
    authorization_error(
        AuthorizationErrorCode::InvalidFormat,
        ["subjectPattern"],
        reason,
    )
}

fn validate_subject_pattern_set(
    patterns: &[String],
    field: &'static str,
) -> Result<(), ProtocolError> {
    for (index, pattern) in patterns.iter().enumerate() {
        if let Err(reason) = parse_pattern(pattern) {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                [field, index.to_string().as_str()],
                reason,
            ));
        }
    }
    if !is_utf16_strictly_sorted(patterns) {
        return Err(authorization_error(
            AuthorizationErrorCode::NonCanonicalSet,
            [field],
            "subject patterns must be sorted and unique",
        ));
    }
    Ok(())
}

/// Parse the restricted NATS grammar the compiler emits: literal tokens, `*`
/// matching exactly one token, and terminal `>` matching one or more tokens.
fn parse_pattern(pattern: &str) -> Result<Vec<Token>, &'static str> {
    if pattern.is_empty() {
        return Err("subject pattern must be nonempty");
    }
    let raw = pattern.split('.').collect::<Vec<_>>();
    let mut tokens = Vec::with_capacity(raw.len());
    for (index, token) in raw.iter().enumerate() {
        if token.is_empty() {
            return Err("subject pattern contains an empty token");
        }
        if *token == ">" {
            if index + 1 != raw.len() {
                return Err("terminal wildcard must be the last subject token");
            }
            tokens.push(Token::Greater);
        } else if *token == "*" {
            tokens.push(Token::Star);
        } else {
            if token.contains('*')
                || token.contains('>')
                || token
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_ascii_control())
            {
                return Err("subject token contains a wildcard or unsafe character");
            }
            tokens.push(Token::Literal((*token).to_owned()));
        }
    }
    Ok(tokens)
}

fn unique_witness_token(literals: &BTreeSet<String>) -> String {
    let mut witness = WITNESS_TOKEN_BASE.to_owned();
    while literals.contains(&witness) {
        witness.push('_');
    }
    witness
}

fn pattern_covered_by_union(
    source: &[Token],
    destination: &[Vec<Token>],
    witness: &str,
    maximum_length: usize,
) -> bool {
    let Some(Token::Greater) = source.last() else {
        let candidate = witness_for(source, witness, source.len());
        return destination
            .iter()
            .any(|pattern| pattern_matches(pattern, &candidate));
    };
    let prefix_length = source.len() - 1;
    for length in (prefix_length + 1)..=(maximum_length + 1) {
        let candidate = witness_for(source, witness, length);
        if !destination
            .iter()
            .any(|pattern| pattern_matches(pattern, &candidate))
        {
            return false;
        }
    }
    true
}

fn witness_for(tokens: &[Token], witness: &str, length: usize) -> Vec<String> {
    (0..length)
        .map(|index| match tokens.get(index) {
            Some(Token::Literal(literal)) => literal.clone(),
            Some(Token::Star | Token::Greater) | None => witness.to_owned(),
        })
        .collect()
}

fn pattern_matches(pattern: &[Token], witness: &[String]) -> bool {
    let mut position = 0;
    for token in pattern {
        match token {
            Token::Literal(literal) => {
                if witness.get(position) != Some(literal) {
                    return false;
                }
                position += 1;
            }
            Token::Star => {
                if position >= witness.len() {
                    return false;
                }
                position += 1;
            }
            Token::Greater => return position < witness.len(),
        }
    }
    position == witness.len()
}

fn response_covered(
    admitted: Option<&TransportResponseAuthorizationV1>,
    allowed: Option<&TransportResponseAuthorizationV1>,
) -> bool {
    match admitted {
        None => true,
        Some(admitted) => allowed.is_some_and(|allowed| {
            allowed.max_messages >= admitted.max_messages && allowed.ttl_ms >= admitted.ttl_ms
        }),
    }
}

fn deadline_covered(admitted: Option<i64>, allowed: Option<i64>) -> bool {
    admitted.unwrap_or(i64::MAX) <= allowed.unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(patterns: &[&str]) -> Vec<String> {
        patterns
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect()
    }

    fn policy(publish: &[&str], subscribe: &[&str]) -> TransportAuthorizationV1 {
        TransportAuthorizationV1 {
            format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
            account: "AACCOUNT".to_owned(),
            publish_allow: v(publish),
            subscribe_allow: v(subscribe),
            response: None,
            hard_expires_at: None,
        }
    }

    #[test]
    fn exact_subject_inclusion_requires_every_literal() {
        assert!(subject_union_covered_by(&v(&["rpc.v1.Foo"]), &v(&["rpc.v1.Foo"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["rpc.v1.Foo"]), &v(&["rpc.v1.Bar"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["rpc.v1.Foo"]), &v(&["rpc.v1.Foo.Bar"])).unwrap());
    }

    #[test]
    fn terminal_wildcard_covers_one_or_more_tokens() {
        assert!(subject_union_covered_by(&v(&["a.b"]), &v(&["a.>"])).unwrap());
        assert!(subject_union_covered_by(&v(&["a.b.c"]), &v(&["a.>"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["a.>"]), &v(&["a.b"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["a.>"]), &v(&["a.b", "a.c"])).unwrap());
    }

    #[test]
    fn single_token_wildcard_does_not_cross_token_boundaries() {
        assert!(subject_union_covered_by(&v(&["a.b"]), &v(&["a.*"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["a.b.c"]), &v(&["a.*"])).unwrap());
        assert!(subject_union_covered_by(&v(&["a.b"]), &v(&["*.*"])).unwrap());
    }

    #[test]
    fn equivalent_union_covers_terminal_wildcard() {
        assert!(subject_union_covered_by(&v(&["a.>"]), &v(&["a.*", "a.*.>"])).unwrap());
        assert!(!subject_union_covered_by(&v(&["a.>"]), &v(&["a.*"])).unwrap());
    }

    #[test]
    fn pure_addition_is_a_passive_upgrade() {
        let admitted = policy(&["a"], &[]);
        let allowed = policy(&["a", "b"], &[]);
        assert_eq!(
            admitted.classify(&allowed, 1_000).unwrap(),
            TransportPolicyClass::UpgradeAvailable
        );
    }

    #[test]
    fn mixed_change_requires_reduction() {
        let admitted = policy(&["a", "b"], &[]);
        let allowed = policy(&["a", "c"], &[]);
        assert_eq!(
            admitted.classify(&allowed, 1_000).unwrap(),
            TransportPolicyClass::ReductionRequired
        );
    }

    #[test]
    fn publish_and_subscribe_changes_are_independent() {
        let admitted = policy(&["a"], &["x"]);
        assert_eq!(
            admitted
                .classify(&policy(&["a", "b"], &["x"]), 1_000)
                .unwrap(),
            TransportPolicyClass::UpgradeAvailable
        );
        assert_eq!(
            admitted.classify(&policy(&["a"], &["y"]), 1_000).unwrap(),
            TransportPolicyClass::ReductionRequired
        );
    }

    #[test]
    fn response_authority_must_be_covered_at_least_as_wide() {
        let mut admitted = policy(&["a"], &[]);
        admitted.response = Some(TransportResponseAuthorizationV1 {
            max_messages: 10,
            ttl_ms: 100,
        });
        let mut narrower = policy(&["a"], &[]);
        narrower.response = Some(TransportResponseAuthorizationV1 {
            max_messages: 5,
            ttl_ms: 100,
        });
        let mut equal = policy(&["a"], &[]);
        equal.response = Some(TransportResponseAuthorizationV1 {
            max_messages: 10,
            ttl_ms: 100,
        });
        assert!(!admitted.is_covered_by(&narrower).unwrap());
        assert!(admitted.is_covered_by(&equal).unwrap());
        assert!(policy(&["a"], &[]).is_covered_by(&equal).unwrap());
        assert!(!equal.is_covered_by(&policy(&["a"], &[])).unwrap());
    }

    #[test]
    fn hard_deadlines_compare_with_null_as_infinity() {
        let mut admitted = policy(&["a"], &[]);
        admitted.hard_expires_at = Some(100);
        let mut allowed = policy(&["a"], &[]);
        allowed.hard_expires_at = Some(200);
        assert_eq!(
            admitted.classify(&allowed, 10).unwrap(),
            TransportPolicyClass::UpgradeAvailable
        );
        let mut unbounded = policy(&["a"], &[]);
        unbounded.hard_expires_at = None;
        assert!(!unbounded.is_covered_by(&admitted).unwrap());
        assert!(admitted.is_covered_by(&unbounded).unwrap());
    }

    #[test]
    fn elapsed_hard_deadline_requires_reduction() {
        let mut admitted = policy(&["a"], &[]);
        admitted.hard_expires_at = Some(50);
        assert_eq!(
            admitted.classify(&admitted, 49).unwrap(),
            TransportPolicyClass::Current
        );
        assert_eq!(
            admitted.classify(&admitted, 50).unwrap(),
            TransportPolicyClass::ReductionRequired
        );
        let mut allowed = policy(&["a"], &[]);
        allowed.hard_expires_at = Some(500);
        assert_eq!(
            admitted.classify(&allowed, 100).unwrap(),
            TransportPolicyClass::ReductionRequired
        );
    }

    #[test]
    fn a_changed_account_requires_a_new_attachment() {
        let admitted = policy(&["a"], &[]);
        let mut allowed = policy(&["a"], &[]);
        allowed.account = "BOTHER".to_owned();
        assert_eq!(
            admitted.classify(&allowed, 1_000).unwrap(),
            TransportPolicyClass::ReductionRequired
        );
    }

    #[test]
    fn malformed_patterns_are_rejected() {
        for pattern in ["", "a..b", ".a", "a.", "a.>.b", "a*b", "a.>x", "a b"] {
            assert!(
                parse_pattern(pattern).is_err(),
                "{pattern:?} unexpectedly parsed"
            );
        }
        let mut policy = policy(&["b", "a"], &[]);
        assert!(policy.classify(&policy, 0).is_err());
        policy.publish_allow = v(&["a"]);
        assert_eq!(
            policy.classify(&policy, 0).unwrap(),
            TransportPolicyClass::Current
        );
        policy.publish_allow = v(&["a", "a"]);
        assert!(policy.classify(&policy, 0).is_err());
    }

    #[test]
    fn digest_is_stable_and_sensitive_to_policy_changes() {
        let first = policy(&["a"], &["x"]);
        let second = policy(&["a"], &["x"]);
        assert_eq!(first.digest().unwrap(), second.digest().unwrap());
        assert_ne!(
            first.digest().unwrap(),
            policy(&["a", "b"], &["x"]).digest().unwrap()
        );
    }

    #[test]
    fn shared_wire_vectors_classify_and_digest_identically() {
        #[derive(Deserialize)]
        struct Vectors {
            cases: Vec<Case>,
            digests: Vec<DigestCase>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Case {
            name: String,
            admitted: TransportAuthorizationV1,
            allowed: TransportAuthorizationV1,
            now: i64,
            class: TransportPolicyClass,
            #[serde(default)]
            reverse_class: Option<TransportPolicyClass>,
        }
        #[derive(Deserialize)]
        struct DigestCase {
            name: String,
            policy: TransportAuthorizationV1,
            digest: String,
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../integration/fixtures/protocol/transport-authorization/vectors.json");
        let raw = std::fs::read_to_string(&path).expect("read shared transport vectors");
        let vectors: Vectors = serde_json::from_str(&raw).expect("decode shared transport vectors");
        assert!(
            !vectors.cases.is_empty(),
            "shared vectors must cover behavior"
        );
        for case in &vectors.cases {
            assert_eq!(
                case.admitted
                    .classify(&case.allowed, case.now)
                    .expect("classify admitted policy"),
                case.class,
                "{}",
                case.name
            );
            if let Some(reverse) = case.reverse_class {
                assert_eq!(
                    case.allowed
                        .classify(&case.admitted, case.now)
                        .expect("classify reverse policy"),
                    reverse,
                    "{} (reverse)",
                    case.name
                );
            }
        }
        assert!(!vectors.digests.is_empty());
        for case in &vectors.digests {
            assert_eq!(
                case.policy.digest().expect("digest policy"),
                case.digest,
                "{}",
                case.name
            );
        }
    }
}
