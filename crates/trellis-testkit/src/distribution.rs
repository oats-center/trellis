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

/// Source of the production server, which also sets up the installation.
#[derive(Clone, Debug)]
pub enum NativeSource {
    /// An explicit executable, also usable offline. An invalid path never triggers a download.
    Path {
        /// Production server executable.
        server: PathBuf,
    },
    /// Acquire a published release: `TRELLIS_TEST_VERSION` when set, otherwise
    /// the latest stable release (the newest prerelease before the first stable
    /// one), each checked against the checksum published with it.
    Release,
    /// Acquire the newest published release no newer than this version: the
    /// release a build of this version upgrades, which an upgrade test
    /// installs first. A stable version takes the newest such stable release
    /// when there is one. It is checked against the checksum published with
    /// it; `TRELLIS_TEST_VERSION` does not apply.
    PreviousRelease(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Distribution {
    pub server: PathBuf,
    /// The version the server reports, without build metadata.
    pub version: String,
    /// The published release the server was acquired from; `None` for a
    /// server given by path.
    pub release: Option<String>,
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
    let _ = rustls::crypto::ring::default_provider().install_default();
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
/// stable one. With `upgraded`, the newest release no newer than it instead,
/// a stable `upgraded` taking the newest such stable release when there is
/// one.
/// A recent answer is reused, and an unreachable GitHub falls back to the last
/// answer.
async fn release_version(cache: &Path, upgraded: Option<&str>) -> Result<String, TrellisTestError> {
    let upgraded = match upgraded {
        None => {
            if let Ok(version) = std::env::var("TRELLIS_TEST_VERSION") {
                return Ok(version.trim_start_matches('v').to_owned());
            }
            None
        }
        Some(upgraded) => Some(
            semver::Version::parse(upgraded.trim_start_matches('v')).map_err(|cause| {
                error(
                    TrellisTestErrorKind::InvalidConfiguration,
                    format!("{upgraded} is not a release version: {cause}"),
                )
            })?,
        ),
    };
    let record = cache.join(match &upgraded {
        None => "latest-release.json".to_owned(),
        Some(upgraded) => format!("release-upgraded-by-{upgraded}.json"),
    });
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
    let chosen = choose_release(RELEASES_PER_PAGE, upgraded.as_ref(), |page| async move {
        get(&format!(
            "https://api.github.com/repos/oats-center/trellis/releases\
            ?per_page={RELEASES_PER_PAGE}&page={page}"
        ))
        .await
    })
    .await;
    let version = match chosen {
        Ok(chosen) => chosen.map(|version| version.to_string()),
        Err(cause) => return cached.map(|cached| cached.version).ok_or(cause),
    }
    .ok_or_else(|| {
        error(
            TrellisTestErrorKind::MissingBinary,
            match &upgraded {
                None => "oats-center/trellis has no published release".to_owned(),
                Some(upgraded) => {
                    format!("oats-center/trellis has no published release up to {upgraded}")
                }
            },
        )
    })?;
    let latest = LatestRelease {
        version: version.clone(),
        checked_unix: now,
    };
    std::fs::write(&record, serde_json::json!(latest).to_string())?;
    Ok(version)
}

/// GitHub's largest release listing page.
const RELEASES_PER_PAGE: usize = 100;

/// Chooses the release to test against from every published release, reading
/// the listing one page at a time (`page` is 1-based) until a page shorter than
/// `per_page` ends it. Without `upgraded`, the newest stable release, else the
/// newest release. With it, the newest release no newer than it, a stable
/// `upgraded` taking the newest such stable release when there is one.
///
/// GitHub orders the listing by creation date, not version, so the whole
/// listing is read before choosing.
async fn choose_release<F, Fut>(
    per_page: usize,
    upgraded: Option<&semver::Version>,
    mut page: F,
) -> Result<Option<semver::Version>, TrellisTestError>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, TrellisTestError>>,
{
    let mut published = Vec::new();
    for number in 1.. {
        let releases = serde_json::from_slice::<Vec<GithubRelease>>(&page(number).await?).map_err(
            |cause| {
                error(
                    TrellisTestErrorKind::InvalidConfiguration,
                    cause.to_string(),
                )
            },
        )?;
        let count = releases.len();
        published.extend(
            releases
                .into_iter()
                .filter(|release| !release.draft)
                .filter_map(|release| {
                    let tag = release.tag_name.trim_start_matches('v');
                    Some((semver::Version::parse(tag).ok()?, release.prerelease))
                })
                .filter(|(version, _)| upgraded.is_none_or(|upgraded| version <= upgraded)),
        );
        if count < per_page {
            break;
        }
    }
    published.sort_by(|a, b| b.0.cmp(&a.0));
    let stable = upgraded.is_none_or(|upgraded| upgraded.pre.is_empty());
    Ok(published
        .iter()
        .find(|(_, prerelease)| stable && !prerelease)
        .or_else(|| published.first())
        .map(|(version, _)| version.clone()))
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
        None => match std::env::var_os("TRELLIS_TEST_SERVER_BIN") {
            Some(server) => NativeSource::Path {
                server: server.into(),
            },
            None => NativeSource::Release,
        },
    };
    let mut expected = None;
    let server = match &source {
        NativeSource::Path { server } => executable(server)?,
        NativeSource::Release | NativeSource::PreviousRelease(_) => {
            let _ = target()?;
            let cache = cache_dir()?;
            private_dir(&cache)?;
            let upgraded = match &source {
                NativeSource::PreviousRelease(upgraded) => Some(upgraded.as_str()),
                _ => None,
            };
            let version = release_version(&cache, upgraded).await?;
            let installed = release_binary(&cache, &version, "trellis-server").await?;
            expected = Some(version);
            installed
        }
    };
    let version = {
        let path = &server;
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(path)
                .env(
                    "TOKIO_WORKER_THREADS",
                    std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| "2".into()),
                )
                .arg("--version")
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
        let actual = text
            .trim()
            .strip_prefix("trellis-server ")
            .unwrap_or_default()
            .to_owned();
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
        static ANNOUNCED: std::sync::Once = std::sync::Once::new();
        ANNOUNCED.call_once(|| {
            eprintln!("trellis-testkit: testing against trellis-server {actual}");
        });
        actual
    };
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
    Ok(Distribution {
        server,
        version,
        release: expected,
        nats,
    })
}

/// The `binary` asset of published release `version`, for this platform,
/// checked against the checksum published with it. Release assets are
/// immutable, so a fetched checksum is kept in `cache`.
async fn release_binary(
    cache: &Path,
    version: &str,
    binary: &str,
) -> Result<PathBuf, TrellisTestError> {
    let platform = if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        "apple-darwin"
    };
    let target = format!("{}-{platform}", std::env::consts::ARCH);
    let recorded = cache.join(format!("{binary}-{version}-{target}.sha256"));
    let checksum = match std::fs::read_to_string(&recorded) {
        Ok(checksum) => checksum,
        Err(_) => {
            let url = format!("{RELEASES}/v{version}/checksum-{version}-{target}-{binary}.sha256");
            let published = String::from_utf8_lossy(&get(&url).await?)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            std::fs::write(&recorded, &published)?;
            published
        }
    };
    Ok(install(
        &format!("{RELEASES}/v{version}/{binary}-{version}-{target}.tar.gz"),
        checksum.trim(),
        &[binary],
        cache,
    )
    .await?
    .remove(0))
}

/// The `trellis` CLI of the same Trellis as `distribution`'s server: the
/// published CLI of its release, or `TRELLIS_TEST_CLI_BIN` for a server given
/// by path.
pub(crate) async fn cli(distribution: &Distribution) -> Result<PathBuf, TrellisTestError> {
    match &distribution.release {
        Some(version) => {
            let cache = cache_dir()?;
            private_dir(&cache)?;
            release_binary(&cache, version, "trellis").await
        }
        None => {
            let path = std::env::var_os("TRELLIS_TEST_CLI_BIN").ok_or_else(|| {
                error(
                    TrellisTestErrorKind::MissingBinary,
                    "set TRELLIS_TEST_CLI_BIN to the trellis CLI built with the server under test",
                )
            })?;
            executable(Path::new(&path))
        }
    }
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
        let quarantine = cache.join(format!(".corrupt-{}", ulid::Ulid::generate()));
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

    /// The listing GitHub returned at 50 releases a page: 53 release
    /// candidates, reduced to the fields kept per release.
    const RECORDED_PAGES: [&[u8]; 2] = [
        include_bytes!("testdata/github-releases-page-1.json"),
        include_bytes!("testdata/github-releases-page-2.json"),
    ];

    async fn choose_from(
        pages: &[Vec<u8>],
        upgraded: &str,
    ) -> Result<Option<semver::Version>, TrellisTestError> {
        let upgraded = semver::Version::parse(upgraded).unwrap();
        choose_release(50, Some(&upgraded), |page| {
            std::future::ready(Ok(pages[page - 1].clone()))
        })
        .await
    }

    #[tokio::test]
    async fn the_previous_release_is_found_beyond_the_first_listing_page() {
        let recorded = RECORDED_PAGES.map(<[u8]>::to_vec);
        // All 50 releases on the first page are newer than rc.4.
        assert_eq!(
            choose_from(&recorded, "0.100.0-rc.4").await.unwrap(),
            Some(semver::Version::parse("0.100.0-rc.4").unwrap())
        );

        // A stable release older than every release candidate, listed last:
        // a stable candidate upgrades it, not the newest release candidate.
        let mut last: Vec<serde_json::Value> = serde_json::from_slice(&recorded[1]).unwrap();
        last.push(serde_json::json!({
            "tag_name": "v0.99.0",
            "name": "v0.99.0",
            "draft": false,
            "prerelease": false,
            "created_at": "2026-10-01T00:00:00Z",
            "published_at": "2026-10-01T00:00:00Z",
        }));
        let with_stable = [recorded[0].clone(), serde_json::to_vec(&last).unwrap()];
        assert_eq!(
            choose_from(&with_stable, "0.100.0").await.unwrap(),
            Some(semver::Version::parse("0.99.0").unwrap())
        );
    }

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
