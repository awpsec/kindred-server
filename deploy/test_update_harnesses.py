"""Offline update tests: no package installs or network access."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('updater',Path(__file__).with_name('update-harnesses.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class Updates(unittest.TestCase):
    def test_pi_verifies_before_switch_and_preserves_old_release(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);old=root/'releases/old';old.mkdir(parents=True)
            (old/'worker.mjs').write_text('original');(old/'session.mjs').write_text("export const SDK_VERSION = '0.85.1';");current=root/'current';current.symlink_to(old)
            npm=old/'node/lib/node_modules/npm/bin/npm-cli.js'
            npm.parent.mkdir(parents=True);npm.write_text('bundled npm')
            (old/'node/bin').mkdir();(old/'node/bin/npm').symlink_to('../lib/node_modules/npm/bin/npm-cli.js')
            (old/'node_modules').mkdir();(old/'node_modules/stale').write_text('old SDK')
            calls=[]
            def run(args,**kwargs):
                self.assertEqual(current.resolve(),old)
                calls.append([str(a) for a in args])
                if 'install' in args:
                    stage=kwargs['cwd']
                    self.assertTrue((stage/'node/bin/npm').is_symlink())
                    self.assertEqual((stage/'node/bin/npm').read_text(),'bundled npm')
                    self.assertFalse((stage/'node_modules').exists())
                    package=kwargs['cwd']/'node_modules/@earendil-works/pi-coding-agent/package.json';package.parent.mkdir(parents=True);package.write_text('{"version":"0.99.0"}')
            with patch.object(m,'run',side_effect=run):m.pi(current)
            self.assertEqual(len(calls),2)
            self.assertIn('--ignore-scripts',calls[0]);self.assertIn('--check',calls[1])
            self.assertNotEqual(current.resolve(),old)
            self.assertEqual((current/'worker.mjs').read_text(),'original')
            self.assertTrue(old.exists())
            self.assertIn('0.99.0',(current/'session.mjs').read_text())
    def test_failed_pi_check_keeps_current_and_removes_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);old=root/'releases/old';old.mkdir(parents=True)
            current=root/'current';current.symlink_to(old)
            with patch.object(m,'run',side_effect=RuntimeError('bad harness')):
                with self.assertRaises(RuntimeError):m.pi(current)
            self.assertEqual(current.resolve(),old)
            self.assertEqual(list((root/'releases').iterdir()),[old])
    def test_absent_pi_is_optional(self):
        with tempfile.TemporaryDirectory() as directory,patch.object(m,'run') as run:
            m.pi(Path(directory)/'absent');run.assert_not_called()
    def test_guest_updates_both_providers_after_checks_and_preserves_credentials(self):
        import io,tarfile,hashlib,base64
        archive=io.BytesIO()
        with tarfile.open(fileobj=archive,mode='w:gz') as tar:
            entry=tarfile.TarInfo('package/claude');entry.size=3;tar.addfile(entry,io.BytesIO(b'cli'))
        data=archive.getvalue()
        for fail in (False,True):
            with self.subTest(fail=fail),tempfile.TemporaryDirectory() as directory:
                root=Path(directory);old=root/'old';old.write_text('known good')
                current=root/'claude-current';current.symlink_to(old)
                credentials=root/'credentials';credentials.write_text('unchanged')
                calls=[]
                def run(args,**kwargs):
                    args=[str(a) for a in args];calls.append(args)
                    self.assertEqual(current.resolve(),old)
                    if 'venv' in args:Path(args[-1]).mkdir()
                    if args[0].endswith('/bin/kimi') and fail:raise RuntimeError('Kimi check failed')
                info=[{'dist':{'tarball':'https://registry.npmjs.org/package','integrity':'sha512-'+base64.b64encode(hashlib.sha512(data).digest()).decode()}},{'info':{'version':'9.8.7'}}]
                with patch.object(m,'metadata',side_effect=info),patch.object(m.urllib.request,'urlopen',return_value=io.BytesIO(data)),patch.object(m,'run',side_effect=run):
                    if fail:
                        with self.assertRaises(RuntimeError):m.guest(root)
                        self.assertEqual(current.resolve(),old)
                        self.assertFalse((root/'kimi-current').exists())
                    else:
                        m.guest(root)
                        self.assertEqual(current.read_text(),'cli')
                        self.assertEqual((root/'kimi-current').resolve(),root/'kimi-update-9.8.7')
                self.assertEqual(credentials.read_text(),'unchanged')
                self.assertEqual(len(calls),4)

    def test_invalid_claude_digest_never_switches_provider(self):
        import io
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);old=root/'old';old.write_text('known good');current=root/'claude-current';current.symlink_to(old)
            with patch.object(m,'metadata',return_value={'dist':{'tarball':'https://registry.npmjs.org/test','integrity':'bad'}}),patch.object(m.urllib.request,'urlopen',return_value=io.BytesIO(b'bad package')):
                with self.assertRaisesRegex(ValueError,'integrity'):m.guest(root)
            self.assertEqual(current.resolve(),old)

if __name__=='__main__':unittest.main()
