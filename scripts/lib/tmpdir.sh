# Sourced by every test and check entry point, after it sets `root`.
#
# Temporary files never go to the system /tmp. A caller's TMPDIR is kept.
# In CI, tests' runtime state (SQLite databases, JetStream stores) goes to a
# folder in memory for this job: on the runners' disks the write syncs of many
# test servers at once slowed startup past readiness deadlines. Builds that
# need room use RUNNER_TEMP on disk explicitly. A runner runs one job at a
# time, so a job first removes any folder its runner left from an earlier job.
# Locally the default is an ignored folder in the checkout.
if [[ -z "${TMPDIR:-}" ]]; then
  if [[ -n "${CI:-}" && -d /dev/shm && -w /dev/shm ]]; then
    runner_prefix="/dev/shm/trellis-${RUNNER_NAME:-runner}"
    TMPDIR="$runner_prefix-${GITHUB_RUN_ID:-0}-${GITHUB_RUN_ATTEMPT:-0}-${GITHUB_JOB:-job}"
    for stale in "$runner_prefix"-*; do
      [[ -d "$stale" && "$stale" != "$TMPDIR" ]] && rm -rf "${stale:?}"
    done
    unset runner_prefix stale
  else
    TMPDIR="$root/target/tmp"
  fi
fi
export TMPDIR
mkdir -p "$TMPDIR"
