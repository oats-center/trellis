//! Native aggregate policy mutations and projections.

use super::Administration;
use crate::platform::auth::blocking;
use std::sync::Arc;

use rusqlite::OptionalExtension;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{__types::trellis::*, apis::trellis_auth_v1::Provider};

use super::{
    policy::apply_policy_in_transaction,
    rpc::{mutation, rpc_error},
    rpc_users::read_mapping,
    sqlite::{decode, id},
    AuthError, Mutation, PolicyMutation, SqliteAuthorizationStore,
};

/// Register multi-role OIDC mapping writes.
pub(crate) fn register_policy_aggregate_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    macro_rules! register_mutation {
        ($registration:ident, $aggregate:ident, $privilege:ident) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(Administration::$privilege),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    blocking(move || {
                        $aggregate(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(rpc_error)
                }
            });
        }};
    }
    register_mutation!(register_oidc_role_mappings_put, put_mapping, ManageRoles);
}

pub(super) fn put_mapping(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCRoleMappingPutRequest,
    now: i64,
) -> Result<AuthOIDCRoleMappingPutResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.oidcRoleMappings.put",
        input,
        now,
        |connection| {
            let mapping_id = input
                .mapping_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(id);
            let config: String = connection
                .query_row(
                    "SELECT config_json FROM auth_oidc_providers \
                    WHERE provider_id=?1 \
                    AND state='active'",
                    [input.provider_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            let config: serde_json::Value = decode(&config)?;
            let claim_name = config
                .get("roleClaim")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| AuthError::Invalid("provider has no role claim".into()))?;
            apply_policy_in_transaction(
                connection,
                identity,
                &PolicyMutation::OidcMappingPut {
                    mapping_id: mapping_id.clone(),
                    provider_id: input.provider_id.to_string(),
                    claim_name: claim_name.into(),
                    claim_value: input.external_role.to_string(),
                    role_ids: input.role_ids.iter().map(ToString::to_string).collect(),
                    expected_revision: input.expected_revision.0,
                },
                now,
            )?;
            Ok(AuthOIDCRoleMappingPutResponse {
                mapping: read_mapping(connection, &mapping_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}
