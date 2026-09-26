use rusqlite::{params, OptionalExtension};

use super::common::sql_error;
use super::SqliteAuthorizationStore;
use crate::platform::auth::AuthorizationStateError;

impl SqliteAuthorizationStore {
    pub(crate) async fn jetstream_identity(
        &self,
    ) -> Result<(String, bool), AuthorizationStateError> {
        self.run_read(|connection| {
            connection
                .query_row(
                    "SELECT jetstream_id, jetstream_bound FROM trellis_platform_store_marker WHERE id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get::<_, bool>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| {
                    AuthorizationStateError::Storage("platform store identity is missing".to_owned())
                })
        })
        .await
    }

    pub(crate) async fn mark_jetstream_bound(
        &self,
        identity: String,
    ) -> Result<(), AuthorizationStateError> {
        self.run(move |connection| {
            let updated = connection
                .execute(
                    "UPDATE trellis_platform_store_marker SET jetstream_bound = 1 WHERE id = 1 AND jetstream_id = ?1",
                    params![identity],
                )
                .map_err(sql_error)?;
            if updated != 1 {
                return Err(AuthorizationStateError::Storage(
                    "platform store identity changed during JetStream binding".to_owned(),
                ));
            }
            Ok(())
        })
        .await
    }
}
