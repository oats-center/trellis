#!/usr/bin/env python3
"""Keep each published model compatibility line bound to one native snapshot."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def snapshot(source):
    with tarfile.open(fileobj=source, mode="r:gz") as archive:
        manifests = [entry for entry in archive if entry.name.count("/") == 1
                     and entry.name.endswith("/Cargo.toml")]
        if len(manifests) != 1 or not manifests[0].isfile():
            raise SystemExit("model archive requires one package manifest")
        manifest = tomllib.loads(archive.extractfile(manifests[0]).read().decode())
        package = manifest["package"]
        if package["name"] != "trellis-models":
            raise SystemExit("expected the trellis-models package")
        prefix = manifests[0].name.rsplit("/", 1)[0]
        metadata = json.load(archive.extractfile(f"{prefix}/generated/models.json"))
        if metadata.get("format") != "trellis.rust-models.v1":
            raise SystemExit("unsupported model snapshot format")
        return package["version"], metadata


def compatibility_line(version):
    major, minor, patch = map(int, version.split("+", 1)[0].split("-", 1)[0].split("."))
    return (major,) if major else (0, minor) if minor else (0, 0, patch)


def fetch(url):
    request = Request(url, headers={"User-Agent": "trellis-release-verification (https://github.com/oats-center/trellis)"})
    with urlopen(request, timeout=30) as response:
        return response.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--previous", type=Path, help="compare an accepted local archive instead of querying crates.io")
    args = parser.parse_args()
    with args.candidate.open("rb") as source:
        version, candidate = snapshot(source)
    if args.previous:
        with args.previous.open("rb") as source:
            previous_version, previous = snapshot(source)
    else:
        try:
            published = json.loads(fetch("https://crates.io/api/v1/crates/trellis-models"))
        except HTTPError as error:
            if error.code != 404:
                raise
            print("First model publication: no existing compatibility line.")
            return
        compatible = [release for release in published["versions"]
                      if compatibility_line(release["num"]) == compatibility_line(version)]
        if not compatible:
            print("New model compatibility line.")
            return
        release = max(compatible, key=lambda release: release["created_at"])
        data = fetch(f"https://static.crates.io/crates/trellis-models/trellis-models-{release['num']}.crate")
        if hashlib.sha256(data).hexdigest() != release["checksum"]:
            raise SystemExit("published model archive checksum mismatch")
        previous_version, previous = snapshot(io.BytesIO(data))
    if compatibility_line(previous_version) == compatibility_line(version) and previous != candidate:
        raise SystemExit("native model snapshot or Rust model ABI changed within a Cargo-compatible line; choose a new incompatible model version")
    print("Model publication preserves existing generated-client bindings.")


if __name__ == "__main__":
    main()
