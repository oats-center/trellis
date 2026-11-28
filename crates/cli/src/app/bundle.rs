//! `trellis svc <ID> export` and `trellis svc import`: moving a service to
//! another installation.
//!
//! Auth reads the service into a bundle's content and sends it over the
//! signed-in session; this command encrypts it with the operator's passphrase
//! as it arrives, so the file is never written in the clear and the
//! passphrase never leaves this process. Import decrypts the file as it
//! sends it, and Auth checks every part against the bundle's manifest before
//! it changes anything. The passphrase is the proof that a bundle is whole and
//! comes from whoever holds it: a changed, cut or swapped file fails the
//! header's MAC or a chunk's tag.
//!
//! The file is a short text header followed by the content in chunks:
//!
//! ```text
//! trellis-service-bundle v1
//! argon2id m=65536 t=3 p=1 salt=<16 bytes, base64url>
//! nonce=<16 bytes, base64url>
//! mac=<HMAC-SHA256 of the lines above, base64url>
//! <ChaCha20-Poly1305 chunks of 64 KiB of content each>
//! ```
//!
//! This is age's passphrase construction, with Argon2id for scrypt: the
//! passphrase and salt give a key through Argon2id, the header is
//! authenticated with an HMAC key derived from it, and the content is
//! encrypted in chunks, each nonce a counter with a final-chunk flag, under a
//! key derived from it and the nonce. A wrong passphrase fails the header's
//! MAC; any change to the content, or cutting it short, fails a chunk's tag.

use std::io::Read;
use std::path::{Path, PathBuf};

use miette::IntoDiagnostic;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::*;
use crate::{output, prompt};

use trellis_bootstrap::envelope::{Decryptor, Encryptor, CHUNK, SERVICE_BUNDLE};

/// The shortest passphrase accepted for a new bundle.
const MIN_PASSPHRASE: usize = trellis_bootstrap::envelope::MIN_PASSPHRASE;

/// The passphrase from `file` (`-` for standard input), else asked for on a
/// terminal (twice for a new bundle), else read from standard input.
fn passphrase(file: Option<&Path>, new: bool) -> miette::Result<String> {
    use std::io::IsTerminal as _;
    let text = match file {
        Some(path) if path != Path::new("-") => std::fs::read_to_string(path)
            .map_err(|error| miette::miette!("cannot read {}: {error}", path.display()))?,
        None if std::io::stdin().is_terminal() => {
            let mut form = prompt::Form::new(true);
            let typed = if new {
                form.explain(
                    "The passphrase encrypts the bundle, secrets included. Whoever has the \
                    bundle and the passphrase can read every secret in it.",
                );
                form.required_secret("Passphrase for the bundle", "--passphrase-file PATH")?
            } else {
                form.secret("Passphrase of the bundle", None)?
                    .unwrap_or_default()
            };
            form.finish()?;
            typed
        }
        _ => std::io::read_to_string(std::io::stdin()).into_diagnostic()?,
    };
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    miette::ensure!(!text.is_empty(), "the passphrase is empty");
    if new {
        miette::ensure!(
            text.chars().count() >= MIN_PASSPHRASE,
            "a bundle's passphrase needs at least {MIN_PASSPHRASE} characters"
        );
    }
    Ok(text)
}

/// The message of a failed Operation, or its state when it ended otherwise.
fn ended(
    state: &str,
    error: Option<&trellis_protocol::operation_record::OperationError>,
) -> miette::Report {
    match error {
        Some(error) => miette::miette!("{}", error.message),
        None => miette::miette!("the operation ended {state}"),
    }
}

/// Write `id`'s bundle, encrypted with a passphrase.
pub(super) async fn export(
    format: OutputFormat,
    id: &str,
    args: &SvcExportArgs,
) -> miette::Result<()> {
    let deployment_id =
        super::deploy::resolve_deployment_id(super::deploy::DeploymentKind::Service, id).await?;
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{id}.trellis-service")));
    miette::ensure!(
        !out.exists(),
        "{} already exists; choose another file with --out",
        out.display()
    );
    let passphrase = passphrase(args.passphrase_file.as_deref(), true)?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1();
    let operation = auth
        .deployments_export()
        .start(&wire(json!({ "deploymentId": deployment_id }))?)
        .await
        .into_diagnostic()?;
    let snapshot = operation.wait().await.into_diagnostic()?;
    let result = match snapshot.output {
        Some(result) => serde_json::to_value(result).into_diagnostic()?,
        None => {
            return Err(ended(
                &format!("{:?}", snapshot.state),
                snapshot.error.as_ref(),
            ))
        }
    };
    let download = auth
        .deployments_exports_download(&wire(json!({ "exportId": result["exportId"] }))?)
        .await
        .into_diagnostic()?;
    let grant = trellis_rs::client::download_transfer_grant_from_value(
        serde_json::to_value(
            download
                .transfer
                .ok_or_else(|| miette::miette!("Auth sent no bundle to download"))?,
        )
        .into_diagnostic()?,
    )
    .into_diagnostic()?;
    // Written to a file of its own beside the destination and linked into
    // place once whole, so a stopped export leaves no file that looks
    // finished, and two exports to one destination never replace each other.
    let directory = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let partial = tempfile::Builder::new()
        .prefix(".trellis-service-")
        .suffix(".partial")
        .tempfile_in(directory)
        .map_err(|error| miette::miette!("cannot write in {}: {error}", directory.display()))?;
    let written = async {
        let file = partial.as_file().try_clone().into_diagnostic()?;
        let mut encryptor =
            Encryptor::new(std::io::BufWriter::new(file), SERVICE_BUNDLE, &passphrase)?;
        let (mut source, mut sink) = tokio::io::duplex(CHUNK);
        let received = async {
            let info = connected
                .download_transfer_into(&grant, &mut sink)
                .await
                .into_diagnostic();
            sink.shutdown().await.into_diagnostic()?;
            info
        };
        let encrypted = async {
            let mut buffer = vec![0; CHUNK];
            loop {
                let read = source.read(&mut buffer).await.into_diagnostic()?;
                if read == 0 {
                    break;
                }
                encryptor.write(&buffer[..read])?;
            }
            Ok::<_, miette::Report>(())
        };
        tokio::try_join!(received, encrypted)?;
        let file = encryptor
            .finish()?
            .into_inner()
            .map_err(|error| miette::miette!("cannot write the bundle: {error}"))?;
        file.sync_all().into_diagnostic()?;
        Ok::<_, miette::Report>(file.metadata().into_diagnostic()?.len())
    }
    .await;
    // Dropping the partial file on any failure deletes it.
    let size = written?;
    partial.persist_noclobber(&out).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            miette::miette!(
                help = "Run the export again with another file for --out.",
                "{} appeared while the export ran; it was left as it is and nothing was written",
                out.display()
            )
        } else {
            miette::miette!("cannot write {}: {}", out.display(), error.error)
        }
    })?;
    let report = &result["report"];
    if output::is_json(format) {
        return output::print_json(&json!({
            "file": out,
            "size": size,
            "deploymentId": deployment_id,
            "report": report,
        }));
    }
    output::success(format!("Exported {id} to {}", out.display()));
    output::Details::untitled()
        .field("File", out.display())
        .field("Size", format!("{size} bytes"))
        .print();
    print_report(report);
    output::note(
        "Give the importing administrator the passphrase separately from the file. This \
        installation still runs the service: stop or remove it once it runs in the new one.",
    );
    output::next(&format!("trellis svc import {}", out.display()));
    Ok(())
}

/// Import a bundle written by `trellis svc <ID> export`.
pub(super) async fn import(format: OutputFormat, args: &SvcImportArgs) -> miette::Result<()> {
    let passphrase = passphrase(args.passphrase_file.as_deref(), false)?;
    let (selection, connected) = connect_authenticated_cli_client().await?;
    let mut result = send(&connected, args, &passphrase, args.replace_existing).await?;
    let replaces = output::list(&result, "replaces");
    // Data under the deployment's own names that loading the bundle
    // replaces, such as the streams an interrupted import of it left. Auth
    // changed nothing; with the operator's approval the import runs again
    // and replaces it.
    if !args.check && result["applied"] == Value::Bool(false) && !replaces.is_empty() {
        let deployment = output::field(&result["report"], "deploymentId");
        let listed = replaces.iter().map(replacement).collect::<Vec<_>>();
        output::warning(format!(
            "{} already holds this under deployment {deployment}'s own names, and importing \
            replaces it with the bundle's data: {}",
            selection.scope.issuer,
            listed.join(", ")
        ));
        output::confirm_typed(
            &format!(
                "what {} holds for deployment {deployment}",
                selection.scope.issuer
            ),
            &deployment,
            "replace",
            "--replace-existing",
        )?;
        result = send(&connected, args, &passphrase, true).await?;
    }
    if output::is_json(format) {
        return output::print_json(&json!({ "import": result }));
    }
    let report = &result["report"];
    let deployment = output::field(report, "deploymentId");
    if result["applied"] == Value::Bool(false) {
        output::success(format!(
            "The bundle checks out; importing it {} deployment {deployment}",
            if result["resumed"] == Value::Bool(true) {
                "finishes"
            } else {
                "adds"
            }
        ));
    } else if result["resumed"] == Value::Bool(true) {
        output::success(format!("Finished importing deployment {deployment}"));
    } else {
        output::success(format!("Imported deployment {deployment}"));
    }
    let replaces = output::list(&result, "replaces");
    if !replaces.is_empty() {
        output::Details::untitled()
            .section(if result["applied"] == Value::Bool(false) {
                "Already here, which importing replaces"
            } else {
                "Replaced with the bundle's data"
            })
            .items(replaces.iter().map(replacement))
            .print();
    }
    print_report(report);
    if result["applied"] != Value::Bool(false) {
        output::note(
            "Point each of the service's processes at this installation's URL and restart \
            it with the identity seed it already has.",
        );
    }
    Ok(())
}

/// One item of an import's `replaces`, in words.
fn replacement(item: &Value) -> String {
    match item["subjects"].as_str() {
        Some(subjects) => format!("{subjects} in stream {}", output::field(item, "stream")),
        None => format!("stream {}", output::field(item, "stream")),
    }
}

/// Send the bundle `args` names to Auth's import, decrypting it as it goes,
/// and return the import's result; `replace` approves replacing what it
/// lists.
async fn send(
    connected: &trellis_rs::generated::Client,
    args: &SvcImportArgs,
    passphrase: &str,
    replace: bool,
) -> miette::Result<Value> {
    let file = std::fs::File::open(&args.file)
        .map_err(|error| miette::miette!("cannot read {}: {error}", args.file.display()))?;
    // A wrong passphrase is refused here, before anything is sent.
    let mut decryptor = Decryptor::open(std::io::BufReader::new(file), SERVICE_BUNDLE, passphrase)?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1();
    let operation = auth
        .deployments_import()
        .start(&wire(json!({
            "check": args.check,
            "acknowledgeSharedApis": args.acknowledge_shared_apis,
            "acknowledgeUnauthenticatedWebhooks": args.acknowledge_unauthenticated_webhooks,
            "replaceExisting": replace,
        }))?)
        .await
        .into_diagnostic()?;
    let (mut source, mut sink) = tokio::io::duplex(CHUNK);
    // Whichever side fails first ends the other: a refusal stops Auth
    // reading, and a chunk that does not decrypt stops the upload unfinished.
    let sent = async {
        operation
            .upload_from(&mut source)
            .await
            .map(|_| ())
            .map_err(|error| (false, miette::miette!("{error}")))
    };
    let decrypted = async {
        let mut buffer = vec![0; CHUNK];
        loop {
            let read = decryptor
                .read(&mut buffer)
                .map_err(|error| (true, miette::miette!("{error}")))?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])
                .await
                .map_err(|error| (false, miette::miette!("{error}")))?;
        }
        sink.shutdown()
            .await
            .map_err(|error| (false, miette::miette!("{error}")))
    };
    let sent = tokio::try_join!(sent, decrypted).map(|_| ());
    if let Err((true, error)) = sent {
        // Auth would otherwise wait for the rest of the bundle.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), operation.cancel()).await;
        return Err(error);
    }
    // When Auth stopped reading, its own answer says why.
    let snapshot = operation.wait().await.into_diagnostic()?;
    let result = match snapshot.output {
        Some(result) => serde_json::to_value(result).into_diagnostic()?,
        None => {
            return Err(ended(
                &format!("{:?}", snapshot.state),
                snapshot.error.as_ref(),
            ))
        }
    };
    sent.map_err(|(_, error)| error)?;
    Ok(result)
}

/// What a bundle holds and what the administrator recreates by hand.
fn print_report(report: &Value) {
    let mut details = output::Details::new(format!(
        "Service {} ({})",
        output::field(report, "title"),
        output::field(report, "deploymentId")
    ))
    .field("Participant", output::field(report, "participantId"))
    .field("Exported from", output::field(report, "sourceOrigin"))
    .time("Exported", &output::field(report, "exportedAt"))
    .field("Content SHA-256", output::field(report, "contentDigest"))
    .field(
        "Enrollment secrets",
        output::field(report, "enrollmentSecrets"),
    );
    details = details
        .section("Instances")
        .items(output::list(report, "instances").iter().map(|instance| {
            format!(
                "{} {} ({})",
                output::field(instance, "instanceId"),
                output::field(instance, "title"),
                output::field(instance, "state")
            )
        }));
    details = details
        .section("Data")
        .items(output::list(report, "resources").iter().map(|resource| {
            format!(
                "{} {}: {} entries, {} bytes",
                output::field(resource, "kind"),
                output::field(resource, "name"),
                output::field(resource, "messages"),
                output::field(resource, "bytes")
            )
        }));
    let webhooks = output::list(report, "webhooks");
    if !webhooks.is_empty() {
        details = details
            .section("Webhook URLs to give senders")
            .items(webhooks.iter().map(|webhook| {
                format!(
                    "{}: {}",
                    output::field(webhook, "endpoint"),
                    output::field(webhook, "url")
                )
            }));
    }
    let roles = output::list(report, "roles");
    let grants = output::list(report, "grants");
    if !roles.is_empty() || !grants.is_empty() {
        details = details.section("Access the bundle carries").items(
            roles
                .iter()
                .map(|role| {
                    format!(
                        "role {} ({} members): {}",
                        output::field(role, "name"),
                        output::field(role, "members"),
                        output::list(role, "capabilities")
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .chain(grants.iter().map(|grant| {
                    format!(
                        "{} granted directly to {} users",
                        output::field(grant, "capability"),
                        output::field(grant, "users")
                    )
                })),
        );
    }
    // What an import recreated and skipped, one line each.
    let change = |item: &Value| {
        let mut line = output::field(item, "kind");
        for (label, key) in [
            ("role", "role"),
            ("capability", "capability"),
            ("user", "userId"),
            ("as", "matchedUserId"),
            ("app", "clientId"),
        ] {
            if item.get(key).is_some_and(|value| !value.is_null()) {
                line.push_str(&format!(" {label} {}", output::field(item, key)));
            }
        }
        if item.get("reason").is_some_and(|value| !value.is_null()) {
            line.push_str(&format!(": {}", output::field(item, "reason")));
        }
        line
    };
    if let Some(access) = report.get("access").filter(|access| !access.is_null()) {
        details = details
            .section("Access recreated")
            .items(output::list(access, "recreated").iter().map(change));
        let skipped = output::list(access, "skipped");
        if !skipped.is_empty() {
            details = details
                .section("Access skipped")
                .items(skipped.iter().map(change));
        }
    }
    details = details.section("Stayed behind").items(
        output::list(report, "leftBehind")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    let unread = output::list(report, "notUnderstood");
    if !unread.is_empty() {
        details = details
            .section("Not understood, left out")
            .items(unread.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    details.print();
}
