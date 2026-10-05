import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import shutil
import contextlib
import io
import zipfile
import tarfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("packager", Path(__file__).with_name("package-server.py"))
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)

def archive_fixture(root, data, manifest):
    path = root / "driver.tar.gz"
    with tarfile.open(path, "w:gz") as archive:
        member = tarfile.TarInfo("cua-driver")
        member.size = len(data)
        archive.addfile(member, io.BytesIO(data))
    manifest.update(archive_sha256=hashlib.sha256(path.read_bytes()).hexdigest(), archive_member="cua-driver", license="MIT", version="0.33.4")
    return path

class DriverPayload(unittest.TestCase):
    def test_archive_contains_exact_executable_and_provenance(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder) / "source"
            root.mkdir()
            original = packager.ROOT
            # Use real tracked distribution resources, but synthetic ELF payloads.
            resources = packager.git("ls-files", "-z").decode().split("\0")
            for name in resources:
                if name and (name.startswith(("deploy/", "third-party/", "harness/pi/", "ui/fonts/", "docs/PROVIDER_MARKS")) or name in {"Cargo.toml", "compose.yaml", "compose.kvm.yaml", "LICENSE", "THIRD_PARTY_NOTICES.md", "ui/artifact-vendor.js.LEGAL.txt"}):
                    destination = root / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(original / name, destination)
            data = b"\x7fELF\x02" + bytes(13) + b"\x3e\x00" + b"fixture"
            binary = Path(folder) / "server"
            binary.write_bytes(data)
            payload = Path(folder) / "driver"
            payload.write_bytes(data)
            payload.chmod(0o755)
            manifest = {"schema": 1, "name": "cua-driver", "target": "x86_64-unknown-linux-gnu", "packaged_path": "deploy/vendor/cua-driver", "source_commit": "a" * 40, "binary_sha256": hashlib.sha256(data).hexdigest(), "binary_bytes": len(data)}
            archive_input = archive_fixture(Path(folder), data, manifest)
            (root / "deploy/cua-driver-manifest.json").write_text(json.dumps(manifest))
            version = packager.tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
            def git(*args):
                if args[0] == "status": return b""
                if args[0] == "rev-parse": return b"b" * 40
                if args[0] == "show": return b"1700000000"
                return ("\0".join(resources + ["deploy/cua-driver-manifest.json"])).encode()
            output = Path(folder) / "candidate"
            with patch.object(packager, "ROOT", root), patch.object(packager, "git", side_effect=git), patch.object(packager.subprocess, "check_output", return_value="kindred " + version), contextlib.redirect_stdout(io.StringIO()):
                packager.package(binary, output, payload, archive_input)
            with zipfile.ZipFile(output / ("kindred-standalone-" + version + ".zip")) as archive:
                self.assertEqual(archive.read(manifest["packaged_path"]), data)
                self.assertEqual((archive.getinfo(manifest["packaged_path"]).external_attr >> 16) & 0o777, 0o755)
                self.assertEqual(json.loads(archive.read("bundle.json"))["cua_driver"], manifest)
                self.assertEqual(archive.read("kindred"), data)
            self.assertEqual(json.loads((output / "SOURCE.json").read_text())["cua_driver"], manifest)

    def test_pinned_payload_and_rejections(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "deploy").mkdir()
            payload = root / "driver"
            data = b"\x7fELF\x02" + bytes(13) + b"\x3e\x00" + b"fixture"
            payload.write_bytes(data)
            payload.chmod(0o755)
            manifest = {"schema": 1, "name": "cua-driver", "target": "x86_64-unknown-linux-gnu", "packaged_path": "deploy/vendor/cua-driver", "source_commit": "a" * 40, "binary_sha256": hashlib.sha256(data).hexdigest(), "binary_bytes": len(data)}
            archive_input = archive_fixture(root, data, manifest)
            path = root / "deploy/cua-driver-manifest.json"
            with patch.object(packager, "ROOT", root):
                self.assertIsNone(packager.driver_payload(None))
                with self.assertRaisesRegex(ValueError, "without tracked manifest"):
                    packager.driver_payload(payload)
                path.write_text(json.dumps(manifest))
                self.assertEqual(packager.driver_payload(payload, archive_input), (manifest, data))
                with self.assertRaisesRegex(ValueError, "Missing regular"):
                    packager.driver_payload(None)
                with self.assertRaisesRegex(ValueError, "Missing regular pinned archive"):
                    packager.driver_payload(payload)
                saved_archive = archive_input.read_bytes()
                archive_input.write_bytes(saved_archive + b"tampered")
                with self.assertRaisesRegex(ValueError, "archive differs"):
                    packager.driver_payload(payload, archive_input)
                archive_input.write_bytes(saved_archive)
                payload.chmod(0o644)
                with self.assertRaisesRegex(ValueError, "must be executable"):
                    packager.driver_payload(payload, archive_input)
                payload.chmod(0o755)
                payload.write_bytes(data + b"tampered")
                with self.assertRaisesRegex(ValueError, "differs"):
                    packager.driver_payload(payload)
                payload.write_bytes(data)
                link = root / "link"
                link.symlink_to(payload)
                with self.assertRaisesRegex(ValueError, "Missing regular"):
                    packager.driver_payload(link)
                manifest["target"] = "aarch64-apple-darwin"
                path.write_text(json.dumps(manifest))
                with self.assertRaisesRegex(ValueError, "Invalid pinned"):
                    packager.driver_payload(payload)

if __name__ == "__main__":
    unittest.main()
