"""Package a verified Linux server binary and tracked standalone deployment files.

Run after committing and building the server from a clean checkout. This creates
local candidates only; it never publishes or changes a running installation.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import time
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[2]
DRIVER_NOTICES = tuple("third-party/cua-driver/" + name for name in (
    "LICENSE.txt", "UPSTREAM-LICENSING.md", "UPSTREAM-THIRD-PARTY-NOTICES.md",
    "BUILD.md", "SOURCE.json", "RUST-DEPENDENCIES.json", "RUST-DEPENDENCY-NOTICES.txt"))


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def driver_payload(payload, archive=None):
    manifest_path = ROOT / "deploy/cua-driver-manifest.json"
    if not manifest_path.exists():
        if payload is not None or archive is not None:
            raise ValueError("Driver payload supplied without tracked manifest")
        return None
    manifest = json.loads(manifest_path.read_text())
    if (manifest.get("schema") != 1 or manifest.get("name") != "cua-driver"
            or manifest.get("target") != "x86_64-unknown-linux-gnu"
            or manifest.get("packaged_path") != "deploy/vendor/cua-driver"
            or not re.fullmatch(r"[0-9a-f]{64}", manifest.get("binary_sha256", ""))
            or not re.fullmatch(r"[0-9a-f]{40}", manifest.get("source_commit", ""))
            or not re.fullmatch(r"[0-9a-f]{64}", manifest.get("archive_sha256", ""))
            or manifest.get("archive_member") != "cua-driver"
            or manifest.get("license") != "MIT"
            or not re.fullmatch(r"\d+\.\d+\.\d+", manifest.get("version", ""))
            or type(manifest.get("binary_bytes")) is not int
            or manifest["binary_bytes"] <= 0):
        raise ValueError("Invalid pinned Cua driver manifest")
    if manifest.get("kindred_policy") != "no_automatic_browser_input_v1":
        raise ValueError("Missing required Cua no-automatic-input policy")
    for field in ("patch_sha256", "upstream_archive_sha256", "upstream_binary_sha256"):
        if not isinstance(manifest.get(field), str) or not re.fullmatch(r"[0-9a-f]{64}", manifest[field]):
            raise ValueError("Missing or invalid Cua provenance: " + field)
    build = manifest.get("build")
    if (not isinstance(build, dict)
            or not isinstance(build.get("compiler"), str) or not build["compiler"].strip()
            or not isinstance(build.get("command"), str) or not build["command"].strip()
            or not isinstance(build.get("cargo_lock_sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", build["cargo_lock_sha256"])):
        raise ValueError("Missing or invalid Cua build provenance")
    libraries = build.get("required_libraries")
    if (not isinstance(libraries, list) or not libraries
            or any(not isinstance(lib, str) or not re.fullmatch(r"lib[A-Za-z0-9_.+-]+\.so(?:\.[0-9]+)*", lib) for lib in libraries)
            or not isinstance(manifest.get("minimum_glibc"), str)
            or not re.fullmatch(r"[0-9]+\.[0-9]+", manifest["minimum_glibc"])):
        raise ValueError("Missing or invalid Cua runtime library requirements")
    if (manifest["archive_sha256"] == manifest["upstream_archive_sha256"]
            or manifest["binary_sha256"] == manifest["upstream_binary_sha256"]
            or manifest.get("source") != "https://github.com/trycua/cua"
            or manifest.get("tag") != "cua-driver-rs-v" + manifest["version"]):
        raise ValueError("Derivative Cua identity must be distinct from upstream baseline")
    for name in DRIVER_NOTICES:
        notice = ROOT / name
        if (notice.is_symlink() or not notice.is_file() or not notice.stat().st_size
                or not notice.resolve().is_relative_to(ROOT.resolve())):
            raise ValueError("Missing regular Cua notice: " + name)
    if json.loads((ROOT / "third-party/cua-driver/SOURCE.json").read_text()) != manifest:
        raise ValueError("Cua notice provenance differs from driver manifest")
    patch_name = manifest.get("patch_path")
    if (not isinstance(patch_name, str) or "\\" in patch_name
            or not patch_name.startswith("third-party/cua-driver/")
            or any(part in ("", ".", "..") for part in patch_name.split("/"))):
        raise ValueError("Unsafe Cua policy patch path")
    patch = ROOT / patch_name
    if (patch.is_symlink() or not patch.is_file()
            or not patch.resolve().is_relative_to(ROOT.resolve())
            or hashlib.sha256(patch.read_bytes()).hexdigest() != manifest["patch_sha256"]):
        raise ValueError("Cua policy patch differs from pinned manifest")
    payload = payload if payload is not None else ROOT / manifest["packaged_path"]
    if payload.is_symlink() or not payload.is_file():
        raise ValueError("Missing regular Cua driver payload; supply --cua-driver")
    data = payload.read_bytes()
    if len(data) != manifest["binary_bytes"] or hashlib.sha256(data).hexdigest() != manifest["binary_sha256"]:
        raise ValueError("Cua driver payload differs from pinned manifest")
    if data[:5] != b"\x7fELF\x02" or data[18:20] != b"\x3e\x00":
        raise ValueError("Expected Linux x86_64 Cua driver executable")
    if not payload.stat().st_mode & 0o111:
        raise ValueError("Cua driver payload must be executable")
    if archive is None or archive.is_symlink() or not archive.is_file():
        raise ValueError("Missing regular pinned archive; supply --cua-driver-archive")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != manifest["archive_sha256"]:
        raise ValueError("Cua driver archive differs from pinned manifest")
    with tarfile.open(archive, "r:gz") as bundle:
        members = [m for m in bundle.getmembers() if m.name == manifest["archive_member"]]
        if len(members) != 1 or not members[0].isfile() or members[0].size != len(data):
            raise ValueError("Invalid Cua driver archive member")
        if bundle.extractfile(members[0]).read() != data:
            raise ValueError("Cua driver archive executable differs from payload")
    return manifest, data


def package(binary, output, cua_driver=None, cua_driver_archive=None):
    if git("status", "--porcelain").strip():
        raise ValueError("Commit the server source before building a release candidate")
    driver = driver_payload(cua_driver, cua_driver_archive)
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
    if driver:
        manifest, driver_data = driver
        if "deploy/cua-driver-manifest.json" not in tracked:
            raise ValueError("Cua driver manifest must be tracked in Git")
        if any(name not in tracked for name in DRIVER_NOTICES):
            raise ValueError("Cua required notices must be tracked in Git")
        if manifest["patch_path"] not in tracked:
            raise ValueError("Cua policy patch must be tracked in Git")
        if manifest["packaged_path"] in tracked:
            raise ValueError("Generated driver binary must not be tracked in Git")
        metadata["cua_driver"] = manifest
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
        if driver:
            info = zipfile.ZipInfo(manifest["packaged_path"], stamp)
            info.create_system = 3
            info.external_attr = 0o100755 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, driver_data)
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
        if driver and z.read(manifest["packaged_path"]) != driver_data:
            raise ValueError("Packaged driver changed")
    metadata["standalone_sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
    metadata["bytes"] = archive.stat().st_size
    (output / "SOURCE.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cua-driver", type=Path, help="Verified generated driver binary; defaults to manifest packaged_path")
    parser.add_argument("--cua-driver-archive", type=Path, help="Retained pinned release tar.gz; required with driver manifest")
    args = parser.parse_args()
    package(args.binary, args.output, args.cua_driver, args.cua_driver_archive)
