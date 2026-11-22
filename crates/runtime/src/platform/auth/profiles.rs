//! Local presentation data, independent of login identity and authorization.

use crate::platform::auth::sqlite::Connection;
use rusqlite::{params, OptionalExtension};
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
    /// The picture of `picture_provider`'s linked identity, when it has one.
    pub(super) picture: Option<String>,
    /// The linked provider whose picture the account shows; `None` is Gravatar.
    pub(super) picture_provider: Option<String>,
    pub(super) revision: u64,
}

/// The name shown for someone who has neither set one nor been given one by
/// their sign-in provider: their username, which people recognise, and the
/// principal ID only for an account without one.
fn default_name(connection: &Connection, principal: &str) -> Result<String, AuthError> {
    let username: Option<String> = connection
        .query_row(
            "SELECT coalesce((SELECT normalized_username \
            FROM auth_local_credentials \
            WHERE principal_id=?1),(SELECT provider_subject \
            FROM auth_provider_identities \
            WHERE principal_id=?1 \
            AND provider='local'))",
            [principal],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(username.unwrap_or_else(|| principal.to_owned()))
}

pub(super) fn read(connection: &Connection, principal: &str) -> Result<Profile, AuthError> {
    let fallback = default_name(connection, principal)?;
    // A default name is never stored, so it follows the username.
    Ok(connection
        .query_row(
            "SELECT coalesce(CASE display_name_source \
            WHEN 'default' THEN NULL \
            ELSE display_name END,?2),given_name,family_name,\
            display_name_source,given_name_source,family_name_source,email,\
            email_verified,email_source,email_provider_id,oidc_picture_url,\
            picture_provider_id,version \
            FROM auth_user_profiles \
            WHERE principal_id=?1",
            rusqlite::params![principal, fallback],
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
                    picture_provider: row.get(11)?,
                    revision: row.get(12)?,
                })
            },
        )
        .optional()?
        .unwrap_or(Profile {
            display_name: fallback,
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
            picture_provider: creation_provider(connection, principal)?,
            revision: 1,
        }))
}

/// The OIDC sign-in an account was created with, whose picture it shows
/// until its owner chooses another. An account with a password was not, and
/// shows its Gravatar. This applies only before the profile is first stored.
fn creation_provider(
    connection: &Connection,
    principal: &str,
) -> Result<Option<String>, AuthError> {
    Ok(connection
        .query_row(
            "SELECT i.provider FROM auth_provider_identities i \
            JOIN auth_oidc_providers p ON p.provider_id=i.provider \
            WHERE i.principal_id=?1 \
            AND NOT EXISTS(SELECT 1 FROM auth_local_credentials \
            WHERE principal_id=?1) \
            ORDER BY i.linked_at,i.identity_id LIMIT 1",
            [principal],
            |row| row.get(0),
        )
        .optional()?)
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
            return Err(super::sqlite::changed_since(
                &format!("profile of user {principal}"),
                patch.expected_revision.0 .0,
            ));
        }
        if (patch.email.is_some() && patch.clear_email == Some(true))
            || (patch.picture_provider_id.is_some() && patch.use_gravatar == Some(true))
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
        if let Some(provider) = &patch.picture_provider_id {
            let provider = provider.to_string();
            let linked: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_provider_identities \
                WHERE principal_id=?1 AND provider=?2)",
                params![principal, provider],
                |row| row.get(0),
            )?;
            if !linked {
                return Err(AuthError::Invalid(
                    "the picture can come only from an OIDC sign-in linked to this account".into(),
                ));
            }
            current.picture_provider = Some(provider);
        }
        if patch.use_gravatar == Some(true) {
            current.picture_provider = None;
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
        current.display_name = match upstream.name {
            Some(name) => name,
            None => default_name(connection, principal)?,
        };
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
    // The chosen provider's picture. Unlinking it, or turning the provider
    // off, returns the account to its Gravatar.
    let chosen = match &current.picture_provider {
        Some(provider) => connection
            .query_row(
                "SELECT i.profile_json FROM auth_provider_identities i \
                JOIN auth_oidc_providers p ON p.provider_id=i.provider \
                WHERE i.principal_id=?1 AND i.provider=?2 AND p.state='active'",
                params![principal, provider],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?,
        None => None,
    };
    if chosen.is_none() {
        current.picture_provider = None;
    }
    current.picture = match chosen.flatten() {
        Some(json) => serde_json::from_str::<VerifiedProfile>(&json)?.picture,
        None => None,
    };
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
            picture_provider_id,created_at,updated_at,version) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14,?15) \
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
            picture_provider_id=excluded.picture_provider_id,\
            updated_at=excluded.updated_at,version=excluded.version",
            params![
                principal,
                (current.display_source != "default").then_some(&current.display_name),
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
                current.picture_provider,
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
