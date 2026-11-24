//! `trellis svc <ID> export` and `trellis svc import`: moving a service to
//! another installation.
//!
//! Auth reads the service into a bundle's content and sends it over the
//! signed-in session; this command encrypts it with the operator's passphrase
//! as it arrives, so the file is never written in the clear and the
//! passphrase never leaves this process. Import checks the file's SHA-256,
//! decrypts it as it sends it, and Auth checks every part against the
//! bundle's manifest before it changes anything.
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

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use miette::IntoDiagnostic;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::*;
use crate::{output, prompt};

/// The header's first line.
const ENVELOPE: &str = "trellis-service-bundle v1";
/// Plaintext bytes per chunk.
const CHUNK: usize = 64 * 1024;
/// Bytes a chunk's tag adds.
const TAG: usize = 16;
/// Argon2id cost this build writes: 64 MiB, three passes, one lane.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 1;
/// Costs above these are refused, so a forged header cannot exhaust memory.
const MAX_MEMORY_KIB: u32 = 2 * 1024 * 1024;
const MAX_PASSES: u32 = 16;
const MAX_LANES: u32 = 16;
/// The shortest passphrase accepted for a new bundle.
const MIN_PASSPHRASE: usize = 12;

/// The keys one passphrase and header give.
struct Keys {
    header: hmac::Key,
    payload: LessSafeKey,
}

fn derive(
    passphrase: &str,
    salt: &[u8],
    (memory, passes, lanes): (u32, u32, u32),
    nonce: &[u8],
) -> miette::Result<Keys> {
    let params = argon2::Params::new(memory, passes, lanes, Some(32))
        .map_err(|error| miette::miette!("the bundle's key derivation: {error}"))?;
    let mut key = [0; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| miette::miette!("the bundle's key derivation: {error}"))?;
    let root = hmac::Key::new(hmac::HMAC_SHA256, &key);
    let header = hmac::sign(&root, b"trellis-service-bundle header");
    let mut payload_label = b"trellis-service-bundle payload".to_vec();
    payload_label.extend_from_slice(nonce);
    let payload = hmac::sign(&root, &payload_label);
    Ok(Keys {
        header: hmac::Key::new(hmac::HMAC_SHA256, header.as_ref()),
        payload: LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, payload.as_ref())
                .map_err(|_| miette::miette!("the bundle's content key"))?,
        ),
    })
}

fn chunk_nonce(counter: u64, last: bool) -> Nonce {
    let mut nonce = [0; 12];
    nonce[3..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    Nonce::assume_unique_for_key(nonce)
}

/// Encrypts content into a bundle file as it is written.
struct Encryptor<W: Write> {
    out: W,
    key: LessSafeKey,
    counter: u64,
    pending: Vec<u8>,
}

impl<W: Write> Encryptor<W> {
    fn new(mut out: W, passphrase: &str) -> miette::Result<Self> {
        let random = SystemRandom::new();
        let (mut salt, mut nonce) = ([0; 16], [0; 16]);
        random
            .fill(&mut salt)
            .and_then(|()| random.fill(&mut nonce))
            .map_err(|_| miette::miette!("no random bytes for the bundle"))?;
        let keys = derive(passphrase, &salt, (MEMORY_KIB, PASSES, LANES), &nonce)?;
        let header = format!(
            "{ENVELOPE}\nargon2id m={MEMORY_KIB} t={PASSES} p={LANES} salt={}\nnonce={}\n",
            URL_SAFE_NO_PAD.encode(salt),
            URL_SAFE_NO_PAD.encode(nonce)
        );
        let mac = hmac::sign(&keys.header, header.as_bytes());
        writeln!(out, "{header}mac={}", URL_SAFE_NO_PAD.encode(mac.as_ref())).into_diagnostic()?;
        Ok(Self {
            out,
            key: keys.payload,
            counter: 0,
            pending: Vec::with_capacity(2 * CHUNK),
        })
    }

    fn seal(&mut self, length: usize, last: bool) -> miette::Result<()> {
        let mut chunk: Vec<u8> = self.pending.drain(..length).collect();
        self.key
            .seal_in_place_append_tag(chunk_nonce(self.counter, last), Aad::empty(), &mut chunk)
            .map_err(|_| miette::miette!("cannot encrypt the bundle"))?;
        self.counter += 1;
        self.out.write_all(&chunk).into_diagnostic()
    }

    fn write(&mut self, data: &[u8]) -> miette::Result<()> {
        self.pending.extend_from_slice(data);
        // The final chunk is sealed by `finish`, so a full one waits until
        // more follows it.
        while self.pending.len() > CHUNK {
            self.seal(CHUNK, false)?;
        }
        Ok(())
    }

    fn finish(mut self) -> miette::Result<W> {
        let length = self.pending.len();
        self.seal(length, true)?;
        self.out.flush().into_diagnostic()?;
        Ok(self.out)
    }
}

/// Decrypts a bundle file's content as it is read.
struct Decryptor<R: BufRead> {
    input: R,
    key: LessSafeKey,
    counter: u64,
    buffer: Vec<u8>,
    position: usize,
    finished: bool,
}

fn header_line(input: &mut impl BufRead, prefix: &str) -> miette::Result<String> {
    let mut line = String::new();
    input
        .by_ref()
        .take(512)
        .read_line(&mut line)
        .into_diagnostic()?;
    line.strip_suffix('\n')
        .and_then(|line| line.strip_prefix(prefix))
        .map(str::to_owned)
        .ok_or_else(|| miette::miette!("this is not a Trellis service bundle"))
}

impl<R: BufRead> Decryptor<R> {
    fn open(mut input: R, passphrase: &str) -> miette::Result<Self> {
        let first = header_line(&mut input, "")?;
        miette::ensure!(
            first == ENVELOPE,
            "this is not a service bundle this Trellis reads (its header says {first:?})"
        );
        let costs = header_line(&mut input, "argon2id ")?;
        let nonce_text = header_line(&mut input, "nonce=")?;
        let mac = header_line(&mut input, "mac=")?;
        let cost = |name: &str, max: u32| -> miette::Result<u32> {
            costs
                .split(' ')
                .find_map(|part| part.strip_prefix(&format!("{name}=")))
                .and_then(|value| value.parse().ok())
                .filter(|value| *value <= max)
                .ok_or_else(|| miette::miette!("the bundle's header has no usable {name}"))
        };
        let costs_used = (
            cost("m", MAX_MEMORY_KIB)?,
            cost("t", MAX_PASSES)?,
            cost("p", MAX_LANES)?,
        );
        let salt = costs
            .split(' ')
            .find_map(|part| part.strip_prefix("salt="))
            .and_then(|salt| URL_SAFE_NO_PAD.decode(salt).ok())
            .ok_or_else(|| miette::miette!("the bundle's header has no salt"))?;
        let nonce = URL_SAFE_NO_PAD
            .decode(&nonce_text)
            .map_err(|_| miette::miette!("the bundle's header has no nonce"))?;
        let mac = URL_SAFE_NO_PAD
            .decode(&mac)
            .map_err(|_| miette::miette!("the bundle's header has no MAC"))?;
        let keys = derive(passphrase, &salt, costs_used, &nonce)?;
        let header = format!("{ENVELOPE}\nargon2id {costs}\nnonce={nonce_text}\n");
        hmac::verify(&keys.header, header.as_bytes(), &mac).map_err(|_| {
            miette::miette!(
                help = "Check the passphrase the bundle was exported with.",
                "the passphrase is wrong, or the bundle's header is damaged"
            )
        })?;
        Ok(Self {
            input,
            key: keys.payload,
            counter: 0,
            buffer: Vec::new(),
            position: 0,
            finished: false,
        })
    }
}

impl<R: BufRead> Read for Decryptor<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.buffer.len() {
            if self.finished {
                return Ok(0);
            }
            let mut chunk = Vec::with_capacity(CHUNK + TAG);
            (&mut self.input)
                .take((CHUNK + TAG) as u64)
                .read_to_end(&mut chunk)?;
            // A chunk shorter than a full one, or one with nothing after it,
            // is the last.
            let last = chunk.len() < CHUNK + TAG || self.input.fill_buf()?.is_empty();
            let damaged =
                || std::io::Error::other("the bundle is damaged: a chunk does not decrypt");
            let plain = self
                .key
                .open_in_place(chunk_nonce(self.counter, last), Aad::empty(), &mut chunk)
                .map_err(|_| damaged())?
                .len();
            chunk.truncate(plain);
            self.counter += 1;
            self.buffer = chunk;
            self.position = 0;
            self.finished = last;
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

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

/// The SHA-256 of a file, in lowercase hex.
fn file_digest(path: &Path) -> miette::Result<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| miette::miette!("cannot read {}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = file.read(&mut buffer).into_diagnostic()?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
        let mut encryptor = Encryptor::new(std::io::BufWriter::new(file), &passphrase)?;
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
        let (info, ()) = tokio::try_join!(received, encrypted)?;
        let file = encryptor
            .finish()?
            .into_inner()
            .map_err(|error| miette::miette!("cannot write the bundle: {error}"))?;
        file.sync_all().into_diagnostic()?;
        Ok::<_, miette::Report>(info)
    }
    .await;
    // Dropping the partial file on any failure deletes it.
    written?;
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
    let digest = file_digest(&out)?;
    let size = std::fs::metadata(&out).into_diagnostic()?.len();
    let report = &result["report"];
    if output::is_json(format) {
        return output::print_json(&json!({
            "file": out,
            "size": size,
            "digest": digest,
            "deploymentId": deployment_id,
            "report": report,
        }));
    }
    output::success(format!("Exported {id} to {}", out.display()));
    output::Details::untitled()
        .field("File", out.display())
        .field("Size", format!("{size} bytes"))
        .field("SHA-256", &digest)
        .print();
    print_report(report);
    output::note(
        "Give the importing administrator the SHA-256 above through a channel you trust, \
        and the passphrase separately. This installation still runs the service: stop or \
        remove it once it runs in the new one.",
    );
    output::next(&format!(
        "trellis svc import {} --digest {digest}",
        out.display()
    ));
    Ok(())
}

/// Import a bundle written by `trellis svc <ID> export`.
pub(super) async fn import(format: OutputFormat, args: &SvcImportArgs) -> miette::Result<()> {
    let digest = file_digest(&args.file)?;
    match &args.digest {
        Some(expected) => miette::ensure!(
            expected.trim().eq_ignore_ascii_case(&digest),
            help = "Copy the file again, or ask the exporting administrator for the right digest.",
            "the bundle's SHA-256 is {digest}, not the {} given; it is not the file that was \
            exported",
            expected.trim()
        ),
        None => {
            miette::ensure!(
                prompt::interactive(),
                help = "Give the digest the export printed with --digest.",
                "the bundle's SHA-256 must be confirmed: it is {digest}"
            );
            let mut form = prompt::Form::new(true);
            form.explain(&format!("The bundle's SHA-256 is {digest}"));
            let same = form.yes_no("Is it the one the export printed?", None, false)?;
            miette::ensure!(same, "not imported: the digest was not confirmed");
        }
    }
    let passphrase = passphrase(args.passphrase_file.as_deref(), false)?;
    let file = std::fs::File::open(&args.file)
        .map_err(|error| miette::miette!("cannot read {}: {error}", args.file.display()))?;
    // A wrong passphrase is refused here, before anything is sent.
    let mut decryptor = Decryptor::open(std::io::BufReader::new(file), &passphrase)?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1();
    let operation = auth
        .deployments_import()
        .start(&wire(json!({
            "check": args.check,
            "acknowledgeSharedApis": args.acknowledge_shared_apis,
            "acknowledgeUnauthenticatedWebhooks": args.acknowledge_unauthenticated_webhooks,
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
    if output::is_json(format) {
        return output::print_json(&json!({ "digest": digest, "import": result }));
    }
    let report = &result["report"];
    let deployment = output::field(report, "deploymentId");
    if result["applied"] == Value::Bool(false) {
        output::success(format!(
            "The bundle checks out; importing it adds deployment {deployment}"
        ));
    } else if result["resumed"] == Value::Bool(true) {
        output::success(format!("Finished importing deployment {deployment}"));
    } else {
        output::success(format!("Imported deployment {deployment}"));
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
        details = details
            .section("Access to recreate (import creates no roles or grants)")
            .items(
                roles
                    .iter()
                    .map(|role| {
                        format!(
                            "role {} ({} assigned): {}",
                            output::field(role, "name"),
                            output::field(role, "assignments"),
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
                            output::field(grant, "principals")
                        )
                    })),
            );
    }
    details = details.section("Stayed behind").items(
        output::list(report, "leftBehind")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    details.print();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed(content: &[u8], passphrase: &str) -> Vec<u8> {
        let mut encryptor = Encryptor::new(Vec::new(), passphrase).unwrap();
        encryptor.write(content).unwrap();
        encryptor.finish().unwrap()
    }

    fn opened(bundle: &[u8], passphrase: &str) -> miette::Result<Vec<u8>> {
        let mut decryptor = Decryptor::open(std::io::BufReader::new(bundle), passphrase)?;
        let mut content = Vec::new();
        decryptor.read_to_end(&mut content).into_diagnostic()?;
        Ok(content)
    }

    /// Content of every length class comes back as written: empty, inside
    /// one chunk, exactly one chunk, and across several.
    #[test]
    fn a_bundle_opens_with_its_passphrase_to_the_content_written() {
        for length in [0, 5, CHUNK, 3 * CHUNK + 17] {
            let content: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let bundle = sealed(&content, "correct horse battery");
            assert_eq!(opened(&bundle, "correct horse battery").unwrap(), content);
        }
    }

    /// A changed byte, a dropped final chunk, or swapped chunks fail the
    /// content, even under the right passphrase.
    #[test]
    fn changed_or_reordered_content_does_not_decrypt() {
        let content: Vec<u8> = (0..2 * CHUNK + 9).map(|index| (index % 7) as u8).collect();
        let bundle = sealed(&content, "correct horse battery");
        let header = bundle
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .nth(3)
            .unwrap()
            .0
            + 1;
        let mut changed = bundle.clone();
        changed[header + 100] ^= 1;
        assert!(opened(&changed, "correct horse battery").is_err());
        let cut = &bundle[..header + CHUNK + TAG];
        assert!(opened(cut, "correct horse battery").is_err());
        let mut swapped = bundle[..header].to_vec();
        swapped.extend_from_slice(&bundle[header + CHUNK + TAG..header + 2 * (CHUNK + TAG)]);
        swapped.extend_from_slice(&bundle[header..header + CHUNK + TAG]);
        swapped.extend_from_slice(&bundle[header + 2 * (CHUNK + TAG)..]);
        assert!(opened(&swapped, "correct horse battery").is_err());
    }
}
