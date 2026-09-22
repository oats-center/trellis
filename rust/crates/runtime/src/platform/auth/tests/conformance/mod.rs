mod accounts;
pub(crate) mod fixtures;
mod portals;

use crate::platform::auth::SqliteAuthorizationStore;

#[tokio::test]
async fn sqlite_accounts_conform() {
    accounts::exercise_accounts(SqliteAuthorizationStore::open_in_memory().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_portals_conform() {
    portals::exercise_portals(SqliteAuthorizationStore::open_in_memory().unwrap())
        .await
        .unwrap();
}
