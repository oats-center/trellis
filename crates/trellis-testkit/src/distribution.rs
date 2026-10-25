//! Verified immutable production executables, shared between isolated runtimes.

use crate::{
    error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage},
    runtime::NatsSource,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

/// Source of the production CLI and server.
#[derive(Clone, Debug)]
pub enum NativeSource {
    /// Explicit executables, also usable offline. Invalid paths never trigger downloads.
    Path {
        /// Production CLI executable.
        cli: PathBuf,
        /// Production server executable.
        server: PathBuf,
    },
    /// Acquire a published release: `TRELLIS_TEST_VERSION` when set, otherwise
    /// the latest stable release (the newest prerelease before the first stable
    /// one), each checked against the checksum published with it.
    Release,
}

#[derive(Clone, Debug)]
pub(crate) struct Distribution {
    pub cli: PathBuf,
    pub server: PathBuf,
    pub nats: PathBuf,
}

fn error(kind: TrellisTestErrorKind, message: impl Into<String>) -> TrellisTestError {
    TrellisTestError::new(kind, TrellisTestStage::Validation, message)
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

#[derive(Deserialize, serde::Serialize)]
struct LatestRelease {
    version: String,
    checked_unix: u64,
}

/// How long a resolved latest release is reused before GitHub is asked again;
/// unauthenticated API requests are rate limited per address.
const LATEST_RELEASE_TTL: Duration = Duration::from_secs(3600);
const RELEASES: &str = "https://github.com/oats-center/trellis/releases/download";

async fn get(url: &str) -> Result<Vec<u8>, TrellisTestError> {
    let mut request = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("trellis-testkit")
        .build()
        .map_err(|cause| error(TrellisTestErrorKind::Io, cause.to_string()))?
        .get(url);
    if url.starts_with("https://api.github.com/") {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            request = request.bearer_auth(token);
        }
    }
    let response = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|cause| error(TrellisTestErrorKind::Io, format!("{url}: {cause}")))?;
    Ok(response
        .bytes()
        .await
        .map_err(|cause| error(TrellisTestErrorKind::Io, cause.to_string()))?
        .to_vec())
}

/// The release version to test against: `TRELLIS_TEST_VERSION`, or the latest
/// stable release, falling back to the newest prerelease before the first
/// stable one. A recent answer is reused, and an unreachable GitHub falls back
/// to the last answer.
async fn release_version(cache: &Path) -> Result<String, TrellisTestError> {
    if let Ok(version) = std::env::var("TRELLIS_TEST_VERSION") {
        return Ok(version.trim_start_matches('v').to_owned());
    }
    let record = cache.join("latest-release.json");
    let cached = std::fs::read(&record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LatestRelease>(&bytes).ok());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    if let Some(cached) = &cached {
        if now.saturating_sub(cached.checked_unix) < LATEST_RELEASE_TTL.as_secs() {
            return Ok(cached.version.clone());
        }
    }
    let releases =
        match get("https://api.github.com/repos/oats-center/trellis/releases?per_page=50")
            .await
            .and_then(|bytes| {
                serde_json::from_slice::<Vec<GithubRelease>>(&bytes).map_err(|cause| {
                    error(
                        TrellisTestErrorKind::InvalidConfiguration,
                        cause.to_string(),
                    )
                })
            }) {
            Ok(releases) => releases,
            Err(cause) => return cached.map(|cached| cached.version).ok_or(cause),
        };
    // GitHub lists releases newest first.
    let published = releases.iter().filter(|release| !release.draft);
    let version = published
        .clone()
        .find(|release| !release.prerelease)
        .or_else(|| published.clone().next())
        .map(|release| release.tag_name.trim_start_matches('v').to_owned())
        .ok_or_else(|| {
            error(
                TrellisTestErrorKind::MissingBinary,
                "oats-center/trellis has no published release",
            )
        })?;
    let latest = LatestRelease {
        version: version.clone(),
        checked_unix: now,
    };
    std::fs::write(&record, serde_json::json!(latest).to_string())?;
    Ok(version)
}
#[derive(Deserialize)]
struct NatsPin {
    version: String,
    sha256: BTreeMap<String, String>,
}

pub(crate) async fn resolve(
    source: Option<NativeSource>,
    nats: &NatsSource,
) -> Result<Distribution, TrellisTestError> {
    let source = match source {
        Some(source) => source,
        None => match (
            std::env::var_os("TRELLIS_TEST_CLI_BIN"),
            std::env::var_os("TRELLIS_TEST_SERVER_BIN"),
        ) {
            (Some(cli), Some(server)) => NativeSource::Path {
                cli: cli.into(),
                server: server.into(),
            },
            (None, None) => NativeSource::Release,
            _ => {
                return Err(error(
                    TrellisTestErrorKind::MissingBinary,
                    "set both TRELLIS_TEST_CLI_BIN and TRELLIS_TEST_SERVER_BIN",
                ))
            }
        },
    };
    let mut expected = None;
    let (cli, server) = match source {
        NativeSource::Path { cli, server } => (executable(&cli)?, executable(&server)?),
        NativeSource::Release => {
            let _ = target()?;
            let cache = cache_dir()?;
            private_dir(&cache)?;
            let version = release_version(&cache).await?;
            let platform = if cfg!(target_os = "linux") {
                "unknown-linux-gnu"
            } else {
                "apple-darwin"
            };
            let target = format!("{}-{platform}", std::env::consts::ARCH);
            // Release assets are immutable, so a fetched checksum is kept.
            let recorded = cache.join(format!("trellis-{version}-{target}.sha256"));
            let checksum = match std::fs::read_to_string(&recorded) {
                Ok(checksum) => checksum,
                Err(_) => {
                    let url =
                        format!("{RELEASES}/v{version}/checksum-{version}-{target}-trellis.sha256");
                    let published = String::from_utf8_lossy(&get(&url).await?)
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    std::fs::write(&recorded, &published)?;
                    published
                }
            };
            let files = install(
                &format!("{RELEASES}/v{version}/trellis-{version}-{target}.tar.gz"),
                checksum.trim(),
                &["trellis", "trellis-server"],
                &cache,
            )
            .await?;
            expected = Some(version);
            (files[0].clone(), files[1].clone())
        }
    };
    for (path, args, cli_output) in [
        (&cli, vec!["--format", "json", "version"], true),
        (&server, vec!["--version"], false),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(path)
                .env(
                    "TOKIO_WORKER_THREADS",
                    std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| "2".into()),
                )
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| {
            error(
                TrellisTestErrorKind::Timeout,
                "verifying native executable version timed out",
            )
        })??;
        let text = String::from_utf8_lossy(&output.stdout);
        let actual = if cli_output {
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| value["version"].as_str().map(str::to_owned))
                .unwrap_or_default()
        } else {
            text.trim()
                .strip_prefix("trellis-server ")
                .unwrap_or_default()
                .to_owned()
        };
        let actual = actual.split('+').next().unwrap_or_default().to_owned();
        if !output.status.success() || actual.is_empty() {
            return Err(error(
                TrellisTestErrorKind::InvalidBinary,
                format!("cannot read the version of {}", path.display()),
            ));
        }
        if let Some(expected) = expected.as_ref().filter(|expected| **expected != actual) {
            return Err(error(
                TrellisTestErrorKind::InvalidBinary,
                format!(
                    "{} reports {actual}, not release {expected}",
                    path.display()
                ),
            ));
        }
        if !cli_output {
            static ANNOUNCED: std::sync::Once = std::sync::Once::new();
            ANNOUNCED.call_once(|| {
                eprintln!("trellis-testkit: testing against trellis-server {actual}");
            });
        }
    }
    let nats = match nats {
        NatsSource::Path(path) => executable(path)?,
        NatsSource::PathLookup => {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .find_map(|dir| executable(&dir.join("nats-server")).ok())
                .ok_or_else(|| {
                    error(
                        TrellisTestErrorKind::MissingBinary,
                        "nats-server is not on PATH",
                    )
                })?
        }
        NatsSource::DownloadPinned => {
            let pins: BTreeMap<String, NatsPin> = serde_json::from_str(include_str!(
                "../generated/nats-binaries.json"
            ))
            .map_err(|cause| {
                error(
                    TrellisTestErrorKind::InvalidConfiguration,
                    cause.to_string(),
                )
            })?;
            let pin = pins.get("nats-server").ok_or_else(|| {
                error(
                    TrellisTestErrorKind::InvalidConfiguration,
                    "missing NATS pin",
                )
            })?;
            let target = target()?;
            let sha = pin.sha256.get(&target).ok_or_else(|| {
                error(
                    TrellisTestErrorKind::UnsupportedPlatform,
                    "no NATS pin for this target",
                )
            })?;
            install(&format!("https://github.com/nats-io/nats-server/releases/download/v{0}/nats-server-v{0}-{target}.tar.gz", pin.version), sha,
                &[&format!("nats-server-v{}-{target}/nats-server", pin.version)], &cache_dir()?).await?.remove(0)
        }
    };
    Ok(Distribution { cli, server, nats })
}

fn target() -> Result<String, TrellisTestError> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => {
            return Err(error(
                TrellisTestErrorKind::UnsupportedPlatform,
                "unsupported native OS",
            ))
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => {
            return Err(error(
                TrellisTestErrorKind::UnsupportedPlatform,
                "unsupported native architecture",
            ))
        }
    };
    Ok(format!("{os}-{arch}"))
}

fn executable(path: &Path) -> Result<PathBuf, TrellisTestError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path).map_err(|_| {
        error(
            TrellisTestErrorKind::MissingBinary,
            format!("missing executable {}", path.display()),
        )
    })?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(error(
            TrellisTestErrorKind::InvalidBinary,
            format!("not a regular executable: {}", path.display()),
        ));
    }
    Ok(std::fs::canonicalize(path)?)
}

fn cache_dir() -> Result<PathBuf, TrellisTestError> {
    std::env::var_os("TRELLIS_TEST_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(|path| PathBuf::from(path).join("trellis-testkit"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".cache/trellis-testkit"))
        })
        .ok_or_else(|| {
            error(
                TrellisTestErrorKind::InvalidConfiguration,
                "set TRELLIS_TEST_CACHE_DIR or HOME",
            )
        })
}

/// Create the cache, refusing one another user could plant executables in.
fn private_dir(cache: &Path) -> Result<(), TrellisTestError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(cache)?;
    let meta = std::fs::symlink_metadata(cache)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(error(
            TrellisTestErrorKind::InvalidConfiguration,
            "native cache must be an owned private directory",
        ));
    }
    Ok(())
}

async fn install(
    url: &str,
    checksum: &str,
    entries: &[&str],
    cache: &Path,
) -> Result<Vec<PathBuf>, TrellisTestError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error(
            TrellisTestErrorKind::InvalidConfiguration,
            "invalid archive checksum",
        ));
    }
    private_dir(cache)?;
    let installed = cache.join(checksum);
    let lock_path = cache.join(format!("{checksum}.lock"));
    let _lock = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)?;
        file.lock()?;
        Ok(file)
    })
    .await
    .map_err(|cause| error(TrellisTestErrorKind::Io, cause.to_string()))??;
    let saved = std::fs::read(installed.join("archive.tar.gz")).ok();
    let bytes = match saved {
        Some(bytes)
            if Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == checksum.to_ascii_lowercase() =>
        {
            bytes
        }
        _ => get(url).await?,
    };
    if Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        != checksum.to_ascii_lowercase()
    {
        return Err(error(
            TrellisTestErrorKind::InvalidBinary,
            "native archive checksum mismatch",
        ));
    }
    let mut extracted = BTreeMap::new();
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice())).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if let Some(name) = entries.iter().find(|name| path == Path::new(name)) {
            if !entry.header().entry_type().is_file() || extracted.contains_key(*name) {
                return Err(error(
                    TrellisTestErrorKind::InvalidBinary,
                    "invalid native archive entry",
                ));
            }
            let mut body = Vec::new();
            entry.read_to_end(&mut body)?;
            extracted.insert((*name).to_owned(), body);
        }
    }
    if extracted.len() != entries.len() {
        return Err(error(
            TrellisTestErrorKind::InvalidBinary,
            "native archive missing executables",
        ));
    }
    let valid = std::fs::symlink_metadata(&installed).is_ok_and(|meta| {
        meta.is_dir()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0
    }) && entries.iter().enumerate().all(|(index, name)| {
        executable(&installed.join(index.to_string()))
            .is_ok_and(|path| std::fs::read(path).ok().as_ref() == extracted.get(*name))
    });
    if !valid {
        let stage = tempfile::Builder::new()
            .prefix(".native-")
            .tempdir_in(cache)?;
        std::fs::write(stage.path().join("archive.tar.gz"), bytes)?;
        for (index, name) in entries.iter().enumerate() {
            let path = stage.path().join(index.to_string());
            std::fs::write(&path, &extracted[*name])?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let quarantine = cache.join(format!(".corrupt-{}", ulid::Ulid::new()));
        match std::fs::rename(&installed, &quarantine) {
            Ok(()) => {}
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
            Err(cause) => return Err(cause.into()),
        }
        match std::fs::rename(stage.path(), &installed) {
            Ok(()) => {}
            Err(cause) if installed.is_dir() => {
                if !entries.iter().enumerate().all(|(index, name)| {
                    executable(&installed.join(index.to_string()))
                        .is_ok_and(|path| std::fs::read(path).ok().as_ref() == extracted.get(*name))
                }) {
                    return Err(cause.into());
                }
            }
            Err(cause) => return Err(cause.into()),
        }
        if quarantine.exists() {
            std::fs::remove_dir_all(quarantine)?;
        }
    }
    Ok((0..entries.len())
        .map(|index| installed.join(index.to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verified_download_serializes_installation_and_repairs_cached_executables_offline() {
        let binary = std::fs::read("/bin/echo").unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "echo", binary.as_slice())
            .unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let checksum = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/native.tar.gz", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(&bytes).await.unwrap();
            }
        });
        let cache = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(cache.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let wrong = "0".repeat(64);
        assert_eq!(
            install(&url, &wrong, &["echo"], cache.path())
                .await
                .unwrap_err()
                .kind(),
            TrellisTestErrorKind::InvalidBinary
        );
        assert!(!cache.path().join(&wrong).exists());
        let (a, b) = tokio::join!(
            install(&url, &checksum, &["echo"], cache.path()),
            install(&url, &checksum, &["echo"], cache.path())
        );
        let a = a.unwrap();
        let b = b.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        for path in [&a[0], &b[0]] {
            let output = std::process::Command::new(path)
                .arg("verified native archive")
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"verified native archive\n");
        }
        let outside = cache.path().join("unrelated");
        std::fs::write(&outside, b"application data").unwrap();
        std::fs::remove_file(&a[0]).unwrap();
        std::os::unix::fs::symlink(&outside, &a[0]).unwrap();
        let repaired = install(&url, &checksum, &["echo"], cache.path())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"application data");
        let output = std::process::Command::new(&repaired[0])
            .arg("offline repair")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"offline repair\n");
    }
}
