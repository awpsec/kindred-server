"""Actual disposable executable/update/rollback; never a VM or owner profile."""
import fcntl
import hashlib
import importlib.util
import io
import json
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('update',ROOT/'deploy/update-guest-runtime.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

def executable(version,broken=False):
    return ('#!/usr/bin/env python3\nimport json,sys\n' + ('sys.exit(1)\n' if broken else f'print("kindred {version}" if sys.argv[1]=="--version" else json.dumps({{"commands":[]}}))\n')).encode()

class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='kindred-runtime-test-');self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.target=self.root/'kindred-bin';self.old=executable('old');self.new=executable('candidate');self.target.write_bytes(self.old);self.target.chmod(0o755)
        self.payload={}
        for name in ('desktop-launch','start-desktop.sh'):
            old=(ROOT/'tools/fixtures/original-managed-launchers'/name).read_bytes();(self.root/name).write_bytes(old);(self.root/name).chmod(0o755)
            self.payload[name]={'source':(ROOT/'deploy'/name).read_text(),'previous_sha256':hashlib.sha256(old).hexdigest()}
        self.payload['browser-launch.py']={'source':(ROOT/'deploy/browser-launch.py').read_text(),'previous_sha256':[None,hashlib.sha256((ROOT/'deploy/browser-launch.py').read_bytes()).hexdigest()]}
        for name in ('ensure-screen','screen-ownership.py'):
            if name=='ensure-screen':
                old=(ROOT/'deploy'/name).read_bytes()
                (self.root/name).write_bytes(old);(self.root/name).chmod(0o755)
                previous=hashlib.sha256(old).hexdigest()
            else:previous=None
            self.payload[name]={'source':(ROOT/'deploy'/name).read_text(),'previous_sha256':previous}

        self.profile=self.root/'browser-fixture';self.profile.mkdir(mode=0o700);(self.profile/'synthetic-state').write_text('preserved');self.inode=self.profile.stat().st_ino
    def install(self,payload=None,data=None,digest=None):
        data=self.new if data is None else data
        return m.install(io.BytesIO(data),self.target,digest or hashlib.sha256(data).hexdigest(),'candidate',payload)
    def preserved(self):
        self.assertEqual(self.profile.stat().st_ino,self.inode);self.assertEqual((self.profile/'synthetic-state').read_text(),'preserved')
    def test_partial_fence_install_is_refused_before_runtime_switch(self):
        incomplete=dict(self.payload);del incomplete['screen-ownership.py']
        with self.assertRaisesRegex(RuntimeError,'Incomplete'):self.install(incomplete)
        self.assertEqual(self.target.read_bytes(),self.old);self.preserved()
    def test_binary_only_baseline_does_not_install_launchers(self):
        before=(self.root/'desktop-launch').read_bytes();result=self.install();self.assertTrue(result['updated']);self.assertFalse(result['launchers_updated']);self.assertEqual((self.root/'desktop-launch').read_bytes(),before);self.assertFalse((self.root/'browser-launch.py').exists());self.preserved()
    def test_production_payload_upgrades_exact_0861_helpers_and_replays(self):
        production=(ROOT/'src/vm_maintenance.rs').read_text()
        payload={}
        for name in self.payload:
            old=subprocess.check_output(['git','show','0874619f13bf708679df3cc2dbfb34a3f5c8b39d:deploy/'+name],cwd=ROOT)
            (self.root/name).write_bytes(old);(self.root/name).chmod(0o755)
            declaration=re.search(r'"'+re.escape(name)+r'":\{"source":include_str!\([^\n]+?\),"previous_sha256":([^}]+)\}',production)
            self.assertIsNotNone(declaration,name)
            payload[name]={'source':(ROOT/'deploy'/name).read_text(),'previous_sha256':json.loads(declaration[1])}
        result=self.install(payload)
        self.assertTrue(result['updated']);self.assertTrue(result['launchers_updated'])
        self.assertEqual(result['launcher_hashes'],{name:hashlib.sha256(v['source'].encode()).hexdigest() for name,v in payload.items()})
        result=self.install(payload)
        self.assertFalse(result['updated']);self.assertFalse(result['launchers_updated']);self.preserved()
    def test_production_payload_upgrades_exact_corrected_0862_helpers(self):
        production=(ROOT/'src/vm_maintenance.rs').read_text(); payload={}
        for name in self.payload:
            old=subprocess.check_output(['git','show','bd9765cc9efbf9b65c503d4b4ca9a6378af71a24:deploy/'+name],cwd=ROOT)
            (self.root/name).write_bytes(old);(self.root/name).chmod(0o755)
            declaration=re.search(r'"'+re.escape(name)+r'":\{"source":include_str!\([^\n]+?\),"previous_sha256":([^}]+)\}',production)
            self.assertIsNotNone(declaration,name)
            payload[name]={'source':(ROOT/'deploy'/name).read_text(),'previous_sha256':json.loads(declaration[1])}
        result=self.install(payload)
        self.assertTrue(result['updated']);self.assertTrue(result['launchers_updated'])
        self.assertEqual(result['launcher_hashes'],{name:hashlib.sha256(v['source'].encode()).hexdigest() for name,v in payload.items()})
        self.assertEqual((self.root/'screen-ownership.py').read_text(),(ROOT/'deploy/screen-ownership.py').read_text())
        result=self.install(payload)
        self.assertFalse(result['updated']);self.assertFalse(result['launchers_updated']);self.preserved()
    def test_production_payload_upgrades_original_managed_helpers(self):
        # Existing installations can retain the first shipped launcher even
        # after multiple binary-only guest updates. This is not customization.
        production=(ROOT/'src/vm_maintenance.rs').read_text(); payload={}
        for name in self.payload:
            path=self.root/name
            old=ROOT/'tools/fixtures/original-managed-launchers'/name
            if old.is_file():
                path.write_bytes(old.read_bytes());path.chmod(0o755)
            else:
                path.unlink(missing_ok=True)
            declaration=re.search(r'"'+re.escape(name)+r'":\{"source":include_str!\([^\n]+?\),"previous_sha256":([^}]+)\}',production)
            self.assertIsNotNone(declaration,name)
            payload[name]={'source':(ROOT/'deploy'/name).read_text(),'previous_sha256':json.loads(declaration[1])}
        result=self.install(payload)
        self.assertTrue(result['updated']);self.assertTrue(result['launchers_updated'])
        self.assertEqual(result['launcher_hashes'],{name:hashlib.sha256(v['source'].encode()).hexdigest() for name,v in payload.items()})
        self.assertEqual((self.root/'start-desktop.sh.previous').read_bytes(),(ROOT/'tools/fixtures/original-managed-launchers/start-desktop.sh').read_bytes())
        self.preserved()

    def test_full_verified_install_and_idempotence(self):
        result=self.install(self.payload);self.assertTrue(result['updated']);self.assertTrue(result['launchers_updated'])
        for name,value in self.payload.items():
            self.assertEqual((self.root/name).read_text(),value['source']);self.assertEqual((self.root/name).stat().st_mode&0o777,0o755);self.assertEqual((self.root/name).stat().st_uid,self.target.stat().st_uid)
        self.assertEqual((self.root/'kindred-bin.previous').read_bytes(),self.old)
        result=self.install(self.payload);self.assertFalse(result['updated']);self.assertFalse(result['launchers_updated']);self.preserved()
    def test_unchanged_runtime_still_installs_and_verifies_launchers(self):
        self.target.write_bytes(self.new)
        result=self.install(self.payload);self.assertFalse(result['updated']);self.assertTrue(result['launchers_updated'])
        self.assertEqual(set(result['launcher_hashes']),set(self.payload));self.preserved()
        (self.root/'browser-launch.py').chmod(0o644)
        result=self.install(self.payload);self.assertTrue(result['launchers_updated']);self.assertEqual((self.root/'browser-launch.py').stat().st_mode&0o777,0o755)
    def test_redirected_lock_refused_without_following(self):
        (self.root/'.kindred-runtime.lock').symlink_to(self.target)
        with self.assertRaises(OSError):self.install(self.payload)
        self.assertEqual(self.target.read_bytes(),self.old);self.preserved()
    def test_silent_replacement_failure_rejected_by_hash_receipt(self):
        replace=m.os.replace
        def silent(source,target):
            if Path(target)==self.root/'desktop-launch' and '.kindred-launcher-' in Path(source).name and 'rollback' not in Path(source).name:return None
            return replace(source,target)
        with patch.object(m.os,'replace',side_effect=silent):
            with self.assertRaisesRegex(RuntimeError,'did not verify'):self.install(self.payload)
        self.assertEqual(self.target.read_bytes(),self.old);self.assertFalse((self.root/'browser-launch.py').exists());self.preserved()
    def test_digest_or_candidate_receipt_failure_changes_nothing(self):
        before={p.name:p.read_bytes() for p in self.root.iterdir() if p.is_file()}
        for data,digest in [(self.new,'0'*64),(executable('candidate',True),None),(executable('wrong'),None)]:
            with self.assertRaises(Exception):self.install(self.payload,data,digest)
            for name,value in before.items():self.assertEqual((self.root/name).read_bytes(),value)
            self.assertFalse((self.root/'browser-launch.py').exists());self.preserved()
    def test_custom_and_symlink_scripts_preserved_before_runtime_change(self):
        path=self.root/'desktop-launch';path.write_text('custom sentinel')
        with self.assertRaisesRegex(RuntimeError,'Customized'):self.install(self.payload)
        self.assertEqual(path.read_text(),'custom sentinel');self.assertEqual(self.target.read_bytes(),self.old);self.assertFalse((self.root/'browser-launch.py').exists())
        path.unlink();path.symlink_to(self.target)
        with self.assertRaisesRegex(RuntimeError,'Customized'):self.install(self.payload)
        self.assertTrue(path.is_symlink());self.assertEqual(self.target.read_bytes(),self.old);self.preserved()
    def test_mid_switch_failure_rolls_back_before_runtime_switch(self):
        before={name:(self.root/name).read_bytes() for name in ('desktop-launch','start-desktop.sh')};replace=m.os.replace
        def fail(source,target):
            if Path(target)==self.root/'start-desktop.sh' and '.kindred-launcher-' in Path(source).name and 'rollback' not in Path(source).name:raise OSError('injected switch failure')
            return replace(source,target)
        with patch.object(m.os,'replace',side_effect=fail):
            with self.assertRaisesRegex(OSError,'injected'):self.install(self.payload)
        self.assertEqual(self.target.read_bytes(),self.old);self.assertFalse((self.root/'browser-launch.py').exists())
        for name,value in before.items():self.assertEqual((self.root/name).read_bytes(),value);self.assertEqual((self.root/name).stat().st_mode&0o777,0o755)
        self.preserved()
    def test_concurrent_install_refused_without_effect(self):
        with open(self.root/'.kindred-runtime.lock','a+b') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaisesRegex(RuntimeError,'Another'):self.install(self.payload)
        self.assertEqual(self.target.read_bytes(),self.old);self.assertFalse((self.root/'browser-launch.py').exists());self.preserved()
    def test_running_process_is_not_restarted_or_killed(self):
        child=subprocess.Popen(['sleep','30'])
        try:
            self.install(self.payload);self.assertIsNone(child.poll());self.preserved()
        finally:child.terminate();child.wait(timeout=5)

if __name__=='__main__':unittest.main()
