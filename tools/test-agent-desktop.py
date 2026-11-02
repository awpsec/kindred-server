"""Pinned guest installer and desktop binding regressions; no owner desktop."""
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]
def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
installer=load('install_agent',ROOT/'deploy/install-agent-desktop.py')
launcher=load('launch_agent',ROOT/'deploy/agent-desktop-launch.py')
packager=load('package_agent',ROOT/'scripts/release/package-server.py')

def fixture():
    resource=installer.resources();data=b'\x7fELF\x02'+bytes(13)+b'\x3e\x00'+b'synthetic'
    resource['manifest']=copy.deepcopy(resource['manifest'])
    for value in resource['manifest']['files'].values():value.update(bytes=len(data),sha256=hashlib.sha256(data).hexdigest())
    stream=io.BytesIO()
    with tarfile.open(fileobj=stream,mode='w:gz') as archive:
        for name in resource['manifest']['files']:
            item=tarfile.TarInfo(name);item.size=len(data);archive.addfile(item,io.BytesIO(data))
    body=stream.getvalue();resource['manifest'].update(archive_bytes=len(body),archive_sha256=hashlib.sha256(body).hexdigest())
    return body,resource

class Tests(unittest.TestCase):
    def test_pinned_release_rejects_corruption_and_wrong_binary(self):
        data,r=fixture();installer.verified(data,r['manifest'])
        with self.assertRaises(ValueError):installer.verified(data+b'x',r['manifest'])
        r['manifest']['files']['agent-desktop']['sha256']='0'*64
        with self.assertRaises(ValueError):installer.verified(data,r['manifest'])
    def test_archive_never_extracts_links_or_traversal(self):
        _,r=fixture();out=io.BytesIO()
        with tarfile.open(fileobj=out,mode='w:gz') as archive:
            p=tarfile.TarInfo('../escape');p.type=tarfile.SYMTYPE;p.linkname='/tmp/unrelated';archive.addfile(p)
        data=out.getvalue();r['manifest'].update(archive_bytes=len(data),archive_sha256=hashlib.sha256(data).hexdigest())
        with self.assertRaises(ValueError):installer.verified(data,r['manifest'])
    def test_atomic_offline_install_idempotence_and_custom_preservation(self):
        # Installer runs as root in a guest. Use an owned, non-world-writable parent.
        with tempfile.TemporaryDirectory(dir=ROOT) as folder:
            prefix=Path(folder)/'local';data,r=fixture()
            first=installer.install(data,r,prefix);self.assertTrue(first['updated'])
            self.assertFalse(installer.install(data,r,prefix)['updated'])
            old=(prefix/'bin/agent-desktop').resolve()
            r['skill']+='\nUpdated guide.';new=installer.install(data,r,prefix)
            self.assertTrue(new['updated']);self.assertTrue(old.exists())
            link=prefix/'bin/agent-desktop';link.unlink();link.write_text('custom')
            with self.assertRaisesRegex(ValueError,'Custom'):installer.install(data,r,prefix)
            self.assertEqual(link.read_text(),'custom')
    def test_corrupt_update_preserves_active_install(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as folder:
            prefix=Path(folder)/'local';data,r=fixture();installer.install(data,r,prefix)
            link=prefix/'bin/agent-desktop';before=link.resolve()
            with self.assertRaises(ValueError):installer.install(data+b'bad',r,prefix)
            self.assertEqual(link.resolve(),before)
    def test_missing_display_never_guesses_a_session(self):
        for env in ({},{'DISPLAY':':0'},{'DISPLAY':':33'},{'DISPLAY':'remote:1'},{'DISPLAY':':2','KINDRED_SCREEN_SLOT':'3'}):
            with self.assertRaises(RuntimeError):launcher.prepare(env,{},Path('/fixture'),'generation')
    def test_each_screen_generation_has_separate_state_and_verified_bus(self):
        env={'DISPLAY':':2','DBUS_SESSION_BUS_ADDRESS':'wrong','AT_SPI_BUS_ADDRESS':'wrong','WAYLAND_DISPLAY':'wrong',
             'AGENT_DESKTOP_HOME':'wrong','AGENT_DESKTOP_SESSION':'wrong'}
        context={'DBUS_SESSION_BUS_ADDRESS':'verified','XDG_RUNTIME_DIR':'private'}
        a=launcher.prepare(env,context,Path('/fixture'),'one');b=launcher.prepare(env,context,Path('/fixture'),'two')
        self.assertNotEqual(a['AGENT_DESKTOP_HOME'],b['AGENT_DESKTOP_HOME'])
        self.assertEqual(a['DBUS_SESSION_BUS_ADDRESS'],'verified');self.assertEqual(a['DISPLAY'],':2')
        for key in ('AT_SPI_BUS_ADDRESS','WAYLAND_DISPLAY','AGENT_DESKTOP_SESSION'):self.assertNotIn(key,a)
    def test_owned_ancestor_and_redirected_install_refused(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as folder:
            root=Path(folder);(root/'redirect').symlink_to(root/'actual')
            data,r=fixture()
            with self.assertRaises(ValueError):installer.install(data,r,root/'redirect')
    def test_real_archive_packaging_matches_source_provenance(self):
        path=os.environ.get('KINDRED_AGENT_DESKTOP_ARCHIVE')
        if not path:self.skipTest('Set KINDRED_AGENT_DESKTOP_ARCHIVE for pinned binary packaging check')
        manifest,data=packager.agent_desktop_payload(Path(path))
        self.assertEqual(manifest['archive_sha256'],hashlib.sha256(data).hexdigest())
        self.assertEqual(set(installer.verified(data,manifest)),{'agent-desktop','agent-desktop-linux-clipboard-owner'})

if __name__=='__main__':unittest.main()
