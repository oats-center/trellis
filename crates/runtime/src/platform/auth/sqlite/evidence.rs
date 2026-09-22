use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use super::super::authority::{
    validate_deployment_evidence, validate_device, validate_device_delegation,
    validate_runtime_instance, validate_session_runtime_binding, AuthorityEvidenceRepository,
};
use super::super::{
    AuthorizationStateError, DeploymentRecord, DeviceDelegationRecord, DeviceRecord,
    GrantOwnerKind, PrincipalKind, ResourceBindingEvidence, RuntimeInstanceRecord,
    SessionRuntimeBinding,
};
use super::common::{
    decode_enum, encode_enum, encode_json, from_sql_version, map_write_error, sql_error,
};
use super::grants::load_installed_participant;
use super::principals::load_principal;
use super::SqliteAuthorizationStore;

impl SqliteAuthorizationStore {
    pub(crate) async fn replace_resource_bindings(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
        installed_revision: u64,
        evidence: Vec<ResourceBindingEvidence>,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            replace_sql_resource_bindings(
                &transaction,
                owner_kind,
                &owner_id,
                &participant_id,
                installed_revision,
                &evidence,
            )?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }
}

pub(in crate::platform::auth) fn replace_sql_resource_bindings(
    connection: &Connection,
    owner_kind: GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
    installed_revision: u64,
    evidence: &[ResourceBindingEvidence],
) -> Result<(), AuthorizationStateError> {
    if load_installed_participant(connection, participant_id, Some(installed_revision))?.is_none() {
        return Err(AuthorizationStateError::ParticipantMissing);
    }
    connection.execute(
        "DELETE FROM auth_resource_binding_evidence
         WHERE owner_kind = ?1 AND owner_id = ?2 AND participant_id = ?3 AND installed_revision = ?4",
        params![encode_enum(owner_kind)?, owner_id, participant_id, installed_revision],
    ).map_err(map_write_error)?;
    for item in evidence {
        connection
            .execute(
                "INSERT INTO auth_resource_binding_evidence (
                owner_kind, owner_id, participant_id, installed_revision,
                resource_kind, local_name, binding_id, provider_identity, actual_json,
                state, materialized_at, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    encode_enum(owner_kind)?,
                    owner_id,
                    participant_id,
                    installed_revision,
                    item.resource_kind,
                    item.local_name,
                    item.binding_id,
                    encode_json(&item.provider_identity)?,
                    item.actual.as_ref().map(encode_json).transpose()?,
                    encode_enum(item.state)?,
                    item.materialized_at,
                    item.error,
                ],
            )
            .map_err(map_write_error)?;
    }
    Ok(())
}

#[async_trait]
impl AuthorityEvidenceRepository for SqliteAuthorizationStore {
    async fn list_runtime_instances(
        &self,
    ) -> Result<Vec<RuntimeInstanceRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare("SELECT instance_id FROM auth_instances ORDER BY instance_id")
                .map_err(sql_error)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            ids.into_iter()
                .map(|instance_id| {
                    load_runtime_instance(connection, &instance_id)?
                        .ok_or(AuthorizationStateError::StorageConflict)
                })
                .collect()
        })
        .await
    }

    async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT principal_id, deployment_id, state, created_at, updated_at, version
                     FROM auth_devices ORDER BY deployment_id, principal_id",
                )
                .map_err(sql_error)?;
            let records = statement
                .query_map([], |row| {
                    Ok(DeviceRecord {
                        principal_id: row.get(0)?,
                        deployment_id: row.get(1)?,
                        state: decode_enum(row.get(2)?)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        version: from_sql_version(row.get(5)?)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            Ok(records)
        })
        .await
    }

    async fn get_deployment_evidence(
        &self,
        deployment_id: &str,
    ) -> Result<Option<DeploymentRecord>, AuthorizationStateError> {
        let deployment_id = deployment_id.to_owned();
        self.run_read(move |connection| load_deployment(connection, &deployment_id))
            .await
    }

    async fn put_deployment_evidence(
        &self,
        deployment: DeploymentRecord,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| put_sql_deployment_evidence(connection, deployment))
            .await
    }

    async fn get_runtime_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<RuntimeInstanceRecord>, AuthorizationStateError> {
        let instance_id = instance_id.to_owned();
        self.run_read(move |connection| load_runtime_instance(connection, &instance_id))
            .await
    }

    async fn get_device(
        &self,
        principal_id: &str,
        deployment_id: &str,
    ) -> Result<Option<DeviceRecord>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        let deployment_id = deployment_id.to_owned();
        self.run_read(move |connection| load_device(connection, &principal_id, &deployment_id))
            .await
    }

    async fn get_device_delegation(
        &self,
        principal_id: &str,
        deployment_id: &str,
    ) -> Result<Option<DeviceDelegationRecord>, AuthorizationStateError> {
        let principal_id = principal_id.to_owned();
        let deployment_id = deployment_id.to_owned();
        self.run_read(move |connection| {
            load_device_delegation(connection, &principal_id, &deployment_id)
        })
        .await
    }

    async fn get_session_runtime_binding(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionRuntimeBinding>, AuthorizationStateError> {
        let session_id = session_id.to_owned();
        self.run_read(move |connection| load_session_runtime_binding(connection, &session_id))
            .await
    }
}

pub(in crate::platform::auth) fn put_sql_deployment_evidence(
    connection: &Connection,
    deployment: DeploymentRecord,
) -> Result<(), AuthorizationStateError> {
    if load_deployment(connection, &deployment.deployment_id)?.is_some_and(|existing| {
        existing.participant_id != deployment.participant_id
            || existing.participant_kind != deployment.participant_kind
    }) {
        return Err(AuthorizationStateError::InvalidRecord(
            "deployment participant identity cannot change".to_owned(),
        ));
    }
    connection
        .execute(
            "INSERT INTO auth_deployments (
            deployment_id, participant_id, participant_kind, state, expires_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(deployment_id) DO UPDATE SET
            state = excluded.state,
            expires_at = excluded.expires_at",
            params![
                deployment.deployment_id,
                deployment.participant_id,
                encode_enum(deployment.participant_kind)?,
                if deployment.active {
                    "active"
                } else {
                    "disabled"
                },
                deployment.expires_at,
            ],
        )
        .map_err(map_write_error)?;
    Ok(())
}

pub(in crate::platform::auth) fn load_runtime_instance(
    connection: &Connection,
    instance_id: &str,
) -> Result<Option<RuntimeInstanceRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT instance_id, deployment_id, principal_id, state, created_at, updated_at, version
         FROM auth_instances WHERE instance_id = ?1",
        [instance_id],
        |row| {
            Ok(RuntimeInstanceRecord {
                instance_id: row.get(0)?,
                deployment_id: row.get(1)?,
                principal_id: row.get(2)?,
                state: decode_enum(row.get(3)?)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                version: from_sql_version(row.get(6)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)
    .and_then(|instance| {
        instance.map_or(Ok(None), |instance| {
            validate_runtime_instance(&instance)?;
            Ok(Some(instance))
        })
    })
}

pub(in crate::platform::auth) fn load_device(
    connection: &Connection,
    principal_id: &str,
    deployment_id: &str,
) -> Result<Option<DeviceRecord>, AuthorizationStateError> {
    connection
    .query_row(
        "SELECT principal_id, deployment_id, state, created_at, updated_at, version FROM auth_devices
         WHERE principal_id = ?1 AND deployment_id = ?2",
        params![principal_id, deployment_id],
        |row| {
            Ok(DeviceRecord {
                principal_id: row.get(0)?,
                deployment_id: row.get(1)?,
                state: decode_enum(row.get(2)?)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                version: from_sql_version(row.get(5)?)?,
            })
        },
    )
    .optional()
    .map_err(sql_error)
    .and_then(|device| {
        device.map_or(Ok(None), |device| {
            validate_device(&device)?;
            Ok(Some(device))
        })
    })
}

pub(in crate::platform::auth) fn load_device_delegation(
    connection: &Connection,
    principal_id: &str,
    deployment_id: &str,
) -> Result<Option<DeviceDelegationRecord>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT principal_id, deployment_id, companion_participant_id,
                user_login_session_id, installation_public_key, device_grant_revision,
                child_grant_revision, required, state, expires_at
         FROM auth_device_delegations
         WHERE principal_id = ?1 AND deployment_id = ?2",
            params![principal_id, deployment_id],
            |row| {
                Ok(DeviceDelegationRecord {
                    principal_id: row.get(0)?,
                    deployment_id: row.get(1)?,
                    companion_participant_id: row.get(2)?,
                    user_login_session_id: row.get(3)?,
                    installation_public_key: row.get(4)?,
                    device_grant_revision: row.get(5)?,
                    child_grant_revision: row.get(6)?,
                    required: row.get(7)?,
                    state: decode_enum(row.get(8)?)?,
                    expires_at: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)
        .and_then(|delegation| {
            delegation.map_or(Ok(None), |delegation| {
                validate_device_delegation(&delegation)?;
                Ok(Some(delegation))
            })
        })
}

pub(in crate::platform::auth) fn load_session_runtime_binding(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<SessionRuntimeBinding>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT session_id, deployment_id, instance_id
         FROM auth_session_runtime_bindings WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok(SessionRuntimeBinding {
                    session_id: row.get(0)?,
                    deployment_id: row.get(1)?,
                    instance_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)
        .and_then(|binding| {
            binding.map_or(Ok(None), |binding| {
                validate_session_runtime_binding(&binding)?;
                Ok(Some(binding))
            })
        })
}

pub(in crate::platform::auth) fn validate_sql_device_relationships(
    connection: &Connection,
    device: &DeviceRecord,
) -> Result<(), AuthorizationStateError> {
    let deployment = load_deployment(connection, &device.deployment_id)?
        .ok_or(AuthorizationStateError::DeploymentInactive)?;
    let principal = load_principal(connection, &device.principal_id)?
        .ok_or(AuthorizationStateError::PrincipalMissing)?;
    if principal.kind != PrincipalKind::Device
        || deployment.participant_kind != trellis_protocol::ParticipantKind::Device
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "device evidence requires a device principal and deployment".to_owned(),
        ));
    }
    Ok(())
}

pub(in crate::platform::auth) fn load_deployment(
    connection: &Connection,
    deployment_id: &str,
) -> Result<Option<DeploymentRecord>, AuthorizationStateError> {
    connection
        .query_row(
            "SELECT deployment_id, participant_id, participant_kind, state, expires_at
         FROM auth_deployments WHERE deployment_id = ?1",
            [deployment_id],
            |row| {
                Ok(DeploymentRecord {
                    deployment_id: row.get(0)?,
                    participant_id: row.get(1)?,
                    participant_kind: decode_enum(row.get::<_, String>(2)?)?,
                    active: row.get::<_, String>(3)? == "active",
                    expires_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)
        .and_then(|deployment| {
            deployment.map_or(Ok(None), |deployment| {
                validate_deployment_evidence(&deployment)?;
                Ok(Some(deployment))
            })
        })
}
