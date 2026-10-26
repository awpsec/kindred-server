"""Setup failure diagnostics and computer-only recovery without real VMs or network."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('manager',Path(__file__).with_name('vm-manager.py'))
manager=importlib.util.module_from_spec(spec);spec.loader.exec_module(manager)


class SetupRecovery(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.profile='00000000-0000-4000-8000-000000000001'
        self.computer=self.root/self.profile/'computer';self.computer.mkdir(parents=True)
        self.values={'cpus':2,'memory_mb':2048,'disk_gb':16}
        (self.computer/'computer.json').write_text(json.dumps({'id':self.profile,'port':22000,**self.values}))
        (self.computer/'disk.qcow2').write_bytes(b'important guest data')
        (self.computer/'ssh_client').write_bytes(b'old private guest key')
        (self.root/self.profile/'workspace.db').write_bytes(b'keep chats')
        self.addCleanup(patch.stopall)
        patch.object(manager,'ROOT',self.root).start()
        patch.object(manager,'running',return_value=False).start()
        patch.object(manager,'resource_capacity',return_value={'cpus':4,'memory_budget_mb':4096,'memory_available_mb':4608,'reserved_memory_mb':512}).start()

    def recreate(self,**changes):
        payload={'confirmation':'RECREATE','resources':self.values}|changes
        with manager.locked(self.root/'_operations'/self.profile):
            return manager.recreate_computer(self.computer,self.profile,payload)

    def test_recreation_archives_guest_bytes_and_keeps_workspace(self):
        inode=(self.computer/'disk.qcow2').stat().st_ino
        receipt=self.recreate()
        backup=self.root/'_computer-backups'/self.profile/receipt['archive_id']
        self.assertEqual((backup/'computer/disk.qcow2').stat().st_ino,inode)
        self.assertEqual((backup/'computer/ssh_client').read_bytes(),b'old private guest key')
        self.assertEqual((self.root/self.profile/'workspace.db').read_bytes(),b'keep chats')
        self.assertFalse((self.computer/'disk.qcow2').exists())
        self.assertFalse(json.loads((self.computer/'power-intent.json').read_text())['running'])
        self.assertEqual(json.loads((self.computer/'resource-overrides.json').read_text()),self.values)
        self.assertEqual(self.computer.stat().st_mode&0o777,0o700)
        with self.assertRaisesRegex(ValueError,'provisioned'):self.recreate()
        self.assertEqual(len(list((backup.parent).iterdir())),1)

    def test_running_unknown_pid_and_redirected_metadata_refuse_before_archive(self):
        with patch.object(manager,'running',return_value=True):
            with self.assertRaisesRegex(ValueError,'Shut down'):self.recreate()
        (self.computer/'qemu.pid').write_text(str(os.getpid()))
        with self.assertRaisesRegex(ValueError,'not exited'):self.recreate()
        (self.computer/'qemu.pid').unlink()
        metadata=self.computer/'computer.json';metadata.rename(self.computer/'original.json');metadata.symlink_to('original.json')
        with self.assertRaisesRegex(ValueError,'provisioned'):self.recreate()
        self.assertFalse((self.root/'_computer-backups').exists())

    def test_cross_profile_symlink_is_rejected(self):
        other='00000000-0000-4000-8000-000000000002'
        (self.root/other).symlink_to(self.root/self.profile,target_is_directory=True)
        with self.assertRaisesRegex(ValueError,'directory'):manager.directory(other)

    def test_confirmation_resources_and_identity_fail_closed(self):
        for changes in ({'confirmation':'yes'},{'resources':self.values|{'disk_gb':0}},{'resources':self.values|{'cpus':True}},{'resources':self.values|{'memory_mb':8192}}):
            with self.assertRaises(ValueError):self.recreate(**changes)
        (self.computer/'computer.json').write_text('{"id":"another profile"}')
        with self.assertRaisesRegex(ValueError,'identity'):self.recreate()
        self.assertFalse((self.root/'_computer-backups').exists())

    def test_preparation_failure_preserves_original_and_key(self):
        with patch.object(manager,'atomic',side_effect=OSError('disk full')):
            with self.assertRaisesRegex(OSError,'disk full'):self.recreate()
        self.assertEqual((self.computer/'disk.qcow2').read_bytes(),b'important guest data')
        self.assertEqual((self.computer/'ssh_client').read_bytes(),b'old private guest key')

    def test_switch_failure_restores_exact_old_directory(self):
        rename=os.rename
        def fail_fresh(source,target):
            if Path(source).name=='fresh':raise OSError('injected switch failure')
            return rename(source,target)
        with patch.object(manager.os,'rename',side_effect=fail_fresh):
            with self.assertRaisesRegex(OSError,'switch failure'):self.recreate()
        self.assertEqual((self.computer/'disk.qcow2').read_bytes(),b'important guest data')
        self.assertEqual((self.computer/'ssh_client').read_bytes(),b'old private guest key')
        self.assertFalse((self.computer/'resource-overrides.json').exists())

    def test_retired_computer_and_redirected_archive_refuse(self):
        retired=self.root/'_retired';retired.mkdir();(retired/self.profile).touch()
        with self.assertRaisesRegex(ValueError,'directory'):self.recreate()
        (retired/self.profile).unlink()
        (self.root/'_computer-backups').symlink_to(self.root/self.profile)
        with self.assertRaisesRegex(ValueError,'archive directory'):self.recreate()

    def test_cli_receipt_and_no_image_download(self):
        output=io.StringIO()
        with patch.object(manager.sys,'argv',['manager','recreate',self.profile]),patch.object(manager.sys,'stdin',io.StringIO(json.dumps({'confirmation':'RECREATE','resources':self.values}))),patch.object(manager,'base_image') as download,contextlib.redirect_stdout(output):
            manager.main();download.assert_not_called()
        self.assertFalse(json.loads(output.getvalue())['provisioned'])

    def test_busy_startup_refuses_immediately_instead_of_recreating_later(self):
        with manager.locked(self.root/'_operations'/self.profile),patch.object(manager.sys,'argv',['manager','recreate',self.profile]):
            with self.assertRaisesRegex(ValueError,'operation is in progress'):manager.main()
        self.assertEqual((self.computer/'disk.qcow2').read_bytes(),b'important guest data')

    def test_failed_setup_without_a_disk_can_change_initial_disk_size(self):
        (self.computer/'disk.qcow2').unlink()
        with patch.object(manager,'base_image') as download:
            result=manager.resize_resources(self.computer,self.profile,self.values|{'disk_gb':8})
            download.assert_not_called()
        self.assertFalse(result['provisioned'])
        self.assertEqual(result['resources']['disk_gb'],8)
        self.assertEqual(json.loads((self.computer/'computer.json').read_text())['port'],22000)

    def test_kvm_missing_permission_and_auto_fallback_are_distinct(self):
        with patch.dict(os.environ,{'KINDRED_VM_ACCEL':'auto'}),patch.object(manager.os,'open',side_effect=FileNotFoundError()):
            diagnostic=manager.acceleration();self.assertEqual(diagnostic['effective'],'tcg');self.assertIn('not exposed',diagnostic['message'])
        with patch.dict(os.environ,{'KINDRED_VM_ACCEL':'kvm'}),patch.object(manager.os,'open',side_effect=PermissionError()):
            self.assertIsNone(manager.acceleration()['effective'])
            with patch.object(manager,'prepare') as prepare:
                with self.assertRaisesRegex(ValueError,'KINDRED_KVM_GID'):manager.start(self.computer,self.profile)
                prepare.assert_not_called()
        with patch.dict(os.environ,{'KINDRED_VM_ACCEL':'tcg'}),patch.object(manager.os,'open',return_value=123),patch.object(manager.os,'close'):
            self.assertEqual(manager.acceleration()['effective'],'tcg')

    def test_checksum_network_failure_explains_container_egress(self):
        with patch.object(manager,'CACHE',self.root/'images'),patch.object(manager.urllib.request,'urlopen',side_effect=urllib.error.URLError('private details')):
            with self.assertRaisesRegex(ValueError,'outbound TCP/443') as error:manager.base_image()
        self.assertNotIn('private details',str(error.exception))
        self.assertEqual((self.computer/'disk.qcow2').read_bytes(),b'important guest data')

    def test_partial_image_failure_removes_only_partial_download(self):
        response=io.BytesIO(('0'*128+'  '+manager.IMAGE+'\n').encode())
        def fail(url,path,limit):path.write_bytes(b'partial');raise TimeoutError('private URL')
        with patch.object(manager,'CACHE',self.root/'images'),patch.object(manager.urllib.request,'urlopen',return_value=response),patch.object(manager,'download',side_effect=fail):
            with self.assertRaisesRegex(ValueError,'incomplete download'):manager.base_image()
        self.assertEqual(list((self.root/'images').glob('*.download')),[])
        self.assertTrue((self.root/'images/download.lock').exists())


if __name__=='__main__':unittest.main()
