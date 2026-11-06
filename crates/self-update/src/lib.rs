//! Replace a Trellis binary with a newer GitHub release: the CLI and the
//! server both update themselves through this.

use miette::IntoDiagnostic;
use self_update::backends::github::ReleaseList;
use self_update::update::Release;
use self_update::version::bump_is_greater;
use self_update::{get_target, ArchiveKind, Compression, Download, Extract};
use serde::Serialize;

/// The binary to update and the GitHub repository whose releases carry it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelfUpdateTarget {
    repo_owner: &'static str,
    repo_name: &'static str,
    bin_name: &'static str,
    current_version: &'static str,
}

impl SelfUpdateTarget {
    /// `bin_name` is the archive and executable name; `current_version` is
    /// the running build's version.
    pub const fn new(
        repo_owner: &'static str,
        repo_name: &'static str,
        bin_name: &'static str,
        current_version: &'static str,
    ) -> Self {
        Self {
            repo_owner,
            repo_name,
            bin_name,
            current_version,
        }
    }

    const fn repo_owner(self) -> &'static str {
        self.repo_owner
    }

    const fn repo_name(self) -> &'static str {
        self.repo_name
    }

    const fn bin_name(self) -> &'static str {
        self.bin_name
    }

    const fn current_version(self) -> &'static str {
        self.current_version
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
/// Which releases count as newer.
#[serde(rename_all = "camelCase")]
pub enum ReleaseChannel {
    /// Stable releases only.
    Stable,
    /// Stable releases and release candidates.
    Prerelease,
}

impl ReleaseChannel {
    /// The channel a `--prerelease` flag selects.
    pub const fn from_prerelease_flag(prerelease: bool) -> Self {
        if prerelease {
            Self::Prerelease
        } else {
            Self::Stable
        }
    }

    fn accepts(self, prerelease: bool) -> bool {
        matches!(self, Self::Prerelease) || !prerelease
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// What a check found, without installing anything.
pub struct UpdateCheck {
    /// The channel checked.
    pub channel: ReleaseChannel,
    /// The running build's version.
    pub current_version: String,
    /// The newest release on the channel with an archive for this platform.
    pub latest_version: String,
    /// Whether `latest_version` is newer than `current_version`.
    pub needs_update: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
/// What an install did.
pub enum UpdateResult {
    /// Nothing newer exists; `version` is the running one.
    UpToDate {
        /// The running version.
        version: String,
    },
    /// The executable was replaced by `version`.
    Updated {
        /// The installed version.
        version: String,
    },
}

/// Find the newest release on `channel` with an archive for this platform.
pub fn check_for_update(
    target: SelfUpdateTarget,
    channel: ReleaseChannel,
) -> miette::Result<UpdateCheck> {
    let latest_version = latest_version(target, channel)?;
    let needs_update =
        bump_is_greater(target.current_version(), &latest_version).into_diagnostic()?;
    Ok(UpdateCheck {
        channel,
        current_version: target.current_version().to_string(),
        latest_version,
        needs_update,
    })
}

/// Replace the running executable with the newest release on `channel`, when
/// it is newer. A running process keeps its old binary until restarted.
pub fn install_update(
    target: SelfUpdateTarget,
    channel: ReleaseChannel,
) -> miette::Result<UpdateResult> {
    let latest_version = latest_version(target, channel)?;
    let needs_update =
        bump_is_greater(target.current_version(), &latest_version).into_diagnostic()?;
    if !needs_update {
        return Ok(UpdateResult::UpToDate {
            version: target.current_version().to_string(),
        });
    }

    let mut last_error = None;
    for candidate_tag in candidate_target_version_tags(&latest_version) {
        match install_update_for_tag(target, &candidate_tag) {
            Ok(result) => return Ok(result),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.expect("update attempted at least once"))
}

/// Download the release's archive and checksum file, refuse the archive
/// unless the checksum names it and matches, then replace this executable with
/// the binary inside.
fn install_update_for_tag(
    target: SelfUpdateTarget,
    target_tag: &str,
) -> miette::Result<UpdateResult> {
    let version = normalize_release_version(target_tag);
    let platform = get_target();
    let bin = target.bin_name();
    let archive_name = format!("{bin}-{version}-{platform}.tar.gz");
    let base = format!(
        "https://github.com/{}/{}/releases/download/{target_tag}",
        target.repo_owner(),
        target.repo_name()
    );
    let mut checksum = Vec::new();
    Download::from_url(format!("{base}/checksum-{version}-{platform}-{bin}.sha256"))
        .download_to(&mut checksum)
        .into_diagnostic()?;
    let work = tempfile::TempDir::new().into_diagnostic()?;
    let archive = work.path().join(&archive_name);
    Download::from_url(format!("{base}/{archive_name}"))
        .show_download_progress(true)
        .download_to(std::fs::File::create(&archive).into_diagnostic()?)
        .into_diagnostic()?;
    verify_checksum(
        &String::from_utf8_lossy(&checksum),
        &archive_name,
        &std::fs::read(&archive).into_diagnostic()?,
    )?;
    Extract::from_source(&archive)
        .archive(ArchiveKind::Tar(Some(Compression::Gz)))
        .extract_file(work.path(), bin)
        .into_diagnostic()?;
    self_replace::self_replace(work.path().join(bin)).into_diagnostic()?;
    Ok(UpdateResult::Updated { version })
}

/// Accept `archive` only when the release's `sha256sum` listing names
/// `archive_name` with the archive's exact SHA-256.
fn verify_checksum(listing: &str, archive_name: &str, archive: &[u8]) -> miette::Result<()> {
    use sha2::Digest as _;

    let actual: String = sha2::Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let listed = listing.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        (fields.next()?.trim_start_matches('*') == archive_name).then_some(digest)
    });
    match listed {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(()),
        Some(_) => Err(miette::miette!(
            "{archive_name} does not match its release checksum; nothing was installed"
        )),
        None => Err(miette::miette!(
            "the release checksum file does not list {archive_name}; nothing was installed"
        )),
    }
}

fn latest_version(target: SelfUpdateTarget, channel: ReleaseChannel) -> miette::Result<String> {
    let releases = ReleaseList::configure()
        .repo_owner(target.repo_owner())
        .repo_name(target.repo_name())
        .filter_target(get_target())
        .build()
        .into_diagnostic()?
        .fetch()
        .into_diagnostic()?;

    select_latest_version(
        releases
            .into_iter()
            .filter(|release| release_has_target_binary_asset(release, target))
            .map(|release| release.version().to_string())
            .filter(|version| channel.accepts(is_prerelease_version(version))),
        channel,
        target,
    )
}

fn release_has_target_binary_asset(release: &Release, target: SelfUpdateTarget) -> bool {
    release.assets().iter().any(|asset| {
        asset.name().contains(get_target()) && asset_matches_binary(asset.name(), target)
    })
}

fn asset_matches_binary(asset_name: &str, target: SelfUpdateTarget) -> bool {
    let Some(version_suffix) = asset_name.strip_prefix(&format!("{}-", target.bin_name())) else {
        return false;
    };

    version_suffix
        .chars()
        .next()
        .is_some_and(|first| first == 'v' || first.is_ascii_digit())
}

fn select_latest_version<I>(
    versions: I,
    channel: ReleaseChannel,
    target: SelfUpdateTarget,
) -> miette::Result<String>
where
    I: IntoIterator<Item = String>,
{
    let mut latest: Option<String> = None;
    for version in versions {
        let version = normalize_release_version(&version);
        match latest.as_deref() {
            None => latest = Some(version),
            Some(current_latest) => {
                if bump_is_greater(current_latest, &version).into_diagnostic()? {
                    latest = Some(version);
                }
            }
        }
    }

    latest.ok_or_else(|| {
        miette::miette!(
            "no {} release found for {}/{}",
            match channel {
                ReleaseChannel::Stable => "stable",
                ReleaseChannel::Prerelease => "prerelease",
            },
            target.repo_owner(),
            target.repo_name(),
        )
    })
}

fn is_prerelease_version(version: &str) -> bool {
    normalize_release_version(version)
        .split_once('+')
        .map_or_else(
            || normalize_release_version(version),
            |(base, _)| base.to_string(),
        )
        .contains('-')
}

fn normalize_release_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn candidate_target_version_tags(version: &str) -> Vec<String> {
    let normalized = normalize_release_version(version);
    let prefixed = format!("v{normalized}");
    if normalized == prefixed {
        vec![normalized]
    } else {
        vec![normalized, prefixed]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use self_update::update::ReleaseAsset;

    #[test]
    fn updater_extracts_and_replaces_a_binary_from_the_linux_archive() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staging = directory.path().join("staging");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(source.join("trellis"), "#!/bin/sh\necho updated-trellis\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                source.join("trellis"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let archive = directory.path().join("trellis.tar.gz");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg("trellis")
            .status()
            .unwrap()
            .success());
        self_update::Extract::from_source(&archive)
            .extract_file(&staging, "trellis")
            .unwrap();
        let installed = directory.path().join("trellis");
        std::fs::write(&installed, "old binary").unwrap();
        self_update::Move::from_source(staging.join("trellis"))
            .replace_using_temp(directory.path().join("previous"))
            .to_dest(&installed)
            .unwrap();
        let output = std::process::Command::new(installed).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"updated-trellis\n");
    }

    #[test]
    fn stable_channel_excludes_prereleases() {
        assert!(ReleaseChannel::Stable.accepts(false));
        assert!(!ReleaseChannel::Stable.accepts(true));
    }

    #[test]
    fn prerelease_channel_accepts_every_release() {
        assert!(ReleaseChannel::Prerelease.accepts(false));
        assert!(ReleaseChannel::Prerelease.accepts(true));
    }

    #[test]
    fn detects_semver_prerelease_suffixes() {
        assert!(is_prerelease_version("1.2.3-rc.1"));
        assert!(is_prerelease_version("1.2.3-beta.2+build.7"));
        assert!(!is_prerelease_version("1.2.3"));
    }

    #[test]
    fn selects_highest_stable_version_from_unsorted_input() {
        let target = SelfUpdateTarget::new("oats-center", "trellis", "trellis", "0.7.0");
        let versions = vec![
            "0.7.3".to_string(),
            "0.7.1".to_string(),
            "0.7.2".to_string(),
        ];

        let selected = select_latest_version(versions, ReleaseChannel::Stable, target).unwrap();
        assert_eq!(selected, "0.7.3");
    }

    #[test]
    fn stable_selection_ignores_prerelease_versions() {
        let target = SelfUpdateTarget::new("oats-center", "trellis", "trellis", "0.7.0");
        let versions = vec![
            "0.8.0-rc.1".to_string(),
            "0.7.4".to_string(),
            "0.7.3".to_string(),
        ];

        let filtered = versions
            .into_iter()
            .filter(|version| ReleaseChannel::Stable.accepts(is_prerelease_version(version)));
        let selected = select_latest_version(filtered, ReleaseChannel::Stable, target).unwrap();
        assert_eq!(selected, "0.7.4");
    }

    #[test]
    fn normalizes_github_style_tags() {
        assert_eq!(normalize_release_version("v0.7.3"), "0.7.3");
        assert_eq!(normalize_release_version("  v0.8.0-rc.1  "), "0.8.0-rc.1");
    }

    #[test]
    fn selects_highest_version_from_github_style_tags() {
        let target = SelfUpdateTarget::new("oats-center", "trellis", "trellis", "0.7.0");
        let versions = vec![
            "v0.7.2".to_string(),
            "v0.7.4".to_string(),
            "v0.7.3".to_string(),
        ];

        let selected = select_latest_version(versions, ReleaseChannel::Stable, target).unwrap();
        assert_eq!(selected, "0.7.4");
    }

    #[test]
    fn detects_prerelease_suffixes_in_github_style_tags() {
        assert!(is_prerelease_version("v1.2.3-rc.1"));
        assert!(!is_prerelease_version("v1.2.3"));
    }

    #[test]
    fn prerelease_target_candidates_try_normalized_then_v_prefixed_tag() {
        let candidates = candidate_target_version_tags("0.8.0-rc.1");
        assert_eq!(candidates, vec!["0.8.0-rc.1", "v0.8.0-rc.1"]);
    }

    #[test]
    fn an_archive_installs_only_when_its_listed_checksum_matches() {
        let archive = b"trellis-server release bytes";
        // `sha256sum` of the archive bytes.
        let digest = "0b6dbb7911a83629474ca306cd35c242696dc80b0e183089d8571cbfdc250423";
        let name = "trellis-server-0.8.0-x86_64-unknown-linux-gnu.tar.gz";
        let listing = format!("{digest}  {name}\n");
        assert!(verify_checksum(&listing, name, archive).is_ok());
        assert!(verify_checksum(&listing, name, b"tampered bytes").is_err());
        // A matching digest listed for a different file is not this archive's.
        assert!(verify_checksum(&format!("{digest}  other.tar.gz\n"), name, archive).is_err());
        assert!(verify_checksum("", name, archive).is_err());
    }

    #[test]
    fn release_asset_filter_requires_binary_name_and_target() {
        let target = SelfUpdateTarget::new("oats-center", "trellis", "trellis", "0.7.0");
        let release = Release::builder()
            .version("0.8.0")
            .assets([
                ReleaseAsset::new(format!("other-binary-0.8.0-{}.tar.gz", get_target()), ""),
                ReleaseAsset::new("trellis-0.8.0-other-target.tar.gz", ""),
                ReleaseAsset::new(
                    format!("checksum-0.8.0-{}-trellis.sha256", get_target()),
                    "",
                ),
            ])
            .build()
            .unwrap();

        assert!(!release_has_target_binary_asset(&release, target));
    }

    #[test]
    fn release_asset_filter_accepts_matching_binary_name_and_target() {
        let target = SelfUpdateTarget::new("oats-center", "trellis", "trellis", "0.7.0");
        let release = Release::builder()
            .version("0.8.0")
            .asset(ReleaseAsset::new(
                format!("trellis-0.8.0-{}.tar.gz", get_target()),
                "",
            ))
            .build()
            .unwrap();

        assert!(release_has_target_binary_asset(&release, target));
    }
}
