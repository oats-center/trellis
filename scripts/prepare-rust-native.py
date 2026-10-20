#!/usr/bin/env python3
"""Embed verified release-archive checksums before packaging the Rust testkit."""

import hashlib
import json
from pathlib import Path
import sys

archives, output, version = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
targets = {}
for archive in sorted(archives.glob(f"trellis-{version}-*.tar.gz")):
    target = archive.name.removeprefix(f"trellis-{version}-").removesuffix(".tar.gz")
    if target not in {
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    }:
        raise SystemExit(f"unsupported native target: {target}")
    checksum = (archives / f"checksum-{version}-{target}-trellis.sha256").read_text().split()[0]
    with archive.open("rb") as handle:
        actual = hashlib.file_digest(handle, "sha256").hexdigest()
    if actual != checksum:
        raise SystemExit(f"native archive checksum mismatch: {archive.name}")
    targets[target] = {"archive": archive.name, "sha256": actual}
if not targets:
    raise SystemExit(f"no native archives for {version}")
staged = output.with_suffix(".tmp")
staged.write_text(json.dumps({"version": version, "targets": targets}, indent=2) + "\n")
staged.replace(output)
