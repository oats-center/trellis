//! Local presentation data, independent of login identity and authorization.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_runtime_apis::AuthUserProfileUpdateRequest;

use super::AuthError;

/// Only validated standard presentation claims are retained per linked identity.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifiedProfile {
    pub(super) name: Option<String>,
    pub(super) given_name: Option<String>,
    pub(super) family_name: Option<String>,
    pub(super) email: Option<String>,
    pub(super) email_verified: Option<bool>,
    pub(super) picture: Option<String>,
}

pub(super) fn image_url(value: &str) -> bool {
    value.len() <= 2048
        && url::Url::parse(value).is_ok_and(|url| super::remote_http::public_url(&url))
}

fn email(value: &str) -> bool {
    value.len() <= 320
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control)
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
}

impl VerifiedProfile {
    pub(super) fn from_claims(claims: &serde_json::Value) -> Self {
        let text = |key: &str, max: usize| {
            claims
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| {
                    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
                })
                .map(str::to_owned)
        };
        let address = text("email", 320).filter(|value| email(value));
        Self {
            name: text("name", 256),
            given_name: text("given_name", 256),
            family_name: text("family_name", 256),
            email_verified: address.as_ref().and_then(|_| {
                claims
                    .get("email_verified")
                    .and_then(|value| value.as_bool())
            }),
            email: address,
            picture: text("picture", 2048).filter(|value| image_url(value)),
        }
    }

    /// Fill missing fields only. Verification is always tied to the same address.
    pub(super) fn fill(&mut self, fallback: Self) {
        if self.name.is_none() {
            self.name = fallback.name;
        }
        if self.given_name.is_none() {
            self.given_name = fallback.given_name;
        }
        if self.family_name.is_none() {
            self.family_name = fallback.family_name;
        }
        if self.picture.is_none() {
            self.picture = fallback.picture;
        }
        if self.email.is_none() {
            self.email = fallback.email;
            self.email_verified = fallback.email_verified;
        } else if self.email == fallback.email && self.email_verified.is_none() {
            self.email_verified = fallback.email_verified;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Profile {
    pub(super) display_name: String,
    pub(super) given_name: Option<String>,
    pub(super) family_name: Option<String>,
    display_source: String,
    given_source: String,
    family_source: String,
    pub(super) email: Option<String>,
    pub(super) email_verified: bool,
    email_source: String,
    email_provider: Option<String>,
    pub(super) picture: Option<String>,
    pub(super) avatar_override: Option<String>,
    pub(super) gravatar_enabled: bool,
    pub(super) revision: u64,
}

pub(super) fn read(connection: &Connection, principal: &str) -> Result<Profile, AuthError> {
    Ok(connection
        .query_row(
            "SELECT coalesce(display_name,principal_id),given_name,family_name,\
            display_name_source,given_name_source,family_name_source,email,\
            email_verified,email_source,email_provider_id,oidc_picture_url,\
            avatar_override_url,gravatar_enabled,version \
            FROM auth_user_profiles \
            WHERE principal_id=?1",
            [principal],
            |row| {
                Ok(Profile {
                    display_name: row.get(0)?,
                    given_name: row.get(1)?,
                    family_name: row.get(2)?,
                    display_source: row.get(3)?,
                    given_source: row.get(4)?,
                    family_source: row.get(5)?,
                    email: row.get(6)?,
                    email_verified: row.get(7)?,
                    email_source: row.get(8)?,
                    email_provider: row.get(9)?,
                    picture: row.get(10)?,
                    avatar_override: row.get(11)?,
                    gravatar_enabled: row.get(12)?,
                    revision: row.get(13)?,
                })
            },
        )
        .optional()?
        .unwrap_or(Profile {
            display_name: principal.to_owned(),
            given_name: None,
            family_name: None,
            display_source: "default".into(),
            given_source: "unset".into(),
            family_source: "unset".into(),
            email: None,
            email_verified: false,
            email_source: "unset".into(),
            email_provider: None,
            picture: None,
            avatar_override: None,
            gravatar_enabled: true,
            revision: 1,
        }))
}

/// Recompute nonmanual fields from verified, still-linked identities and apply
/// explicit owner/admin choices. The caller owns the aggregate transaction.
pub(super) fn write(
    connection: &Connection,
    principal: &str,
    patch: Option<&AuthUserProfileUpdateRequest>,
    now: i64,
) -> Result<Profile, AuthError> {
    let mut current = read(connection, principal)?;
    let before = current.clone();
    if let Some(patch) = patch {
        if current.revision != patch.expected_revision.0 .0 {
            return Err(AuthError::Conflict);
        }
        if (patch.email.is_some() && patch.clear_email == Some(true))
            || (patch.avatar_override_url.is_some() && patch.clear_avatar_override == Some(true))
        {
            return Err(AuthError::Invalid("contradictory profile changes".into()));
        }
        if let Some(value) = &patch.display_name {
            let value = value.as_str().trim();
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(AuthError::Invalid("invalid profile name".into()));
            }
            current.display_name = value.to_owned();
            current.display_source = "manual".into();
        }
        for (value, target, source) in [
            (
                patch.given_name.as_ref(),
                &mut current.given_name,
                &mut current.given_source,
            ),
            (
                patch.family_name.as_ref(),
                &mut current.family_name,
                &mut current.family_source,
            ),
        ] {
            if let Some(value) = value {
                let value = value.as_str().trim();
                if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                    return Err(AuthError::Invalid("invalid profile name".into()));
                }
                *target = Some(value.to_owned());
                *source = "manual".into();
            }
        }
        if let Some(value) = &patch.email {
            if !email(value.trim()) {
                return Err(AuthError::Invalid("invalid profile email".into()));
            }
            current.email = Some(value.trim().to_owned());
            current.email_verified = false;
            current.email_source = "manual".into();
            current.email_provider = None;
        }
        if patch.clear_email == Some(true) {
            current.email = None;
            current.email_source = "unset".into();
        }
        if let Some(value) = &patch.avatar_override_url {
            if !image_url(value) {
                return Err(AuthError::Invalid("invalid avatar URL".into()));
            }
            current.avatar_override = Some(value.clone());
        }
        if patch.clear_avatar_override == Some(true) {
            current.avatar_override = None;
        }
        if let Some(enabled) = patch.gravatar_enabled {
            current.gravatar_enabled = enabled;
        }
    }
    let mut profiles = connection.prepare(
        "SELECT i.provider,i.profile_json \
        FROM auth_provider_identities i \
        JOIN auth_oidc_providers p ON p.provider_id=i.provider \
        WHERE i.principal_id=?1 \
        AND p.state='active' \
        AND i.profile_json IS NOT NULL \
        ORDER BY i.last_seen_at DESC,i.identity_id DESC",
    )?;
    let mut upstream = VerifiedProfile::default();
    let mut email_provider = None;
    for row in profiles.query_map([principal], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (provider, json) = row?;
        let profile: VerifiedProfile = serde_json::from_str(&json)?;
        let retained_verification = upstream.email.as_ref().map(|_| upstream.email_verified);
        if upstream.email.is_none() && profile.email.is_some() {
            email_provider = Some(provider);
        }
        upstream.fill(profile);
        if let Some(verified) = retained_verification {
            upstream.email_verified = verified;
        }
    }
    if current.display_source != "manual" {
        current.display_source = if upstream.name.is_some() {
            "oidc"
        } else {
            "default"
        }
        .into();
        current.display_name = upstream.name.unwrap_or_else(|| principal.to_owned());
    }
    if current.given_source != "manual" {
        current.given_source = if upstream.given_name.is_some() {
            "oidc"
        } else {
            "unset"
        }
        .into();
        current.given_name = upstream.given_name;
    }
    if current.family_source != "manual" {
        current.family_source = if upstream.family_name.is_some() {
            "oidc"
        } else {
            "unset"
        }
        .into();
        current.family_name = upstream.family_name;
    }
    if current.email_source != "manual" {
        current.email_source = if upstream.email.is_some() {
            "oidc"
        } else {
            "unset"
        }
        .into();
        current.email_verified = upstream.email.is_some() && upstream.email_verified == Some(true);
        current.email = upstream.email;
        current.email_provider = email_provider;
    }
    current.picture = upstream.picture;
    if current != before {
        current.revision += 1;
        connection.execute(
            "DELETE FROM auth_avatar_cache WHERE principal_id=?1",
            [principal],
        )?;
        connection.execute(
            "INSERT INTO auth_user_profiles(principal_id,display_name,given_name,\
            family_name,display_name_source,given_name_source,family_name_source,\
            email,email_verified,email_source,email_provider_id,oidc_picture_url,\
            avatar_override_url,gravatar_enabled,created_at,updated_at,version) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?16) \
            ON CONFLICT(principal_id) \
            DO UPDATE SET display_name=excluded.display_name,\
            given_name=excluded.given_name,family_name=excluded.family_name,\
            display_name_source=excluded.display_name_source,\
            given_name_source=excluded.given_name_source,\
            family_name_source=excluded.family_name_source,email=excluded.email,\
            email_verified=excluded.email_verified,\
            email_source=excluded.email_source,\
            email_provider_id=excluded.email_provider_id,\
            oidc_picture_url=excluded.oidc_picture_url,\
            avatar_override_url=excluded.avatar_override_url,\
            gravatar_enabled=excluded.gravatar_enabled,\
            updated_at=excluded.updated_at,version=excluded.version",
            params![
                principal,
                current.display_name,
                current.given_name,
                current.family_name,
                current.display_source,
                current.given_source,
                current.family_source,
                current.email,
                current.email_verified,
                current.email_source,
                current.email_provider,
                current.picture,
                current.avatar_override,
                current.gravatar_enabled,
                now,
                current.revision
            ],
        )?;
    }
    Ok(current)
}

/// Called only after the authenticated subject has been linked/rechecked in the
/// same login/link transaction. Missing claims preserve that identity's snapshot.
pub(super) fn ingest(
    connection: &Connection,
    principal: &str,
    provider: &str,
    subject: &str,
    verified: &VerifiedProfile,
    now: i64,
) -> Result<(), AuthError> {
    let retained: Option<String> = connection
        .query_row(
            "SELECT profile_json \
            FROM auth_provider_identities \
            WHERE principal_id=?1 \
            AND provider=?2 AND provider_subject=?3",
            params![principal, provider, subject],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AuthError::Denied)?;
    let mut profile = verified.clone();
    if let Some(retained) = retained {
        profile.fill(serde_json::from_str(&retained)?);
    }
    connection.execute(
        "UPDATE auth_provider_identities \
        SET profile_json=?1,last_seen_at=?2 \
        WHERE principal_id=?3 \
        AND provider=?4 AND provider_subject=?5",
        params![
            serde_json::to_string(&profile)?,
            now,
            principal,
            provider,
            subject
        ],
    )?;
    write(connection, principal, None, now)?;
    Ok(())
}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
