#!/usr/bin/env python3
"""Require Cargo's publication archive to match the accepted Check contents."""

import hashlib
import sys
import tarfile


def contents(path):
    files = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            if member.isdir():
                continue
            if not member.isfile():
                raise SystemExit(f"unsupported archive entry: {member.name}")
            name = member.name
            if not name or name in files:
                raise SystemExit(f"invalid or duplicate archive entry: {member.name}")
            with archive.extractfile(member) as source:
                files[name] = (
                    member.mode,
                    hashlib.file_digest(source, "sha256").hexdigest(),
                )
    return files


accepted = contents(sys.argv[1])
publication = contents(sys.argv[2])
differences = sorted(
    name for name in accepted.keys() | publication.keys()
    if accepted.get(name) != publication.get(name)
)
if differences:
    raise SystemExit(
        "publication differs from accepted Check: " + ", ".join(differences)
    )
print(f"Publication contents match accepted Check: {sys.argv[1]}")
