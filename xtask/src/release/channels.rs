//! Moving the shared image channels (`rc`, `latest`) to a published release.
//!
//! A channel only ever moves forward: a retried publication of an older
//! release must not copy its digests over a channel a newer release already
//! holds. The workflow serializes promotions; this module decides, per image
//! and channel, whether the channel moves.

use std::fs;
use std::path::Path;

use miette::{miette, IntoDiagnostic, Result, WrapErr};
use semver::Version;
use sha2::{Digest, Sha256};

use super::runner::{command_text, run_checked_command, run_output_command, CommandSpec};
use super::versioning::write_github_env;

/// What promotion does with one image's channel tag.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum ChannelDecision {
    /// The channel is missing, or holds no newer release's image: move it.
    Move,
    /// The channel already holds this release's image.
    Current,
    /// The channel holds a newer release's image: leave it there.
    Newer(Version),
}

/// One release's channel promotion, as the publication job describes it.
pub(super) struct Promotion<'a> {
    pub(super) tag: &'a str,
    pub(super) branch: &'a str,
    pub(super) commit: &'a str,
    pub(super) repository: &'a str,
    pub(super) digests: &'a Path,
    pub(super) images: &'a [String],
    pub(super) latest: bool,
    pub(super) authfile: &'a Path,
}

/// Move each image's channels to the release, never backward. Records in
/// `TRELLIS_LATEST_CHANNEL` whether every image's `latest` holds this release
/// afterward.
pub(super) fn promote_channels(repo_root: &Path, promotion: &Promotion<'_>) -> Result<()> {
    let release = promotion
        .tag
        .strip_prefix('v')
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| {
            miette!(
                "invalid release tag `{}`; expected vX.Y.Z or vX.Y.Z-rc.N",
                promotion.tag
            )
        })?;
    // The run verified one commit; a release branch that moved since belongs
    // to a newer run.
    let branch = format!("refs/heads/{}", promotion.branch);
    let spec = CommandSpec::new("git", ["ls-remote", "origin", branch.as_str()]);
    let output = run_output_command(repo_root, &spec)?;
    if !output.status.success() {
        return Err(miette!(
            "{} failed: {}",
            command_text(&spec),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let remote = stdout.split_whitespace().next().unwrap_or("nothing");
    if remote != promotion.commit {
        return Err(miette!(
            "{} now names {remote}, not the verified {}; not promoting",
            promotion.branch,
            promotion.commit
        ));
    }

    let mut channels = Vec::new();
    if !release.pre.is_empty() {
        channels.push("rc");
    }
    if promotion.latest {
        channels.push("latest");
    }

    let auth = promotion.authfile;
    let mut latest_holds_release = promotion.latest;
    for image in promotion.images {
        let path = promotion.digests.join(format!("{image}.digest"));
        let digest = fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read {}", path.display()))?
            .trim()
            .to_string();
        let reference = format!("{}/{image}", promotion.repository);
        let mut tags: Option<Vec<String>> = None;
        for channel in &channels {
            let current = tagged_digest(repo_root, auth, &format!("{reference}:{channel}"))?;
            let decision = decide(
                &release,
                &digest,
                current.as_deref(),
                || {
                    if tags.is_none() {
                        tags = Some(list_tags(repo_root, auth, &reference)?);
                    }
                    Ok(tags.clone().unwrap_or_default())
                },
                |tag| tagged_digest(repo_root, auth, &format!("{reference}:{tag}")),
            )?;
            match decision {
                ChannelDecision::Current => {
                    println!("{reference}:{channel} already holds {digest}");
                }
                ChannelDecision::Newer(version) => {
                    println!("{reference}:{channel} holds the newer v{version}; leaving it there");
                    if *channel == "latest" {
                        latest_holds_release = false;
                    }
                }
                ChannelDecision::Move => {
                    let authfile = auth.display().to_string();
                    run_checked_command(
                        repo_root,
                        &CommandSpec::new(
                            "skopeo",
                            [
                                "copy".to_string(),
                                "--all".to_string(),
                                "--preserve-digests".to_string(),
                                "--src-authfile".to_string(),
                                authfile.clone(),
                                "--dest-authfile".to_string(),
                                authfile,
                                format!("docker://{reference}@{digest}"),
                                format!("docker://{reference}:{channel}"),
                            ],
                        ),
                        "failed to move the image channel",
                    )?;
                    let moved = tagged_digest(repo_root, auth, &format!("{reference}:{channel}"))?;
                    if moved.as_deref() != Some(digest.as_str()) {
                        return Err(miette!(
                            "{reference}:{channel} is {}, not the verified {digest}",
                            moved.as_deref().unwrap_or("missing")
                        ));
                    }
                    println!("{reference}:{channel} is the verified {digest}");
                }
            }
        }
    }
    write_github_env(
        "TRELLIS_LATEST_CHANNEL",
        if latest_holds_release {
            "true"
        } else {
            "false"
        },
    )
}

/// Whether a channel holding `current` moves to the release's `digest`. The
/// registry's tags are listed, and newer release tags inspected, only when
/// the channel holds some other image.
pub(super) fn decide(
    release: &Version,
    digest: &str,
    current: Option<&str>,
    list_tags: impl FnOnce() -> Result<Vec<String>>,
    mut tagged_digest: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<ChannelDecision> {
    let Some(current) = current else {
        return Ok(ChannelDecision::Move);
    };
    if current == digest {
        return Ok(ChannelDecision::Current);
    }
    let mut newer: Vec<_> = list_tags()?
        .into_iter()
        .filter_map(|tag| {
            let version = Version::parse(tag.strip_prefix('v')?).ok()?;
            version
                .cmp_precedence(release)
                .is_gt()
                .then_some((version, tag))
        })
        .collect();
    newer.sort_by(|(a, _), (b, _)| b.cmp_precedence(a));
    for (version, tag) in newer {
        if tagged_digest(&tag)?.as_deref() == Some(current) {
            return Ok(ChannelDecision::Newer(version));
        }
    }
    Ok(ChannelDecision::Move)
}

/// The digest `reference` points at, or `None` when the tag does not exist.
/// Like the image publication job, the digest is the hash of the raw
/// manifest the registry serves.
fn tagged_digest(repo_root: &Path, authfile: &Path, reference: &str) -> Result<Option<String>> {
    let spec = CommandSpec::new(
        "skopeo",
        [
            "inspect".to_string(),
            "--authfile".to_string(),
            authfile.display().to_string(),
            "--raw".to_string(),
            format!("docker://{reference}"),
        ],
    );
    let output = run_output_command(repo_root, &spec)?;
    if output.status.success() {
        let hash = Sha256::digest(&output.stdout);
        let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        return Ok(Some(format!("sha256:{hex}")));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.to_ascii_lowercase().contains("manifest unknown") {
        Ok(None)
    } else {
        Err(miette!("{} failed: {stderr}", command_text(&spec)))
    }
}

fn list_tags(repo_root: &Path, authfile: &Path, reference: &str) -> Result<Vec<String>> {
    let spec = CommandSpec::new(
        "skopeo",
        [
            "list-tags".to_string(),
            "--authfile".to_string(),
            authfile.display().to_string(),
            format!("docker://{reference}"),
        ],
    );
    let output = run_output_command(repo_root, &spec)?;
    if !output.status.success() {
        return Err(miette!(
            "{} failed: {}",
            command_text(&spec),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let listing: serde_json::Value = serde_json::from_slice(&output.stdout)
        .into_diagnostic()
        .wrap_err("skopeo list-tags printed invalid JSON")?;
    Ok(listing["Tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_string))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A registry's tags and the digests they point at.
    fn registry(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(tag, digest)| (tag.to_string(), digest.to_string()))
            .collect()
    }

    fn decide_in(
        registry: &BTreeMap<String, String>,
        release: &str,
        digest: &str,
        channel: &str,
    ) -> ChannelDecision {
        decide(
            &Version::parse(release).unwrap(),
            digest,
            registry.get(channel).map(String::as_str),
            || Ok(registry.keys().cloned().collect()),
            |tag| Ok(registry.get(tag).cloned()),
        )
        .unwrap()
    }

    #[test]
    fn a_retried_older_release_leaves_a_newer_channel_alone() {
        // rc.10 was published and promoted after rc.9's final job failed.
        let ghcr = registry(&[
            ("v0.100.0-rc.9", "sha256:nine"),
            ("v0.100.0-rc.10", "sha256:ten"),
            ("sha-aa1fc33", "sha256:ten"),
            ("rc", "sha256:ten"),
        ]);
        // Numeric prerelease identifiers compare as numbers: rc.10 > rc.9.
        assert_eq!(
            decide_in(&ghcr, "0.100.0-rc.9", "sha256:nine", "rc"),
            ChannelDecision::Newer(Version::parse("0.100.0-rc.10").unwrap())
        );
        // The newer release's own retry finds its digest already there.
        assert_eq!(
            decide_in(&ghcr, "0.100.0-rc.10", "sha256:ten", "rc"),
            ChannelDecision::Current
        );
    }

    #[test]
    fn a_newer_release_moves_an_older_or_missing_channel() {
        let ghcr = registry(&[
            ("v0.100.0-rc.50", "sha256:fifty"),
            ("v0.100.0", "sha256:stable"),
            ("latest", "sha256:fifty"),
            ("rc", "sha256:unreleased"),
        ]);
        // The stable release outranks every prerelease of its version.
        assert_eq!(
            decide_in(&ghcr, "0.100.0", "sha256:stable", "latest"),
            ChannelDecision::Move
        );
        // A channel holding no newer release's image moves.
        assert_eq!(
            decide_in(&ghcr, "0.100.0-rc.51", "sha256:fiftyone", "rc"),
            ChannelDecision::Move
        );
        assert_eq!(
            decide_in(&ghcr, "0.101.0-rc.1", "sha256:next", "missing"),
            ChannelDecision::Move
        );
    }
}
