import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('runtime_update',Path(__file__).with_name('update-guest-runtime.py'))
update=importlib.util.module_from_spec(spec);spec.loader.exec_module(update)


def binary(version='test', supported=True):
    return ('#!/usr/bin/python3\nimport sys,json\n'
            'if sys.argv[1]=="--version": print("kindred '+version+'")\n'
            'else:\n packet=json.load(sys.stdin)\n assert packet=={"tool":"command_poll","args":{"commands":[]},"screen":1}\n print('+repr('{"commands":[]}' if supported else '{"error":"unknown guest tool"}')+')\n').encode()


class RuntimeUpdate(unittest.TestCase):
    def test_verified_switch_preserves_backup_and_repeated_update(self):
        with tempfile.TemporaryDirectory() as root:
            target=Path(root)/'kindred-bin';target.write_bytes(b'original runtime')
            data=binary();digest=hashlib.sha256(data).hexdigest()
            self.assertTrue(update.install(io.BytesIO(data),target,digest,'test')['updated'])
            self.assertEqual(target.read_bytes(),data)
            self.assertEqual(target.with_name('kindred-bin.previous').read_bytes(),b'original runtime')
            self.assertFalse(update.install(io.BytesIO(data),target,digest,'test')['updated'])
            self.assertEqual(target.with_name('kindred-bin.previous').read_bytes(),b'original runtime')

    def test_bad_transfer_version_and_capability_preserve_original(self):
        for data,digest in [(binary(), '0'*64),(binary('old'),None),(binary(supported=False),None)]:
            with self.subTest(),tempfile.TemporaryDirectory() as root:
                target=Path(root)/'kindred-bin';target.write_bytes(b'original runtime')
                with self.assertRaises(Exception):
                    update.install(io.BytesIO(data),target,digest or hashlib.sha256(data).hexdigest(),'test')
                self.assertEqual(target.read_bytes(),b'original runtime')
                self.assertEqual(list(Path(root).iterdir()),[target])


if __name__=='__main__':unittest.main()
