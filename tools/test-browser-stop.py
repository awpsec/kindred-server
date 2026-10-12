"""Shutdown scope/refusal checks; real save/restart coverage lives in persistence fixture."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('stop_browser', ROOT / 'deploy/stop-browser.py')
stop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stop)


class ShutdownScope(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.proc = self.root / 'proc'
        self.proc.mkdir()
        self.profile = self.root / 'browser-2'

    def parent(self, arguments=None, identity=b'123', state=b'S'):
        entry = self.proc / '42'
        entry.mkdir(exist_ok=True)
        (entry / 'exe').symlink_to('/fixture/chromium')
        (entry / 'cmdline').write_bytes(b'\0'.join(arguments or [b'/fixture/chromium', os.fsencode('--user-data-dir=' + str(self.profile))]))
        (entry / 'stat').write_bytes(b'42 (chromium) ' + b' '.join([state, *([b'0'] * 18), identity]))
        return entry

    def test_joined_process_title_and_exact_profile_identity(self):
        self.parent([os.fsencode('/fixture/chromium --user-data-dir=' + str(self.profile))])
        self.assertEqual(stop.browser_processes(self.profile, self.proc), [(42, b'123')])
        self.assertTrue(stop.process_current((42, b'123'), self.proc))
        self.assertFalse(stop.process_current((42, b'other'), self.proc))

    def test_foreign_duplicate_and_renderer_arguments_never_close(self):
        base = [b'/fixture/chromium', os.fsencode('--user-data-dir=' + str(self.profile))]
        for args in [base + [b'--type=renderer'], base + [b'--app=test'],
                     base + [b'--user-data-dir=/foreign'], [b'/different/chromium', base[1]]]:
            with self.subTest(args=args):
                self.parent(args)
                self.assertEqual(stop.browser_processes(self.profile, self.proc), [])
                (self.proc / '42/exe').unlink()

    def test_exited_zombie_is_complete_without_waiting_for_parent_reap(self):
        self.parent(state=b'Z')
        self.assertFalse(stop.process_current((42, b'123'), self.proc))

    def test_missing_profile_and_no_browser_have_no_side_effect(self):
        with patch.object(stop, 'desktop_context') as context:
            stop.close_browser(2, root=self.root)
            self.assertFalse(self.profile.exists())
            self.profile.mkdir(mode=0o700)
            with patch.object(stop, 'browser_processes', return_value=[]):
                stop.close_browser(2, root=self.root)
            context.assert_not_called()

    def test_redirected_profile_and_ambiguous_parents_are_preserved(self):
        other = self.root / 'other'; other.mkdir()
        self.profile.symlink_to(other)
        with self.assertRaises(stop.BrowserContextError):
            stop.close_browser(2, root=self.root)
        self.assertEqual(list(other.iterdir()), [])
        self.profile.unlink(); self.profile.mkdir(mode=0o700)
        with patch.object(stop, 'browser_processes', return_value=[(42, b'a'), (43, b'b')]), patch.object(stop, 'desktop_context') as context:
            with self.assertRaisesRegex(stop.BrowserContextError, 'ambiguous'):
                stop.close_browser(2, root=self.root)
            context.assert_not_called()


if __name__ == '__main__':
    unittest.main()
