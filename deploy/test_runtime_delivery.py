import importlib.util
import io
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('updater',Path(__file__).with_name('update-guest-runtime.py'))
u=importlib.util.module_from_spec(spec);spec.loader.exec_module(u)

def binary():
    return b'#!/usr/bin/python3\nimport sys\nprint("kindred test" if sys.argv[1]=="--version" else \'{"commands":[]}\')\n'

class Delivery(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.target=self.root/'kindred-bin';self.target.write_bytes(b'old runtime');self.target.chmod(0o755)
        self.data=binary();self.digest=hashlib.sha256(self.data).hexdigest();self.job='11111111-1111-4111-8111-111111111111'
        self.payload={name:{'source':'new '+name,'previous_sha256':[None]} for name in ('browser-launch.py','desktop-launch','start-desktop.sh','ensure-screen','screen-ownership.py','wait-screen-ready')}
        self.hashes={k:hashlib.sha256(v['source'].encode()).hexdigest() for k,v in self.payload.items()}
    def call(self,operation='install'):
        return u.delivery(operation,self.job,io.BytesIO(self.data),self.target,self.digest,'test',self.payload,package_root=self.root/'maintenance')
    def test_success_journal_and_status_preserve_predecessor(self):
        receipt=self.call();self.assertEqual(receipt['phase'],'completed');self.assertEqual(receipt['sha256'],self.digest)
        self.assertEqual(self.call('status'),receipt)
        record=json.loads((self.root/'.kindred-runtime-operations'/self.job/'journal.json').read_text())
        self.assertEqual(record['previous']['kindred-bin']['sha256'],hashlib.sha256(b'old runtime').hexdigest())
        self.assertEqual((self.root/'.kindred-runtime-operations'/self.job/'previous'/'kindred-bin').read_bytes(),b'old runtime')
        self.assertEqual(self.call(),receipt)
    def test_status_before_delayed_install_fences_id(self):
        self.assertEqual(self.call('status')['phase'],'failed');self.assertEqual(self.call()['phase'],'failed');self.assertEqual(self.target.read_bytes(),b'old runtime')
    def test_repaired_helper_and_before_backup_are_untouched(self):
        self.payload['wait-screen-ready']['source']=Path(__file__).with_name('wait-screen-ready').read_text()
        helper=self.root/'wait-screen-ready';helper.write_text(self.payload['wait-screen-ready']['source']);helper.chmod(0o755)
        self.assertEqual(hashlib.sha256(helper.read_bytes()).hexdigest(),'0c7258b71126fdd3904dd723dbd09aff75eadb735feb47a9a9562864dc45c701')
        (self.root/'wait-screen-ready.previous').write_bytes(b'STALE_PREVIOUS')
        backup=self.root/'wait-screen-ready.before-20261016';backup.write_bytes(b'RETAIN_REPAIR_BACKUP');before=helper.stat()
        self.assertEqual(self.call()['phase'],'completed');self.assertEqual(helper.stat().st_ino,before.st_ino);self.assertEqual(backup.read_bytes(),b'RETAIN_REPAIR_BACKUP');self.assertEqual((self.root/'wait-screen-ready.previous').read_bytes(),b'STALE_PREVIOUS')
    def test_foreign_target_cannot_reuse_id(self):
        self.call()
        with self.assertRaises(Exception):u.delivery('status',self.job,None,self.target,'a'*64,'test',self.payload,package_root=self.root/'maintenance')
    def test_partial_switch_stays_checking(self):
        self.call();self.target.write_bytes(b'unknown partial')
        self.assertEqual(self.call('status')['phase'],'checking')
        self.assertEqual(self.call()['phase'],'checking')
        self.assertEqual(self.target.read_bytes(),b'unknown partial')
    def test_lost_output_after_switch_reconciles_without_redispatch(self):
        self.call();journal=self.root/'.kindred-runtime-operations'/self.job/'journal.json';record=json.loads(journal.read_text());record['phase']='installing';record.pop('receipt',None);journal.write_text(json.dumps(record))
        with patch.object(u,'_install',side_effect=AssertionError('must not dispatch again')):self.assertEqual(self.call()['phase'],'completed')
    def test_held_lock_never_reports_settled(self):
        import fcntl
        with (self.root/'.kindred-runtime.lock').open('a+b') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(Exception):self.call('status')
        self.assertEqual(self.target.read_bytes(),b'old runtime')
    def test_switch_failure_restores_predecessor_and_closes_operation(self):
        original=u.os.replace
        def replace(a,b):
            if Path(b)==self.target:raise OSError('owned switch failure')
            return original(a,b)
        with patch.object(u.os,'replace',side_effect=replace):self.assertEqual(self.call()['phase'],'failed')
        self.assertEqual(self.target.read_bytes(),b'old runtime')
        self.assertEqual(self.call()['phase'],'failed')
    def test_active_package_worker_defers_without_starting_any_optional_process(self):
        import fcntl
        directory=self.root/'maintenance';directory.mkdir(mode=0o700)
        with (directory/'upgrade.lock').open('a+b') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(Exception):self.call()
        self.assertFalse((self.root/'.kindred-runtime-operations').exists())
        self.assertEqual(self.target.read_bytes(),b'old runtime')

    def test_validation_failures_never_change_old_runtime_or_custom_helpers(self):
        for variant in ('hash','version','rpc','custom','missing'):
            with self.subTest(variant=variant),tempfile.TemporaryDirectory() as temp:
                root=Path(temp);target=root/'kindred-bin';target.write_bytes(b'original');target.chmod(0o755)
                payload={n:dict(v) for n,v in self.payload.items()};data=self.data;digest=self.digest
                if variant=='hash':digest='a'*64
                if variant=='version':data=data.replace(b'kindred test',b'kindred wrong');digest=hashlib.sha256(data).hexdigest()
                if variant=='rpc':data=data.replace(b'{"commands":[]}',b'{"unknown":0}');digest=hashlib.sha256(data).hexdigest()
                if variant=='custom':(root/'browser-launch.py').write_text('UNMANAGED')
                if variant=='missing':payload['ensure-screen']['previous_sha256']=['a'*64]
                result=u.delivery('install',self.job,io.BytesIO(data),target,digest,'test',payload,package_root=root/'maintenance')
                self.assertEqual(result['phase'],'failed');self.assertEqual(target.read_bytes(),b'original')
                if variant=='custom':self.assertEqual((root/'browser-launch.py').read_text(),'UNMANAGED')
    def test_crash_between_helper_switches_remains_fenced(self):
        original=u.os.replace
        def replace(a,b):
            if Path(b)==self.root/'desktop-launch':raise SystemExit('owned simulated process loss')
            return original(a,b)
        with patch.object(u.os,'replace',side_effect=replace):
            with self.assertRaises(SystemExit):self.call()
        self.assertEqual(self.call('status')['phase'],'checking')
        with patch.object(u,'_install',side_effect=AssertionError('must never replay')):self.assertEqual(self.call()['phase'],'checking')
        self.assertEqual(self.target.read_bytes(),b'old runtime')
    def test_untrusted_journal_or_runtime_symlinks_refuse(self):
        directory=self.root/'.kindred-runtime-operations';outside=self.root/'outside';outside.mkdir();directory.symlink_to(outside,target_is_directory=True)
        with self.assertRaises(Exception):self.call()
        self.assertEqual(list(outside.iterdir()),[]);directory.unlink()
        self.target.unlink();self.target.symlink_to(outside/'absent')
        with self.assertRaises(Exception):self.call()
        self.assertFalse((outside/'absent').exists())

    def test_unknown_mode_and_invalid_id_refuse(self):
        with self.assertRaises(Exception):self.call('restore')
        with self.assertRaises(Exception):u.delivery('status','../outside',None,self.target,self.digest,'test',self.payload,package_root=self.root/'maintenance')

if __name__=='__main__':unittest.main()
