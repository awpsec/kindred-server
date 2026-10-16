"""Owned offline layouts: an optional shell may be absent, never redirected."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import test_download_codex as fixtures

package = fixtures.package
ZSH = package.FILES[-1]


class OptionalZsh(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)

    def layout(self, version='0.163.0', absent=True):
        archive = self.base / 'package.tar.gz'
        root = self.base / 'runtime'
        with patch.object(package, 'VERSION', version):
            fixtures.CodexPackage().archive(archive)
            package.extract(archive, root)
        if absent:
            (root / ZSH).unlink()
        return root

    def validate(self, root):
        return package.CHECKER['validate'](root)

    def test_new_version_truly_absent_shell_is_valid(self):
        root = self.layout()
        shutil.rmtree(root / 'codex-resources/zsh')
        self.assertEqual(self.validate(root)['version'], '0.163.0')

    def test_new_version_valid_present_shell_is_checked(self):
        self.assertEqual(self.validate(self.layout(absent=False))['version'], '0.163.0')

    def test_dangling_shell_link_is_not_absence(self):
        root = self.layout()
        (root / ZSH).symlink_to(self.base / 'missing-owned-shell')
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_shell_link_to_valid_owned_foreign_file_is_refused(self):
        root = self.layout()
        foreign = self.base / 'foreign-shell'
        shutil.copyfile(root / 'bin/codex', foreign)
        foreign.chmod(0o755)
        (root / ZSH).symlink_to(foreign)
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_dangling_ancestor_link_is_not_absence(self):
        root = self.layout()
        shutil.rmtree(root / 'codex-resources/zsh')
        (root / 'codex-resources/zsh').symlink_to(self.base / 'missing-owned-directory')
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_linked_ancestor_with_valid_foreign_shell_is_refused(self):
        root = self.layout()
        foreign = self.base / 'foreign-bin'
        foreign.mkdir()
        shutil.copyfile(root / 'bin/codex', foreign / 'zsh')
        (foreign / 'zsh').chmod(0o755)
        (root / ZSH).parent.rmdir()
        (root / ZSH).parent.symlink_to(foreign)
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_non_directory_ancestor_is_refused(self):
        root = self.layout()
        shutil.rmtree(root / 'codex-resources/zsh')
        (root / 'codex-resources/zsh').write_bytes(b'owned-wrong-type')
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_directory_shell_is_refused(self):
        root = self.layout()
        (root / ZSH).mkdir()
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_fifo_shell_is_refused_without_opening_it(self):
        root = self.layout()
        os.mkfifo(root / ZSH, 0o600)
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_present_shell_still_requires_executable_linux_x64(self):
        root = self.layout(absent=False)
        zsh = root / ZSH
        original = zsh.read_bytes()
        for data, mode in [(b'invalid', 0o755), (original, 0o644)]:
            with self.subTest(mode=mode, valid_header=data == original):
                zsh.write_bytes(data)
                zsh.chmod(mode)
                with self.assertRaises(ValueError):
                    self.validate(root)

    def test_old_version_requires_shell_and_accepts_valid_one(self):
        root = self.layout('0.162.1', absent=False)
        self.assertEqual(self.validate(root)['version'], '0.162.1')
        (root / ZSH).unlink()
        with self.assertRaises(ValueError):
            self.validate(root)

    def test_core_executables_remain_mandatory(self):
        root = self.layout()
        for name in package.FILES[:-1]:
            if name == 'codex-package.json':
                continue
            with self.subTest(name=name):
                path = root / name
                saved = path.read_bytes()
                path.unlink()
                with self.assertRaises(ValueError):
                    self.validate(root)
                path.write_bytes(saved)
                path.chmod(0o755)

    def test_metadata_version_and_path_policy_are_unchanged(self):
        root = self.layout()
        meta = root / 'codex-package.json'
        original = json.loads(meta.read_text())
        for key, value in [('version', '0.163.0-PRIVATE'), ('version', 'unknown'),
                           ('layoutVersion', 999), ('entrypoint', '../PRIVATE-CANARY'),
                           ('resourcesDir', '../PRIVATE-CANARY'), ('target', 'foreign')]:
            with self.subTest(key=key):
                meta.write_text(json.dumps({**original, key: value}))
                with self.assertRaises(ValueError):
                    self.validate(root)
        meta.write_text(json.dumps(original))
        self.validate(root)

    def test_cli_refusal_keeps_private_paths_out_of_output(self):
        root = self.layout()
        (root / ZSH).symlink_to(self.base / 'PRIVATE-CANARY-MISSING')
        checker = Path(__file__).with_name('check-codex.py')
        result = subprocess.run([sys.executable, str(checker), str(root / 'bin/codex'), '--files-only'],
                                capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 78)
        self.assertEqual(result.stdout, b'')
        self.assertIn(b'Codex tool runtime is incomplete', result.stderr)
        self.assertNotIn(b'PRIVATE', result.stderr)
        self.assertNotIn(str(self.base).encode(), result.stderr)


if __name__ == '__main__':
    unittest.main()
