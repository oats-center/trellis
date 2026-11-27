# Sourced by every test and check entry point, after it sets `root`.
#
# A caller's TMPDIR is kept. In GitHub Actions, tests' runtime state (SQLite
# databases, JetStream stores) goes to a folder for this job in the runner's
# own /tmp. On the self-hosted runners that is a private in-memory tmpfs for
# each runner container, while their work disk is btrfs, where the write syncs
# of many test servers at once slowed startup past readiness deadlines. That
# tmpfs is only 8 GB, so builds that need room use RUNNER_TEMP on disk
# explicitly. A runner runs one job at a time, so a job first removes the
# folders earlier jobs made, recognised by the marker file each one holds.
# Everywhere else the default is an ignored folder in the checkout, never /tmp.
if [[ -z "${TMPDIR:-}" ]]; then
  if [[ "${GITHUB_ACTIONS:-}" == true && -d /tmp && -w /tmp ]]; then
    TMPDIR="/tmp/trellis-ci-${GITHUB_RUN_ID:-0}-${GITHUB_RUN_ATTEMPT:-0}-${GITHUB_JOB:-job}"
    # Later steps of the same job share its folder.
    for stale in /tmp/trellis-ci-*; do
      [[ "$stale" != "$TMPDIR" && -f "$stale/.trellis-ci-job" ]] && rm -rf "${stale:?}"
    done
    unset stale
    mkdir -p "$TMPDIR"
    : > "$TMPDIR/.trellis-ci-job"
  else
    TMPDIR="$root/target/tmp"
  fi
fi
export TMPDIR
mkdir -p "$TMPDIR"
