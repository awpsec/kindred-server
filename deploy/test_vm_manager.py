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

class PowerIntentRecovery(unittest.TestCase):
    def test_restore_isolated_preserves_stopped_and_retired(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            ids=['00000000-0000-4000-8000-'+str(n).zfill(12) for n in range(5)]
            for profile in ids:
                computer=root/profile/'computer';computer.mkdir(parents=True)
                (computer/'computer.json').write_text('{}')
                (computer/'disk.qcow2').write_bytes(b'preserve')
            (root/ids[1]/'computer'/'power-intent.json').write_text('{"running":false}')
            (root/'_retired').mkdir();(root/'_retired'/ids[2]).touch()
            (root/ids[4]/'computer'/'power-intent.json').write_text('{"running":true}')
            def start(computer,profile):
                if profile==ids[3]: raise ValueError('Not enough memory')
            with patch.object(manager,'ROOT',root),patch.object(manager,'start',side_effect=start) as launch:
                manager.restore_computers()
                self.assertEqual([call.args[1] for call in launch.call_args_list],[ids[0],ids[3],ids[4]])
            self.assertTrue(json.loads((root/ids[0]/'computer'/'power-intent.json').read_text())['running'])
            for profile in ids:self.assertEqual((root/profile/'computer'/'disk.qcow2').read_bytes(),b'preserve')

    def test_explicit_shutdown_persists_opt_out_and_start_reenables(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);profile='00000000-0000-4000-8000-000000000001'
            with patch.object(manager,'ROOT',root),patch.object(manager,'running',return_value=False):
                with patch.object(manager.sys,'argv',['manager','shutdown',profile]):manager.main()
                intent=root/profile/'computer'/'power-intent.json'
                self.assertFalse(json.loads(intent.read_text())['running'])
                with patch.object(manager.sys,'argv',['manager','start',profile]),patch.object(manager,'start',return_value={'cpus':2,'memory_mb':2048,'disk_gb':30}):manager.main()
                self.assertTrue(json.loads(intent.read_text())['running'])

class SoftwareCache(unittest.TestCase):
    def test_retirement_stops_computer_and_prevents_restart(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);profile='01234567-89ab-4cde-8fab-0123456789ab'
            with patch.object(manager,'ROOT',root),patch.object(manager.sys,'argv',['manager','retire',profile]),patch.object(manager,'running',side_effect=[True,False,False,False,False]),patch.object(manager,'qmp') as qmp:
                manager.main()
                qmp.assert_called_once_with(root/profile/'computer','system_powerdown')
                self.assertTrue((root/'_retired'/profile).exists())
                with self.assertRaisesRegex(ValueError,'removed account'):manager.directory(profile)
            with patch.object(manager,'ROOT',root),patch.object(manager.sys,'argv',['manager','retire',profile]),patch.object(manager,'running',return_value=False):
                manager.main() # Retrying deletion is safe.

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

class StartupRecovery(unittest.TestCase):
    def test_existing_disk_never_downloads_replacement_backing_image(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);(root/'disk.qcow2').write_bytes(b'existing overlay');(root/'seed.iso').write_bytes(b'existing seed');(root/'ssh_host_ed25519_key.pub').write_text('ssh-ed25519 fixture')
            with patch.object(manager,'base_image') as base,patch.object(manager,'software_image',return_value='software.iso'):
                self.assertEqual(manager.seed(root,'fixture',{'port':22000}),'software.iso')
                base.assert_not_called()
                self.assertEqual((root/'disk.qcow2').read_bytes(),b'existing overlay')
                self.assertEqual((root/'seed.iso').read_bytes(),b'existing seed')

    def test_running_vm_needs_no_install_media_after_update(self):
        root=Path('/fixture')
        settings={'id':'fixture','memory_mb':6144}
        with patch.object(manager,'prepare',return_value=settings),patch.object(manager,'qmp',return_value={'status':'running'}),patch.object(manager,'seed') as seed,patch.object(manager,'run') as run:
            self.assertEqual(manager.start(root,'fixture'),settings)
            seed.assert_not_called();run.assert_not_called()

    def test_faulted_qemu_still_owns_disk_and_cannot_be_started_twice(self):
        root=Path('/fixture')
        for state in ('io-error','paused','shutdown','guest-panicked','prelaunch'):
            with self.subTest(state=state),patch.object(manager,'qmp',return_value={'status':state}),patch.object(manager,'prepare',return_value={}),patch.object(manager,'seed') as seed,patch.object(manager,'run') as run:
                self.assertTrue(manager.running(root))
                with self.assertRaises(ValueError):manager.start(root,'fixture')
                with self.assertRaisesRegex(ValueError,'Shut down'):
                    manager.resize_resources(root,'fixture',{'cpus':2,'memory_mb':2048,'disk_gb':30})
                seed.assert_not_called();run.assert_not_called()

    def test_launch_errors_are_actionable_and_diagnostics_are_retained(self):
        errors=[(b'cannot allocate memory','memory'),(b'No space left on device','disk is full'),(b'Could not set up host forwarding rule','port is already in use'),(b'Failed to get write lock','disk is still in use'),(b'failed to initialize kvm','virtualization'),(b'unexpected fixture error /private/path','startup-error.log')]
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            for detail,message in errors:
                with self.subTest(detail=detail),patch.object(manager,'run',side_effect=subprocess.CalledProcessError(1,['qemu'],stderr=detail)):
                    with self.assertRaisesRegex(ValueError,message) as error:manager.launch(root,['qemu'])
                    self.assertEqual((root/'startup-error.log').read_text(),detail.decode())
                    self.assertNotIn('/private/path',str(error.exception))

    def test_exit_after_daemonize_is_not_a_success(self):
        with tempfile.TemporaryDirectory() as folder,patch.object(manager,'run'),patch.object(manager,'running',return_value=False):
            with self.assertRaisesRegex(ValueError,'exited during startup'):manager.launch(Path(folder),['qemu'])

    def test_success_clears_old_failure(self):
        with tempfile.TemporaryDirectory() as folder,patch.object(manager,'run'),patch.object(manager,'qmp',return_value={'status':'running'}):
            root=Path(folder);(root/'startup-error.log').write_text('old failure')
            manager.launch(root,['qemu']);self.assertFalse((root/'startup-error.log').exists())

@unittest.skipUnless(shutil.which('qemu-system-x86_64'), 'QEMU required')
class RealQemuStartup(unittest.TestCase):
    def test_retire_stops_real_qemu_and_rejects_new_starts(self):
        with tempfile.TemporaryDirectory() as folder:
            base=Path(folder);profile='01234567-89ab-4cde-8fab-0123456789ab'
            with patch.object(manager,'ROOT',base):
                root=manager.directory(profile)
                args=['qemu-system-x86_64','-machine','q35,accel=tcg','-m','64','-display','none','-nodefaults','-qmp',f'unix:{root}/monitor.sock,server=on,wait=off','-pidfile',str(root/'qemu.pid'),'-daemonize']
                clock=manager.time.monotonic;calls=[0]
                def fast_deadline():
                    calls[0]+=1
                    return clock()+(61 if calls[0]>1 else 0)
                try:
                    manager.launch(root,args)
                    with patch.object(manager.sys,'argv',['manager','retire',profile]),patch.object(manager.time,'monotonic',fast_deadline):manager.main()
                    self.assertFalse(manager.running(root))
                    with self.assertRaisesRegex(ValueError,'removed account'):manager.directory(profile)
                finally:
                    if manager.running(root):manager.qmp(root,'quit')

    def test_launch_pause_and_shutdown_keep_process_ownership(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            args=['qemu-system-x86_64','-machine','q35,accel=tcg','-m','64','-display','none','-nodefaults','-qmp',f'unix:{root}/monitor.sock,server=on,wait=off','-daemonize']
            try:
                manager.launch(root,args)
                self.assertTrue(manager.running(root))
                manager.qmp(root,'stop')
                self.assertTrue(manager.running(root))
                with self.assertRaisesRegex(ValueError,'paused'):manager.require_running(root)
                manager.qmp(root,'cont');manager.require_running(root)
            finally:
                if manager.running(root):manager.qmp(root,'quit')

    def test_occupied_forward_port_reports_actionable_failure(self):
        import socket
        with tempfile.TemporaryDirectory() as folder,socket.socket() as blocker:
            root=Path(folder);blocker.bind(('127.0.0.1',0));blocker.listen()
            port=blocker.getsockname()[1]
            args=['qemu-system-x86_64','-machine','q35,accel=tcg','-m','64','-display','none','-nodefaults','-netdev',f'user,id=network,hostfwd=tcp:127.0.0.1:{port}-:22','-qmp',f'unix:{root}/monitor.sock,server=on,wait=off','-daemonize']
            try:
                with self.assertRaisesRegex(ValueError,'port is already in use'):manager.launch(root,args)
                self.assertIn('forward', (root/'startup-error.log').read_text().lower())
            finally:
                if manager.running(root):manager.qmp(root,'quit')

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



class ResourceChanges(unittest.TestCase):
    @unittest.skipUnless(shutil.which('qemu-img'), 'qemu-img required')
    def test_real_disk_growth_and_integrity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            subprocess.run(['qemu-img','create','-f','qcow2',str(root/'disk.qcow2'),'8G'],check=True,capture_output=True)
            (root/'computer.json').write_text(json.dumps({'cpus':2,'memory_mb':2048,'disk_gb':8,'id':'fixture','port':22000}))
            with patch.object(manager,'running',return_value=False):
                result=manager.resize_resources(root,'fixture',{'cpus':3,'memory_mb':4096,'disk_gb':10})
                self.assertEqual(result['resources']['disk_gb'],10)
            subprocess.run(['qemu-img','check',str(root/'disk.qcow2')],check=True,capture_output=True)

    def test_stopped_vm_expansion_preserves_identity_and_disk(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);disk=root/'disk.qcow2';disk.write_bytes(b'existing disk')
            original={'cpus':2,'memory_mb':6144,'disk_gb':30,'port':22000,'id':'fixture'}
            (root/'computer.json').write_text(json.dumps(original));size=30
            def command(args):
                nonlocal size
                if args[1]=='resize': size=int(args[-1][:-1])
                return Mock(stdout=json.dumps({'virtual-size':size*1024**3}).encode())
            with patch.object(manager,'running',return_value=False),patch.object(manager,'run',side_effect=command):
                result=manager.resize_resources(root,'fixture',{'cpus':4,'memory_mb':8192,'disk_gb':40})
                self.assertEqual(result['resources'],{'cpus':4,'memory_mb':8192,'disk_gb':40})
                saved=json.loads((root/'computer.json').read_text());self.assertEqual(saved['port'],22000);self.assertEqual(saved['id'],'fixture')
                self.assertEqual(disk.read_bytes(),b'existing disk')
                for invalid in [{'cpus':True,'memory_mb':8192,'disk_gb':40},{'cpus':4,'memory_mb':8192,'disk_gb':29},{'cpus':33,'memory_mb':8192,'disk_gb':40}]:
                    with self.assertRaises(ValueError): manager.resize_resources(root,'fixture',invalid)
                # An interrupted metadata write must never allow a disk shrink.
                size=45
                with self.assertRaisesRegex(ValueError,'only be increased'): manager.resize_resources(root,'fixture',{'cpus':4,'memory_mb':8192,'disk_gb':40})
                self.assertEqual(json.loads((root/'computer.json').read_text())['disk_gb'],45)
            with patch.object(manager,'running',return_value=True),patch.object(manager,'run') as run:
                with self.assertRaisesRegex(ValueError,'Shut down'):manager.resize_resources(root,'fixture',{'cpus':4,'memory_mb':8192,'disk_gb':50})
                run.assert_not_called()

if __name__ == "__main__":
    unittest.main()
