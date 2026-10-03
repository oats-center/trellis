//! Exact grant administration through the real CLI and Auth runtime.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use trellis_rs::auth::{
    complete_local_login, connect_admin_client_async, start_agent_login, StartAgentLoginOpts,
};
use trellis_runtime_apis::apis::trellis_auth_v1::Client as AuthClient;
use trellis_testkit::TrellisTestRuntime;

fn cli(config_home: &Path) -> Command {
    let mut command = Command::new(
        std::env::var_os("TRELLIS_TEST_CLI_BIN")
            .expect("TRELLIS_TEST_CLI_BIN must select the prebuilt CLI"),
    );
    command
        .env("XDG_CONFIG_HOME", config_home)
        .args(["--format", "json"])
        .kill_on_drop(true);
    command
}

async fn output(command: &mut Command) -> std::process::Output {
    tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("CLI deadline")
        .expect("run CLI")
}

#[tokio::test]
async fn identity_grants_set_round_trips_exact_grants_and_rejects_stale_revision() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start live runtime");
    let config_home = runtime.workdir().join("grant-cli-config");

    // Let the CLI own its login and credential persistence; approve its real
    // portal flow with the sandbox administrator's password.
    let mut login = cli(&config_home)
        .args(["login", runtime.trellis_url()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start CLI login");
    let mut lines = BufReader::new(login.stderr.take().expect("login stderr")).lines();
    let login_url = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await.expect("login progress") {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if let Some(url) = value["loginUrl"].as_str() {
                    return url.to_owned();
                }
            }
        }
        panic!("CLI exited without a login URL");
    })
    .await
    .expect("login URL deadline");
    complete_local_login(
        runtime.trellis_url(),
        &login_url,
        runtime.admin_username(),
        runtime.admin_password(),
    )
    .await
    .expect("approve CLI login");
    let status = tokio::time::timeout(Duration::from_secs(30), login.wait())
        .await
        .expect("login deadline")
        .expect("wait for CLI login");
    assert!(status.success(), "CLI login failed");

    let challenge = start_agent_login(&StartAgentLoginOpts {
        trellis_url: runtime.trellis_url(),
        participant_id: "trellis.cli",
    })
    .await
    .expect("start readback login");
    complete_local_login(
        runtime.trellis_url(),
        challenge.login_url(),
        runtime.admin_username(),
        runtime.admin_password(),
    )
    .await
    .expect("approve readback login");
    let state = challenge
        .complete_session(runtime.trellis_url())
        .await
        .expect("bind readback login");
    let connected = connect_admin_client_async(&state)
        .await
        .expect("connect readback client");
    let auth = AuthClient::from_generated(connected.clone());
    let participant_id = "trellis.console";
    let installed = auth
        .participants_get(
            &serde_json::from_value(json!({"participantId": participant_id, "revision": null}))
                .unwrap(),
        )
        .await
        .expect("installed participant");
    let installed = serde_json::to_value(installed.participant).unwrap();
    let created = auth.users_create(&serde_json::from_value(json!({"username": "grant-cli-user", "name": "Grant CLI user", "email": null, "image": null, "idempotencyKey": ulid::Ulid::new().to_string()})).unwrap()).await.expect("create non-bootstrap user");
    let user_id = created.user.user_id.to_string();
    let target = serde_json::from_value(
        json!({"ownerKind":"user", "ownerId":user_id, "participantId":participant_id}),
    )
    .unwrap();
    let input_path = runtime.workdir().join("grant-input.json");
    let installed_revision: u64 = installed["revision"].as_str().unwrap().parse().unwrap();
    let mut input = json!({"installedRevision":installed_revision, "grants":installed["requiredGrants"], "platformPrivileges":["trellis.auth::admin"], "expiresAt":null});
    std::fs::write(&input_path, serde_json::to_vec(&input).unwrap()).unwrap();
    let set_args = [
        "identity",
        "grants",
        "set",
        participant_id,
        "--user",
        &user_id,
        "--input",
        input_path.to_str().unwrap(),
    ];
    let result = output(cli(&config_home).args(set_args)).await;
    assert!(
        result.status.success(),
        "create: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let first = serde_json::to_value(auth.grants_get(&target).await.expect("read created grant"))
        .unwrap()["binding"]
        .clone();
    assert_eq!(first["state"], "active");
    assert_eq!(first["ownerKind"], "user");
    assert_eq!(first["ownerId"], user_id);
    assert_eq!(first["participantId"], participant_id);
    assert_eq!(first["installedRevision"], installed["revision"]);
    assert_eq!(first["approvalMode"], "exact");
    assert_eq!(first["grants"], input["grants"]);
    assert_eq!(first["platformPrivileges"], input["platformPrivileges"]);
    assert_eq!(first["expiresAt"], Value::Null);

    // Replacement uses string input revision and numeric expiry. The explicit
    // expected revision must cross the generated wire boundary successfully.
    input["installedRevision"] = installed["revision"].clone();
    input["grants"]["permissions"] = json!([]);
    input["platformPrivileges"] = json!([]);
    let expiry = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 86_400_000) as u64;
    input["expiresAt"] = json!(expiry);
    std::fs::write(&input_path, serde_json::to_vec(&input).unwrap()).unwrap();
    let revision = first["revision"].as_str().unwrap();
    let result = output(
        cli(&config_home)
            .args(set_args)
            .args(["--expected-revision", revision]),
    )
    .await;
    assert!(
        result.status.success(),
        "replace: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let second = serde_json::to_value(auth.grants_get(&target).await.expect("read replacement"))
        .unwrap()["binding"]
        .clone();
    assert!(
        second["revision"].as_str().unwrap().parse::<u64>().unwrap()
            > revision.parse::<u64>().unwrap()
    );
    assert_eq!(second["grants"], input["grants"]);
    assert_eq!(second["platformPrivileges"], json!([]));
    assert_eq!(second["expiresAt"], expiry.to_string());
    assert_eq!(second["approvalMode"], "exact");

    input["grants"] = installed["requiredGrants"].clone();
    input["expiresAt"] = json!(expiry.to_string());
    std::fs::write(&input_path, serde_json::to_vec(&input).unwrap()).unwrap();
    let result = output(
        cli(&config_home)
            .args(set_args)
            .args(["--expected-revision", revision]),
    )
    .await;
    assert!(!result.status.success(), "stale replacement succeeded");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("revision_conflict"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::to_value(auth.grants_get(&target).await.unwrap()).unwrap()["binding"],
        second
    );

    // Inference must also read an existing binding's revision, and string expiry
    // must normalize exactly like numeric expiry.
    let result = output(cli(&config_home).args(set_args)).await;
    assert!(
        result.status.success(),
        "inferred replacement: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let third =
        serde_json::to_value(auth.grants_get(&target).await.unwrap()).unwrap()["binding"].clone();
    assert_eq!(third["grants"], input["grants"]);
    assert_eq!(third["expiresAt"], expiry.to_string());
    let result = output(
        cli(&config_home)
            .args(set_args)
            .args(["--expected-revision", "0"]),
    )
    .await;
    assert!(
        !result.status.success(),
        "zero revision replaced an existing grant"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("revision_conflict"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = output(
        cli(&config_home)
            .args(set_args)
            .args(["--expected-revision", "9007199254740992"]),
    )
    .await;
    assert!(
        !result.status.success(),
        "out-of-range expected revision succeeded"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("invalid expectedRevision"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    // Bad operator input must fail locally, identify the field, and leave the
    // previously stored authority untouched.
    let mut missing_expiry = input.clone();
    missing_expiry.as_object_mut().unwrap().remove("expiresAt");
    let mut unknown_field = input.clone();
    unknown_field["ownerId"] = json!(user_id);
    let mut invalid_revision = input.clone();
    invalid_revision["installedRevision"] = json!(0);
    let mut oversized_revision = input.clone();
    oversized_revision["installedRevision"] = json!(9_007_199_254_740_992_u64);
    let mut noncanonical_revision = input.clone();
    noncanonical_revision["installedRevision"] = json!("01");
    let mut oversized_expiry = input.clone();
    oversized_expiry["expiresAt"] = json!("9007199254740992");
    let mut fractional_expiry = input.clone();
    fractional_expiry["expiresAt"] = json!(1.5);
    for (invalid, message) in [
        (json!([]), "grant input must be a JSON object"),
        (missing_expiry, "missing field `expiresAt`"),
        (unknown_field, "unknown field `ownerId`"),
        (invalid_revision, "invalid installedRevision"),
        (oversized_revision, "invalid installedRevision"),
        (noncanonical_revision, "invalid installedRevision"),
        (oversized_expiry, "invalid expiresAt"),
        (fractional_expiry, "invalid expiresAt"),
    ] {
        std::fs::write(&input_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let result = output(cli(&config_home).args(set_args)).await;
        assert!(!result.status.success(), "invalid grant input succeeded");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        serde_json::to_value(auth.grants_get(&target).await.unwrap()).unwrap()["binding"],
        third
    );

    drop(auth);
    drop(connected);
    runtime.shutdown().await.expect("shutdown runtime");
}
