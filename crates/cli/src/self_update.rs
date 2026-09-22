use miette::IntoDiagnostic;
use self_update::backends::github::{ReleaseList, Update};
use self_update::get_target;
use self_update::update::Release;
use self_update::version::bump_is_greater;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelfUpdateTarget {
    repo_owner: &'static str,
    repo_name: &'static str,
    bin_name: &'static str,
    current_version: &'static str,
}

impl SelfUpdateTarget {
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

    pub const fn repo_owner(self) -> &'static str {
        self.repo_owner
    }

    pub const fn repo_name(self) -> &'static str {
        self.repo_name
    }

    pub const fn bin_name(self) -> &'static str {
        self.bin_name
    }

    pub const fn current_version(self) -> &'static str {
        self.current_version
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReleaseChannel {
    Stable,
    Prerelease,
}

impl ReleaseChannel {
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
pub struct UpdateCheck {
    pub channel: ReleaseChannel,
    pub current_version: String,
    pub latest_version: String,
    pub needs_update: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum UpdateResult {
    UpToDate { version: String },
    Updated { version: String },
}

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

fn install_update_for_tag(
    target: SelfUpdateTarget,
    target_tag: &str,
) -> miette::Result<UpdateResult> {
    let mut builder = Update::configure();
    builder
        .repo_owner(target.repo_owner())
        .repo_name(target.repo_name())
        .identifier(&asset_identifier(target, target_tag))
        .bin_name(target.bin_name())
        .show_download_progress(true)
        .no_confirm(true)
        .target_version_tag(target_tag)
        .current_version(target.current_version());

    let status = builder
        .build()
        .into_diagnostic()?
        .update()
        .into_diagnostic()?;
    Ok(if status.updated() {
        UpdateResult::Updated {
            version: status.version().to_string(),
        }
    } else {
        UpdateResult::UpToDate {
            version: status.version().to_string(),
        }
    })
}

fn latest_version(target: SelfUpdateTarget, channel: ReleaseChannel) -> miette::Result<String> {
    let releases = ReleaseList::configure()
        .repo_owner(target.repo_owner())
        .repo_name(target.repo_name())
        .with_target(get_target())
        .build()
        .into_diagnostic()?
        .fetch()
        .into_diagnostic()?;

    select_latest_version(
        releases
            .into_iter()
            .filter(|release| release_has_target_binary_asset(release, target))
            .map(|release| release.version)
            .filter(|version| channel.accepts(is_prerelease_version(version))),
        channel,
        target,
    )
}

fn release_has_target_binary_asset(release: &Release, target: SelfUpdateTarget) -> bool {
    release
        .assets
        .iter()
        .any(|asset| asset.name.contains(get_target()) && asset_matches_binary(&asset.name, target))
}

fn asset_identifier(target: SelfUpdateTarget, target_tag: &str) -> String {
    format!("{}-{}-", target.bin_name(), target_tag)
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
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
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
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
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
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
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
    fn asset_identifier_includes_binary_name_and_tag() {
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
        assert_eq!(asset_identifier(target, "v0.8.0"), "trellis-v0.8.0-");
    }

    #[test]
    fn release_asset_filter_requires_binary_name_and_target() {
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
        let release = Release {
            assets: vec![
                self_update::update::ReleaseAsset {
                    name: format!("other-binary-0.8.0-{}.tar.gz", get_target()),
                    download_url: String::new(),
                },
                self_update::update::ReleaseAsset {
                    name: "trellis-0.8.0-other-target.tar.gz".to_string(),
                    download_url: String::new(),
                },
                self_update::update::ReleaseAsset {
                    name: format!("checksum-0.8.0-{}-trellis.sha256", get_target()),
                    download_url: String::new(),
                },
            ],
            ..Release::default()
        };

        assert!(!release_has_target_binary_asset(&release, target));
    }

    #[test]
    fn release_asset_filter_accepts_matching_binary_name_and_target() {
        let target = SelfUpdateTarget::new("qlever-llc", "trellis", "trellis", "0.7.0");
        let release = Release {
            assets: vec![self_update::update::ReleaseAsset {
                name: format!("trellis-0.8.0-{}.tar.gz", get_target()),
                download_url: String::new(),
            }],
            ..Release::default()
        };

        assert!(release_has_target_binary_asset(&release, target));
    }
}
