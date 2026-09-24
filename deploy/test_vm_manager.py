"""Checks for guest software upgrades, without launching or modifying any VM."""
import importlib.util
from pathlib import Path
import tempfile
import json
import os
import subprocess
import contextlib
import shutil
import test_download_codex as fixture
import unittest
from unittest.mock import patch, Mock
spec=importlib.util.spec_from_file_location('vm_manager',Path(__file__).with_name('vm-manager.py'))
manager=importlib.util.module_from_spec(spec);spec.loader.exec_module(manager)
def software_fixture(root):
    software=root/'software';software.mkdir()
    archive=root/'package.tar.gz';fixture.CodexPackage().archive(archive)
    fixture.package.extract(archive,software/'runtime')
    (software/'codex').symlink_to('runtime/bin/codex')
    (software/'kindred').write_bytes(b'server')
    (software/'install-guest.sh').write_bytes(b'old policy')
    shutil.copy(Path(__file__).with_name('check-codex.py'),software/'check-codex.py')
    return software

class SoftwareCache(unittest.TestCase):
    def test_inaccessible_control_socket_is_not_reported_as_stopped(self):
        with patch.object(manager,'qmp',side_effect=PermissionError('denied')):
            with self.assertRaisesRegex(ValueError,'service account'):
                manager.running(Path('/fixture'))
        with patch.object(manager,'qmp',side_effect=FileNotFoundError('absent')):
            self.assertFalse(manager.running(Path('/fixture')))

    def test_updated_software_gets_new_media_without_replacing_attached_media(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);software=software_fixture(root);cache=root/'cache'
            calls=[]
            def image(args):calls.append(args);Path(args[3]).write_bytes((software/'install-guest.sh').read_bytes())
            with patch.object(manager,'SOFTWARE',software),patch.object(manager,'CACHE',cache),patch.object(manager,'run',image):
                old=manager.software_image();self.assertEqual(manager.software_image(),old);self.assertEqual(len(calls),1)
                (software/'install-guest.sh').write_bytes(b'new policy')
                new=manager.software_image();self.assertNotEqual(old,new);self.assertEqual(old.read_bytes(),b'old policy');self.assertEqual(new.read_bytes(),b'new policy');self.assertEqual(len(calls),2)
                self.assertEqual(manager.software_image(),new)
    def test_missing_or_changing_bundle_cannot_publish_media(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);software=root/'software';software.mkdir();cache=root/'cache'
            with patch.object(manager,'SOFTWARE',software),patch.object(manager,'CACHE',cache):
                with self.assertRaisesRegex(ValueError,'missing'):manager.software_image()
                (software/'kindred').write_bytes(b'server');(software/'codex').write_bytes(b'provider')
                with self.assertRaisesRegex(ValueError,'checker is missing'):manager.software_image()
                shutil.rmtree(software);software_fixture(root)
                helper=software/'runtime/bin/codex-code-mode-host';saved=helper.read_bytes();helper.unlink()
                with self.assertRaisesRegex(ValueError,'codex-code-mode-host'):manager.software_image()
                helper.write_bytes(saved);helper.chmod(0o755)
                def image(args):Path(args[3]).write_bytes(b'partial');(software/'kindred').write_bytes(b'changed')
                with patch.object(manager,'run',image),self.assertRaisesRegex(ValueError,'changed during'):manager.software_image()
                self.assertFalse(any(p for p in cache.glob('software-*.iso') if not p.name.endswith('.creating.iso')))

class ResourcePersistence(unittest.TestCase):
    def test_existing_computer_keeps_allocation_when_service_defaults_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            settings={'id':'fixture','port':22000,'cpus':4,'memory_mb':8192,'disk_gb':75,'created_at':1}
            (root/'computer.json').write_text(json.dumps(settings))
            with patch.dict(os.environ,{'KINDRED_VM_CPUS':'2','KINDRED_VM_MEMORY_MB':'6144','KINDRED_VM_DISK_GB':'30'}),patch.object(manager,'base_image') as base,patch.object(manager,'run') as run:
                self.assertEqual(manager.prepare(root,'fixture'),settings)
                base.assert_not_called();run.assert_not_called()
                self.assertEqual(json.loads((root/'computer.json').read_text()),settings)

    def test_new_computer_persists_configured_service_defaults(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            with patch.dict(os.environ,{'KINDRED_VM_CPUS':'4','KINDRED_VM_MEMORY_MB':'8192','KINDRED_VM_DISK_GB':'75'}),patch.object(manager,'ROOT',root),patch.object(manager,'base_image',return_value=root/'base'),patch.object(manager,'run'),patch.object(manager,'locked',return_value=contextlib.nullcontext()):
                saved=manager.prepare(root,'fixture')
                self.assertEqual((saved['cpus'],saved['memory_mb'],saved['disk_gb']),(4,8192,75))
                self.assertEqual(json.loads((root/'computer.json').read_text()),saved)

class ConnectionStatus(unittest.TestCase):
    def test_codex_health_does_not_block_starting_an_installed_shared_guest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            with patch.object(manager.sys,'argv',['manager','ensure','fixture']), \
                 patch.object(manager,'directory',return_value=root), \
                 patch.object(manager,'locked',return_value=contextlib.nullcontext()), \
                 patch.object(manager,'start',return_value={'cpus':2,'memory_mb':6144,'disk_gb':30}), \
                 patch.object(manager,'connection_status',return_value='runtime_failed') as status, \
                 patch.object(manager,'sync_provider_helper') as sync, \
                 patch.object(manager.time,'sleep',side_effect=AssertionError('Must not wait on a provider-specific failure')), \
                 patch('builtins.print') as output:
                manager.main()
                status.assert_called_once_with(root)
                sync.assert_called_once_with(root)
                self.assertIn('"state": "running"',output.call_args.args[0])

    def test_failed_first_boot_stops_login_wait_without_reprovisioning(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            settings={'cpus':2,'memory_mb':6144,'disk_gb':30}
            with patch.object(manager.sys,'argv',['manager','ensure','fixture']), \
                 patch.object(manager,'directory',return_value=root), \
                 patch.object(manager,'locked',return_value=contextlib.nullcontext()), \
                 patch.object(manager,'start',return_value=settings) as start, \
                 patch.object(manager,'connection_status',return_value='failed'), \
                 patch.object(manager.time,'sleep') as sleep:
                with self.assertRaisesRegex(ValueError,'software setup failed'):manager.main()
                start.assert_called_once_with(root,'fixture');sleep.assert_not_called()

    def test_ssh_configuration_does_not_imply_finished_installation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            self.enterContext(patch.object(manager,'SOFTWARE',software_fixture(root)))
            with patch.object(manager,'running',return_value=True),patch.object(manager.subprocess,'run') as probe:
                self.assertEqual(manager.connection_status(root),'not_started');probe.assert_not_called()
                (root/'ssh_config').write_text('fixture')
                for phase in ('installing','ready','failed','runtime_failed'):
                    probe.return_value=Mock(stdout=(phase+'\n').encode())
                    self.assertEqual(manager.connection_status(root),phase)
                    self.assertEqual(probe.call_args.kwargs['timeout'],5)
                    command=probe.call_args.args[0][-1]
                    self.assertIn('/var/lib/kindred-ready',command)
                    self.assertIn('/var/lib/cloud/instance/boot-finished',command)
                for error in (subprocess.TimeoutExpired('ssh',5),subprocess.CalledProcessError(255,'ssh')):
                    probe.side_effect=error
                    self.assertEqual(manager.connection_status(root),'starting')
            with patch.object(manager,'running',return_value=False),patch.object(manager.subprocess,'run') as probe:
                self.assertEqual(manager.connection_status(root),'stopped');probe.assert_not_called()

    def test_old_ready_marker_cannot_mask_missing_tool_runtime(self):
        # Execute the exact remote shell probe locally, with only its fixed guest
        # paths redirected to an isolated fixture. No SSH or VM is started.
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);software=software_fixture(root)
            (root/'ssh_config').write_text('fixture');(root/'ready').touch()
            actual_run=subprocess.run
            def ssh(args,**kwargs):
                command=args[-1].replace('/var/lib/kindred-ready',str(root/'ready')).replace('/usr/local/bin/codex',str(software/'codex'))
                return actual_run(['sh','-c',command],**kwargs)
            with patch.object(manager,'SOFTWARE',software),patch.object(manager,'running',return_value=True),patch.object(manager.subprocess,'run',ssh):
                self.assertEqual(manager.connection_status(root),'ready')
                (software/'runtime/bin/codex-code-mode-host').unlink()
                self.assertEqual(manager.connection_status(root),'runtime_failed')

class MemoryCapacity(unittest.TestCase):
    def test_native_service_and_parent_limits_constrain_host_available_memory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);mount=root/'cgroup';service=mount/'system.slice/kindred.service';service.mkdir(parents=True)
            meminfo=root/'meminfo';meminfo.write_text('MemAvailable: 33554432 kB\n')
            membership=root/'membership';membership.write_text('0::/system.slice/kindred.service\n')
            (service/'memory.max').write_text(str(128*1024**2));(service/'memory.current').write_text(str(96*1024**2))
            self.assertEqual(manager.memory_available(meminfo,mount,membership),32*1024**2)
            (service/'memory.max').write_text(str(6*1024**3));(service/'memory.current').write_text(str(1024**3))
            (service.parent/'memory.max').write_text(str(10*1024**3));(service.parent/'memory.current').write_text(str(8*1024**3))
            self.assertEqual(manager.memory_available(meminfo,mount,membership),2*1024**3)

    def test_container_mount_limits_and_exhausted_budget_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);mount=root/'cgroup';mount.mkdir()
            meminfo=root/'meminfo';meminfo.write_text('MemAvailable: 33554432 kB\n')
            membership=root/'membership'
            (mount/'memory.max').write_text(str(8*1024**3));(mount/'memory.current').write_text(str(3*1024**3))
            for path in ('/','/host/container/not/exposed','/../../outside'):
                membership.write_text('0::'+path+'\n')
                self.assertEqual(manager.memory_available(meminfo,mount,membership),5*1024**3)
            (mount/'memory.current').write_text(str(9*1024**3))
            self.assertEqual(manager.memory_available(meminfo,mount,membership),0)

    def test_insufficient_service_memory_never_launches_qemu(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            with patch.object(manager,'ROOT',root), \
                 patch.object(manager,'prepare',return_value={'memory_mb':6144}), \
                 patch.object(manager,'seed'),patch.object(manager,'running',return_value=False), \
                 patch.object(manager,'memory_available',return_value=128*1024**2), \
                 patch.object(manager,'run') as launch:
                with self.assertRaisesRegex(ValueError,'service/container memory limit'):manager.start(root,'fixture')
                launch.assert_not_called()


class ProviderBridgeUpgrade(unittest.TestCase):
    def test_upgrade_is_atomic_idempotent_and_preserves_other_files(self):
        import sys,hashlib
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);target=root/'provider-cli.py';target.write_text('old = True\n')
            credentials=root/'credentials';credentials.write_text('private fixture')
            source='unlimited = True\n'
            payload={'source':source,'sha256':hashlib.sha256(source.encode()).hexdigest()}
            def update(value):
                return subprocess.run([sys.executable,'-c',manager.PROVIDER_HELPER_UPDATE,str(root)],input=json.dumps(value),text=True,capture_output=True)
            self.assertEqual(json.loads(update(payload).stdout),{'updated':True})
            self.assertEqual(target.read_text(),source)
            self.assertEqual(len(list(root.glob('provider-cli.py.backup-*'))),1)
            self.assertEqual(next(root.glob('provider-cli.py.backup-*')).read_text(),'old = True\n')
            self.assertEqual(json.loads(update(payload).stdout),{'updated':False})
            self.assertEqual(credentials.read_text(),'private fixture')
            self.assertNotEqual(update(dict(payload,sha256='invalid')).returncode,0)
            self.assertEqual(target.read_text(),source)
            broken='this is invalid python !!!'
            self.assertNotEqual(update({'source':broken,'sha256':hashlib.sha256(broken.encode()).hexdigest()}).returncode,0)
            self.assertEqual(target.read_text(),source)

if __name__=='__main__':unittest.main()
