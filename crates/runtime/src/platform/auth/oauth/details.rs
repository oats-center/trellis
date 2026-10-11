//! Trellis RAR semantics layered over the engine's RFC 9396 validation.

use std::collections::BTreeSet;

use oauth_as::error::{ErrorCode, ErrorResponse};
use serde::Deserialize;
use trellis_protocol::PlatformPrivilege;

use super::super::CapabilitySelection;

pub(crate) struct RequestedAuthority {
    pub(crate) capabilities: CapabilitySelection,
    pub(crate) required_privileges: Vec<PlatformPrivilege>,
    pub(crate) optional_privileges: Vec<PlatformPrivilege>,
    pub(crate) unavailable_privileges: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Detail {
    #[serde(rename = "trellis_capabilities")]
    Capabilities {
        required: Vec<String>,
        #[serde(default)]
        optional: Vec<String>,
    },
    #[serde(rename = "trellis_platform_privileges")]
    Privileges {
        required: Vec<String>,
        #[serde(default)]
        optional: Vec<String>,
    },
}

impl RequestedAuthority {
    pub(crate) fn parse(raw: &str) -> Result<Self, ErrorResponse> {
        let invalid = || ErrorResponse::new(ErrorCode::InvalidAuthorizationDetails);
        if raw.len() > 16 * 1024 {
            return Err(invalid());
        }
        let details = oauth_as::rar::AuthorizationDetails::parse(raw)?;
        details.require_supported_types(Some(&[
            "trellis_capabilities".into(),
            "trellis_platform_privileges".into(),
        ]))?;
        let details: Vec<Detail> = serde_json::from_str(raw).map_err(|_| invalid())?;
        let mut capabilities = None;
        let mut privileges = None;
        for detail in details {
            match detail {
                Detail::Capabilities { required, optional } => {
                    if capabilities.is_some() || required.len().saturating_add(optional.len()) > 128
                    {
                        return Err(invalid());
                    }
                    for name in required.iter().chain(&optional) {
                        let (api, capability) = name.split_once("::").ok_or_else(invalid)?;
                        if name.len() > 1024
                            || trellis_protocol::validate_api_id(api).is_err()
                            || capability.is_empty()
                            || !capability.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
                            })
                        {
                            return Err(invalid());
                        }
                    }
                    let required: BTreeSet<_> = required.into_iter().collect();
                    let optional: BTreeSet<_> = optional
                        .into_iter()
                        .filter(|name| !required.contains(name))
                        .collect();
                    capabilities = Some(CapabilitySelection {
                        required: required.into_iter().collect(),
                        optional: optional.into_iter().collect(),
                    });
                }
                Detail::Privileges { required, optional } => {
                    if privileges.is_some() || required.len().saturating_add(optional.len()) > 32 {
                        return Err(invalid());
                    }
                    privileges = Some((required, optional));
                }
            }
        }
        let capabilities = capabilities.ok_or_else(invalid)?;
        let (required, optional) = privileges.unwrap_or_default();
        let required: BTreeSet<_> = required.into_iter().collect();
        let optional: BTreeSet<_> = optional
            .into_iter()
            .filter(|name| !required.contains(name))
            .collect();
        let required_privileges = required
            .into_iter()
            .map(|name| {
                serde_json::from_value(serde_json::Value::String(name)).map_err(|_| invalid())
            })
            .collect::<Result<_, _>>()?;
        let mut optional_privileges = Vec::new();
        let mut unavailable_privileges = Vec::new();
        for name in optional {
            match serde_json::from_value(serde_json::Value::String(name.clone())) {
                Ok(privilege) => optional_privileges.push(privilege),
                Err(_) => unavailable_privileges.push(name),
            }
        }
        Ok(Self {
            capabilities,
            required_privileges,
            optional_privileges,
            unavailable_privileges,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_details_normalize_selection_and_fail_closed_on_ambiguous_requests() {
        let request = RequestedAuthority::parse(r#"[{"type":"trellis_capabilities","required":["acme.documents@v1::read","acme.documents@v1::read"],"optional":["acme.documents@v1::read","acme.documents@v1::export","acme.documents@v1::export"]},{"type":"trellis_platform_privileges","required":["roles.manage"],"optional":["roles.manage","future.privilege"]}]"#).unwrap();
        assert_eq!(request.capabilities.required, ["acme.documents@v1::read"]);
        assert_eq!(request.capabilities.optional, ["acme.documents@v1::export"]);
        assert_eq!(
            request.required_privileges,
            [PlatformPrivilege::RolesManage]
        );
        assert!(request.optional_privileges.is_empty());
        assert_eq!(request.unavailable_privileges, ["future.privilege"]);
        for raw in [
            r#"[]"#,
            r#"[{"type":"future_type","required":[]}]"#,
            r#"[{"type":"trellis_capabilities","required":[],"required":["acme.documents@v1::write"]}]"#,
            r#"[{"type":"trellis_capabilities","required":[]},{"type":"trellis_capabilities","required":[]}]"#,
            r#"[{"type":"trellis_capabilities","required":["not-qualified"]}]"#,
            r#"[{"type":"trellis_capabilities","required":[]},{"type":"trellis_platform_privileges","required":["future.privilege"]}]"#,
        ] {
            assert!(RequestedAuthority::parse(raw).is_err(), "{raw}");
        }
    }
}
