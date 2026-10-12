#!/usr/bin/env python3
import importlib.util, tempfile, unittest, os, json, fcntl, subprocess, sys, uuid, io, contextlib
from unittest.mock import patch
from types import SimpleNamespace
from pathlib import Path
spec=importlib.util.spec_from_file_location('ownership',Path(__file__).parents[1]/'deploy/screen-ownership.py')
m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
class EmptyScreen(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.base=Path(self.temp.name)
        self.root=self.base/'root'; self.root.mkdir(mode=0o700)
        self.proc=self.base/'proc'; self.proc.mkdir(); (self.proc/'net').mkdir()
        for name in ('tcp','tcp6'): (self.proc/'net'/name).write_text('header\n')
        self.xroot=self.base/'tmp'; self.xroot.mkdir()
        self.service={'ActiveState':'inactive','SubState':'dead','LoadState':'loaded','Job':''}
    def tearDown(self): self.temp.cleanup()
    def check(self): m.empty(2,self.root,self.proc,self.xroot,self.service)
    def process(self,argv=b'chromium\0',env=b''):
        p=self.proc/'123';p.mkdir();(p/'stat').write_text('123 (browser) S '+' '.join(['1']+['0']*17+['123']+['0']*5))
        (p/'cmdline').write_bytes(argv);(p/'environ').write_bytes(env); return p
    def test_empty_is_verified(self): self.check()
    def test_directory_pins_old_profile(self):
        (self.root/'browser-2').mkdir()
        with self.assertRaisesRegex(RuntimeError,'retains browser'): self.check()
    def test_broken_symlink_refused(self):
        (self.root/'browser-2').symlink_to(self.root/'missing')
        with self.assertRaises(RuntimeError): self.check()
    def test_regular_file_refused(self):
        (self.root/'browser-2').write_text('not a directory')
        with self.assertRaises(RuntimeError): self.check()
    def test_cleared_environment_profile_in_argv_refused(self):
        self.process(b'chromium\0--user-data-dir='+str(self.root/'browser-2').encode()+b'\0')
        with self.assertRaisesRegex(RuntimeError,'owns this browser'): self.check()
    def test_display_in_argv_refused(self):
        self.process(b'Xvfb\0:2\0')
        with self.assertRaisesRegex(RuntimeError,'owns this display'): self.check()
    def test_display_in_environment_refused(self):
        self.process(env=b'DISPLAY=:2.0\0')
        with self.assertRaisesRegex(RuntimeError,'owns this display'): self.check()
    def test_hidden_argv_refused(self):
        self.process(b'')
        with self.assertRaisesRegex(RuntimeError,'cannot be identified'): self.check()
    def test_incomplete_live_process_refused(self):
        (self.process()/'environ').unlink()
        with self.assertRaisesRegex(RuntimeError,'incomplete'): self.check()
    def test_pid_identity_change_refuses_even_without_matching_display(self):
        self.process()
        original=m.process_identity
        calls=0
        def changing(entry):
            nonlocal calls
            calls+=1
            value=original(entry)
            return (*value[:3], 'changed') if calls>1 else value
        with patch.object(m,'process_identity',side_effect=changing):
            with self.assertRaisesRegex(RuntimeError,'identity changed'):self.check()
    def test_unreadable_live_environment_does_not_fall_back_to_argv(self):
        entry=self.process(b'unrelated\0')
        original=Path.read_bytes
        def read(path):
            if path==entry/'environ':raise PermissionError()
            return original(path)
        with patch.object(Path,'read_bytes',read):
            with self.assertRaisesRegex(RuntimeError,'incomplete'):self.check()
    def test_process_exit_during_scan_refuses(self):
        entry=self.process()
        original=Path.read_bytes
        def read(path):
            if path==entry/'environ':raise FileNotFoundError()
            return original(path)
        with patch.object(Path,'read_bytes',read):
            with self.assertRaisesRegex(RuntimeError,'incomplete'):self.check()
    def test_other_display_does_not_match_prefix(self):
        self.process(b'Xvfb\0:20\0',b'DISPLAY=:20\0');self.check()
    def test_active_or_queued_service_refused(self):
        for key,value in [('ActiveState','active'),('Job','73'),('LoadState','error')]:
            old=self.service[key];self.service[key]=value
            with self.assertRaises(RuntimeError): self.check()
            self.service[key]=old
    def test_x_socket_refused(self):
        (self.xroot/'.X11-unix').mkdir();(self.xroot/'.X11-unix'/'X2').write_text('')
        with self.assertRaisesRegex(RuntimeError,'has a display'): self.check()
    def test_network_listener_refused(self):
        (self.proc/'net'/'tcp').write_text('header\n0: 0100007F:170E 00000000:0000 0A\n')
        with self.assertRaisesRegex(RuntimeError,'network connection'): self.check()

class LaunchFence(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name)/'root'; self.root.mkdir(mode=0o700)
        self.records=self.root/'screen-owners'; self.records.mkdir(mode=0o700)
        self.record={'owner':str(uuid.uuid4()),'generation':str(uuid.uuid4())}
        self.path=self.records/'2.json'; self.path.write_text(json.dumps(self.record)); self.path.chmod(0o600)
        self.lockpath=self.records/'2.lock'
    def tearDown(self): self.temp.cleanup()
    def test_current_display_can_pin_profile(self):
        with m.launch_guard(2,self.root,{'KINDRED_SCREEN_OWNER':self.record['owner'],'KINDRED_SCREEN_GENERATION':self.record['generation']}): pass
        self.assertTrue((self.root/'browser-2').is_dir())
        with self.assertRaisesRegex(RuntimeError,'retains browser'): m.empty(2,self.root,Path('/missing'),Path('/missing'),{})
    def test_old_generation_does_not_provision(self):
        with self.assertRaisesRegex(RuntimeError,'ownership changed'):
            with m.launch_guard(2,self.root,{'KINDRED_SCREEN_OWNER':self.record['owner'],'KINDRED_SCREEN_GENERATION':str(uuid.uuid4())}): pass
        self.assertFalse((self.root/'browser-2').exists())
    def test_unowned_direct_launch_does_not_provision(self):
        with self.assertRaisesRegex(RuntimeError,'ownership changed'):
            with m.launch_guard(2,self.root): pass
        self.assertFalse((self.root/'browser-2').exists())
    def test_startup_pins_before_display_launch(self):
        with m.launch_guard(2,self.root,startup=True) as identity: self.assertEqual(identity,self.record)
        self.assertTrue((self.root/'browser-2').exists())
    def child(self,fd):
        env=dict(os.environ,KINDRED_SCREEN_SLOT='2',KINDRED_SCREEN_OWNER=self.record['owner'],KINDRED_SCREEN_GENERATION=self.record['generation'],KINDRED_SCREEN_LOCK_PID=str(os.getpid()),KINDRED_SCREEN_LOCK_FD=str(fd))
        source="import importlib.util,sys; from pathlib import Path; s=importlib.util.spec_from_file_location('m',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); guard=m.launch_guard(2,Path(sys.argv[2])); guard.__enter__(); guard.__exit__(None,None,None)"
        return subprocess.run([sys.executable,'-c',source,str(Path(m.__file__).resolve()),str(self.root)],env=env,capture_output=True)
    def test_actual_ancestor_flock_is_borrowed_without_deadlock(self):
        fd=os.open(self.lockpath,os.O_CREAT|os.O_RDWR,0o600)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            result=self.child(fd); self.assertEqual(result.returncode,0,result.stderr.decode())
        finally: os.close(fd)
    def test_open_fd_without_flock_is_not_borrowed(self):
        fd=os.open(self.lockpath,os.O_CREAT|os.O_RDWR,0o600)
        try:
            result=self.child(fd); self.assertNotEqual(result.returncode,0)
            self.assertFalse((self.root/'browser-2').exists())
        finally: os.close(fd)
    def test_busy_claim_lock_refuses_independent_startup(self):
        fd=os.open(self.lockpath,os.O_CREAT|os.O_RDWR,0o600)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(BlockingIOError):
                with m.launch_guard(2,self.root,startup=True): pass
            self.assertFalse((self.root/'browser-2').exists())
        finally: os.close(fd)


class ClaimDispatch(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)/'root';self.root.mkdir(mode=0o700)
        self.owner=str(uuid.uuid4());self.generation=str(uuid.uuid4())
    def tearDown(self):self.temp.cleanup()
    def invoke(self,mode,owner=None,generation=None,legacy=False,command=None,occupied=False):
        # Only absence metadata is injected; actual parser, flock, record CAS,
        # durable write and child dispatch execute unchanged production functions.
        source="import importlib.util,sys; from pathlib import Path; s=importlib.util.spec_from_file_location('m',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);m.ROOT=Path(sys.argv[2]);occupied=sys.argv[3]=='1';m.empty=lambda slot: (_ for _ in ()).throw(RuntimeError('occupied')) if occupied else None;sys.argv=sys.argv[4:];m.main()"
        args=['ownership','--slot','2','--owner',owner or self.owner,'--generation',generation or self.generation,'--mode',mode]
        if legacy:args+=['--legacy']
        if command:args+=['--',*command]
        return subprocess.run([sys.executable,'-c',source,str(Path(m.__file__).resolve()),str(self.root),str(int(occupied)),*args],capture_output=True)
    def test_claim_then_stale_legacy_dispatch_is_rejected(self):
        self.assertEqual(self.invoke('claim-empty').returncode,0)
        marker=self.root/'effect'
        command=[sys.executable,'-c',"from pathlib import Path; import sys; Path(sys.argv[1]).write_text('effect')",str(marker)]
        old=self.invoke('dispatch',owner=str(uuid.uuid4()),generation=str(uuid.uuid4()),legacy=True,command=command)
        self.assertNotEqual(old.returncode,0);self.assertFalse(marker.exists())
        fresh=self.invoke('dispatch',command=command);self.assertEqual(fresh.returncode,0,fresh.stderr.decode());self.assertTrue(marker.exists())
    def test_legacy_public_state_root_keeps_mode_and_private_claim_record(self):
        self.root.chmod(0o755); before=self.root.stat().st_ino
        result=self.invoke('claim-empty')
        self.assertEqual(result.returncode,0,result.stderr.decode())
        self.assertEqual(self.root.stat().st_ino,before)
        self.assertEqual(self.root.stat().st_mode & 0o777,0o755)
        self.assertEqual((self.root/'screen-owners').stat().st_mode & 0o777,0o700)
        self.assertEqual((self.root/'screen-owners/2.json').stat().st_mode & 0o777,0o600)
    def test_interrupted_database_commit_can_repeat_exact_claim(self):
        self.assertEqual(self.invoke('claim-empty').returncode,0)
        # Startup may pin the new owner's profile while the backend CAS is pending.
        (self.root/'browser-2').mkdir()
        repeated=self.invoke('claim-empty',occupied=True)
        self.assertEqual(repeated.returncode,0,repeated.stderr.decode())
        self.assertEqual(json.loads(repeated.stdout)['generation'],self.generation)
        loser=self.invoke('claim-empty',generation=str(uuid.uuid4()),occupied=True)
        self.assertNotEqual(loser.returncode,0)
        self.assertEqual(m.read_record(self.root/'screen-owners'/'2.json')['generation'],self.generation)
    def test_occupied_first_claim_has_no_record_or_effect(self):
        result=self.invoke('claim-empty',occupied=True);self.assertNotEqual(result.returncode,0)
        self.assertIsNone(m.read_record(self.root/'screen-owners'/'2.json'))
    def test_stale_temporary_file_does_not_prevent_recovery(self):
        records=self.root/'screen-owners';records.mkdir(mode=0o700)
        (records/('.2-'+self.generation+'.tmp')).write_text('interrupted old staging')
        self.assertEqual(self.invoke('claim-empty').returncode,0)
        self.assertEqual(m.read_record(records/'2.json')['generation'],self.generation)


class LegacyRootBoundary(unittest.TestCase):
    def test_only_exact_owned_legacy_root_can_be_public(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'state';root.mkdir(mode=0o755)
            with patch.object(m,'ROOT',root):
                m.private(root)
                for name in ['screen-owners','browser-2','other']:
                    child=root/name;child.mkdir(mode=0o755)
                    with self.assertRaises(RuntimeError):m.private(child)
                for mode in [0o775,0o777,0o750,0o1755]:
                    root.chmod(mode)
                    with self.assertRaises(RuntimeError):m.private(root)
                root.chmod(0o755)
                with patch.object(m.os,'getuid',return_value=os.getuid()+1):
                    with self.assertRaises(RuntimeError):m.private(root)
                link=Path(d)/'link';link.symlink_to(root)
                with patch.object(m,'ROOT',link):
                    with self.assertRaises(RuntimeError):m.private(link)
    def test_private_records_remain_required_under_legacy_root(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'state';root.mkdir(mode=0o755)
            with patch.object(m,'ROOT',root):
                with m.launch_guard(2,root,startup=True):pass
                self.assertEqual((root/'screen-owners').stat().st_mode & 0o777,0o700)
                self.assertEqual((root/'browser-2').stat().st_mode & 0o777,0o700)
                (root/'screen-owners').chmod(0o755)
                with self.assertRaises(RuntimeError):
                    with m.launch_guard(3,root,startup=True):pass
                self.assertFalse((root/'browser-3').exists())

class RootInspectorContract(unittest.TestCase):
    @unittest.skipUnless(os.geteuid()==0,'root-only refusal check')
    def test_root_cannot_enter_claim_or_dispatch_mode(self):
        result=subprocess.run([sys.executable,str(Path(m.__file__).resolve()),'--slot','2','--owner',str(uuid.uuid4()),'--generation',str(uuid.uuid4()),'--mode','claim-empty'],capture_output=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn(b'Ownership changes require the guest user',result.stderr)
    def test_missing_or_changed_helper_refuses_before_privileged_command(self):
        for failure in [FileNotFoundError(),RuntimeError('Privileged process inspector does not match')]:
            with patch.object(m,'validate_installed_helper',side_effect=failure),patch.object(m.subprocess,'run') as run:
                with self.assertRaises((FileNotFoundError,RuntimeError)):m.privileged_process_receipt(2)
                run.assert_not_called()
    def test_wrong_shape_refuses_before_identity_or_inspection(self):
        for argv in [[],['--inspect-proc','--slot','0'],['--inspect-proc','--slot','33'],['--inspect-proc','--slot','2','--root','/other'],['--inspect-proc','--slot','2','--mode','claim-empty']]:
            with patch.object(m.pwd,'getpwnam') as account:
                with self.assertRaisesRegex(RuntimeError,'Invalid process inspector'):m.root_inspector(argv)
                account.assert_not_called()
    def test_wrong_caller_refuses_before_helper_or_proc_read(self):
        for euid,caller in [(1000,'1000'),(0,'1001'),(0,'')]:
            with patch.object(m.pwd,'getpwnam',return_value=SimpleNamespace(pw_uid=1000)),patch.object(m.os,'geteuid',return_value=euid),patch.dict(m.os.environ,{'SUDO_UID':caller}),patch.object(m,'validate_installed_helper') as helper,patch.object(m,'inspect_processes') as inspect:
                with self.assertRaisesRegex(RuntimeError,'caller is invalid'):m.root_inspector(['--inspect-proc','--slot','2'])
                helper.assert_not_called();inspect.assert_not_called()
    def test_typed_clear_and_unknown_receipts_have_no_process_contents(self):
        for error,code,status in [(None,0,'clear'),(RuntimeError('sensitive synthetic value'),1,'unknown')]:
            out=io.StringIO()
            with patch.object(m.pwd,'getpwnam',return_value=SimpleNamespace(pw_uid=1000)),patch.object(m.os,'geteuid',return_value=0),patch.dict(m.os.environ,{'SUDO_UID':'1000'}),patch.object(m,'validate_installed_helper'),patch.object(m,'verify_parent_lock'),patch.object(m,'inspect_processes',side_effect=error) as inspect,contextlib.redirect_stdout(out):
                self.assertEqual(m.root_inspector(['--inspect-proc','--slot','2']),code)
                inspect.assert_called_once_with(2,1000)
            self.assertEqual(json.loads(out.getvalue()),{'protocol':1,'slot':2,'uid':1000,'status':status})
            self.assertNotIn('sensitive',out.getvalue())
    def test_wrong_receipt_exit_or_size_never_authorizes_claim(self):
        good={'protocol':1,'slot':2,'uid':os.getuid(),'status':'clear'}
        receipts=[dict(good,slot=3),dict(good,uid=os.getuid()+1),dict(good,status='unknown'),dict(good,extra=True)]
        for returncode,output in [(0,json.dumps(r).encode()) for r in receipts]+[(1,json.dumps(good).encode()),(0,b'x'*513)]:
            with patch.object(m,'validate_installed_helper'),patch.object(m.subprocess,'run',return_value=SimpleNamespace(returncode=returncode,stdout=output)):
                with self.assertRaisesRegex(RuntimeError,'could not be verified'):m.privileged_process_receipt(2)
    def test_fixed_absolute_isolated_python_command(self):
        result=SimpleNamespace(returncode=0,stdout=json.dumps({'protocol':1,'slot':2,'uid':os.getuid(),'status':'clear'}).encode())
        with patch.object(m,'validate_installed_helper'),patch.object(m.subprocess,'run',return_value=result) as run:
            m.privileged_process_receipt(2)
            self.assertEqual(run.call_args.args[0],['/usr/bin/sudo','-n','/usr/bin/python3','-I','-B','/usr/local/lib/kindred/screen-ownership.py','--inspect-proc','--slot','2'])
            self.assertEqual(run.call_args.kwargs['timeout'],5)

class ParentLockMetadata(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.base=Path(self.temp.name)
        self.root=self.base/'root'; self.root.mkdir(mode=0o700)
        self.records=self.root/'screen-owners'; self.records.mkdir(mode=0o700)
        self.lock=self.records/'2.lock'; self.lock.touch(mode=0o600)
        self.fd=os.open(self.lock,os.O_RDWR)
        fcntl.flock(self.fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
    def tearDown(self):
        os.close(self.fd); self.temp.cleanup()
    def check_child(self, root=None, uid=None):
        # Fresh child must find the real kernel flock held by this test parent.
        script="import importlib.util,sys;from pathlib import Path;s=importlib.util.spec_from_file_location('ownership',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);m.ROOT=Path(sys.argv[2]);m.verify_parent_lock(2,int(sys.argv[3]))"
        return subprocess.run([sys.executable,'-c',script,str(Path(m.__file__).resolve()),str(root or self.root),str(os.getuid() if uid is None else uid)],capture_output=True,timeout=5)
    def test_owned_legacy_root_accepts_actual_ancestor_flock_without_mutation(self):
        self.root.chmod(0o755)
        paths=(self.root,self.records,self.lock)
        before=[(p.stat().st_ino,p.stat().st_mode) for p in paths]
        result=self.check_child()
        self.assertEqual(result.returncode,0,result.stderr.decode())
        self.assertEqual(before,[(p.stat().st_ino,p.stat().st_mode) for p in paths])
    def test_private_root_still_accepts_actual_ancestor_flock(self):
        result=self.check_child(); self.assertEqual(result.returncode,0,result.stderr.decode())
    def test_writable_or_other_public_root_refuses(self):
        for mode in (0o775,0o777,0o750,0o705):
            with self.subTest(mode=mode):
                self.root.chmod(mode);self.assertNotEqual(self.check_child().returncode,0)
    def test_private_children_and_lock_remain_required(self):
        self.root.chmod(0o755)
        for path in (self.records,self.lock):
            path.chmod(0o755)
            self.assertNotEqual(self.check_child().returncode,0)
            path.chmod(0o700 if path==self.records else 0o600)
    def test_redirected_private_records_or_lock_refuse(self):
        self.root.chmod(0o755)
        self.lock.unlink(); target=self.base/'other-lock';target.touch(mode=0o600)
        self.lock.symlink_to(target)
        self.assertNotEqual(self.check_child().returncode,0)
        self.lock.unlink();self.lock.touch(mode=0o600)
        moved=self.base/'other-records';self.records.rename(moved)
        self.records.symlink_to(moved,target_is_directory=True)
        self.assertNotEqual(self.check_child().returncode,0)
    def test_foreign_uid_refuses(self):
        self.root.chmod(0o755);self.assertNotEqual(self.check_child(uid=os.getuid()+1).returncode,0)
    def test_root_and_ancestor_redirects_refuse(self):
        alias=self.base/'alias';alias.symlink_to(self.root,target_is_directory=True)
        self.assertNotEqual(self.check_child(root=alias).returncode,0)
        parent_alias=self.base/'parent-alias';parent_alias.symlink_to(self.base,target_is_directory=True)
        self.assertNotEqual(self.check_child(root=parent_alias/'root').returncode,0)
    def test_missing_ancestor_flock_refuses(self):
        self.root.chmod(0o755);fcntl.flock(self.fd,fcntl.LOCK_UN)
        result=self.check_child();self.assertNotEqual(result.returncode,0)
        self.assertIn(b'requires its ancestor slot lock',result.stderr)
    def test_foreign_process_flock_is_not_an_ancestor(self):
        self.root.chmod(0o755);fcntl.flock(self.fd,fcntl.LOCK_UN)
        script="import fcntl,sys;f=open(sys.argv[1],'r+');fcntl.flock(f,fcntl.LOCK_EX);print('held',flush=True);sys.stdin.read()"
        with subprocess.Popen([sys.executable,'-c',script,str(self.lock)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True) as holder:
            try:
                self.assertEqual(holder.stdout.readline().strip(),'held')
                result=self.check_child();self.assertNotEqual(result.returncode,0)
                self.assertIn(b'requires its ancestor slot lock',result.stderr)
            finally:
                holder.stdin.close();holder.wait(timeout=5)

if __name__=='__main__':unittest.main()
