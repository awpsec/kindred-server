"""Disposable ownership/context regressions; no browser or owner state."""
import importlib.util
import os
from pathlib import Path
import shutil
import socket
import tempfile
import unittest
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('launch', ROOT/'deploy/browser-launch.py')
launch = importlib.util.module_from_spec(spec); spec.loader.exec_module(launch)

class LaunchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='kindred-launch-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.proc = self.root/'proc'; self.proc.mkdir()
        self.runtime = self.root/'runtime'; self.runtime.mkdir(mode=0o700)
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.bind(str(self.runtime/'bus')); self.socket.listen(32)
        self.addCleanup(self.socket.close)
        self.env = {'DISPLAY': ':2', 'XDG_RUNTIME_DIR': str(self.runtime), 'DBUS_SESSION_BUS_ADDRESS': 'unix:path='+str(self.runtime/'bus')}
    def desktop(self, pid='101', display=':2', env=None, executable=None):
        p=self.proc/pid; p.mkdir()
        (p/'exe').symlink_to(executable or shutil.which('openbox'))
        (p/'stat').write_text(pid+' (openbox) '+ ' '.join(['S']+['0']*18+['123']+['0']*5))
        context=dict(self.env if env is None else env, DISPLAY=display)
        (p/'environ').write_bytes(b'\0'.join((k+'='+v).encode() for k,v in context.items()))
    def prepare(self, screen=2, **kwargs):
        return launch.prepare(screen, root=self.root/'profiles', environ={}, proc=self.proc, **kwargs)
    def test_ssh_uses_exact_desktop_context_and_owned_profile(self):
        self.desktop(); result=self.prepare()
        self.assertEqual({k:result['env'][k] for k in self.env}, self.env)
        self.assertEqual(result['env']['XDG_CURRENT_DESKTOP'],'')
        self.assertIn('--user-data-dir='+str(self.root/'profiles/browser-2'), result['args'])
        self.assertNotIn('--password-store=basic',result['args'])
        self.assertEqual((self.root/'profiles/browser-2').stat().st_mode & 0o777,0o700)
    def test_desktop_markers_replace_unrelated_caller_detection(self):
        env=dict(self.env,XDG_CURRENT_DESKTOP='OPENBOX',DESKTOP_SESSION='openbox')
        self.desktop(env=env)
        result=launch.prepare(2,root=self.root/'profiles',proc=self.proc,environ={'XDG_CURRENT_DESKTOP':'GNOME','KDE_FULL_SESSION':'true'})
        self.assertEqual(result['env']['XDG_CURRENT_DESKTOP'],'OPENBOX');self.assertEqual(result['env']['KDE_FULL_SESSION'],'')
    def test_foreign_uid_context_refused_before_effect(self):
        self.desktop()
        with self.assertRaises(launch.BrowserContextError):launch.desktop_context(':2',{},self.proc,uid=os.getuid()+1)
        self.assertFalse((self.root/'profiles').exists())
    def test_abstract_bus_requires_live_matching_peer_uid(self):
        abstract=socket.socket(socket.AF_UNIX);name='kindred-bus-'+Path(self.temp.name).name
        abstract.bind('\0'+name);abstract.listen(10)
        try:
            env=dict(self.env,DBUS_SESSION_BUS_ADDRESS='unix:abstract='+name)
            self.assertEqual(launch.bus_context(env,os.getuid())['DBUS_SESSION_BUS_ADDRESS'],env['DBUS_SESSION_BUS_ADDRESS'])
            with self.assertRaises(launch.BrowserContextError):launch.bus_context(env,os.getuid()+1)
        finally:abstract.close()
        with self.assertRaises(launch.BrowserContextError):launch.bus_context(env,os.getuid())
    def test_foreign_display_does_not_launch_or_create_profile(self):
        self.desktop(display=':3')
        with self.assertRaisesRegex(launch.BrowserContextError,'unavailable'): self.prepare()
        self.assertFalse((self.root/'profiles').exists())
    def test_same_uid_abstract_bus_cannot_supply_another_display(self):
        abstract=socket.socket(socket.AF_UNIX);name='kindred-screen-'+Path(self.temp.name).name
        abstract.bind('\0'+name);abstract.listen(10)
        try:
            self.desktop(display=':3',env=dict(self.env,DBUS_SESSION_BUS_ADDRESS='unix:abstract='+name))
            with self.assertRaisesRegex(launch.BrowserContextError,'unavailable'):self.prepare(2)
            self.assertFalse((self.root/'profiles').exists())
            selected=self.prepare(3)
            self.assertEqual(selected['env']['DISPLAY'],':3')
            self.assertEqual(selected['env']['DBUS_SESSION_BUS_ADDRESS'],'unix:abstract='+name)
        finally:abstract.close()
    def test_foreign_process_identity_refused(self):
        self.desktop(executable=shutil.which('python3'))
        with self.assertRaises(launch.BrowserContextError): self.prepare()
    def test_ambiguous_desktop_refused(self):
        self.desktop(); second=dict(self.env); other=self.root/'runtime-other';other.mkdir(mode=0o700);second['XDG_RUNTIME_DIR']=str(other); self.desktop('102',env=second)
        with self.assertRaisesRegex(launch.BrowserContextError,'ambiguous'): self.prepare()
    def test_missing_and_stale_bus_refused(self):
        self.desktop(); self.socket.close()
        with self.assertRaisesRegex(launch.BrowserContextError,'bus is unavailable'): self.prepare()
        self.assertFalse((self.root/'profiles').exists())
    def test_nonprivate_runtime_refused(self):
        self.desktop(); self.runtime.chmod(0o755)
        with self.assertRaises(launch.BrowserContextError): self.prepare()
    def test_profile_symlink_refused_without_target_change(self):
        self.desktop(); root=self.root/'profiles';root.mkdir(mode=0o700);target=self.root/'other';target.mkdir();(root/'browser-2').symlink_to(target)
        with self.assertRaises(launch.BrowserContextError): self.prepare()
        self.assertEqual(list(target.iterdir()),[])
    def test_existing_profile_permissions_and_content_preserved(self):
        self.desktop(); result=self.prepare(); profile=self.root/'profiles/browser-2'; marker=profile/'marker';marker.write_text('synthetic');before=profile.stat().st_ino
        self.prepare(mode='new');self.assertEqual(profile.stat().st_ino,before);self.assertEqual(marker.read_text(),'synthetic')
        profile.chmod(0o755)
        with self.assertRaises(launch.BrowserContextError): self.prepare()
        self.assertEqual(profile.stat().st_mode & 0o777,0o755)
    def test_separate_screens_profiles_and_startup_same_policy(self):
        self.desktop();self.desktop('102',display=':3');a=self.prepare();b=self.prepare(3)
        self.assertNotEqual(a['args'][2],b['args'][2])
        self.desktop('103',display=':1');startup=launch.prepare(1, root=self.root/'profiles', proc=self.root/'proc', environ=dict(self.env,DISPLAY=':1',KINDRED_DESKTOP_STARTING='1'))
        self.assertEqual(a['args'][:2],startup['args'][:2]);self.assertEqual(startup['args'][2],'--user-data-dir='+str(self.root/'profiles/browser'))
    def test_url_validation_never_includes_secret_in_error(self):
        for url in ['http://example.test','https://user:secret@example.test','https://[','https://example.test/'+('x'*2000)]:
            with self.assertRaises(launch.BrowserContextError) as error: launch.browser_arguments('fixture','url',url)
            self.assertNotIn('secret',str(error.exception))
        self.assertEqual(launch.browser_arguments('fixture','url','https://example.test')[-1],'https://example.test')
    def test_all_entrypoints_bind_shared_policy(self):
        self.assertIn('include_str!("../deploy/browser-launch.py")',(ROOT/'src/guest.rs').read_text())
        for filename in ['start-desktop.sh','desktop-launch','install-guest.sh']:
            self.assertIn('browser-launch.py',(ROOT/'deploy'/filename).read_text())

if __name__ == '__main__': unittest.main()
