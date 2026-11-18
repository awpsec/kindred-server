import importlib.util
from pathlib import Path
import tempfile
import unittest
import uuid

spec = importlib.util.spec_from_file_location('grok', Path(__file__).with_name('grok-account.py'))
grok = importlib.util.module_from_spec(spec)
spec.loader.exec_module(grok)


class Isolation(unittest.TestCase):
    def test_private_account_homes_and_environment(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'providers'
            a, b = [grok.account_directory(root, str(uuid.uuid4())) for _ in range(2)]
            self.assertNotEqual(a, b)
            for path in (a, b):
                self.assertEqual(path.stat().st_mode & 0o777, 0o700)
                env = grok.environment(path)
                self.assertEqual(env['HOME'], str(path))
                self.assertEqual(env['GROK_HOME'], str(path / '.grok'))
                self.assertNotIn('XAI_API_KEY', env)

    def test_reject_symlink_and_path_account(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'providers'
            home = grok.account_directory(root, str(uuid.uuid4()))
            link = home.with_name(str(uuid.uuid4()))
            link.symlink_to(home)
            for account in (link.name, '../elsewhere', home.name.upper()):
                with self.assertRaises(ValueError):
                    grok.account_directory(root, account)
