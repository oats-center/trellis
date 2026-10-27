//! How people may sign in to this installation, kept in the Auth store and
//! edited live through `Auth.SignInSettings`. Apps narrow self sign-up further
//! (`Auth.OAuthClients.SignUp.Update`); they never widen what this allows.

use std::sync::Arc;

use rusqlite::params;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{__types::trellis::*, apis::trellis_auth_v1::Provider};

use super::{
    blocking,
    rpc::{caller, mutation, rpc_error},
    sqlite::Connection,
    Administration, AuthError, Mutation, SqliteAuthorizationStore,
};

/// The installation's current sign-in settings.
pub(super) fn read(connection: &Connection) -> Result<AuthSignInSettings, AuthError> {
    Ok(connection.query_row(
        "SELECT local_users,local_sign_up,providers,revision \
        FROM auth_sign_in_settings WHERE singleton=1",
        [],
        |row| {
            Ok(AuthSignInSettings {
                local_users: row.get(0)?,
                local_sign_up: row.get(1)?,
                providers: row.get(2)?,
                revision: AuthRevision(row.get::<_, u64>(3)?.into()),
                extra: Default::default(),
            })
        },
    )?)
}

pub(super) fn update(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthSignInSettingsUpdateRequest,
    now: i64,
) -> Result<AuthSignInSettingsGetResponse, AuthError> {
    // Sign-up creates local users, and turning both methods off would leave
    // nobody, administrators included, able to sign in.
    if input.local_sign_up && !input.local_users {
        return Err(AuthError::Invalid("local sign-up needs local users".into()));
    }
    if !input.local_users && !input.providers {
        return Err(AuthError::Invalid(
            "keep local users or company sign-in on".into(),
        ));
    }
    store.mutate(
        identity,
        "rpc.signInSettings.update",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePlatform,
            )?;
            if connection.execute(
                "UPDATE auth_sign_in_settings \
            SET local_users=?1,local_sign_up=?2,providers=?3,\
            revision=revision+1,updated_at=?4 \
            WHERE singleton=1 AND revision=?5",
                params![
                    input.local_users,
                    input.local_sign_up,
                    input.providers,
                    now,
                    input.expected_revision.0 .0
                ],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            Ok(AuthSignInSettingsGetResponse {
                settings: read(connection)?,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn register_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    let getter = store.clone();
    provider.register_sign_in_settings_get(move |context, _| {
        let store = getter.clone();
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePlatform)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePlatform,
                    )?;
                    Ok(AuthSignInSettingsGetResponse {
                        settings: read(connection)?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_sign_in_settings_update(move |context, input| {
        let store = store.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePlatform),
                &input,
            )
            .map_err(rpc_error)?;
            blocking(move || update(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });
}
