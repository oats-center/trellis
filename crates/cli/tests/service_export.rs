//! A service exported from one installation with `trellis svc <ID> export`
//! and imported into another with `trellis svc import` runs there as it did:
//! its process reconnects with the identity seed it already had and finds its
//! settings, secret, data, waiting job, webhook URL and enrollment secret,
//! while the installation it left is unchanged.
//! The move guide's commands act on the installations they name.
//! Two exports never replace each other's file.
//! An export holds only unfinished jobs.
//! An import that waited for an instance finishes once one exists.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_moving_service::{
    types::{ResourceValue, Value as Payload},
    Participant, Provider,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::auth::discover_native_oauth;
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::ServiceConnectOptions;
use trellis_testkit::TrellisTestRuntime;

const CLI: &str = env!("CARGO_BIN_EXE_trellis");

/// The production CLI signed in to one runtime as its administrator, under
/// its own configuration or under a profile of a shared one.
struct Cli {
    config: PathBuf,
    profile: Option<String>,
}

impl Cli {
    async fn login(runtime: &mut TrellisTestRuntime) -> Self {
        let config = runtime
            .workdir()
            .join(format!("cli-{}", ulid::Ulid::generate()));
        Self::login_as(runtime, config, None).await
    }

    /// Sign in as `trellis --profile <profile> login <url>` does, beside any
    /// other profile kept in `config`.
    async fn login_as(
        runtime: &mut TrellisTestRuntime,
        config: PathBuf,
        profile: Option<&str>,
    ) -> Self {
        std::fs::create_dir_all(&config).unwrap();
        let cli = Self {
            config,
            profile: profile.map(str::to_owned),
        };
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

    /// `trellis --format json [--profile <profile>]`, as a script runs it.
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(CLI);
        command
            .args(["--format", "json"])
            .env("XDG_CONFIG_HOME", &self.config)
            .kill_on_drop(true);
        if let Some(profile) = &self.profile {
            command.args(["--profile", profile]);
        }
        command
    }

    /// Run as a script does, without a terminal, within a deadline.
    async fn run(&self, args: &[&str]) -> std::process::Output {
        tokio::time::timeout(
            Duration::from_secs(120),
            self.command()
                .args(args)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await
        .expect("the command finishes")
        .unwrap()
    }

    async fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args).await;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The run's failure text, squashed so a wrapped report still matches.
    async fn refusal(&self, args: &[&str]) -> String {
        let output = self.run(args).await;
        assert!(!output.status.success(), "{args:?} succeeded");
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect()
    }
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn payload(value: &str) -> Payload {
    Payload {
        value: value.into(),
        extra: Default::default(),
    }
}

/// Options that connect a process holding `seed` to `runtime`, found from
/// its URL alone, as a process pointed at a new installation finds it.
async fn options<'a>(runtime: &TrellisTestRuntime, seed: &'a str) -> ServiceConnectOptions<'a> {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    ServiceConnectOptions::new(server.nats_servers, server.trust, seed)
}

/// The bundle format of `design/auth/service-export.md`, written from the
/// design rather than taken from the CLI, so a test can open a bundle the CLI
/// wrote and seal content the CLI never would.
mod bundle {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ring::{aead, hmac};
    use serde_json::{json, Value};
    use sha2::{Digest as _, Sha256};

    const CHUNK: usize = 64 * 1024;
    const TAG: usize = 16;

    fn keys(
        passphrase: &str,
        salt: &[u8],
        (memory, passes, lanes): (u32, u32, u32),
        nonce: &[u8],
    ) -> (hmac::Key, aead::LessSafeKey) {
        let mut root = [0; 32];
        argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(memory, passes, lanes, Some(32)).unwrap(),
        )
        .hash_password_into(passphrase.as_bytes(), salt, &mut root)
        .unwrap();
        let root = hmac::Key::new(hmac::HMAC_SHA256, &root);
        let header = hmac::sign(&root, b"trellis-service-bundle header");
        let mut label = b"trellis-service-bundle payload".to_vec();
        label.extend_from_slice(nonce);
        let payload = hmac::sign(&root, &label);
        (
            hmac::Key::new(hmac::HMAC_SHA256, header.as_ref()),
            aead::LessSafeKey::new(
                aead::UnboundKey::new(&aead::CHACHA20_POLY1305, payload.as_ref()).unwrap(),
            ),
        )
    }

    fn nonce(counter: u64, last: bool) -> aead::Nonce {
        let mut nonce = [0; 12];
        nonce[3..11].copy_from_slice(&counter.to_be_bytes());
        nonce[11] = u8::from(last);
        aead::Nonce::assume_unique_for_key(nonce)
    }

    /// The content of a bundle file.
    pub fn open(file: &[u8], passphrase: &str) -> Vec<u8> {
        let text = |line: &[u8]| String::from_utf8(line.to_vec()).unwrap();
        let mut lines = file.splitn(5, |byte| *byte == b'\n');
        assert_eq!(text(lines.next().unwrap()), "trellis-service-bundle v1");
        let costs = text(lines.next().unwrap());
        let nonce_text = text(lines.next().unwrap());
        let mac = text(lines.next().unwrap());
        let sealed = lines.next().unwrap();
        let field = |name: &str| {
            costs
                .split(' ')
                .find_map(|part| part.strip_prefix(&format!("{name}=")))
                .unwrap()
                .to_owned()
        };
        let salt = URL_SAFE_NO_PAD.decode(field("salt")).unwrap();
        let random = URL_SAFE_NO_PAD
            .decode(nonce_text.strip_prefix("nonce=").unwrap())
            .unwrap();
        let costs_used = (
            field("m").parse().unwrap(),
            field("t").parse().unwrap(),
            field("p").parse().unwrap(),
        );
        let (header, payload) = keys(passphrase, &salt, costs_used, &random);
        hmac::verify(
            &header,
            format!("trellis-service-bundle v1\n{costs}\n{nonce_text}\n").as_bytes(),
            &URL_SAFE_NO_PAD
                .decode(mac.strip_prefix("mac=").unwrap())
                .unwrap(),
        )
        .unwrap();
        let chunks: Vec<&[u8]> = sealed.chunks(CHUNK + TAG).collect();
        let mut content = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let mut chunk = chunk.to_vec();
            let plain = payload
                .open_in_place(
                    nonce(index as u64, index + 1 == chunks.len()),
                    aead::Aad::empty(),
                    &mut chunk,
                )
                .unwrap();
            content.extend_from_slice(plain);
        }
        content
    }

    /// A bundle file of `content`, sealed with `passphrase` at a low cost.
    pub fn seal(content: &[u8], passphrase: &str) -> Vec<u8> {
        let (salt, random): ([u8; 16], [u8; 16]) = (rand::random(), rand::random());
        let (header, payload) = keys(passphrase, &salt, (8_192, 1, 1), &random);
        let text = format!(
            "trellis-service-bundle v1\nargon2id m=8192 t=1 p=1 salt={}\nnonce={}\n",
            URL_SAFE_NO_PAD.encode(salt),
            URL_SAFE_NO_PAD.encode(random)
        );
        let mac = hmac::sign(&header, text.as_bytes());
        let mut file = format!("{text}mac={}\n", URL_SAFE_NO_PAD.encode(mac.as_ref())).into_bytes();
        let chunks: Vec<&[u8]> = if content.is_empty() {
            vec![&[]]
        } else {
            content.chunks(CHUNK).collect()
        };
        for (index, chunk) in chunks.iter().enumerate() {
            let mut chunk = chunk.to_vec();
            payload
                .seal_in_place_append_tag(
                    nonce(index as u64, index + 1 == chunks.len()),
                    aead::Aad::empty(),
                    &mut chunk,
                )
                .unwrap();
            file.extend_from_slice(&chunk);
        }
        file
    }

    /// The content's named entries, in order, the manifest last.
    pub fn entries(content: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&content[..16], b"TRELLIS-SERVICE\n");
        let mut rest = &content[20..];
        let take = |rest: &mut &[u8], count: usize| {
            let (part, after) = rest.split_at(count);
            *rest = after;
            part.to_vec()
        };
        let mut entries = Vec::new();
        while !rest.is_empty() {
            let length = u16::from_be_bytes(take(&mut rest, 2).try_into().unwrap());
            let name = String::from_utf8(take(&mut rest, usize::from(length))).unwrap();
            let mut bytes = Vec::new();
            loop {
                let length = u32::from_be_bytes(take(&mut rest, 4).try_into().unwrap()) as usize;
                if length == 0 {
                    break;
                }
                bytes.extend_from_slice(&take(&mut rest, length));
            }
            entries.push((name, bytes));
        }
        entries
    }

    /// Content of `entries`, with a manifest that lists them as they now are.
    pub fn content(mut entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
        let at = entries
            .iter()
            .position(|(name, _)| name == "manifest.json")
            .unwrap();
        let (_, manifest) = entries.remove(at);
        let mut manifest: Value = serde_json::from_slice(&manifest).unwrap();
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
        let mut content = b"TRELLIS-SERVICE\n".to_vec();
        content.extend_from_slice(&1u32.to_be_bytes());
        for (name, bytes) in entries {
            content.extend_from_slice(&u16::try_from(name.len()).unwrap().to_be_bytes());
            content.extend_from_slice(name.as_bytes());
            for chunk in bytes.chunks(CHUNK) {
                content.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
                content.extend_from_slice(chunk);
            }
            content.extend_from_slice(&0u32.to_be_bytes());
        }
        content
    }

    /// The JSON document `name` among `entries`.
    pub fn document(entries: &[(String, Vec<u8>)], name: &str) -> Value {
        let (_, bytes) = entries.iter().find(|(entry, _)| entry == name).unwrap();
        serde_json::from_slice(bytes).unwrap()
    }

    /// Replace the JSON document `name` among `entries`.
    pub fn set_document(entries: &mut [(String, Vec<u8>)], name: &str, value: &Value) {
        let (_, bytes) = entries.iter_mut().find(|(entry, _)| entry == name).unwrap();
        *bytes = serde_json::to_vec(value).unwrap();
    }

    /// The SHA-256 of a file, in lowercase hex, as import's `--digest` takes it.
    pub fn digest(file: &[u8]) -> String {
        Sha256::digest(file)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Seal `entries` as a bundle file at `out`, and return its digest.
fn write_bundle(entries: Vec<(String, Vec<u8>)>, passphrase: &Path, out: &Path) -> String {
    let passphrase = std::fs::read_to_string(passphrase).unwrap();
    let file = bundle::seal(&bundle::content(entries), passphrase.trim_end());
    std::fs::write(out, &file).unwrap();
    bundle::digest(&file)
}

/// The entries of the bundle file at `file`.
fn read_bundle(file: &Path, passphrase: &Path) -> Vec<(String, Vec<u8>)> {
    let passphrase = std::fs::read_to_string(passphrase).unwrap();
    bundle::entries(&bundle::open(
        &std::fs::read(file).unwrap(),
        passphrase.trim_end(),
    ))
}

/// Export `id` with the production CLI into `out` and return the digest.
async fn export(cli: &Cli, id: &str, out: &Path, passphrase: &Path) -> String {
    cli.json(&[
        "svc",
        id,
        "export",
        "--out",
        path(out),
        "--passphrase-file",
        path(passphrase),
    ])
    .await["digest"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Import `file` with the production CLI.
async fn import(cli: &Cli, file: &Path, digest: &str, passphrase: &Path) -> std::process::Output {
    cli.run(&[
        "svc",
        "import",
        path(file),
        "--digest",
        digest,
        "--passphrase-file",
        path(passphrase),
    ])
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exported_service_runs_in_another_installation_with_everything_trellis_kept() {
    let mut old = TrellisTestRuntime::builder().start().await.unwrap();
    let mut new = TrellisTestRuntime::builder().start().await.unwrap();
    // One shell, as the move guide has it: each installation is a profile of
    // the same CLI configuration, and every command names its profile.
    let shell = old.workdir().join("shell");
    let old_cli = Cli::login_as(&mut old, shell.clone(), Some("old")).await;
    let new_cli = Cli::login_as(&mut new, shell, Some("new")).await;
    for (cli, runtime) in [(&old_cli, &old), (&new_cli, &new)] {
        let server = cli.json(&["whoami"]).await["server"].clone();
        assert_eq!(
            server.as_str().unwrap().trim_end_matches('/'),
            runtime.trellis_url().trim_end_matches('/')
        );
    }

    // In the old installation: a service with settings and a secret, a
    // record written twice, a stored file, a job no worker has taken yet,
    // an enrollment secret, and access people hold to its API.
    let identity = old.register_service::<Participant>("moving").await.unwrap();
    let deployment = identity.deployment_id().to_owned();
    old.set_config(
        &identity,
        json!({ "label": "moved label" }),
        json!({ "apiKey": "hook-key-0123456789" }),
    )
    .await
    .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let provider = Provider::new(&mut service);
    let records = provider.client().records().await.unwrap();
    for value in ["first", "second"] {
        records
            .put(
                "order.1",
                &ResourceValue {
                    value: value.into(),
                    extra: Default::default(),
                },
            )
            .await
            .unwrap();
    }
    let record = records.get_entry("order.1").await.unwrap().unwrap();
    let object: Vec<u8> = (0..200_000u32).map(|index| (index % 253) as u8).collect();
    let files = provider.client().files().await.unwrap();
    files.write("doc/1", object.clone()).await.unwrap();
    let job = provider
        .jobs_client()
        .create_work(payload("waiting work"))
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    drop((records, files, job));
    drop(service);
    let issued = old_cli
        .json(&["svc", &deployment, "enrollment", "issue", "--title", "pods"])
        .await;
    let enrollment = issued["text"].as_str().unwrap().to_owned();
    let capability =
        runtime_trellis::apis::runtime_trellis_moving_v1::rpc::Ping::CALLER_CAPABILITIES[0];
    old_cli
        .json(&[
            "roles",
            "create",
            "--title",
            "Moving pingers",
            "--description",
            "Reach the moving service",
            "--capability",
            capability,
        ])
        .await;
    let user = old.create_user("Pinger").await.unwrap();
    old.grant_user_capabilities(&user, &[capability])
        .await
        .unwrap();
    let old_url = old.webhook_urls(&identity).await.unwrap()["token"].clone();
    let before = (
        old_cli.json(&["svc", &deployment, "show"]).await,
        old_cli.json(&["svc", &deployment, "config"]).await,
        old_cli.json(&["svc", &deployment, "instances"]).await,
    );

    // The export writes an encrypted bundle and prints its SHA-256.
    let work = old.workdir().join("move");
    std::fs::create_dir_all(&work).unwrap();
    let passphrase = work.join("passphrase");
    std::fs::write(&passphrase, "a long enough passphrase\n").unwrap();
    let bundle = work.join("moving.trellis-service");
    let exported = old_cli
        .json(&[
            "svc",
            "moving",
            "export",
            "--out",
            path(&bundle),
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    let digest = exported["digest"].as_str().unwrap().to_owned();
    assert_eq!(digest.len(), 64);
    let bytes = std::fs::read(&bundle).unwrap();
    assert!(
        !bytes
            .windows(b"hook-key-0123456789".len())
            .any(|window| window == b"hook-key-0123456789"),
        "the secret is not in the file in the clear"
    );

    // The old installation is unchanged: the same deployment, settings and
    // instances, and its process still connects and reads its record.
    let after = (
        old_cli.json(&["svc", &deployment, "show"]).await,
        old_cli.json(&["svc", &deployment, "config"]).await,
        old_cli.json(&["svc", &deployment, "instances"]).await,
    );
    assert_eq!(after, before);
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let still = Provider::new(&mut service)
        .client()
        .records()
        .await
        .unwrap()
        .get_entry("order.1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.revision, record.revision);
    drop(service);

    // A wrong passphrase, and a copy changed after the export, are refused
    // before the new installation changes.
    let wrong = work.join("wrong");
    std::fs::write(&wrong, "not the passphrase at all\n").unwrap();
    let refused = new_cli
        .refusal(&[
            "svc",
            "import",
            path(&bundle),
            "--digest",
            &digest,
            "--passphrase-file",
            path(&wrong),
        ])
        .await;
    assert!(refused.contains("passphraseiswrong"), "{refused}");
    let mut changed = bytes.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    let tampered = work.join("tampered.trellis-service");
    std::fs::write(&tampered, &changed).unwrap();
    let refused = new_cli
        .refusal(&[
            "svc",
            "import",
            path(&tampered),
            "--digest",
            &digest,
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    assert!(refused.contains("notthefilethatwasexported"), "{refused}");
    // A check reports what the import would do and leaves nothing behind.
    let checked = new_cli
        .json(&[
            "svc",
            "import",
            path(&bundle),
            "--digest",
            &digest,
            "--passphrase-file",
            path(&passphrase),
            "--check",
        ])
        .await;
    assert_eq!(checked["import"]["applied"], false);
    assert_eq!(
        checked["import"]["report"]["deploymentId"],
        deployment.as_str()
    );
    assert!(new_cli
        .refusal(&["svc", &deployment, "show"])
        .await
        .contains("notfound"));

    // The import keeps the deployment ID and reports the access to
    // recreate, which it does not create.
    let imported = new_cli
        .json(&[
            "svc",
            "import",
            path(&bundle),
            "--digest",
            &digest,
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    let report = &imported["import"]["report"];
    assert_eq!(imported["import"]["applied"], true);
    assert_eq!(report["deploymentId"], deployment.as_str());
    assert_eq!(
        report["roles"],
        json!([{ "name": "Moving pingers", "capabilities": [capability], "assignments": "0" }])
    );
    assert_eq!(
        report["grants"],
        json!([{ "capability": capability, "principals": "1" }])
    );
    let roles = new_cli.json(&["roles", "list"]).await.to_string();
    assert!(!roles.contains("Moving pingers"), "{roles}");
    let new_url = new.webhook_urls(&identity).await.unwrap()["token"].clone();
    let url_path = |url: &str| url::Url::parse(url).unwrap().path().to_owned();
    assert_eq!(url_path(&new_url), url_path(&old_url));
    assert!(new_url.starts_with(new.trellis_url()), "{new_url}");
    assert_eq!(report["webhooks"][0]["url"], new_url.as_str());

    // The same bundle again, or a later export of the same deployment, is
    // refused, naming the deployment that exists.
    let refused = new_cli
        .refusal(&[
            "svc",
            "import",
            path(&bundle),
            "--digest",
            &digest,
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    assert!(
        refused.contains(&format!("alreadyimportedasdeployment{deployment}")),
        "{refused}"
    );
    let second = work.join("again.trellis-service");
    let again = old_cli
        .json(&[
            "svc",
            &deployment,
            "export",
            "--out",
            path(&second),
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    let refused = new_cli
        .refusal(&[
            "svc",
            "import",
            path(&second),
            "--digest",
            again["digest"].as_str().unwrap(),
            "--passphrase-file",
            path(&passphrase),
        ])
        .await;
    assert!(
        refused.contains(&format!("deployment{deployment}(moving)alreadyexists")),
        "{refused}"
    );

    // The old process, pointed at the new installation with the same seed,
    // finds its settings and secret, its record at the same revision, its
    // file, and its waiting job, which runs now.
    // A service missing a required setting waits to connect, so a deadline
    // turns that into a failure.
    let mut service = tokio::time::timeout(
        Duration::from_secs(60),
        Participant::connect(options(&new, identity.seed()).await),
    )
    .await
    .expect("the moved process connects with its settings in place")
    .unwrap();
    let mut provider = Provider::new(&mut service);
    let settings = provider.config().current().unwrap();
    assert_eq!(settings.label, "moved label");
    assert_eq!(settings.api_key, "hook-key-0123456789");
    let moved = provider
        .client()
        .records()
        .await
        .unwrap()
        .get_entry("order.1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.revision, record.revision);
    assert_eq!(moved.value.unwrap().value, "second");
    let stored = provider
        .client()
        .files()
        .await
        .unwrap()
        .read("doc/1")
        .await
        .unwrap()
        .expect("the stored file");
    assert!(
        stored.as_ref() == object.as_slice(),
        "the stored file changed"
    );
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    provider
        .register_work(move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send((job.id().to_owned(), job.payload().value.clone()));
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    provider
        .runtime_trellis_moving_v1()
        .register_ping(|_, input| async move { Ok(input) });
    let mut deliveries = provider.token().unwrap().subscribe().await.unwrap();
    let lifecycle = service.lifecycle();
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the moved service serves");
    let (ran_id, ran_value) = tokio::time::timeout(Duration::from_secs(30), runs.recv())
        .await
        .expect("the waiting job runs in the new installation")
        .unwrap();
    assert_eq!(
        (ran_id.as_str(), ran_value.as_str()),
        (job_id.as_str(), "waiting work")
    );

    // A sender that uses the new URL reaches the service, verified with the
    // moved secret.
    let response = new
        .https_client_builder()
        .build()
        .unwrap()
        .post(&new_url)
        .header("x-hook-token", "hook-key-0123456789")
        .body("moved request")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(30), deliveries.next())
        .await
        .expect("the request arrives")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, b"moved request");

    // A new process enrolls with the moved enrollment secret.
    let server = discover_native_oauth(new.trellis_url(), &new.trust_roots())
        .await
        .unwrap();
    let enrolled = Participant::connect(
        ServiceConnectOptions::enrolling(server.nats_servers, server.trust, new.trellis_url())
            .with_enrollment_secret(&enrollment),
    )
    .await
    .unwrap();
    drop(enrolled);

    // Disabling the deployment just imported, as a cleanup run against the
    // wrong installation would, is refused without a confirmation, which a
    // script cannot give, and changes nothing.
    let state = |shown: Value| shown["deployment"]["state"].clone();
    assert!(!new_cli
        .run(&["svc", "moving", "disable"])
        .await
        .status
        .success());
    assert_eq!(
        state(new_cli.json(&["svc", "moving", "show"]).await),
        "enabled"
    );

    // The guide's cleanup names the old installation: it disables, then
    // removes, the deployment there, and the moved one keeps serving.
    old_cli.json(&["svc", "moving", "disable"]).await;
    assert_eq!(
        state(old_cli.json(&["svc", &deployment, "show"]).await),
        "disabled"
    );
    assert_eq!(
        state(new_cli.json(&["svc", "moving", "show"]).await),
        "enabled"
    );
    old_cli.json(&["svc", &deployment, "remove", "-f"]).await;
    assert_eq!(
        state(old_cli.json(&["svc", &deployment, "show"]).await),
        "removed"
    );
    assert_eq!(
        state(new_cli.json(&["svc", "moving", "show"]).await),
        "enabled"
    );
    let response = new
        .https_client_builder()
        .build()
        .unwrap()
        .post(&new_url)
        .header("x-hook-token", "hook-key-0123456789")
        .body("after the cleanup")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    lifecycle.stop();
    running.await.unwrap().unwrap();
    new.shutdown().await.unwrap();
    old.shutdown().await.unwrap();
}

/// A bundle whose deployment has no instance that can run stops before it
/// prepares the resources and queues the waiting jobs. Once an instance is
/// provisioned in the new installation, running the same bundle again
/// finishes: the waiting job runs on the new instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_waiting_for_an_instance_finishes_once_one_is_provisioned() {
    let mut old = TrellisTestRuntime::builder().start().await.unwrap();
    let mut new = TrellisTestRuntime::builder().start().await.unwrap();
    let (old_cli, new_cli) = (Cli::login(&mut old).await, Cli::login(&mut new).await);
    let identity = old
        .register_service::<Participant>("waiting")
        .await
        .unwrap();
    let deployment = identity.deployment_id().to_owned();
    old.set_config(
        &identity,
        json!({ "label": "waiting label" }),
        json!({ "apiKey": "hook-key-0123456789" }),
    )
    .await
    .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let job = Provider::new(&mut service)
        .jobs_client()
        .create_work(payload("work for later"))
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    drop(job);
    drop(service);
    let work = old.workdir().join("move");
    std::fs::create_dir_all(&work).unwrap();
    let passphrase = work.join("passphrase");
    std::fs::write(&passphrase, "a long enough passphrase\n").unwrap();
    let exported = work.join("waiting.trellis-service");
    export(&old_cli, &deployment, &exported, &passphrase).await;
    // What an export writes once every instance has been retired: no
    // instance moves, the waiting job does.
    let mut entries = read_bundle(&exported, &passphrase);
    let mut instances = bundle::document(&entries, "instances.json");
    instances["instances"] = json!([]);
    bundle::set_document(&mut entries, "instances.json", &instances);
    let bundle_file = work.join("no-instance.trellis-service");
    let digest = write_bundle(entries, &passphrase, &bundle_file);

    let first = import(&new_cli, &bundle_file, &digest, &passphrase).await;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let provisioned = new_cli.json(&["svc", &deployment, "provision"]).await;
    let seed = provisioned["instanceSeed"].as_str().unwrap().to_owned();
    let again = import(&new_cli, &bundle_file, &digest, &passphrase).await;
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["import"]["resumed"], true);

    let mut service = tokio::time::timeout(
        Duration::from_secs(60),
        Participant::connect(options(&new, &seed).await),
    )
    .await
    .expect("the provisioned instance connects")
    .unwrap();
    let mut provider = Provider::new(&mut service);
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    provider
        .register_work(move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send(job.id().to_owned());
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    provider
        .runtime_trellis_moving_v1()
        .register_ping(|_, input| async move { Ok(input) });
    let lifecycle = service.lifecycle();
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the imported service serves");
    let ran_id = tokio::time::timeout(Duration::from_secs(30), runs.recv())
        .await
        .expect("the waiting job runs once the import finishes")
        .unwrap();
    assert_eq!(ran_id, job_id);
    lifecycle.stop();
    running.await.unwrap().unwrap();
    new.shutdown().await.unwrap();
    old.shutdown().await.unwrap();
}

/// The peak resident memory of `runtime`'s server while `work` runs, above
/// what it held when `work` began, read from Linux's own accounting.
async fn peak_growth<F: std::future::Future>(
    runtime: &TrellisTestRuntime,
    work: F,
) -> (u64, F::Output) {
    let config = runtime.workdir().to_owned();
    let pid = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .find(|pid| {
            let Ok(command) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                return false;
            };
            let args: Vec<&[u8]> = command.split(|byte| *byte == 0).collect();
            args.len() >= 3
                && args[0].ends_with(b"trellis-server")
                && args[1] == b"--config"
                && Path::new(std::str::from_utf8(args[2]).unwrap_or_default()).starts_with(&config)
        })
        .expect("the runtime's server");
    let kib = |field: &str| -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .unwrap()
            .trim()
            .trim_end_matches(" kB")
            .parse()
            .unwrap()
    };
    // Writing 5 resets the peak to what the process holds now.
    std::fs::write(format!("/proc/{pid}/clear_refs"), "5").unwrap();
    let before = kib("VmRSS:");
    let output = work.await;
    (kib("VmHWM:").saturating_sub(before) * 1024, output)
}

/// A service whose queue retains far more finished history than an export
/// could hold exports with only its unfinished job, and the export's memory
/// does not grow with the history it reads past.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_holds_only_unfinished_jobs_whatever_history_it_reads() {
    const JOBS: usize = 96;
    const PAYLOAD: usize = 512 * 1024;
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime).await;
    let identity = runtime
        .register_service::<Participant>("history")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "label": "history" }),
            json!({ "apiKey": "hook-key-0123456789" }),
        )
        .await
        .unwrap();
    // Each finished job keeps its payload and its result in the queue.
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let mut provider = Provider::new(&mut service);
    provider
        .register_work(|job| async move { Ok::<_, JobProcessError<String>>(job.payload().clone()) })
        .await
        .unwrap();
    provider
        .runtime_trellis_moving_v1()
        .register_ping(|_, input| async move { Ok(input) });
    let jobs = provider.jobs_client();
    let lifecycle = service.lifecycle();
    let running = tokio::spawn(service.run());
    assert!(lifecycle.ready().await);
    let large = "x".repeat(PAYLOAD);
    let mut created = Vec::new();
    for _ in 0..JOBS {
        created.push(jobs.create_work(payload(&large)).await.unwrap());
    }
    for job in &created {
        tokio::time::timeout(Duration::from_secs(60), job.wait())
            .await
            .expect("the job finishes")
            .unwrap();
    }
    drop((created, jobs));
    lifecycle.stop();
    running.await.unwrap().unwrap();
    // One job no worker takes.
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let waiting = Provider::new(&mut service)
        .jobs_client()
        .create_work(payload("still waiting"))
        .await
        .unwrap();
    drop(waiting);
    drop(service);
    // Memory the allocator kept from running the jobs would hide the
    // export's own, so the export runs in a server that just started.
    runtime.restart().await.unwrap();

    let work = runtime.workdir().join("export");
    std::fs::create_dir_all(&work).unwrap();
    let passphrase = work.join("passphrase");
    std::fs::write(&passphrase, "a long enough passphrase\n").unwrap();
    let out = work.join("history.trellis-service");
    let (growth, exported) = peak_growth(
        &runtime,
        cli.json(&[
            "svc",
            identity.deployment_id(),
            "export",
            "--out",
            path(&out),
            "--passphrase-file",
            path(&passphrase),
        ]),
    )
    .await;
    let queue = exported["report"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|resource| resource["kind"] == "job")
        .unwrap()
        .clone();
    assert_eq!(queue["messages"], "1", "{queue}");
    let history = 2 * JOBS * PAYLOAD;
    assert!(
        growth < (48 << 20) as u64,
        "the export grew the server by {growth} bytes reading {history} bytes of finished jobs"
    );
    let entries = read_bundle(&out, &passphrase);
    let (_, lines) = entries
        .iter()
        .find(|(name, _)| name == "jobs.jsonl")
        .unwrap();
    assert!(
        lines.len() < PAYLOAD,
        "only the unfinished job's messages: {}",
        lines.len()
    );
    runtime.shutdown().await.unwrap();
}

/// Two exports to the same file, the second already past its check that
/// the file is free, never replace each other: the first writes the file,
/// the second refuses and leaves it as the first wrote it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_exports_to_one_file_never_replace_each_other() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let cli = Cli::login(&mut runtime).await;
    let identity = runtime
        .register_service::<Participant>("twice")
        .await
        .unwrap();
    let work = runtime.workdir().join("export");
    std::fs::create_dir_all(&work).unwrap();
    let passphrase = work.join("passphrase");
    std::fs::write(&passphrase, "a long enough passphrase\n").unwrap();
    let out = work.join("twice.trellis-service");
    // The second export reads its passphrase from a FIFO, so it waits there,
    // after its check that the file is free, until this test writes it.
    let fifo = work.join("passphrase.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let second = cli
        .command()
        .args([
            "svc",
            identity.deployment_id(),
            "export",
            "--out",
            path(&out),
            "--passphrase-file",
            path(&fifo),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Opening the FIFO to write returns once the second export opened it.
    let writer = {
        let fifo = fifo.clone();
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(fifo)),
        )
        .await
        .expect("the second export asks for its passphrase")
        .unwrap()
        .unwrap()
    };
    let first = export(&cli, identity.deployment_id(), &out, &passphrase).await;
    let written = std::fs::read(&out).unwrap();
    assert_eq!(bundle::digest(&written), first);
    {
        use std::io::Write as _;
        let mut writer = writer;
        writer.write_all(b"another long passphrase\n").unwrap();
    }
    let second = tokio::time::timeout(Duration::from_secs(120), second.wait_with_output())
        .await
        .expect("the second export ends")
        .unwrap();
    assert!(
        !second.status.success(),
        "the second export replaced the first: {}",
        String::from_utf8_lossy(&second.stdout)
    );
    assert_eq!(std::fs::read(&out).unwrap(), written);
    let mut left: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["passphrase", "passphrase.fifo", "twice.trellis-service"],
        "no partial file stays behind"
    );
    runtime.shutdown().await.unwrap();
}
