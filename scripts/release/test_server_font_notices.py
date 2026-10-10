"""Exercise actual ZIP file selection; synthetic executable/driver gates are stubbed."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("server_package", ROOT / "scripts/release/package-server.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)

class ServerFontNoticesTests(unittest.TestCase):
    def test_actual_package_contains_exact_font_notices(self):
        version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        original_output = subprocess.check_output
        original_git = package.git
        with tempfile.TemporaryDirectory(prefix="kindred-font-notices-test-") as temporary:
            root = Path(temporary)
            binary = root / "synthetic-executable"
            header = bytearray(64)
            header[:5] = b"\x7fELF\x02"
            header[18:20] = b"\x3e\x00"
            binary.write_bytes(header)
            def output(args, **kwargs):
                if args == [str(binary.resolve()), "--version"]:
                    return "kindred " + version
                return original_output(args, **kwargs)
            def git(*args):
                # This tests file selection before/after editing, not release eligibility.
                return b"" if args == ("status", "--porcelain") else original_git(*args)
            with patch.object(package, "driver_payload", return_value=None), patch.object(package, "git", side_effect=git), patch.object(package.subprocess, "check_output", side_effect=output), contextlib.redirect_stdout(io.StringIO()):
                package.package(binary, root / "output")
            with zipfile.ZipFile(root / "output" / ("kindred-standalone-" + version + ".zip")) as archive:
                for name in ["DMSans-LICENSE.txt", "DMSans-SOURCE.json", "Manrope-LICENSE.txt", "Manrope-SOURCE.json", "LICENSE.txt", "SOURCE.json", "Liberation-LICENSE.txt", "Liberation-SOURCE.json"]:
                    path = "ui/fonts/" + name
                    self.assertIn(path, archive.namelist(), path)
                    self.assertEqual(archive.read(path), (ROOT / path).read_bytes(), path)
                self.assertEqual(archive.read("THIRD_PARTY_NOTICES.md"), (ROOT / "THIRD_PARTY_NOTICES.md").read_bytes())
                self.assertEqual(archive.read("kindred"), binary.read_bytes())
                self.assertIsNone(archive.testzip())

if __name__ == "__main__":
    unittest.main()
