//! A service moves between releases with `trellis svc <ID> export` and
//! `trellis svc import`, each run by the CLI of the release its installation
//! runs. A bundle Trellis 0.100.0-rc.55 exported, in the format from before
//! bundles carried roles and grants, imports into the build under test,
//! whose report says what the bundle did not carry. A bundle the build under
//! test exports imports into the previous release, its role with it, and so
//! does one with a field a newer Trellis may add, which that release reads
//! past.
//!
//! Needs `TRELLIS_TEST_SERVER_BIN` and `TRELLIS_TEST_CLI_BIN`, the build under
//! test; the earlier releases are downloaded and checked against their
//! published checksums.
use std::path::{Path, PathBuf};
use std::time::Duration;

use runtime_trellis::apis::runtime_trellis_moving_v1::rpc::Ping;
use runtime_trellis::participants::runtime_trellis_moving_service::{
    Participant as Moving, Provider as MovingProvider,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::Client;
use trellis_rs::service::ServiceConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1;
use trellis_testkit::{NativeSource, TestServiceIdentity, TrellisTestRuntime};

/// The last release whose bundles have container format 1.
const FORMAT_ONE: &str = "0.100.0-rc.55";
const PASSPHRASE: &str = "the passphrase of a moving service\n";

/// The build under test.
fn current() -> NativeSource {
    NativeSource::Path {
        server: std::env::var_os("TRELLIS_TEST_SERVER_BIN")
            .expect("TRELLIS_TEST_SERVER_BIN names the build under test")
            .into(),
    }
}

/// The `trellis` CLI of one runtime's release, signed in to it as its
/// administrator, with a configuration of its own.
struct Cli {
    program: PathBuf,
    config: PathBuf,
}

impl Cli {
    async fn login(runtime: &mut TrellisTestRuntime) -> Self {
        let cli = Self {
            program: runtime.cli().await.unwrap(),
            config: runtime
                .workdir()
                .join(format!("cli-{}", ulid::Ulid::generate())),
        };
        std::fs::create_dir_all(&cli.config).unwrap();
        let mut login = cli
            .command()
            .args(["login", runtime.trellis_url(), "--ca-certificate"])
            .arg(&runtime.trust_roots()[0])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(login.stderr.take().unwrap()).lines();
        let challenge: Value = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let line = lines.next_line().await.unwrap().expect("a challenge");
                if line.starts_with('{') {
                    return serde_json::from_str(&line).unwrap();
                }
            }
        })
        .await
        .unwrap();
        runtime
            .approve_device_login_with_administration(
                "trellis.cli",
                challenge["userCode"].as_str().unwrap(),
            )
            .await
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(30), login.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "login failed");
        cli
    }

    /// `trellis --format json`, as a script runs it.
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(["--format", "json"])
            .env("XDG_CONFIG_HOME", &self.config)
            .kill_on_drop(true);
        command
    }

    /// The JSON a command prints, which must succeed within a deadline.
    async fn json(&self, args: &[&str]) -> Value {
        let output = tokio::time::timeout(
            Duration::from_secs(120),
            self.command()
                .args(args)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await
        .expect("the command finishes")
        .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// In `runtime`: the service `moving` with settings, and a role that gives
/// its API.
async fn moving_service(runtime: &mut TrellisTestRuntime, cli: &Cli) -> TestServiceIdentity {
    let service = runtime.register_service::<Moving>("moving").await.unwrap();
    runtime
        .set_config(
            &service,
            json!({ "label": "moved label" }),
            json!({ "apiKey": "moved-key-0123456789" }),
        )
        .await
        .unwrap();
    cli.json(&[
        "roles",
        "create",
        "--title",
        "Pingers",
        "--capability",
        Ping::CALLER_CAPABILITIES[0],
    ])
    .await;
    service
}

/// Export `service` with `cli` into a new file in `runtime`'s work directory,
/// and return the file and its passphrase file.
async fn export(
    runtime: &TrellisTestRuntime,
    cli: &Cli,
    service: &TestServiceIdentity,
) -> (PathBuf, PathBuf) {
    let work = runtime.workdir().join("move");
    std::fs::create_dir_all(&work).unwrap();
    let passphrase = work.join("passphrase");
    std::fs::write(&passphrase, PASSPHRASE).unwrap();
    let file = work.join("moving.trellis-service");
    cli.json(&[
        "svc",
        service.deployment_id(),
        "export",
        "--out",
        path(&file),
        "--passphrase-file",
        path(&passphrase),
    ])
    .await;
    (file, passphrase)
}

/// The label the settings in `runtime` give the service process holding
/// `seed`, which finds `runtime` from its URL alone, as a moved process does.
async fn moved_label(runtime: &TrellisTestRuntime, seed: &str) -> String {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let mut moved = tokio::time::timeout(
        Duration::from_secs(60),
        Moving::connect(ServiceConnectOptions::new(
            server.nats_servers,
            server.trust,
            seed,
        )),
    )
    .await
    .expect("the moved service connects with its settings in place")
    .unwrap();
    MovingProvider::new(&mut moved)
        .config()
        .current()
        .unwrap()
        .label
        .clone()
}

/// A client of `runtime`'s administrator that may export and import
/// services, signed in through the CLI's device flow.
async fn administrator(runtime: &mut TrellisTestRuntime) -> Client {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime
            .workdir()
            .join(format!("admin-{}", ulid::Ulid::generate())),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let device = native
        .start_device(&json!([{
            "type": "trellis_capabilities",
            "required": [
                "trellis.auth.v1:users.manage",
                "trellis.auth.v1:config.manageSecrets"
            ],
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    Client::connect_user(UserConnectOptions::new(
        server.nats_servers,
        30_000,
        device.complete().await.unwrap(),
    ))
    .await
    .unwrap()
}

fn wire<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// The content of a bundle of `deployment`, which `client`'s installation
/// exports, as Auth sends it before the CLI seals it.
async fn exported_content(client: &Client, deployment: &str) -> Vec<u8> {
    let auth = trellis_auth_v1::Client::from_generated(client.clone());
    let operation = auth
        .deployments_export()
        .start(&wire(json!({ "deploymentId": deployment })))
        .await
        .unwrap();
    let finished = operation.wait().await.unwrap();
    let finished = serde_json::to_value(
        finished
            .output
            .unwrap_or_else(|| panic!("the export failed: {:?}", finished.error)),
    )
    .unwrap();
    let download = auth
        .deployments_exports_download(&wire(json!({ "exportId": finished["exportId"] })))
        .await
        .unwrap();
    let grant = trellis_rs::client::download_transfer_grant_from_value(
        serde_json::to_value(download.transfer.unwrap()).unwrap(),
    )
    .unwrap();
    let mut content = Vec::new();
    client
        .download_transfer_into(&grant, &mut content)
        .await
        .unwrap();
    content
}

/// `content` with `change` applied to its `deployment.json`, and its
/// manifest listing the entries as they then are, framed as
/// `design/auth/service-export.md` describes.
fn with_deployment(content: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    fn take<'a>(content: &'a [u8], at: &mut usize, count: usize) -> &'a [u8] {
        *at += count;
        &content[*at - count..*at]
    }
    assert_eq!(&content[..16], b"TRELLIS-SERVICE\n");
    let mut at = 20;
    let mut entries = Vec::new();
    while at < content.len() {
        let length = u16::from_be_bytes(take(content, &mut at, 2).try_into().unwrap());
        let name = String::from_utf8(take(content, &mut at, length.into()).to_vec()).unwrap();
        let mut bytes = Vec::new();
        loop {
            let length = u32::from_be_bytes(take(content, &mut at, 4).try_into().unwrap());
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(take(content, &mut at, length as usize));
        }
        entries.push((name, bytes));
    }
    let (_, manifest) = entries.pop().unwrap();
    let mut manifest: Value = serde_json::from_slice(&manifest).unwrap();
    for (name, bytes) in &mut entries {
        if name == "deployment.json" {
            let mut deployment: Value = serde_json::from_slice(bytes).unwrap();
            change(&mut deployment);
            *bytes = serde_json::to_vec(&deployment).unwrap();
            break;
        }
    }
    manifest["entries"] = entries
        .iter()
        .map(|(name, bytes)| {
            let sha256: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            json!({ "name": name, "size": bytes.len(), "sha256": sha256 })
        })
        .collect();
    entries.push((
        "manifest.json".into(),
        serde_json::to_vec(&manifest).unwrap(),
    ));
    let mut framed = content[..20].to_vec();
    for (name, bytes) in entries {
        framed.extend_from_slice(&u16::try_from(name.len()).unwrap().to_be_bytes());
        framed.extend_from_slice(name.as_bytes());
        for chunk in bytes.chunks(64 * 1024) {
            framed.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
            framed.extend_from_slice(chunk);
        }
        framed.extend_from_slice(&0u32.to_be_bytes());
    }
    framed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bundle_from_before_roles_moved_imports_into_this_build() {
    let mut old = TrellisTestRuntime::builder()
        .native(NativeSource::PreviousRelease(FORMAT_ONE.into()))
        .start()
        .await
        .unwrap();
    assert_eq!(old.server_version(), FORMAT_ONE);
    let old_cli = Cli::login(&mut old).await;
    let service = moving_service(&mut old, &old_cli).await;
    // Someone given the service's API directly, not through a role.
    let person = old.create_user("Pat").await.unwrap();
    old.grant_user_capabilities(&person, &[Ping::CALLER_CAPABILITIES[0]])
        .await
        .unwrap();
    let (file, passphrase) = export(&old, &old_cli, &service).await;

    let mut new = TrellisTestRuntime::builder()
        .native(current())
        .start()
        .await
        .unwrap();
    let new_cli = Cli::login(&mut new).await;
    let imported = new_cli
        .json(&[
            "svc",
            "import",
            path(&file),
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    let report = &imported["import"]["report"];
    assert_eq!(report["deploymentId"], service.deployment_id(), "{report}");
    // The role and the direct grant stayed behind, and the report says so
    // and names them to recreate, the grant with how many held it.
    assert_eq!(report["roles"], json!([]), "{report}");
    assert_eq!(report["grants"], json!([]), "{report}");
    assert_eq!(report["access"]["recreated"], json!([]), "{report}");
    let hint = format!(
        "before bundles carried them; recreate the roles Pingers, and the capabilities \
        granted directly {} (1 held it) by hand",
        Ping::CALLER_CAPABILITIES[0]
    );
    assert!(
        report["leftBehind"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().ends_with(&hint)),
        "{report}"
    );
    assert_eq!(report["notUnderstood"], json!([]), "{report}");
    assert_eq!(moved_label(&new, service.seed()).await, "moved label");

    new.shutdown().await.unwrap();
    old.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bundle_this_build_exports_imports_into_the_previous_release() {
    let mut new = TrellisTestRuntime::builder()
        .native(current())
        .start()
        .await
        .unwrap();
    let release = new.server_version().to_owned();
    let new_cli = Cli::login(&mut new).await;
    let service = moving_service(&mut new, &new_cli).await;
    let (file, passphrase) = export(&new, &new_cli, &service).await;
    let content = exported_content(&administrator(&mut new).await, service.deployment_id()).await;

    let mut old = TrellisTestRuntime::builder()
        .native(NativeSource::PreviousRelease(release))
        .start()
        .await
        .unwrap();
    // A field a newer Trellis may add to the deployment, which the previous
    // release reads past: its check of the bundle finds nothing wrong.
    let newer = with_deployment(&content, |deployment| {
        deployment["futureField"] = json!({ "added": "later" });
    });
    let auth = trellis_auth_v1::Client::from_generated(administrator(&mut old).await);
    let operation = auth
        .deployments_import()
        .start(&wire(json!({ "check": true })))
        .await
        .unwrap();
    operation.upload_from(&mut newer.as_slice()).await.unwrap();
    let checked = operation.wait().await.unwrap();
    let checked = serde_json::to_value(
        checked
            .output
            .unwrap_or_else(|| panic!("the check was refused: {:?}", checked.error)),
    )
    .unwrap();
    assert_eq!(checked["applied"], false, "{checked}");
    assert_eq!(
        checked["report"]["deploymentId"],
        service.deployment_id(),
        "{checked}"
    );
    drop(auth);

    // The bundle itself moves with the previous release's CLI, and its role
    // with it.
    let old_cli = Cli::login(&mut old).await;
    let imported = old_cli
        .json(&[
            "svc",
            "import",
            path(&file),
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    let report = &imported["import"]["report"];
    assert_eq!(report["deploymentId"], service.deployment_id(), "{report}");
    assert!(
        report["access"]["recreated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "role" && item["role"] == "Pingers"),
        "{report}"
    );
    assert_eq!(moved_label(&old, service.seed()).await, "moved label");

    old.shutdown().await.unwrap();
    new.shutdown().await.unwrap();
}
