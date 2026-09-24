"""Package a verified Linux server binary and tracked standalone deployment files.

Run after committing and building the server from a clean checkout. This creates
local candidates only; it never publishes or changes a running installation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def package(binary, output):
    if git("status", "--porcelain").strip():
        raise ValueError("Commit the server source before building a release candidate")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    source = git("rev-parse", "HEAD").decode().strip()
    data = binary.read_bytes()
    if data[:5] != b"\x7fELF\x02" or data[18:20] != b"\x3e\x00":
        raise ValueError("Expected a Linux x86_64 server executable")
    actual = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip()
    if actual != "kindred " + version:
        raise ValueError(f"Wrong binary version: {actual}")
    tracked = git("ls-files", "-z").decode().split("\0")
    files = {name: ROOT / name for name in tracked if name.startswith("deploy/") or name.startswith("third-party/")}
    for name in ["compose.yaml", "compose.kvm.yaml", "LICENSE", "THIRD_PARTY_NOTICES.md",
                 "harness/pi/package.json", "harness/pi/package-lock.json",
                 "harness/pi/worker.mjs", "harness/pi/session.mjs", "harness/pi/opencode-models.json",
                 "ui/fonts/LICENSE.txt", "ui/fonts/SOURCE.json",
                 "ui/fonts/Liberation-LICENSE.txt", "ui/fonts/Liberation-SOURCE.json",
                 "docs/PROVIDER_MARKS.md", "docs/PROVIDER_MARKS_LICENSE.txt"]:
        files[name] = ROOT / name
    files["compose.yaml"] = ROOT / "deploy/compose.standalone.yaml"
    files["Dockerfile"] = ROOT / "deploy/Dockerfile.standalone"
    files["third-party/artifact-vendor.js.LEGAL.txt"] = ROOT / "ui/artifact-vendor.js.LEGAL.txt"
    files["kindred"] = binary
    metadata = {"version": version, "source_commit": source,
                "server_sha256": hashlib.sha256(data).hexdigest(),
                "target": "x86_64-unknown-linux-gnu"}
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"kindred-standalone-{version}.zip"
    if archive.exists():
        raise ValueError(f"Candidate already exists: {archive}")
    stamp = time.gmtime(int(git("show", "-s", "--format=%ct", "HEAD")))[:6]
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for name, path in sorted(files.items()):
            info = zipfile.ZipInfo(name, stamp)
            info.create_system = 3
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (0o100755 if name == "kindred" or os.access(path, os.X_OK) else 0o100644) << 16
            z.writestr(info, path.read_bytes())
        info = zipfile.ZipInfo("bundle.json", stamp)
        info.create_system = 3
        info.external_attr = 0o100644 << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, json.dumps(metadata, indent=2) + "\n")
    with zipfile.ZipFile(archive) as z:
        if z.testzip() is not None:
            raise ValueError("Archive integrity check failed")
        if z.read("kindred") != data:
            raise ValueError("Packaged binary changed")
    metadata["standalone_sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
    metadata["bytes"] = archive.stat().st_size
    (output / "SOURCE.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    package(args.binary, args.output)
