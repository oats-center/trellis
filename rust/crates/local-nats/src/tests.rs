use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use flate2::write::GzEncoder;
use flate2::Compression;
use tar::Builder;

use super::*;

#[test]
fn asset_name_resolves_supported_platforms() {
    assert_eq!(
        asset_name("linux", "x86_64").expect("linux amd64"),
        "linux-amd64"
    );
    assert_eq!(
        asset_name("linux", "aarch64").expect("linux arm64"),
        "linux-arm64"
    );
    assert_eq!(
        asset_name("macos", "x86_64").expect("darwin amd64"),
        "darwin-amd64"
    );
    assert_eq!(
        asset_name("macos", "aarch64").expect("darwin arm64"),
        "darwin-arm64"
    );
}

#[test]
fn asset_name_rejects_unsupported_platforms() {
    assert!(matches!(
        asset_name("windows", "x86_64"),
        Err(LocalNatsError::UnsupportedPlatform { .. })
    ));
    assert!(matches!(
        asset_name("linux", "riscv64"),
        Err(LocalNatsError::UnsupportedPlatform { .. })
    ));
}

#[test]
fn download_url_uses_pinned_version_and_asset() {
    let version = pinned_version().expect("pinned version");
    assert_eq!(
        download_url("linux-amd64").expect("download url"),
        format!(
            "https://github.com/nats-io/nats-server/releases/download/v{version}/nats-server-v{version}-linux-amd64.tar.gz"
        )
    );
    assert_eq!(
        download_url("darwin-arm64").expect("download url"),
        format!(
            "https://github.com/nats-io/nats-server/releases/download/v{version}/nats-server-v{version}-darwin-arm64.tar.gz"
        )
    );
}

#[test]
fn pinned_manifest_has_all_supported_assets() {
    for asset in ["linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64"] {
        pinned_sha256(asset).expect("pinned checksum");
    }
}

#[test]
fn checksum_verification_rejects_bad_bytes() {
    let expected = sha256_hex(b"good bytes");
    verify_sha256(&expected, b"good bytes").expect("matching bytes verify");

    let error = verify_sha256(&expected, b"evil bytes").expect_err("mismatch rejected");
    assert!(matches!(error, LocalNatsError::ChecksumMismatch { .. }));
}

#[test]
fn verify_and_write_stores_only_verified_bytes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let expected = sha256_hex(b"good bytes");

    let path = temp.path().join("archive.tar.gz.tmp");
    verify_and_write(b"good bytes", &expected, &path).expect("verified bytes stored");
    assert_eq!(fs::read(&path).expect("read temp archive"), b"good bytes");

    let bad_path = temp.path().join("bad.tar.gz.tmp");
    let error = verify_and_write(b"evil bytes", &expected, &bad_path)
        .expect_err("mismatched bytes rejected");
    assert!(matches!(error, LocalNatsError::ChecksumMismatch { .. }));
    assert!(!bad_path.exists(), "no temp file left behind on mismatch");
}

#[test]
fn rename_over_moves_temp_archive_into_place() {
    let temp = tempfile::tempdir().expect("temp dir");
    let from = temp.path().join("archive.tar.gz.tmp");
    let to = temp.path().join("archive.tar.gz");
    fs::write(&from, b"archive").expect("write temp archive");

    rename_over(&from, &to).expect("rename into place");
    assert_eq!(fs::read(&to).expect("read archive"), b"archive");
    assert!(!from.exists(), "temp file consumed by rename");

    fs::write(&from, b"replacement").expect("write temp archive");
    rename_over(&from, &to).expect("rename over existing archive");
    assert_eq!(fs::read(&to).expect("read archive"), b"replacement");
}

#[test]
fn tar_extraction_extracts_single_binary_and_sets_executable() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    write_fixture_archive(
        &archive,
        "nats-server-v2.14.4-linux-amd64/nats-server",
        b"bin!",
    );

    let destination = temp.path().join("nats-server");
    extract_binary(&archive, &destination).expect("extract binary");
    assert_eq!(fs::read(&destination).expect("read binary"), b"bin!");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_ne!(
            fs::metadata(&destination)
                .expect("binary metadata")
                .permissions()
                .mode()
                & 0o111,
            0,
            "extracted binary must be executable"
        );
    }
}

#[test]
fn extract_rejects_symlink_entry_named_nats_server() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    let encoder = GzEncoder::new(
        fs::File::create(&archive).expect("create archive"),
        Compression::default(),
    );
    let mut tar = Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_mode(0o777);
    header.set_size(0);
    header.set_cksum();
    tar.append_link(
        &mut header,
        "nats-server-v2.14.4-linux-amd64/nats-server",
        "/bin/true",
    )
    .expect("append symlink entry");
    tar.into_inner()
        .expect("tar encoder")
        .finish()
        .expect("finish gzip");

    let destination = temp.path().join("nats-server");
    let error = extract_binary(&archive, &destination).expect_err("symlink entry rejected");
    assert!(matches!(error, LocalNatsError::MissingBinary { .. }));
    assert!(!destination.exists(), "symlink must not be unpacked");
}

#[test]
fn extract_rejects_hardlink_entry_named_nats_server() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    let encoder = GzEncoder::new(
        fs::File::create(&archive).expect("create archive"),
        Compression::default(),
    );
    let mut tar = Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Link);
    header.set_mode(0o644);
    header.set_size(0);
    header.set_cksum();
    tar.append_link(
        &mut header,
        "nats-server-v2.14.4-linux-amd64/nats-server",
        "nats-server-v2.14.4-linux-amd64/real-binary",
    )
    .expect("append hardlink entry");
    tar.into_inner()
        .expect("tar encoder")
        .finish()
        .expect("finish gzip");

    let destination = temp.path().join("nats-server");
    let error = extract_binary(&archive, &destination).expect_err("hardlink entry rejected");
    assert!(matches!(error, LocalNatsError::MissingBinary { .. }));
    assert!(!destination.exists(), "hardlink must not be unpacked");
}

#[test]
fn install_extracts_nested_binary_and_cleans_staging() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    write_fixture_archive(
        &archive,
        "nats-server-v2.14.4-linux-amd64/nats-server",
        b"bin!",
    );
    let binary = temp.path().join("nats-server");
    let staging = temp.path().join(".staging");

    install_binary(&archive, &staging, &binary).expect("install binary");

    assert_eq!(fs::read(&binary).expect("read installed binary"), b"bin!");
    assert!(!staging.exists(), "staging dir is removed after install");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_ne!(
            fs::metadata(&binary)
                .expect("binary metadata")
                .permissions()
                .mode()
                & 0o111,
            0,
            "installed binary must be executable"
        );
    }
}

#[test]
fn failed_extraction_leaves_no_staging_dir() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    write_fixture_archive(
        &archive,
        "nats-server-v2.14.4-linux-amd64/not-the-binary",
        b"nope",
    );
    let binary = temp.path().join("nats-server");
    let staging = temp.path().join(".staging");

    let error = install_binary(&archive, &staging, &binary)
        .expect_err("archive without nats-server must fail");
    assert!(matches!(error, LocalNatsError::MissingBinary { .. }));
    assert!(!binary.exists(), "no binary installed on failure");
    assert!(
        !staging.exists(),
        "staging dir is removed on extraction failure"
    );
}

// The probe-based readiness tests below are deterministic (no sockets): real TCP
// behavior of the probe is covered by `ManagedNatsServer::start` in the live
// `cli.server-managed-nats` case, the trellis-test NATS smoke, and the executed
// runtime image smoke.

#[test]
fn readiness_probe_succeeds_when_expected_ports_are_ready() {
    wait_for_readiness_with(
        &mut || Ok(None),
        &[4000, 4001],
        Duration::from_millis(100),
        Duration::from_millis(20),
        |port| matches!(*port, 4000 | 4001),
    )
    .expect("expected ports are ready");
}

#[test]
fn readiness_probe_times_out_when_no_port_is_ready() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let error = wait_for_readiness_with(
        &mut || Ok(None),
        &[4000, 4001],
        Duration::from_millis(100),
        Duration::from_millis(20),
        |_| {
            attempts.fetch_add(1, Ordering::Relaxed);
            false
        },
    )
    .expect_err("no ready port must time out");
    assert!(matches!(
        error,
        LocalNatsError::ReadinessTimeout {
            ports,
            timeout,
        } if ports == vec![4000, 4001] && timeout == Duration::from_millis(100)
    ));
    assert!(
        attempts.load(Ordering::Relaxed) >= 2,
        "every iteration must probe the ports until the deadline: {} attempts",
        attempts.load(Ordering::Relaxed)
    );
}

#[cfg(unix)]
#[test]
fn readiness_reports_exited_child_before_probing() {
    use std::os::unix::process::ExitStatusExt as _;

    let probes = std::sync::atomic::AtomicUsize::new(0);
    let error = wait_for_readiness_with(
        &mut || Ok(Some(ExitStatus::from_raw(1))),
        &[4000, 4001],
        Duration::from_secs(1),
        Duration::from_millis(20),
        |_| {
            probes.fetch_add(1, Ordering::Relaxed);
            true
        },
    )
    .expect_err("exited child must fail");
    assert!(matches!(error, LocalNatsError::SpawnFailed(_)));
    assert_eq!(
        probes.load(Ordering::Relaxed),
        0,
        "an exited child wins before any port is probed"
    );
}

#[test]
fn acquire_pid_file_removes_stale_and_garbage_files() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pid_file = temp.path().join("nats-server.pid");
    let binary = temp.path().join("nats-server");

    fs::write(&pid_file, format!("{}\n", dead_pid())).expect("write stale pid");
    let _pid_lock = acquire_pid_file(&pid_file, &binary).expect("stale pid proceeds");
    assert!(pid_file.exists(), "pid file is acquired");
}

#[test]
fn acquire_pid_file_rejects_unparsable_content_as_busy() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pid_file = temp.path().join("nats-server.pid");
    let binary = temp.path().join("nats-server");

    fs::write(&pid_file, "not-a-pid\n").expect("write garbage pid");
    let error = acquire_pid_file(&pid_file, &binary)
        .expect_err("unparsable pid file must be treated as busy");
    assert!(matches!(error, LocalNatsError::PidFileUnparsable { .. }));
    assert!(
        pid_file.exists(),
        "busy pid file is preserved for inspection"
    );

    fs::write(&pid_file, "\n").expect("write empty pid");
    let error =
        acquire_pid_file(&pid_file, &binary).expect_err("empty pid file must be treated as busy");
    assert!(matches!(error, LocalNatsError::PidFileUnparsable { .. }));
    assert!(
        pid_file.exists(),
        "busy pid file is preserved for inspection"
    );
}

#[test]
fn absent_pid_file_is_acquired() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pid_file = temp.path().join("nats-server.pid");
    let binary = temp.path().join("nats-server");
    let _pid_lock = acquire_pid_file(&pid_file, &binary).expect("absent pid file acquired");
    assert!(pid_file.exists());
}

#[cfg(unix)]
#[test]
fn pid_write_through_lock_handle_ignores_planted_symlink() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pid_file = temp.path().join("nats-server.pid");
    let binary = temp.path().join("nats-server");
    let mut lock = acquire_pid_file(&pid_file, &binary).expect("acquire pid lock");

    // A tamperer replaces the pid path with a symlink while the lock handle is held.
    let target = temp.path().join("target");
    fs::write(&target, "precious").expect("write target");
    fs::remove_file(&pid_file).expect("remove lock path");
    std::os::unix::fs::symlink(&target, &pid_file).expect("plant symlink");

    write_pid(&mut lock, 4242).expect("write through held handle must not follow the symlink");
    let error =
        verify_owned_pid_file(&pid_file, 4242).expect_err("replaced pid path must be rejected");
    assert!(matches!(error, LocalNatsError::PidFileUnparsable { .. }));
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "precious",
        "the symlink target must be untouched"
    );
}

#[cfg(unix)]
#[test]
fn pid_read_rejects_fifo_without_blocking() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pid_file = temp.path().join("nats-server.pid");
    let path =
        std::ffi::CString::new(pid_file.as_os_str().as_encoded_bytes()).expect("pid path CString");
    // SAFETY: mkfifo on a fresh tempdir path owned by this test.
    assert_eq!(
        unsafe { libc::mkfifo(path.as_ptr(), 0o600) },
        0,
        "mkfifo failed"
    );

    let (sent, received) = std::sync::mpsc::channel();
    let read_path = pid_file.clone();
    std::thread::spawn(move || {
        let _ = sent.send(read_pid_file(&read_path));
    });
    let error = received
        .recv_timeout(Duration::from_secs(5))
        .expect("reading a FIFO pid path must not block")
        .expect_err("FIFO pid path rejected");
    assert!(matches!(error, LocalNatsError::PidFileUnparsable { .. }));

    // The acquire path must reject the FIFO the same way, without blocking.
    let (sent, received) = std::sync::mpsc::channel();
    let read_path = pid_file.clone();
    std::thread::spawn(move || {
        let _ = sent.send(acquire_pid_file(&read_path, &binary_path(&read_path)));
    });
    let error = received
        .recv_timeout(Duration::from_secs(5))
        .expect("acquiring a FIFO pid path must not block")
        .expect_err("FIFO pid path rejected by acquire");
    assert!(matches!(error, LocalNatsError::PidFileUnparsable { .. }));
}

#[cfg(unix)]
fn binary_path(pid_file: &Path) -> PathBuf {
    pid_file.parent().expect("parent").join("nats-server")
}

#[cfg(target_os = "linux")]
#[test]
fn live_process_without_nats_identity_is_stale() {
    // Our own test process is alive but its exe is not the managed binary, so the
    // identity gate must classify it as stale rather than "already running".
    let binary = Path::new("/tmp/trellis-managed-nats-server");
    assert!(!process_is_managed_nats(std::process::id() as i32, binary));
    assert!(process_alive(std::process::id() as i32));
}

#[cfg(target_os = "linux")]
#[test]
fn exe_identity_wins_over_comm_mismatch() {
    // A `--local-nats=<PATH>` escape-hatch path may have a basename that does not start with
    // `nats-server` (for example `/opt/bin/custom-nats`); the exe-based identity must
    // still recognize the live process as ours instead of misclassifying it as stale.
    let temp = tempfile::tempdir().expect("temp dir");
    let custom_binary = temp.path().join("custom-nats");
    fs::copy("/bin/sleep", &custom_binary).expect("copy sleep");
    set_executable(&custom_binary).expect("make executable");
    let mut child = spawn_renamed_sleep(&custom_binary);
    let identity_visible = (0..500).any(|_| {
        if process_is_managed_nats(child.id() as i32, &custom_binary) {
            true
        } else {
            std::thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(
        identity_visible,
        "exe identity must match despite the comm mismatch"
    );

    let pid_file = temp.path().join("nats-server.pid");
    fs::write(&pid_file, format!("{}\n", child.id())).expect("write live pid");
    let error = acquire_pid_file(&pid_file, &custom_binary)
        .expect_err("live custom-nats identity rejected");
    assert!(matches!(error, LocalNatsError::AlreadyRunning { .. }));
    assert!(pid_file.exists(), "live pid file is preserved");

    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_exe_with_comm_mismatch_is_stale() {
    // A zombie has an unreadable /proc/<pid>/exe; the comm fallback ("true") does not
    // start with `nats-server`, so the gate must classify the pid as stale.
    let pid = zombie_child(Path::new("/bin/true"));
    let binary = Path::new("/tmp/trellis-managed-nats-server");
    assert!(process_alive(pid));
    assert!(!process_is_managed_nats(pid, binary));
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_exe_with_comm_match_falls_back_to_alive() {
    // A zombie's exe is unreadable; the comm-prefix fallback must still recognize a
    // versioned managed binary (comm truncated to `nats-server-v2.`).
    let temp = tempfile::tempdir().expect("temp dir");
    let versioned_binary = temp.path().join("nats-server-v2.14.4-linux-amd64");
    fs::copy("/bin/sleep", &versioned_binary).expect("copy sleep");
    set_executable(&versioned_binary).expect("make executable");
    let pid = zombie_child(&versioned_binary);
    assert!(process_alive(pid));
    assert!(process_is_managed_nats(pid, &versioned_binary));
}

#[cfg(target_os = "linux")]
#[test]
fn acquire_rejects_live_process_with_nats_server_identity() {
    let temp = tempfile::tempdir().expect("temp dir");
    let fake_binary = temp.path().join("nats-server");
    fs::copy("/bin/sleep", &fake_binary).expect("copy sleep");
    set_executable(&fake_binary).expect("make executable");
    let mut child = spawn_renamed_sleep(&fake_binary);
    // The kernel publishes comm during exec; allow a short window under parallel load.
    let identity_visible = (0..500).any(|_| {
        if process_is_managed_nats(child.id() as i32, &fake_binary) {
            true
        } else {
            std::thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(
        identity_visible,
        "renamed process must expose the nats-server comm identity"
    );

    let pid_file = temp.path().join("nats-server.pid");
    fs::write(&pid_file, format!("{}\n", child.id())).expect("write live pid");
    let error =
        acquire_pid_file(&pid_file, &fake_binary).expect_err("live nats-server identity rejected");
    assert!(matches!(error, LocalNatsError::AlreadyRunning { .. }));
    assert!(
        process_alive(child.id() as i32),
        "foreign process is never killed"
    );
    assert!(pid_file.exists(), "live pid file is preserved");

    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn acquire_matches_truncated_comm_of_versioned_binary() {
    // The real managed binary is `nats-server-v2.14.4-<asset>`; the exe-based identity
    // matches it exactly (and the comm fallback would match the 15-char truncated
    // `nats-server-v2.` if the exe were ever unreadable).
    let temp = tempfile::tempdir().expect("temp dir");
    let versioned_binary = temp.path().join("nats-server-v2.14.4-linux-amd64");
    fs::copy("/bin/sleep", &versioned_binary).expect("copy sleep");
    set_executable(&versioned_binary).expect("make executable");
    let mut child = spawn_renamed_sleep(&versioned_binary);
    let identity_visible = (0..500).any(|_| {
        if process_is_managed_nats(child.id() as i32, &versioned_binary) {
            true
        } else {
            std::thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(
        identity_visible,
        "versioned binary must match via truncated comm + exe path"
    );

    let pid_file = temp.path().join("nats-server.pid");
    fs::write(&pid_file, format!("{}\n", child.id())).expect("write live pid");
    let error = acquire_pid_file(&pid_file, &versioned_binary)
        .expect_err("versioned live nats-server rejected");
    assert!(matches!(error, LocalNatsError::AlreadyRunning { .. }));
    assert!(pid_file.exists(), "live pid file is preserved");

    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn comm_prefix_without_matching_exe_is_stale() {
    // A process whose comm truncates to `nats-server-v2.` but whose executable is not
    // the binary we manage (recycled pid / unrelated server) must be treated as stale.
    let temp = tempfile::tempdir().expect("temp dir");
    let other_binary = temp.path().join("nats-server-v2.14.4-other");
    fs::copy("/bin/sleep", &other_binary).expect("copy sleep");
    set_executable(&other_binary).expect("make executable");
    let mut child = spawn_renamed_sleep(&other_binary);
    let managed_binary = temp.path().join("nats-server-v2.14.4-linux-amd64");

    let pid_file = temp.path().join("nats-server.pid");
    fs::write(&pid_file, format!("{}\n", child.id())).expect("write pid");
    let _pid_lock =
        acquire_pid_file(&pid_file, &managed_binary).expect("unrelated binary is stale");
    assert!(
        process_alive(child.id() as i32),
        "unrelated process is never killed"
    );
    assert!(
        pid_file.exists(),
        "stale pid file is replaced by our acquire"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn ensure_rejects_symlinked_cache_root() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let real = temp.path().join("real-cache");
        fs::create_dir_all(&real).expect("create real cache");
        set_private_dir_permissions(&real).expect("make private");
        let cache = temp.path().join("cache");
        std::os::unix::fs::symlink(&real, &cache).expect("symlink cache root");

        let error =
            NatsServerBinary::ensure(Some(&cache)).expect_err("symlinked cache root rejected");
        assert!(matches!(error, LocalNatsError::CacheDirUnsafe { .. }));
    }
}

#[test]
fn ensure_replaces_symlinked_binary_from_verified_archive() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = private_cache_dir(temp.path());
        let asset = platform().expect("current platform");
        let version = "2.14.4-fixture";
        let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
        write_fixture_archive(
            &archive_path,
            &format!("nats-server-v{version}-{asset}/nats-server"),
            b"bin!",
        );
        let archive_bytes = fs::read(&archive_path).expect("read fixture archive");
        let pin = fixture_pin(version, &archive_bytes);
        let binary = cache.join(format!("nats-server-v{version}-{asset}"));
        std::os::unix::fs::symlink("/bin/true", &binary).expect("create symlink");

        let resolved = NatsServerBinary::ensure_with_pin(Some(&cache), &pin)
            .expect("symlinked binary is replaced, not rejected");
        assert_eq!(resolved, binary);
        let metadata = fs::symlink_metadata(&binary).expect("binary metadata");
        assert!(
            metadata.file_type().is_file(),
            "the symlink must be replaced by a regular file"
        );
        assert_eq!(
            fs::read(&binary).expect("read reinstalled binary"),
            b"bin!",
            "the binary must be a fresh extraction of the verified archive"
        );
    }
}

#[test]
fn ensure_replaces_non_regular_cache_entry_from_verified_archive() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = private_cache_dir(temp.path());
        let asset = platform().expect("current platform");
        let version = "2.14.4-fixture";
        let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
        write_fixture_archive(
            &archive_path,
            &format!("nats-server-v{version}-{asset}/nats-server"),
            b"bin!",
        );
        let archive_bytes = fs::read(&archive_path).expect("read fixture archive");
        let pin = fixture_pin(version, &archive_bytes);
        let binary = cache.join(format!("nats-server-v{version}-{asset}"));
        fs::create_dir_all(binary.join("stale")).expect("plant a directory at the binary path");

        let resolved = NatsServerBinary::ensure_with_pin(Some(&cache), &pin)
            .expect("non-regular cache entry is replaced, not rejected");
        assert_eq!(resolved, binary);
        let metadata = fs::symlink_metadata(&binary).expect("binary metadata");
        assert!(
            metadata.file_type().is_file(),
            "the directory must be replaced by a regular file"
        );
        assert_eq!(
            fs::read(&binary).expect("read reinstalled binary"),
            b"bin!",
            "the binary must be a fresh extraction of the verified archive"
        );
    }
}

#[test]
fn ensure_rejects_world_writable_cache_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("temp dir");
        let cache = temp.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache dir");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o777)).expect("chmod");

        let error =
            NatsServerBinary::ensure(Some(&cache)).expect_err("world-writable cache dir rejected");
        assert!(matches!(error, LocalNatsError::CacheDirUnsafe { .. }));
    }
}

#[test]
fn ensure_cache_dir_creates_private_dir() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = temp.path().join("cache");
    ensure_cache_dir(&cache).expect("create cache dir");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&cache)
                .expect("cache metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "cache dir must be private"
        );
    }
    ensure_cache_dir(&cache).expect("existing private dir is accepted");
}

#[test]
fn ensure_reuses_verified_archive_and_binary_without_download() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = private_cache_dir(temp.path());
    let asset = platform().expect("current platform");
    let version = "2.14.4-fixture";
    let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
    write_fixture_archive(
        &archive_path,
        &format!("nats-server-v{version}-{asset}/nats-server"),
        b"bin!",
    );
    let archive_bytes = fs::read(&archive_path).expect("read fixture archive");
    let pin = fixture_pin(version, &archive_bytes);
    let binary = cache.join(format!("nats-server-v{version}-{asset}"));
    fs::write(&binary, b"bin!").expect("write fake binary");
    set_executable(&binary).expect("make executable");

    let resolved = NatsServerBinary::ensure_with_pin(Some(&cache), &pin).expect("ensure reuses");
    assert_eq!(resolved, binary);
}

#[test]
fn ensure_reinstalls_replaced_binary_from_verified_archive_without_download() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = private_cache_dir(temp.path());
    let asset = platform().expect("current platform");
    let version = "2.14.4-fixture";
    let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
    write_fixture_archive(
        &archive_path,
        &format!("nats-server-v{version}-{asset}/nats-server"),
        b"bin!",
    );
    let archive_bytes = fs::read(&archive_path).expect("read fixture archive");
    let pin = fixture_pin(version, &archive_bytes);
    let binary = cache.join(format!("nats-server-v{version}-{asset}"));
    // A regular executable file whose bytes do not match the verified archive: the
    // cached binary was replaced or corrupted and must be reinstalled from the archive.
    fs::write(&binary, b"evil bytes").expect("write replaced binary");
    set_executable(&binary).expect("make executable");

    let resolved =
        NatsServerBinary::ensure_with_pin(Some(&cache), &pin).expect("ensure reinstalls");
    assert_eq!(resolved, binary);
    assert_eq!(
        fs::read(&binary).expect("read reinstalled binary"),
        b"bin!",
        "the replaced binary must be replaced by a fresh extraction of the archive"
    );
}

#[test]
fn ensure_redownloads_corrupt_archive() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = private_cache_dir(temp.path());
    let asset = platform().expect("current platform");
    let version = "2.14.4-fixture";
    let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
    let archive_bytes = b"corrupt archive bytes";
    let pin = fixture_pin(version, archive_bytes);
    // A cached archive whose bytes no longer match the pin: it must be removed and
    // re-downloaded from the pin URL (which fails here for the fixture version, proving
    // the download path was taken instead of trusting the corrupt cache).
    fs::write(&archive_path, b"tampered").expect("write corrupt archive");

    let error = NatsServerBinary::ensure_with_pin(Some(&cache), &pin)
        .expect_err("corrupt archive must be re-downloaded");
    assert!(
        matches!(error, LocalNatsError::Download { .. }),
        "expected a download attempt for the fixture version, got {error}"
    );
    assert!(
        !archive_path.exists(),
        "the corrupt cached archive must be removed before re-downloading"
    );
}

#[test]
fn from_path_accepts_regular_executable() {
    let temp = tempfile::tempdir().expect("temp dir");
    let binary = temp.path().join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");

    let resolved = NatsServerBinary::from_path(&binary).expect("valid binary accepted");
    assert_eq!(
        resolved,
        fs::canonicalize(&binary).expect("canonical binary path"),
        "from_path must return the canonical path used for spawn"
    );
}

#[test]
fn path_lookup_uses_controlled_search_path_without_download() {
    let temp = tempfile::tempdir().expect("temp dir");
    let binary = temp.path().join(if cfg!(windows) {
        "nats-server.exe"
    } else {
        "nats-server"
    });
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");

    let path = std::env::join_paths([temp.path()]).expect("join PATH");
    let resolved = NatsServerBinary::from_search_path(&path).expect("find PATH binary");
    assert_eq!(
        resolved,
        fs::canonicalize(binary).expect("canonical binary")
    );
}

#[test]
fn missing_path_binary_and_explicit_path_never_create_download_cache() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = temp.path().join("cache");
    let empty_path = std::env::join_paths([temp.path()]).expect("join PATH");
    assert!(NatsServerBinary::from_search_path(&empty_path).is_err());
    assert!(NatsServerBinary::resolve(
        &NatsBinarySource::Path(temp.path().join("missing")),
        Some(&cache),
    )
    .is_err());
    assert!(!cache.exists());
}

#[test]
fn pinned_download_requires_explicit_cache() {
    let error = NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, None)
        .expect_err("download without cache rejected");
    assert!(matches!(error, LocalNatsError::MissingCacheDir));
}

#[cfg(unix)]
#[test]
fn from_path_returns_canonical_path_through_symlinked_parents() {
    let temp = tempfile::tempdir().expect("temp dir");
    let real_dir = temp.path().join("real");
    fs::create_dir_all(&real_dir).expect("create real dir");
    let binary = real_dir.join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");
    let link_dir = temp.path().join("link-dir");
    std::os::unix::fs::symlink(&real_dir, &link_dir).expect("create symlinked parent");

    let resolved = NatsServerBinary::from_path(&link_dir.join("nats-server")).expect("accepted");
    assert_eq!(
        resolved,
        fs::canonicalize(&binary).expect("canonical binary path"),
        "spawn must use the canonical path, not the symlinked one"
    );
}

#[cfg(unix)]
#[test]
fn from_path_rejects_group_or_world_writable_binary() {
    let temp = tempfile::tempdir().expect("temp dir");
    let binary = temp.path().join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o777)).expect("chmod 777");

    let error = NatsServerBinary::from_path(&binary).expect_err("writable binary rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error.to_string().contains("group or world writable"));
}

#[cfg(unix)]
#[test]
fn from_path_rejects_world_writable_parent_directory() {
    let temp = tempfile::tempdir().expect("temp dir");
    let writable_dir = temp.path().join("writable");
    fs::create_dir_all(&writable_dir).expect("create dir");
    let binary = writable_dir.join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&writable_dir, fs::Permissions::from_mode(0o777)).expect("chmod 777");

    let error = NatsServerBinary::from_path(&binary).expect_err("writable parent rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error.to_string().contains("parent directory"));
}

#[cfg(unix)]
#[test]
fn from_path_rejects_foreign_owner_when_running_as_root() {
    // SAFETY: geteuid never fails.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        eprintln!("skipped: changing file ownership requires root");
        return;
    }
    let temp = tempfile::tempdir().expect("temp dir");
    let binary = temp.path().join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");
    set_executable(&binary).expect("make executable");
    Command::new("chown")
        .args(["65534:65534", binary.to_str().expect("UTF-8 path")])
        .status()
        .expect("run chown");

    let error = NatsServerBinary::from_path(&binary).expect_err("foreign owner rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error
        .to_string()
        .contains("owned by root or the current user"));
}

#[test]
fn from_path_rejects_missing_binary() {
    let temp = tempfile::tempdir().expect("temp dir");
    let missing = temp.path().join("missing");
    let error = NatsServerBinary::from_path(&missing).expect_err("missing binary rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error.to_string().contains("does not exist"));
}

#[cfg(unix)]
#[test]
fn from_path_rejects_nonexecutable_binary() {
    let temp = tempfile::tempdir().expect("temp dir");
    let binary = temp.path().join("nats-server");
    fs::write(&binary, "#!/bin/sh\n").expect("write binary");

    let error = NatsServerBinary::from_path(&binary).expect_err("non-executable binary rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error.to_string().contains("not executable"));
}

#[cfg(unix)]
#[test]
fn from_path_rejects_symlink_without_following() {
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("target");
    fs::write(&target, "#!/bin/sh\n").expect("write target");
    set_executable(&target).expect("make executable");
    let link = temp.path().join("nats-server");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let error = NatsServerBinary::from_path(&link).expect_err("symlink rejected");
    assert!(matches!(error, LocalNatsError::InvalidBinaryPath { .. }));
    assert!(error.to_string().contains("must not be a symlink"));
}

#[test]
fn unique_temp_paths_do_not_collide_within_one_process() {
    let temp = tempfile::tempdir().expect("temp dir");
    let first = unique_temp_path(temp.path(), "nats-server-v2.14.4-linux-amd64.tar.gz");
    let second = unique_temp_path(temp.path(), "nats-server-v2.14.4-linux-amd64.tar.gz");
    assert_ne!(first, second, "each call must get its own temp path");
}

#[test]
fn builder_requires_explicit_policy_without_side_effects() {
    let Err(error) = LocalNats::builder().start() else {
        panic!("missing binary policy must fail");
    };
    assert!(matches!(error, LocalNatsError::InvalidPolicy(_)));
    assert!(error.to_string().contains("binary source is required"));
}

#[test]
fn concurrent_ensure_calls_share_one_cache() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = private_cache_dir(temp.path());
    let asset = platform().expect("current platform");
    let version = "2.14.4-fixture";
    let archive_path = cache.join(format!("nats-server-v{version}-{asset}.tar.gz"));
    write_fixture_archive(
        &archive_path,
        &format!("nats-server-v{version}-{asset}/nats-server"),
        b"bin!",
    );
    let archive_bytes = fs::read(&archive_path).expect("read fixture archive");
    let pin = fixture_pin(version, &archive_bytes);
    let binary = cache.join(format!("nats-server-v{version}-{asset}"));
    fs::write(&binary, b"bin!").expect("write fake binary");
    set_executable(&binary).expect("make executable");

    let cache_path = cache.clone();
    let handles = (0..2)
        .map(|_| {
            let cache_path = cache_path.clone();
            let pin = pin.clone();
            std::thread::spawn(move || {
                NatsServerBinary::ensure_with_pin(Some(&cache_path), &pin)
                    .expect("concurrent ensure")
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        assert_eq!(handle.join().expect("ensure thread"), binary);
    }
}

#[test]
fn concurrent_ensure_with_empty_cache_downloads_once() {
    // True concurrency over an EMPTY cache: the install lock must serialize the real
    // archive download (bounded by the agent timeouts), and both threads must resolve
    // the same binary with no temp artifacts left behind.
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = private_cache_dir(temp.path());
    DOWNLOAD_ATTEMPTS.store(0, Ordering::Relaxed);

    let cache_path = cache.clone();
    let handles = (0..2)
        .map(|_| {
            let cache_path = cache_path.clone();
            std::thread::spawn(move || NatsServerBinary::ensure(Some(&cache_path)))
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("ensure thread"))
        .collect::<Vec<_>>();
    for result in &results {
        result
            .as_ref()
            .expect("concurrent ensure with an empty cache must succeed");
    }
    assert_eq!(
        results[0].as_ref().expect("first result"),
        results[1].as_ref().expect("second result"),
        "both threads must resolve the same binary path"
    );
    assert_eq!(
        DOWNLOAD_ATTEMPTS.load(Ordering::Relaxed),
        1,
        "the install lock must serialize the archive download"
    );

    // Exactly the archive and the binary remain; no temp artifacts.
    let mut entries = fs::read_dir(&cache)
        .expect("read cache")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort();
    let asset = platform().expect("current platform");
    let version = pinned_version().expect("pinned version");
    assert_eq!(
        entries,
        vec![
            format!("nats-server-v{version}-{asset}"),
            format!("nats-server-v{version}-{asset}.tar.gz"),
        ],
        "only the binary and its archive may remain: {entries:?}"
    );
}

#[test]
fn concurrent_installs_into_same_destination_both_succeed() {
    let temp = tempfile::tempdir().expect("temp dir");
    let archive = temp.path().join("fixture.tar.gz");
    write_fixture_archive(
        &archive,
        "nats-server-v2.14.4-linux-amd64/nats-server",
        b"bin!",
    );
    let binary = temp.path().join("nats-server");

    let staging_root = temp.path().join("staging");
    let handles = (0..2)
        .map(|_| {
            let archive = archive.clone();
            let binary = binary.clone();
            let staging_root = staging_root.clone();
            std::thread::spawn(move || {
                let staging = unique_temp_path(&staging_root, "install");
                install_binary(&archive, &staging, &binary).expect("concurrent install")
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("install thread");
    }
    assert_eq!(
        fs::read(&binary).expect("read installed binary"),
        b"bin!",
        "concurrent installs must leave a valid binary"
    );
}

#[test]
fn cache_base_dir_follows_platform_convention() {
    assert_eq!(
        cache_base_dir(Path::new("/home/user"), false),
        Path::new("/home/user/.cache/trellis")
    );
    assert_eq!(
        cache_base_dir(Path::new("/Users/user"), true),
        Path::new("/Users/user/Library/Caches/trellis")
    );
}

/// Spawns `program` with `args`, retrying transient `ETXTBSY` ("text file busy")
/// exec failures and bogus spawns: under parallel load a spawn can return `Ok` for a
/// child whose exec failed, which dies without ever running (its comm stays the forking
/// thread's name). Exec is confirmed via `/proc/<pid>/comm` matching the program
/// basename before returning; a bogus child is reaped and the spawn retried.
#[cfg(target_os = "linux")]
fn spawn_with_retry(program: &Path, args: &[&str]) -> Child {
    let basename = program
        .file_name()
        .expect("program has a file name")
        .to_string_lossy();
    // comm is truncated to 15 chars (TASK_COMM_LEN - 1).
    let expected_comm = &basename[..basename.len().min(15)];
    for attempt in 0..20 {
        match Command::new(program).args(args).spawn() {
            Ok(mut child) => {
                let pid = child.id() as i32;
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let exec_confirmed = fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|comm| comm.trim().starts_with(expected_comm));
                    if exec_confirmed {
                        return child;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The child never exec'd (bogus spawn): reap it and retry.
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 19 => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("spawn {}: {error}", program.display()),
        }
    }
    unreachable!("retry loop always returns or panics")
}

/// Spawns a copy of `/bin/sleep` under `binary` with a bounded retry (see
/// [`spawn_with_retry`]).
#[cfg(target_os = "linux")]
fn spawn_renamed_sleep(binary: &Path) -> Child {
    spawn_with_retry(binary, &["30"])
}

/// Spawns `program` and leaves it as a zombie: the child is SIGKILLed and the `Child`
/// handle is dropped without reaping, so `/proc/<pid>/exe` becomes unreadable while
/// `/proc/<pid>/comm` stays readable. The zombie is reaped by init when the test
/// process exits.
#[cfg(target_os = "linux")]
fn zombie_child(program: &Path) -> i32 {
    let mut child = spawn_with_retry(program, &[]);
    let pid = child.id() as i32;
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::canonicalize(format!("/proc/{pid}/exe")).is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        fs::canonicalize(format!("/proc/{pid}/exe")).is_err(),
        "child {pid} must become a zombie with an unreadable exe"
    );
    drop(child);
    pid
}

/// A private (0o700) cache dir under `parent`, matching what `ensure` requires.
fn private_cache_dir(parent: &Path) -> PathBuf {
    let dir = parent.join("cache");
    fs::create_dir_all(&dir).expect("create cache dir");
    set_private_dir_permissions(&dir).expect("make private");
    dir
}

/// A pid that is guaranteed dead: a reaped child of this test process.
fn dead_pid() -> i32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    child.wait().expect("wait for true");
    child.id() as i32
}

fn write_fixture_archive(path: &Path, entry_name: &str, contents: &[u8]) {
    let encoder = GzEncoder::new(
        fs::File::create(path).expect("create archive"),
        Compression::default(),
    );
    let mut tar = Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, entry_name, contents)
        .expect("append entry");
    tar.into_inner()
        .expect("tar encoder")
        .finish()
        .expect("finish gzip");
}

/// A pin whose archive checksum matches `archive_bytes` for the current platform asset,
/// so tests exercise the cache flow hermetically without network access.
fn fixture_pin(version: &str, archive_bytes: &[u8]) -> PinnedNatsServer {
    let asset = platform().expect("current platform");
    PinnedNatsServer {
        version: version.to_string(),
        sha256: HashMap::from([(asset.to_string(), sha256_hex(archive_bytes))]),
    }
}
