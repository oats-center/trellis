use runtime_trellis::participants::runtime_trellis_provider::{
    types::ResourceValue, Participant, Provider,
};
use std::io::Write as _;
use trellis_rs::client::{UserConnectOptions, UserSessionCredentials};
use trellis_rs::service::{KvResourceReadError, ServiceConnectOptions, StoreListOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    if std::env::var_os("TRELLIS_STATE_ACCEPTANCE").is_some() {
        return state_acceptance().await;
    }
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut runtime = Participant::connect(ServiceConnectOptions::new(&url, &identity)).await?;
    let client = Provider::new(&mut runtime).client();

    let kv = client.records().await?;
    let _ = kv.delete("record", None).await;
    let created = kv
        .create(
            "record",
            &ResourceValue {
                value: "one".into(),
                extra: Default::default(),
            },
        )
        .await?;
    assert_eq!(kv.get("record").await?.expect("KV value").value, "one");
    kv.replace(
        "record",
        created.revision,
        &ResourceValue {
            value: "two".into(),
            extra: Default::default(),
        },
    )
    .await?;
    assert!(kv
        .replace(
            "record",
            created.revision,
            &ResourceValue {
                value: "stale".into(),
                extra: Default::default(),
            },
        )
        .await
        .is_err());
    kv.put(
        "record",
        &ResourceValue {
            value: "three".into(),
            extra: Default::default(),
        },
    )
    .await?;
    let current = kv.get_entry("record").await?.expect("KV entry");
    assert_eq!(current.value.expect("active value").value, "three");
    kv.delete("record", Some(current.revision)).await?;
    assert!(kv.get("record").await?.is_none());
    assert!(kv
        .history("record")
        .await?
        .iter()
        .any(|entry| entry.value.is_none()));

    let consumer = client.changes()?;
    assert!(!consumer.binding().stream.is_empty());
    assert!(!consumer.binding().consumer_name.is_empty());
    assert!(!consumer.binding().filter_subjects.is_empty());

    let files = client.files().await?;
    let keys = (0..502)
        .map(|index| format!("page/{index:03}"))
        .collect::<Vec<_>>();
    for key in &keys {
        files.write(key, vec![1]).await?;
    }
    let maximum = files
        .list_page(StoreListOptions {
            prefix: "page/".into(),
            cursor: None,
            limit: Some(500),
        })
        .await?;
    assert_eq!(maximum.entries.len(), 500);
    assert!(maximum.next_cursor.is_some());
    let first = files
        .list_page(StoreListOptions {
            prefix: "page/".into(),
            cursor: None,
            limit: None,
        })
        .await?;
    assert_eq!(first.entries.len(), 100);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| &entry.key)
            .collect::<Vec<_>>(),
        keys[..100].iter().collect::<Vec<_>>()
    );
    files.write("page/050a", vec![1]).await?;
    files.delete("page/000").await?;
    let cursor = first.next_cursor.expect("first page continuation");
    let second = files
        .list_page(StoreListOptions {
            prefix: "page/".into(),
            cursor: Some(cursor.clone()),
            limit: Some(500),
        })
        .await?;
    assert!(second.next_cursor.is_none());
    let seen = first
        .entries
        .iter()
        .chain(&second.entries)
        .map(|entry| entry.key.clone())
        .collect::<Vec<_>>();
    assert_eq!(seen, keys);
    assert_eq!(
        seen.iter().collect::<std::collections::HashSet<_>>().len(),
        seen.len()
    );
    for limit in [0, 501] {
        assert!(files
            .list_page(StoreListOptions {
                prefix: String::new(),
                cursor: None,
                limit: Some(limit),
            })
            .await
            .is_err());
    }
    assert!(files
        .list_page(StoreListOptions {
            prefix: String::new(),
            cursor: Some("malformed".into()),
            limit: None,
        })
        .await
        .is_err());
    assert!(files
        .list_page(StoreListOptions {
            prefix: "other/".into(),
            cursor: Some(cursor),
            limit: None,
        })
        .await
        .is_err());

    println!("rust resources ready");
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        let mut replacement = String::new();
        std::io::stdin().read_line(&mut replacement)
    })
    .await??;
    for _ in 0..60 {
        let result = kv.get("record").await;
        if matches!(result, Err(KvResourceReadError::Unavailable)) {
            assert!(consumer.messages().await.is_err());
            println!("rust resources invalidated");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    return Err("resource handles remained available after replacement".into());
}

async fn state_acceptance() -> Result<(), Box<dyn std::error::Error>> {
    use runtime_trellis::participants::runtime_trellis_state_caller::{
        types::ResourceValue, Client,
    };
    use trellis_rs::auth::{load_admin_session, start_agent_login, StartAgentLoginOpts};
    use trellis_rs::client::StateWriteError;

    let url = std::env::var("TRELLIS_URL")?;
    let challenge = start_agent_login(&StartAgentLoginOpts {
        trellis_url: &url,
        participant_id: "runtime-trellis.StateCaller",
    })
    .await?;
    println!("rust login {}", challenge.login_url());
    std::io::stdout().flush()?;
    let outcome = challenge.complete_without_persistence(&url).await?;
    trellis_rs::auth::save_admin_session(&outcome.state)?;
    let session = load_admin_session()?;
    let client = Client::connect(UserConnectOptions::new(
        &url,
        5_000,
        UserSessionCredentials {
            login_session_id: &session.login_session_id,
            session_key_seed_base64url: &session.session_seed,
        },
        "runtime-trellis.StateCaller",
    ))
    .await?;
    let state = client.saved_resource()?;
    let created = state
        .create(&ResourceValue {
            value: "one".into(),
            extra: Default::default(),
        })
        .await?;
    assert_eq!(state.get().await?.expect("State value").value.value, "one");
    assert!(matches!(
        state
            .create(&ResourceValue {
                value: "duplicate".into(),
                extra: Default::default(),
            })
            .await,
        Err(StateWriteError::Conflict { .. })
    ));
    let set = state
        .set(&ResourceValue {
            value: "two".into(),
            extra: Default::default(),
        })
        .await?;
    assert!(matches!(
        state
            .replace(
                created.revision,
                &ResourceValue {
                    value: "stale".into(),
                    extra: Default::default(),
                },
            )
            .await,
        Err(StateWriteError::Conflict { .. })
    ));
    let replaced = state
        .replace(
            set.revision,
            &ResourceValue {
                value: "three".into(),
                extra: Default::default(),
            },
        )
        .await?;
    assert!(matches!(
        state.delete(Some(set.revision)).await,
        Err(StateWriteError::Conflict { .. })
    ));
    state.delete(Some(replaced.revision)).await?;
    assert!(state.get().await?.is_none());

    println!("rust state complete");
    std::io::stdout().flush()?;
    Ok(())
}
