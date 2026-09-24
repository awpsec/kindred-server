"""Offline coverage of complete Codex packages and lossless guest installation."""
import importlib.util,io,json,tarfile,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('codex_package',Path(__file__).with_name('download-codex.py'))
package=importlib.util.module_from_spec(spec);spec.loader.exec_module(package)

class CodexPackage(unittest.TestCase):
    def archive(self,path,omit='',extra=None):
        with tarfile.open(path,'w:gz') as tar:
            for name in package.FILES:
                if name==omit:continue
                data=json.dumps({'layoutVersion':1,'version':package.VERSION,'target':'x86_64-unknown-linux-musl','entrypoint':'bin/codex','resourcesDir':'codex-resources','pathDir':'codex-path'}).encode() if name=='codex-package.json' else b'\x7fELF\x02\x01'+bytes(12)+b'\x3e\x00'+bytes(44)+name.encode()
                member=tarfile.TarInfo(name);member.size=len(data);member.mode=0o644 if name=='codex-package.json' else 0o755;tar.addfile(member,io.BytesIO(data))
            if extra:tar.addfile(extra,io.BytesIO(b'x'*extra.size))

    def test_update_selects_stable_release_and_installs_complete_runtime(self):
        reply=io.BytesIO(json.dumps({'tag_name':'rust-v9.8.7','prerelease':False,'draft':False}).encode())
        with patch.object(package,'VERSION',package.VERSION),patch.object(package.urllib.request,'urlopen',return_value=reply),patch.object(package,'download') as download,patch.object(package,'install') as install:
            package.update(Path('/fixture/bin/codex'))
            self.assertEqual(package.VERSION,'9.8.7')
            download.assert_called_once()
            self.assertEqual(install.call_args.args[1],Path('/fixture/bin/codex'))
    def test_prerelease_does_not_replace_runtime(self):
        reply=io.BytesIO(json.dumps({'tag_name':'rust-v9.8.7-alpha','prerelease':True}).encode())
        with patch.object(package.urllib.request,'urlopen',return_value=reply),patch.object(package,'download') as download:
            with self.assertRaises(ValueError):package.update(Path('/fixture/bin/codex'))
            download.assert_not_called()

    def test_complete_runtime_survives_relocation_and_repeated_install(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);archive=root/'package.tar.gz';self.archive(archive)
            staged=root/'staged';package.extract(archive,staged)
            self.enterContext(patch.dict(package.CHECKER, {'check':lambda entry: package.validate(entry.parent.parent)}))
            entry=root/'media/codex';package.publish(staged,root/'media/codex-runtime',entry)
            target=root/'prefix/bin/codex';target.parent.mkdir(parents=True);target.write_bytes(b'previous CLI')
            package.install(entry,target);installed=target.resolve().parent.parent
            self.assertEqual(target.read_bytes(),(staged/'bin/codex').read_bytes())
            self.assertTrue((target.resolve().parent/'codex-code-mode-host').is_file())
            self.assertEqual(target.with_name('codex-code-mode-host').resolve(),installed/'bin/codex-code-mode-host')
            for name in package.FILES:self.assertEqual((installed/name).read_bytes(),(staged/name).read_bytes())
            package.install(entry,target)
            (installed/'bin/codex-code-mode-host').write_bytes((staged/'bin/codex-code-mode-host').read_bytes()+b'changed')
            with self.assertRaisesRegex(ValueError,'differs'):package.install(entry,target)

    def test_extra_resources_are_preserved_without_accepting_unsafe_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);archive=root/'package.tar.gz'
            extra=tarfile.TarInfo('codex-resources/voice/NOTICE.md');extra.size=5;extra.mode=0o644
            self.archive(archive,extra=extra);package.extract(archive,root/'staged')
            self.assertEqual((root/'staged/codex-resources/voice/NOTICE.md').read_bytes(),b'xxxxx')
            self.assertEqual((root/'staged/codex-resources/voice/NOTICE.md').stat().st_mode & 0o777,0o644)
            extra=tarfile.TarInfo('codex-resources/../../escape');extra.size=1
            self.archive(archive,extra=extra)
            with self.assertRaises(ValueError):package.extract(archive,root/'unsafe')

    def test_missing_code_mode_host_never_replaces_existing_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);archive=root/'package.tar.gz';self.archive(archive,omit='bin/codex-code-mode-host')
            target=root/'codex';target.write_bytes(b'previous CLI')
            with self.assertRaisesRegex(ValueError,'incomplete'):package.extract(archive,root/'staged')
            self.assertEqual(target.read_bytes(),b'previous CLI')

    def test_failed_destination_probe_never_replaces_existing_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);archive=root/'package.tar.gz';self.archive(archive)
            staged=root/'staged';package.extract(archive,staged)
            target=root/'prefix/bin/codex';target.parent.mkdir(parents=True);target.write_bytes(b'previous CLI')
            def broken(entry):raise ValueError('tool round trip failed')
            with patch.dict(package.CHECKER, {'check':broken}),self.assertRaisesRegex(ValueError,'round trip'):
                package.install(staged/'bin/codex',target)
            self.assertEqual(target.read_bytes(),b'previous CLI')
            self.assertFalse(target.with_name('codex-code-mode-host').exists())

    def test_archive_links_and_path_traversal_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for index,name in enumerate(['../escaped','bin/codex']):
                member=tarfile.TarInfo(name)
                if index:member.type=tarfile.SYMTYPE;member.linkname='/outside'
                else:member.size=1
                archive=root/f'{index}.tar.gz';self.archive(archive,extra=member)
                with self.assertRaisesRegex(ValueError,'Unexpected'):package.extract(archive,root/f'staged{index}')
            self.assertFalse((root/'escaped').exists())

if __name__=='__main__':unittest.main()
