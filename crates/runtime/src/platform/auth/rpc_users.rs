//! Typed account adapters over Auth's authoritative SQLite aggregates.

use super::sqlite::timestamp;
use super::Administration;
use crate::platform::auth::blocking;
use std::sync::Arc;

use crate::platform::auth::sqlite::Connection;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rusqlite::{params, OptionalExtension};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo, CursorQuery},
    apis::trellis_auth_v1::Provider,
};

use super::{
    policy::apply_policy_in_transaction,
    revocation::EnforcementScope,
    rpc::{caller, mutation, rpc_error},
    sqlite::{digest, id},
    AuthError, Mutation, PolicyMutation, SqliteAuthorizationStore,
};

/// Register account/profile and configured upstream-provider management routes.
pub(crate) fn register_user_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    let users = Arc::clone(&store);
    provider.register_users_get(move |context, input| {
        let store = Arc::clone(&users);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            let delegated = caller(&context, Some(Administration::ManageAccounts)).is_ok();
            blocking(move || {
                store.transaction(|connection| {
                    let administrative = directory_account_access(connection, &actor, delegated)?;
                    Ok(AuthUserGetResponse {
                        user: read_user(
                            &store,
                            connection,
                            &input.user_id.to_string(),
                            administrative,
                        )?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let users = Arc::clone(&store);
    provider.register_users_find(move |context, input| {
        let store = Arc::clone(&users);
        async move {
            let actor =
                caller(&context, Some(Administration::ManageAccounts)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageAccounts,
                    )?;
                    find_user(&store, connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let users = Arc::clone(&store);
    provider.register_users_resolve(move |context, _| {
        let store = Arc::clone(&users);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    Ok(AuthUserGetResponse {
                        user: read_user(&store, connection, &actor, true)?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    macro_rules! admin_mutation {
        ($register:ident, $operation:ident, $administration:ident) => {{
            let store = Arc::clone(&store);
            provider.$register(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(Administration::$administration),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    blocking(move || {
                        $operation(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(rpc_error)
                }
            });
        }};
    }
    admin_mutation!(register_users_create, create_user, ManageAccounts);
    admin_mutation!(register_users_update, update_user, ManageAccounts);
    admin_mutation!(register_users_delete, delete_user, ManageAccounts);
    admin_mutation!(
        register_users_identities_link,
        link_user_identity,
        ManageAccounts
    );
    admin_mutation!(
        register_users_identities_unlink,
        unlink_user_identity,
        ManageAccounts
    );
    let profiles = Arc::clone(&store);
    provider.register_users_profile_update(move |context, input| {
        let store = Arc::clone(&profiles);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || update_profile(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });
    admin_mutation!(
        register_oidc_providers_enable,
        enable_provider,
        ManagePrincipals
    );
    admin_mutation!(
        register_oidc_providers_disable,
        disable_provider,
        ManagePrincipals
    );
    admin_mutation!(
        register_oidc_providers_delete,
        delete_provider,
        ManagePrincipals
    );

    let links = Arc::clone(&store);
    provider.register_users_identity_link_providers(move |context, _input| {
        let store = Arc::clone(&links);
        async move {
            caller(&context, None).map_err(rpc_error)?;
            blocking(move || store.read(read_link_providers))
                .await
                .map_err(rpc_error)
        }
    });
    let identities = Arc::clone(&store);
    provider.register_user_identities_list(move |context, input| {
        let store = Arc::clone(&identities);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| read_identities(connection, &actor, &input))
            })
            .await
            .map_err(rpc_error)
        }
    });
    let identities = Arc::clone(&store);
    provider.register_user_identities_unlink(move |context, input| {
        let store = Arc::clone(&identities);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || unlink_identity(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });

    let mappings = Arc::clone(&store);
    provider.register_oidc_role_mappings_list(move |context, input| {
        let store = Arc::clone(&mappings);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    let (cursor, limit) = page_bounds(input.page.as_ref())?;
                    let mut statement = connection.prepare(
                        "SELECT mapping_id FROM auth_oidc_role_mappings \
                        WHERE provider_id=?1 \
                        AND mapping_id>?2 ORDER BY mapping_id \
                        LIMIT ?3",
                    )?;
                    let mut ids = statement
                        .query_map(
                            params![input.provider_id.to_string(), cursor, limit + 1],
                            |row| row.get::<_, String>(0),
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
                    ids.truncate(limit);
                    Ok(CursorPage {
                        items: ids
                            .iter()
                            .map(|id| read_mapping(connection, id))
                            .collect::<Result<Vec<_>, _>>()?,
                        page: CursorPageInfo { next_cursor },
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let mappings = Arc::clone(&store);
    provider.register_oidc_role_mappings_delete(move |context, input| {
        let store = Arc::clone(&mappings);
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManageRoles),
                &input,
            )
            .map_err(rpc_error)?;
            blocking(move || delete_mapping(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });

    let users = Arc::clone(&store);
    provider.register_users_password_change(move |context, input| {
        let store = Arc::clone(&users);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            let login = context
                .request()
                .verified_request()
                .and_then(|request| request.caller().login_session_id.clone())
                .ok_or_else(|| rpc_error(AuthError::Denied))?;
            blocking(move || {
                change_password(&store, &identity, &input, &login, crate::clock::unix_now())
            })
            .await
            .map_err(rpc_error)
        }
    });
    let providers = Arc::clone(&store);
    provider.register_oidc_providers_get(move |context, input| {
        let store = Arc::clone(&providers);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    Ok(AuthOIDCProviderGetResponse {
                        provider: read_provider(connection, &input.provider_id.to_string())?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_oidc_providers_list(move |context, input| {
        let store = Arc::clone(&store);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    read_providers(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
}

/// Register provider writes with the production engine's existing sealing custody.
pub(crate) fn register_user_provider_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    engine: Arc<super::OAuthEngine>,
) {
    let resets = Arc::clone(&engine);
    provider.register_users_password_reset_create(move |context, input| {
        let engine = Arc::clone(&resets);
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManageAccounts),
                &input,
            )
            .map_err(rpc_error)?;
            let requester_session = context
                .request()
                .verified_request()
                .ok_or_else(|| rpc_error(AuthError::Denied))?
                .caller()
                .authorization_session_id
                .clone();
            blocking(move || {
                super::oauth::account::create_reset(
                    engine.server.store(),
                    &identity,
                    &input,
                    &requester_session,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    let links = Arc::clone(&engine);
    provider.register_users_identity_link_create(move |context, input| {
        let engine = Arc::clone(&links);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || {
                super::oauth::account::create_link(
                    engine.server.store(),
                    &identity,
                    &input,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_oidc_providers_put(move |context, input| {
        let engine = Arc::clone(&engine);
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePrincipals),
                &input,
            )
            .map_err(rpc_error)?;
            put_oidc_provider(
                engine.server.store().clone(),
                identity,
                input,
                crate::clock::unix_now(),
            )
            .await
            .map_err(rpc_error)
        }
    });
}

/// Handles an authorized provider Put. A retry of a request that already
/// completed gets its recorded result before Auth contacts the issuer, so an
/// issuer that went offline or changed certificates since cannot fail it.
/// Otherwise the issuer certificate pin is resolved against the live issuer
/// ([`super::oauth::oidc::issuer_pin`]) and [`put_provider`] stores the
/// change, checking the idempotency record again inside its write.
pub(super) async fn put_oidc_provider(
    storage: super::OAuthStorage,
    identity: Mutation,
    input: AuthOIDCProviderPutRequest,
    now: i64,
) -> Result<AuthOIDCProviderGetResponse, AuthError> {
    let audit = serde_json::json!({"requestDigest": identity.request_digest});
    let recorded = {
        let (storage, recorded_identity) = (
            storage.clone(),
            Mutation {
                request_digest: digest(&audit)?,
                ..identity.clone()
            },
        );
        blocking(move || {
            storage
                .authority
                .recorded(&recorded_identity, "rpc.oidcProviders.put")
        })
        .await?
    };
    if let Some(recorded) = recorded {
        return Ok(recorded);
    }
    let pin = super::oauth::oidc::issuer_pin(
        &input.issuer,
        input.certificate_fingerprint.as_ref(),
        input.expected_revision.0 == 0,
    )
    .await?;
    blocking(move || put_provider(&storage, &identity, &input, pin, now)).await
}

/// Stores a provider Put. `pin` is the issuer certificate decision
/// [`super::oauth::oidc::issuer_pin`] reached for the same input.
pub(super) fn put_provider(
    storage: &super::OAuthStorage,
    identity: &Mutation,
    input: &AuthOIDCProviderPutRequest,
    pin: super::oauth::oidc::IssuerPin,
    now: i64,
) -> Result<AuthOIDCProviderGetResponse, AuthError> {
    if identity.request_digest != digest(input)? {
        return Err(AuthError::Invalid(
            "mutation digest does not match its input".into(),
        ));
    }
    if (!input.role_claim.is_empty() && !input.role_claim.starts_with('/'))
        || matches!(
            input.prompt_after_sign_out,
            Some(AuthOIDCSignOutPrompt::Unknown(_))
        )
        || input.scopes.len() > 32
        || input.scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 256
                || scope
                    .bytes()
                    .any(|byte| !(0x21..=0x7e).contains(&byte) || byte == b'"' || byte == b'\\')
        })
    {
        return Err(AuthError::Invalid(
            "invalid upstream claims or scopes".into(),
        ));
    }
    let parameters = input
        .authorization_parameters
        .as_deref()
        .unwrap_or_default();
    if parameters.len() > 16 {
        return Err(AuthError::Invalid(
            "a provider takes at most 16 custom authorization parameters".into(),
        ));
    }
    for (index, parameter) in parameters.iter().enumerate() {
        let name = parameter.name.as_str();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(AuthError::Invalid(format!(
                "authorization parameter name {name:?} must be 1 to 64 letters, digits, \
                '-', '_' or '.'"
            )));
        }
        if matches!(
            name,
            "client_id"
                | "redirect_uri"
                | "response_type"
                | "scope"
                | "state"
                | "nonce"
                | "code_challenge"
                | "code_challenge_method"
                | "prompt"
        ) {
            return Err(AuthError::Invalid(format!(
                "authorization parameter {name} is set by Trellis itself{}",
                if name == "prompt" {
                    "; use the provider's prompt after sign-out setting instead"
                } else {
                    ""
                }
            )));
        }
        if parameters[..index]
            .iter()
            .any(|earlier| earlier.name == name)
        {
            return Err(AuthError::Invalid(format!(
                "authorization parameter {name} is given more than once"
            )));
        }
        if parameter.value.len() > 1024 || parameter.value.chars().any(char::is_control) {
            return Err(AuthError::Invalid(format!(
                "authorization parameter {name} needs a value of at most 1024 bytes without \
                control characters"
            )));
        }
    }
    let audit = serde_json::json!({"requestDigest": identity.request_digest});
    let safe_identity = Mutation {
        request_digest: digest(&audit)?,
        ..identity.clone()
    };
    let provider = input
        .provider_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(id);
    let sealed = input
        .client_secret
        .as_ref()
        .map(|secret| {
            storage
                .seal_upstream_client_secret(&provider, secret)
                .map_err(|_| AuthError::Unavailable)
        })
        .transpose()?;
    storage.authority.mutate(
        &safe_identity,
        "rpc.oidcProviders.put",
        &audit,
        now,
        |connection| {
            let previous = connection
                .query_row(
                    "SELECT sealed_client_secret,state,issuer_certificate \
                    FROM auth_oidc_providers \
                    WHERE provider_id=?1",
                    [&provider],
                    |row| {
                        Ok((
                            row.get::<_, Option<Vec<u8>>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                        ))
                    },
                )
                .optional()?;
            if previous
                .as_ref()
                .is_some_and(|(_, state, _)| state == "deleted")
            {
                return Err(AuthError::Conflict);
            }
            let (previous_secret, previous_certificate) = previous
                .map(|(secret, _, certificate)| (secret, certificate))
                .unwrap_or_default();
            let registration = super::lifecycle::OidcProviderRegistration {
                provider_id: provider.clone(),
                issuer: input.issuer.clone(),
                display_name: input.title.to_string(),
                client_id: input.client_id.clone(),
                sealed_client_secret: sealed.or(previous_secret),
                issuer_certificate: match &pin {
                    super::oauth::oidc::IssuerPin::Keep => previous_certificate,
                    super::oauth::oidc::IssuerPin::Remove => None,
                    super::oauth::oidc::IssuerPin::Set(certificate) => Some(certificate.clone()),
                },
                config: serde_json::json!({
                    "scopes": input.scopes,
                    "roleClaim": input.role_claim,
                    "promptAfterSignOut": input
                        .prompt_after_sign_out
                        .clone()
                        .unwrap_or(AuthOIDCSignOutPrompt::Login),
                    "authorizationParameters": parameters,
                }),
                claim_freshness_seconds: input.claim_freshness_seconds.0,
                expected_revision: input.expected_revision.0,
            };
            let registration_identity = Mutation {
                request_digest: digest(&registration)?,
                ..identity.clone()
            };
            storage.authority.register_oidc_provider_in_transaction(
                connection,
                &registration_identity,
                &registration,
                now,
            )?;
            Ok(AuthOIDCProviderGetResponse {
                provider: read_provider(connection, &provider)?,
                extra: Default::default(),
            })
        },
    )
}

/// The providers `Users.IdentityLink.Create` accepts, by name.
pub(super) fn read_link_providers(
    connection: &Connection,
) -> Result<AuthIdentityLinkProvidersResponse, AuthError> {
    let mut statement = connection.prepare(
        "SELECT provider_id,display_name FROM auth_oidc_providers \
        WHERE state='active' ORDER BY display_name,provider_id",
    )?;
    let providers = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (provider, title) = row?;
            Ok(AuthIdentityLinkProvider {
                provider_id: provider.parse().map_err(|_| AuthError::Unavailable)?,
                title,
                extra: Default::default(),
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    Ok(AuthIdentityLinkProvidersResponse {
        providers,
        extra: Default::default(),
    })
}

pub(super) fn read_identities(
    connection: &Connection,
    actor: &str,
    input: &AuthPageRequest,
) -> Result<AuthUserIdentitiesListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT i.identity_id,i.provider,o.issuer,i.provider_subject,i.linked_at \
        FROM auth_provider_identities i \
        JOIN auth_oidc_providers o ON o.provider_id=i.provider \
        WHERE i.principal_id=?1 \
        AND i.identity_id>?2 \
        AND (?3 IS NULL OR instr(lower(i.provider_subject),lower(?3))>0 \
        OR instr(lower(o.display_name),lower(?3))>0) \
        ORDER BY i.identity_id \
        LIMIT ?4",
    )?;
    let mut items = statement
        .query_map(
            params![
                actor,
                cursor,
                input.search.as_ref().map(|value| value.as_ref()),
                limit + 1
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?
        .map(|row| {
            let (identity, provider, issuer, subject, linked) = row?;
            Ok(AuthUserIdentity {
                identity_id: identity.parse().map_err(|_| AuthError::Unavailable)?,
                provider_id: provider.parse().map_err(|_| AuthError::Unavailable)?,
                issuer,
                subject,
                linked_at: timestamp(linked)?,
                extra: Default::default(),
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].identity_id.to_string());
    items.truncate(limit);
    Ok(CursorPage {
        items,
        page: CursorPageInfo { next_cursor },
    })
}

pub(super) fn unlink_identity(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserIdentityUnlinkRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.userIdentities.unlink",
        input,
        now,
        |connection| {
            let (provider, subject): (String, String) = connection
                .query_row(
                    "SELECT provider,provider_subject \
                    FROM auth_provider_identities \
                    WHERE identity_id=?1 \
                    AND principal_id=?2 \
                    AND provider!='local'",
                    params![input.identity_id.to_string(), identity.actor],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_bootstrap_administrator \
                WHERE principal_id=?1)",
                [&identity.actor],
                |row| row.get(0),
            )?;
            if protected {
                return Err(AuthError::Invalid(
                    "the default administrator's OIDC sign-ins cannot be disconnected".into(),
                ));
            }
            remove_identity(
                store,
                connection,
                identity,
                &identity.actor,
                &input.identity_id.to_string(),
                (&provider, &subject),
                now,
            )?;
            Ok(AuthMutationResult {
                revision: AuthRevision(1.into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

/// Remove one linked OIDC sign-in from `principal`, unless it is the account's
/// only way to sign in: it then needs a password or another sign-in through an
/// enabled provider. The sign-ins made with it end, and the profile drops what
/// that provider said.
fn remove_identity(
    store: &SqliteAuthorizationStore,
    connection: &Connection,
    identity: &Mutation,
    principal: &str,
    identity_id: &str,
    (provider, subject): (&str, &str),
    now: i64,
) -> Result<(), AuthError> {
    let alternative: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 \
        FROM auth_local_credentials \
        WHERE principal_id=?1 \
        UNION ALL SELECT 1 FROM auth_provider_identities i \
        JOIN auth_oidc_providers o ON o.provider_id=i.provider \
        WHERE i.principal_id=?1 \
        AND i.identity_id!=?2 \
        AND o.state='active')",
        params![principal, identity_id],
        |row| row.get(0),
    )?;
    if !alternative {
        return Err(AuthError::Invalid(
            "this is the account's only way to sign in; set a password or connect \
            another OIDC sign-in first"
                .into(),
        ));
    }
    let mut statement = connection.prepare(
        "SELECT login_session_id \
        FROM auth_login_sessions \
        WHERE principal_id=?1 \
        AND provider_id=?2 AND upstream_subject=?3 \
        AND revoked_at IS NULL",
    )?;
    let logins = statement
        .query_map(params![principal, provider, subject], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for login in logins {
        let scope = EnforcementScope::Login(login);
        let root = Mutation {
            actor: identity.actor.clone(),
            request_id: id(),
            request_digest: digest(&scope)?,
            session: identity.session.clone(),
        };
        store.revoke_root_in_transaction(connection, &root, &scope, now)?;
    }
    connection.execute(
        "DELETE FROM auth_provider_identities \
        WHERE identity_id=?1 \
        AND principal_id=?2",
        params![identity_id, principal],
    )?;
    super::profiles::write(connection, principal, None, now)?;
    SqliteAuthorizationStore::policy_changed(
        connection,
        &EnforcementScope::Principal(principal.to_owned()),
        now,
    )?;
    Ok(())
}

/// Whether `principal` already has the OIDC sign-in `identity`, after checking
/// that its provider is enabled and that no other account has it. The subject
/// is matched exactly, as `Users.Find` and sign-in match it.
fn has_identity(
    connection: &Connection,
    principal: &str,
    identity: &AuthUserFindIdentity,
) -> Result<bool, AuthError> {
    if identity.subject.is_empty() {
        return Err(AuthError::Invalid("the subject must not be empty".into()));
    }
    let provider = identity.provider_id.to_string();
    let state: Option<String> = connection
        .query_row(
            "SELECT state FROM auth_oidc_providers WHERE provider_id=?1",
            [&provider],
            |row| row.get(0),
        )
        .optional()?;
    match state.as_deref() {
        Some("active") => {}
        Some("disabled") => {
            return Err(AuthError::Invalid(
                "that OIDC provider is turned off; turn it on first".into(),
            ))
        }
        _ => return Err(AuthError::Invalid("no OIDC provider has that ID".into())),
    }
    let owner: Option<String> = connection
        .query_row(
            "SELECT principal_id FROM auth_provider_identities \
            WHERE provider=?1 AND provider_subject=?2",
            params![provider, identity.subject],
            |row| row.get(0),
        )
        .optional()?;
    match owner {
        Some(owner) if owner == principal => Ok(true),
        Some(_) => Err(AuthError::IdentityTaken),
        None => Ok(false),
    }
}

/// Check that an account is at `expected_revision` and move it on, so another
/// change made against the same revision fails.
fn advance_account_revision(
    connection: &Connection,
    principal: &str,
    expected_revision: u64,
    now: i64,
) -> Result<(), AuthError> {
    if connection.execute(
        "UPDATE auth_principals SET version=version+1,updated_at=?1 \
        WHERE principal_id=?2 AND kind='user' \
        AND version=?3 AND state!='revoked'",
        params![now, principal, expected_revision],
    )? != 1
    {
        return Err(AuthError::Conflict);
    }
    Ok(())
}

/// An administrator changes the sign-ins of every account in reach but the
/// default administrator's, their own included, as for password resets.
fn require_identity_administration(
    connection: &Connection,
    actor: &str,
    principal: &str,
) -> Result<(), AuthError> {
    SqliteAuthorizationStore::require_account_access(connection, actor, principal)?;
    let protected: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator WHERE principal_id=?1)",
        [principal],
        |row| row.get(0),
    )?;
    if protected {
        return Err(AuthError::Denied);
    }
    Ok(())
}

/// The account and its linked sign-ins, as `Users.Find` returns them.
fn account_with_identities(
    store: &SqliteAuthorizationStore,
    connection: &Connection,
    principal: &str,
) -> Result<AuthUserFindMatch, AuthError> {
    find_user(
        store,
        connection,
        &AuthUserFindRequest {
            user_id: Some(principal.parse().map_err(|_| AuthError::Unavailable)?),
            identity: None,
            extra: Default::default(),
        },
    )?
    .found
    .ok_or(AuthError::NotFound)
}

/// Link an OIDC sign-in to an enabled account. One the account already has is
/// a success that changes nothing, whatever revision it names, so a script can
/// run again. The identity has no upstream claims until its first sign-in, so
/// until then it adds no name, email or verification to the profile.
pub(super) fn link_user_identity(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserIdentitiesLinkRequest,
    now: i64,
) -> Result<AuthUserFindMatch, AuthError> {
    store.mutate(
        identity,
        "rpc.users.identities.link",
        input,
        now,
        |connection| {
            let principal = input.user_id.to_string();
            require_identity_administration(connection, &identity.actor, &principal)?;
            let user = read_user(store, connection, &principal, true)?;
            if !user
                .account
                .as_ref()
                .is_some_and(|account| account.principal.enabled)
            {
                return Err(AuthError::Invalid(
                    "the account is turned off; turn it on first".into(),
                ));
            }
            if !has_identity(connection, &principal, &input.identity)? {
                advance_account_revision(
                    connection,
                    &principal,
                    input.expected_revision.0 .0,
                    now,
                )?;
                SqliteAuthorizationStore::link_oidc_identity_in(
                    connection,
                    &principal,
                    &input.identity.provider_id.to_string(),
                    &input.identity.subject,
                    now,
                )?;
            }
            account_with_identities(store, connection, &principal)
        },
    )
}

/// Remove an OIDC sign-in from an account, as its owner can, and end the
/// sign-ins made with it.
pub(super) fn unlink_user_identity(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserIdentitiesUnlinkRequest,
    now: i64,
) -> Result<AuthUserFindMatch, AuthError> {
    store.mutate(
        identity,
        "rpc.users.identities.unlink",
        input,
        now,
        |connection| {
            let principal = input.user_id.to_string();
            require_identity_administration(connection, &identity.actor, &principal)?;
            read_user(store, connection, &principal, true)?;
            let provider = input.identity.provider_id.to_string();
            let identity_id: String = connection
                .query_row(
                    "SELECT identity_id FROM auth_provider_identities \
                    WHERE principal_id=?1 AND provider=?2 \
                    AND provider_subject=?3 AND provider!='local'",
                    params![principal, provider, input.identity.subject],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            advance_account_revision(connection, &principal, input.expected_revision.0 .0, now)?;
            remove_identity(
                store,
                connection,
                identity,
                &principal,
                &identity_id,
                (&provider, &input.identity.subject),
                now,
            )?;
            account_with_identities(store, connection, &principal)
        },
    )
}

pub(super) fn directory_account_access(
    connection: &Connection,
    actor: &str,
    delegated: bool,
) -> Result<bool, AuthError> {
    if !delegated {
        return Ok(false);
    }
    match SqliteAuthorizationStore::require_administration(
        connection,
        actor,
        Administration::ManageAccounts,
    ) {
        Ok(()) => Ok(true),
        Err(AuthError::Denied) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Find one user, enabled or not, by ID or by the exact provider and subject of
/// a linked OIDC sign-in. A miss is an empty result, not an error.
pub(super) fn find_user(
    store: &SqliteAuthorizationStore,
    connection: &Connection,
    input: &AuthUserFindRequest,
) -> Result<AuthUserFindResponse, AuthError> {
    let principal: Option<String> = match (&input.user_id, &input.identity) {
        (Some(user), None) => Some(user.to_string()),
        (None, Some(identity)) => connection
            .query_row(
                "SELECT principal_id FROM auth_provider_identities \
                WHERE provider=?1 AND provider_subject=?2 AND provider!='local'",
                params![identity.provider_id.to_string(), identity.subject],
                |row| row.get(0),
            )
            .optional()?,
        _ => {
            return Err(AuthError::Invalid(
                "give exactly one of userId and identity".into(),
            ))
        }
    };
    let user = match principal.map(|principal| read_user(store, connection, &principal, true)) {
        None | Some(Err(AuthError::NotFound)) => None,
        Some(user) => Some(user?),
    };
    let Some(user) = user else {
        return Ok(AuthUserFindResponse {
            found: None,
            extra: Default::default(),
        });
    };
    let mut statement = connection.prepare(
        "SELECT i.identity_id,i.provider,o.issuer,i.provider_subject,i.linked_at \
        FROM auth_provider_identities i \
        JOIN auth_oidc_providers o ON o.provider_id=i.provider \
        WHERE i.principal_id=?1 ORDER BY i.identity_id",
    )?;
    let identities = statement
        .query_map([user.principal_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .map(|row| {
            let (identity, provider, issuer, subject, linked) = row?;
            Ok(AuthUserIdentity {
                identity_id: identity.parse().map_err(|_| AuthError::Unavailable)?,
                provider_id: provider.parse().map_err(|_| AuthError::Unavailable)?,
                issuer,
                subject,
                linked_at: timestamp(linked)?,
                extra: Default::default(),
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    Ok(AuthUserFindResponse {
        found: Some(AuthUserFindMatch {
            user,
            identities,
            extra: Default::default(),
        }),
        extra: Default::default(),
    })
}

pub(super) fn read_user(
    store: &SqliteAuthorizationStore,
    connection: &Connection,
    principal: &str,
    account: bool,
) -> Result<AuthUser, AuthError> {
    let (state, revision, created, username, password) = connection
        .query_row(
            "SELECT p.state,p.version,p.created_at,coalesce(c.normalized_username,\
            (SELECT i.provider_subject \
            FROM auth_provider_identities i \
            WHERE i.principal_id=p.principal_id \
            AND i.provider='local'),''),c.password_hash IS NOT NULL \
            FROM auth_principals p \
            LEFT JOIN auth_local_credentials c USING(principal_id) \
            WHERE p.principal_id=?1 \
            AND p.kind='user' AND p.state!='revoked' AND (?2 \
            OR p.state='active')",
            params![principal, account],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let profile = super::profiles::read(connection, principal)?;
    let avatar_url = if state == "active" {
        super::avatars::avatar_url(store, connection, principal, &profile)
    } else {
        None
    };
    Ok(AuthUser {
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        account: if account {
            Some(AuthUserAccountDetails {
                principal: AuthPrincipal {
                    principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
                    kind: AuthPrincipalKind::User,
                    enabled: state == "active",
                    revision: AuthRevision(revision.into()),
                    created_at: timestamp(created)?,
                    title: profile.display_name.clone(),
                    extra: Default::default(),
                },
                username,
                has_local_password: password,
                profile_revision: AuthRevision(profile.revision.into()),
                picture_provider_id: profile
                    .picture_provider
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| AuthError::Unavailable)?,
                extra: Default::default(),
            })
        } else {
            None
        },
        display_name: profile.display_name,
        given_name: profile.given_name,
        family_name: profile.family_name,
        email: profile.email,
        email_verified: profile.email_verified,
        avatar_url,
        extra: Default::default(),
    })
}

/// Insert an active user and give it the roles an administrator marked for new
/// users. Marking them was the policy decision, so this needs no role rights.
pub(super) fn insert_user(
    connection: &Connection,
    principal: &str,
    now: i64,
) -> Result<(), AuthError> {
    connection.execute(
        "INSERT INTO auth_principals(principal_id,kind,state,created_at,\
        updated_at,version) \
        VALUES(?1,'user','active',?2,?2,1)",
        params![principal, now],
    )?;
    let defaults = connection
        .prepare(
            "SELECT role_id,identity_generation FROM auth_roles \
            WHERE state='active' AND assign_to_new_users=1",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (role, generation) in defaults {
        connection.execute(
            "INSERT INTO auth_role_assignments(assignment_id,principal_id,role_id,\
            role_generation,revision,expires_at,created_at) \
            VALUES(?1,?2,?3,?4,1,NULL,?5)",
            params![id(), principal, role, generation, now],
        )?;
    }
    Ok(())
}

/// Reserve a new user's normalized username, store its password hash when it
/// has one, and write its profile. A taken username is a conflict.
pub(super) fn insert_local_account(
    connection: &Connection,
    principal: &str,
    username: &str,
    hash: Option<&str>,
    profile: &AuthUserProfileUpdateRequest,
    now: i64,
) -> Result<(), AuthError> {
    let taken: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 \
        FROM auth_local_credentials \
        WHERE normalized_username=?1 \
        UNION ALL SELECT 1 FROM auth_provider_identities \
        WHERE provider='local' \
        AND provider_subject=?1)",
        [username],
        |row| row.get(0),
    )?;
    if taken {
        return Err(AuthError::Conflict);
    }
    let profile = AuthUserProfileUpdateRequest {
        expected_revision: AuthRevision(
            super::profiles::read(connection, principal)?
                .revision
                .into(),
        ),
        ..profile.clone()
    };
    super::profiles::write(connection, principal, Some(&profile), now)?;
    // The local identity reserves the normalized username even before an
    // account establishes a password. It is not itself a sign-in credential.
    connection.execute(
        "INSERT INTO auth_provider_identities(identity_id,provider,provider_subject,\
        principal_id,linked_at,last_seen_at) \
        VALUES(?1,'local',?2,?3,?4,?4)",
        params![id(), username, principal, now],
    )?;
    if let Some(hash) = hash {
        connection.execute(
            "INSERT INTO auth_local_credentials(principal_id,normalized_username,\
            password_hash,hash_profile,password_changed_at,updated_at,version) \
            VALUES(?1,?2,?3,1,?4,?4,1)",
            params![principal, username, hash, now],
        )?;
    }
    Ok(())
}

pub(super) fn create_user(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserCreateRequest,
    now: i64,
) -> Result<AuthUserGetResponse, AuthError> {
    if identity.request_digest != digest(input)? {
        return Err(AuthError::Invalid(
            "mutation digest does not match its input".into(),
        ));
    }
    let username = input.username.trim().to_lowercase();
    if username.is_empty()
        || username.len() > 256
        || input.username.trim() != input.username.as_str()
        || input.display_name.trim().is_empty()
    {
        return Err(AuthError::Invalid("invalid user profile".into()));
    }
    // The audit owner serializes its domain input. Keep all credential material,
    // including hashes, out of that input while binding replay to the exact RPC.
    // The OIDC sign-ins it links carry no credential, and the audit names them.
    // A creation without them keeps the digest it had before they existed, so
    // a retry of one recorded then still gets its result.
    let audit = match input.identities.as_deref() {
        Some(linked) if !linked.is_empty() => serde_json::json!({
            "requestDigest": identity.request_digest,
            "identities": linked,
        }),
        _ => serde_json::json!({"requestDigest": identity.request_digest}),
    };
    let safe_identity = Mutation {
        request_digest: digest(&audit)?,
        ..identity.clone()
    };
    // The caller is already authorized. A retry of a creation that completed
    // gets its recorded result before the password meets the current minimum,
    // which an administrator may have raised since. The write checks again.
    if let Some(previous) = store.recorded(&safe_identity, "rpc.users.create")? {
        return Ok(previous);
    }
    let hash = match &input.password {
        Some(password) => {
            let minimum = super::sign_in::read(&*store.reader()?)?.password_min_length;
            Some(hash_password(password.as_str(), minimum.0)?)
        }
        None => None,
    };
    store.mutate(
        &safe_identity,
        "rpc.users.create",
        &audit,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManageAccounts,
            )?;
            let principal = id();
            insert_user(connection, &principal, now)?;
            insert_local_account(
                connection,
                &principal,
                &username,
                hash.as_deref(),
                &AuthUserProfileUpdateRequest {
                    display_name: Some(input.display_name.clone()),
                    given_name: input.given_name.clone(),
                    family_name: input.family_name.clone(),
                    email: input.email.clone(),
                    clear_email: None,
                    picture_provider_id: None,
                    use_gravatar: None,
                    expected_revision: AuthRevision(1u64.into()),
                    idempotency_key: input.idempotency_key.clone(),
                    extra: Default::default(),
                },
                now,
            )?;
            // Any refused sign-in rolls the whole creation back.
            for linked in input.identities.iter().flatten() {
                if !has_identity(connection, &principal, linked)? {
                    SqliteAuthorizationStore::link_oidc_identity_in(
                        connection,
                        &principal,
                        &linked.provider_id.to_string(),
                        &linked.subject,
                        now,
                    )?;
                }
            }
            Ok(AuthUserGetResponse {
                user: read_user(store, connection, &principal, true)?,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn update_user(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserUpdateRequest,
    now: i64,
) -> Result<AuthUserGetResponse, AuthError> {
    store.mutate(identity, "rpc.users.update", input, now, |connection| {
        let principal = input.user_id.to_string();
        read_user(store, connection, &principal, true)?;
        apply_policy_in_transaction(
            connection,
            identity,
            &PolicyMutation::PrincipalState {
                principal_id: principal.clone(),
                expected_revision: input.expected_revision.0 .0,
                state: if input.enabled { "active" } else { "disabled" }.into(),
            },
            now,
        )?;
        if input.display_name.is_some()
            || input.given_name.is_some()
            || input.family_name.is_some()
            || input.email.is_some()
            || input.clear_email.is_some()
        {
            let patch = AuthUserProfileUpdateRequest {
                display_name: input.display_name.clone(),
                given_name: input.given_name.clone(),
                family_name: input.family_name.clone(),
                email: input.email.clone(),
                clear_email: input.clear_email,
                picture_provider_id: None,
                use_gravatar: None,
                expected_revision: input
                    .expected_profile_revision
                    .ok_or_else(|| AuthError::Invalid("profile revision required".into()))?,
                idempotency_key: input.idempotency_key.clone(),
                extra: Default::default(),
            };
            super::profiles::write(connection, &principal, Some(&patch), now)?;
        }
        Ok(AuthUserGetResponse {
            user: read_user(store, connection, &principal, true)?,
            extra: Default::default(),
        })
    })
}

/// Delete an account: revoke the principal, which signs it out everywhere, then
/// remove its sign-ins, profile and access. The principal row stays behind as
/// a tombstone for the security audit that names it, and its username is free
/// for a new account.
pub(super) fn delete_user(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserDeleteRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(identity, "rpc.users.delete", input, now, |connection| {
        let principal = input.user_id.to_string();
        read_user(store, connection, &principal, true)?;
        let revision = apply_policy_in_transaction(
            connection,
            identity,
            &PolicyMutation::PrincipalState {
                principal_id: principal.clone(),
                expected_revision: input.expected_revision.0 .0,
                state: "revoked".into(),
            },
            now,
        )?;
        for table in [
            "auth_local_credentials",
            "auth_provider_identities",
            "auth_user_profiles",
            "auth_avatar_cache",
            "auth_role_assignments",
            "auth_direct_capabilities",
            "auth_remembered_consent",
        ] {
            connection.execute(
                &format!("DELETE FROM {table} WHERE principal_id=?1"),
                [&principal],
            )?;
        }
        connection.execute(
            "DELETE FROM auth_account_flows WHERE target_principal_id=?1",
            [&principal],
        )?;
        Ok(AuthMutationResult {
            revision: AuthRevision(revision.into()),
            enforcement: None,
            extra: Default::default(),
        })
    })
}

pub(super) fn update_profile(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthUserProfileUpdateRequest,
    now: i64,
) -> Result<AuthUserGetResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.users.profile.update",
        input,
        now,
        |connection| {
            let active: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_principals \
                WHERE principal_id=?1 \
                AND kind='user' AND state='active')",
                [&identity.actor],
                |row| row.get(0),
            )?;
            if !active {
                return Err(AuthError::Denied);
            }
            super::profiles::write(connection, &identity.actor, Some(input), now)?;
            Ok(AuthUserGetResponse {
                user: read_user(store, connection, &identity.actor, true)?,
                extra: Default::default(),
            })
        },
    )
}

/// Hashes a new local password at least `minimum` characters long.
///
/// Characters are Unicode scalar values, the unit the Portal and Console count.
/// The separate 4096-byte cap bounds the hashing work a request can cause.
pub(super) fn hash_password(password: &str, minimum: u32) -> Result<String, AuthError> {
    if password.len() > 4096 || password.chars().count() < minimum as usize {
        return Err(AuthError::Invalid("invalid password length".into()));
    }
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|_| AuthError::Unavailable)?
        .to_string())
}

pub(super) fn change_password(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthPasswordChangeRequest,
    login: &str,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    if identity.request_digest != digest(input)? {
        return Err(AuthError::Invalid(
            "mutation digest does not match its input".into(),
        ));
    }
    let audit = serde_json::json!({"requestDigest": identity.request_digest});
    let safe_identity = Mutation {
        request_digest: digest(&audit)?,
        ..identity.clone()
    };
    if let Some(previous) = store.recorded(&safe_identity, "rpc.users.password.change")? {
        return Ok(previous);
    }
    let (hash, version): (String, u64) = store.transaction(|connection| {
        connection
            .query_row(
                "SELECT c.password_hash,c.version \
                FROM auth_local_credentials c \
                JOIN auth_principals p USING(principal_id) \
                JOIN auth_login_sessions l USING(principal_id) \
                WHERE c.principal_id=?1 \
                AND c.password_hash IS NOT NULL \
                AND p.state='active' \
                AND l.login_session_id=?2 \
                AND l.revoked_at IS NULL \
                AND l.expires_at>?3",
                params![identity.actor, login, now],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(AuthError::Denied)
    })?;
    if input.current_password.len() > 4096 {
        return Err(AuthError::Denied);
    }
    Argon2::default()
        .verify_password(
            input.current_password.as_bytes(),
            &PasswordHash::new(&hash).map_err(|_| AuthError::Unavailable)?,
        )
        .map_err(|_| AuthError::Denied)?;
    let minimum = super::sign_in::read(&*store.reader()?)?.password_min_length;
    let replacement = hash_password(input.new_password.as_str(), minimum.0)?;
    store.mutate(
        &safe_identity,
        "rpc.users.password.change",
        &audit,
        now,
        |connection| {
            let live: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_login_sessions l \
                JOIN auth_principals p USING(principal_id) \
                WHERE l.login_session_id=?1 \
                AND l.principal_id=?2 \
                AND l.revoked_at IS NULL \
                AND l.expires_at>?3 \
                AND p.state='active')",
                params![login, identity.actor, now],
                |row| row.get(0),
            )?;
            if !live {
                return Err(AuthError::Denied);
            }
            if connection.execute(
                "UPDATE auth_local_credentials \
                SET password_hash=?1,version=version+1,password_changed_at=?2,\
                updated_at=?2,failed_attempts=0,locked_until=NULL \
                WHERE principal_id=?3 \
                AND version=?4 AND password_hash=?5",
                params![replacement, now, identity.actor, version, hash],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            let mut statement = connection.prepare(
                "SELECT login_session_id \
                FROM auth_login_sessions \
                WHERE principal_id=?1 \
                AND login_session_id!=?2 \
                AND revoked_at IS NULL",
            )?;
            let siblings = statement
                .query_map(params![identity.actor, login], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for sibling in siblings {
                let scope = EnforcementScope::Login(sibling);
                let root = Mutation {
                    actor: identity.actor.clone(),
                    request_id: id(),
                    request_digest: digest(&scope)?,
                    session: identity.session.clone(),
                };
                store.revoke_root_in_transaction(connection, &root, &scope, now)?;
            }
            Ok(AuthMutationResult {
                revision: AuthRevision((version + 1).into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn read_provider(
    connection: &Connection,
    provider: &str,
) -> Result<AuthOIDCProvider, AuthError> {
    let (issuer, title, client, secret, config, freshness, state, revision, certificate) =
        connection
            .query_row(
                "SELECT issuer,display_name,client_id,sealed_client_secret IS NOT NULL,\
                config_json,claim_freshness_seconds,state,revision,issuer_certificate \
                FROM auth_oidc_providers \
                WHERE provider_id=?1 \
                AND state!='deleted'",
                [provider],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, u32>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, u64>(7)?,
                        row.get::<_, Option<Vec<u8>>>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::NotFound)?;
    let config: serde_json::Value = serde_json::from_str(&config)?;
    Ok(AuthOIDCProvider {
        provider_id: provider.parse().map_err(|_| AuthError::Unavailable)?,
        issuer,
        title,
        client_id: client,
        has_client_secret: secret,
        certificate_fingerprint: certificate
            .map(|certificate| trellis_rs::auth::certificate_fingerprint(&certificate.into())),
        scopes: config
            .get("scopes")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default(),
        role_claim: config
            .get("roleClaim")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        prompt_after_sign_out: config
            .get("promptAfterSignOut")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or(AuthOIDCSignOutPrompt::Login),
        authorization_parameters: config
            .get("authorizationParameters")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default(),
        claim_freshness_seconds: freshness.into(),
        enabled: state == "active",
        revision: AuthRevision(revision.into()),
        extra: Default::default(),
    })
}

pub(super) fn read_providers(
    connection: &Connection,
    input: &AuthPageRequest,
) -> Result<AuthOIDCProvidersListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT provider_id FROM auth_oidc_providers \
        WHERE state!='deleted' \
        AND provider_id>?1 AND (?2 IS NULL \
        OR instr(lower(display_name),lower(?2))>0 \
        OR instr(lower(issuer),lower(?2))>0) \
        ORDER BY provider_id \
        LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.search.as_ref().map(|value| value.as_ref()),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|id| read_provider(connection, id))
            .collect::<Result<Vec<_>, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

pub(super) fn enable_provider(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCProviderMutationRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    set_provider_state(store, identity, input, "active", now)
}

pub(super) fn disable_provider(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCProviderMutationRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    set_provider_state(store, identity, input, "disabled", now)
}

pub(super) fn delete_provider(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCProviderMutationRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    set_provider_state(store, identity, input, "deleted", now)
}

fn set_provider_state(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCProviderMutationRequest,
    state: &str,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        &format!("rpc.oidcProviders.{state}"),
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePrincipals,
            )?;
            let provider = input.provider_id.to_string();
            if connection.execute(
                "UPDATE auth_oidc_providers \
                SET state=?1,revision=revision+1,updated_at=?2 \
                WHERE provider_id=?3 \
                AND revision=?4 AND state!='deleted'",
                params![state, now, provider, input.expected_revision.0 .0],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            // The last active provider cannot go while it is the only way in.
            super::sign_in::require_a_way_in(connection, super::sign_in::Change::Provider)?;
            let mut statement = connection.prepare(
                "SELECT DISTINCT principal_id \
                FROM auth_provider_identities \
                WHERE provider=?1",
            )?;
            let owners = statement
                .query_map([&provider], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for owner in owners {
                super::profiles::write(connection, &owner, None, now)?;
            }
            SqliteAuthorizationStore::policy_changed(
                connection,
                &EnforcementScope::OidcProvider(provider),
                now,
            )?;
            Ok(AuthMutationResult {
                revision: AuthRevision((input.expected_revision.0 .0 + 1).into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

fn page_bounds(page: Option<&CursorQuery>) -> Result<(&str, usize), AuthError> {
    let limit = page.and_then(|page| page.limit).unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    Ok((
        page.and_then(|page| page.cursor.as_deref()).unwrap_or(""),
        limit as usize,
    ))
}

pub(super) fn read_mapping(
    connection: &Connection,
    mapping: &str,
) -> Result<AuthOIDCRoleMapping, AuthError> {
    let (provider, value, revision) = connection
        .query_row(
            "SELECT provider_id,claim_value,revision \
            FROM auth_oidc_role_mappings \
            WHERE mapping_id=?1",
            [mapping],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let roles = connection
        .prepare(
            "SELECT role_id FROM auth_oidc_mapping_roles WHERE mapping_id=?1 ORDER BY role_id",
        )?
        .query_map([mapping], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(AuthOIDCRoleMapping {
        mapping_id: mapping.parse().map_err(|_| AuthError::Unavailable)?,
        provider_id: provider.parse().map_err(|_| AuthError::Unavailable)?,
        external_role: value,
        role_ids: roles
            .iter()
            .map(|role| role.parse().map_err(|_| AuthError::Unavailable))
            .collect::<Result<Vec<_>, _>>()?,
        revision: AuthRevision(revision.into()),
        extra: Default::default(),
    })
}

pub(super) fn delete_mapping(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCRoleMappingDeleteRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.oidcRoleMappings.delete",
        input,
        now,
        |connection| {
            let revision = apply_policy_in_transaction(
                connection,
                identity,
                &PolicyMutation::OidcMappingDelete {
                    mapping_id: input.mapping_id.to_string(),
                    expected_revision: input.expected_revision.0 .0,
                },
                now,
            )?;
            Ok(AuthMutationResult {
                revision: AuthRevision(revision.into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

#[cfg(test)]
#[path = "rpc_users_tests.rs"]
mod tests;
