use rusqlite::{Connection, OptionalExtension};

use super::super::authority::validate_persisted_principal;
use super::super::{AuthorizationStateError, PrincipalRecord};
use super::common::{decode_enum, from_sql_version, sql_error};

pub(in crate::platform::auth) fn load_principal(
    connection: &Connection,
    id: &str,
) -> Result<Option<PrincipalRecord>, AuthorizationStateError> {
    let principal = connection
        .query_row(
            "SELECT principal_id, kind, state, created_at, updated_at, version,
                disabled_at, revoked_at
         FROM auth_principals WHERE principal_id = ?1",
            [id],
            |row| {
                Ok(PrincipalRecord {
                    principal_id: row.get(0)?,
                    kind: decode_enum(row.get::<_, String>(1)?)?,
                    state: decode_enum(row.get::<_, String>(2)?)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    version: from_sql_version(row.get(5)?)?,
                    disabled_at: row.get(6)?,
                    revoked_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)?;
    principal.map_or(Ok(None), |principal| {
        validate_persisted_principal(&principal)?;
        Ok(Some(principal))
    })
}
