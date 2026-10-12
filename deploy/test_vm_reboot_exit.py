"""Disposable process metadata and API doubles; no QEMU or live VM operations."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('manager',Path(os.environ.get('KINDRED_REVIEW_MANAGER',Path(__file__).with_name('vm-manager.py'))))
manager=importlib.util.module_from_spec(spec);spec.loader.exec_module(manager)
PROFILE='00000000-0000-4000-8000-000000000001'
class RebootExit(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.base=Path(self.tmp.name)
  self.vm=self.base/PROFILE/'computer';self.vm.mkdir(parents=True)
  self.disk=self.vm/'disk.qcow2';self.disk.write_bytes(b'unchanged disk')
  (self.vm/'computer.json').write_text('{}')
  (self.vm/'qemu.pid').write_text('12345')
  self.proc=self.base/'proc'/'12345';self.proc.mkdir(parents=True)
  self.stattext='12345 (qemu worker) S '+' '.join(['0']*18+['900','0'])
  (self.proc/'stat').write_text(self.stattext)
  (self.proc/'exe').symlink_to('/usr/bin/qemu-system-x86_64')
  self.command=['qemu-system-x86_64','-name','kindred-'+PROFILE,'-drive',f'file={self.disk},if=virtio,format=qcow2']
  (self.proc/'cmdline').write_bytes(('\0'.join(self.command)+'\0').encode())
  self.stack=contextlib.ExitStack();self.addCleanup(self.stack.close)
  self.stack.enter_context(patch.object(manager,'ROOT',self.base))
  self.stack.enter_context(patch.object(sys,'argv',['manager','reboot',PROFILE]))
  realpath=Path
  self.stack.enter_context(patch.object(manager,'Path',side_effect=lambda p:realpath(str(p).replace('/proc/12345',str(self.proc),1))))
  self.qmp=self.stack.enter_context(patch.object(manager,'qmp',side_effect=self.control))
  self.launch=self.stack.enter_context(patch.object(manager,'start'))
  self.polls=0;self.powerdown=False
 def control(self,r,c):
  if c=='system_powerdown':self.powerdown=True;return {}
  if self.powerdown:raise FileNotFoundError('QMP gone')
  return {'status':'running'}
 def main(self):
  output=io.StringIO()
  with contextlib.redirect_stdout(output):manager.main()
  self.assertEqual(self.disk.read_bytes(),b'unchanged disk')
  return json.loads(output.getvalue())
 def timeout(self):
  self.stack.enter_context(patch.object(manager.time,'monotonic',side_effect=[0,61]))
 def test_lost_qmp_live_pid_never_launches(self):
  self.timeout()
  with self.assertRaisesRegex(ValueError,'still shutting down'):self.main()
  self.launch.assert_not_called();self.assertTrue(self.proc.exists());self.qmp.assert_any_call(self.vm,'system_powerdown')
 def test_confirmed_exit_single_launch_after_powerdown_under_operation_lock(self):
  import fcntl
  events=[]
  def sleep(s):events.append('exit');shutil.rmtree(self.proc)
  def launch(root,profile):
   self.assertFalse(self.proc.exists());self.assertTrue(self.powerdown)
   self.assertEqual((root,profile),(self.vm,PROFILE))
   with (self.base/'_operations'/PROFILE).open('a') as other:
    with self.assertRaises(BlockingIOError):fcntl.flock(other,fcntl.LOCK_EX|fcntl.LOCK_NB)
   events.append('launch')
  self.launch.side_effect=launch
  with patch.object(manager.time,'sleep',side_effect=sleep):self.assertEqual(self.main(),{'state':'restarting'})
  self.launch.assert_called_once();self.assertEqual(events,['exit','launch']);self.qmp.assert_any_call(self.vm,'system_powerdown')
 def test_pid_reuse_refuses(self):
  def sleep(s):(self.proc/'stat').write_text(self.stattext.replace('900','901'))
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called()
 def test_wrong_name_refuses_before_mutation(self):
  (self.proc/'cmdline').write_bytes(b'qemu-system-x86_64\0-name\0kindred-foreign\0')
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.assertFalse(self.powerdown);self.assertFalse((self.vm/'power-intent.json').exists());self.launch.assert_not_called()
 def test_wrong_disk_refuses_before_mutation(self):
  (self.proc/'cmdline').write_bytes(('\0'.join(self.command[:-1]+['file=/foreign/disk,if=virtio,format=qcow2'])+'\0').encode())
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.assertFalse(self.powerdown);self.launch.assert_not_called()
 def test_missing_pid_refuses_before_mutation(self):
  (self.vm/'qemu.pid').unlink()
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.assertFalse(self.powerdown);self.launch.assert_not_called()
 def test_missing_initial_process_refuses(self):
  shutil.rmtree(self.proc)
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called();self.assertFalse(self.powerdown)
 def test_unreadable_stat_refuses(self):
  realread=Path.read_text
  def read(path,*a,**k):
   if path==self.proc/'stat':raise PermissionError('unreadable')
   return realread(path,*a,**k)
  with patch.object(Path,'read_text',read):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called();self.assertFalse(self.powerdown)
 def test_unreadable_after_powerdown_refuses(self):
  realread=Path.read_text
  def read(path,*a,**k):
   if self.powerdown and path==self.proc/'stat':raise PermissionError('unreadable')
   return realread(path,*a,**k)
  with patch.object(Path,'read_text',read):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called()
 def test_recorded_pid_changed_refuses(self):
  def sleep(s):(self.vm/'qemu.pid').write_text('56789')
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called()
 def test_exit_removes_pidfile_and_process_then_launches_once(self):
  def sleep(s):
   (self.vm/'qemu.pid').unlink();shutil.rmtree(self.proc)
  with patch.object(manager.time,'sleep',side_effect=sleep):self.assertEqual(self.main(),{'state':'restarting'})
  self.launch.assert_called_once_with(self.vm,PROFILE)
 def test_missing_pidfile_with_live_process_refuses(self):
  def sleep(s):(self.vm/'qemu.pid').unlink()
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called()
 def test_missing_stat_but_present_process_refuses(self):
  def sleep(s):(self.proc/'stat').unlink()
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called()
 def test_malformed_stat_refuses(self):
  (self.proc/'stat').write_text('invalid')
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called();self.assertFalse(self.powerdown)
 def test_redirected_pid_record_refuses(self):
  (self.vm/'qemu.pid').unlink();(self.base/'pid').write_text('12345');(self.vm/'qemu.pid').symlink_to(self.base/'pid')
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called();self.assertFalse(self.powerdown)
 def test_wrong_executable_refuses_before_powerdown(self):
  (self.proc/'exe').unlink();(self.proc/'exe').symlink_to('/usr/bin/python3')
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.assertFalse(self.powerdown);self.launch.assert_not_called()
 def test_unreadable_command_refuses(self):
  realread=Path.read_bytes
  def read(path):
   if path==self.proc/'cmdline':raise PermissionError('unreadable')
   return realread(path)
  with patch.object(Path,'read_bytes',read):
   with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.assertFalse(self.powerdown);self.launch.assert_not_called()
 def test_disk_removed_during_shutdown_never_reprovisions(self):
  def sleep(s):shutil.rmtree(self.proc);self.disk.unlink()
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'files are unavailable'):self.main()
  self.launch.assert_not_called()
 def test_disk_replaced_during_shutdown_refuses(self):
  def sleep(s):
   shutil.rmtree(self.proc);self.disk.rename(self.vm/'old-disk');self.disk.write_bytes(b'replacement')
  with patch.object(manager.time,'sleep',side_effect=sleep):
   with self.assertRaisesRegex(ValueError,'disk changed'):self.main()
  self.launch.assert_not_called();self.assertEqual((self.vm/'old-disk').read_bytes(),b'unchanged disk')
 def test_missing_disk_initially_refuses_before_powerdown(self):
  self.disk.unlink()
  with self.assertRaisesRegex(ValueError,'identity'):self.main()
  self.launch.assert_not_called();self.assertFalse(self.powerdown)
 def test_real_kernel_process_absence_required_until_natural_exit_and_reap(self):
  import subprocess
  child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(.1)'])
  try:
   (self.vm/'qemu.pid').write_text(str(child.pid))
   process=Path(f'/proc/{child.pid}')
   started=manager.reboot_process_start(process,child.pid)
   disk=self.disk.stat();identity=(child.pid,started,(disk.st_dev,disk.st_ino))
   self.assertFalse(manager.reboot_process_exited(self.vm,identity))
   self.assertEqual(child.wait(timeout=5),0)
   self.assertTrue(manager.reboot_process_exited(self.vm,identity))
  finally:
   # Finite child exits normally; no signal/kill/terminate cleanup path.
   child.wait(timeout=5)
 def test_powerdown_rejection_propagates(self):
  def control(r,c):
   if c=='system_powerdown':raise ValueError('control rejected')
   return {'status':'running'}
  self.qmp.side_effect=control
  with self.assertRaisesRegex(ValueError,'control rejected'):self.main()
  self.launch.assert_not_called()
 def test_launch_error_propagates(self):
  with patch.object(manager.time,'sleep',side_effect=lambda s:shutil.rmtree(self.proc)):
   self.launch.side_effect=ValueError('launch failed')
   with self.assertRaisesRegex(ValueError,'launch failed'):self.main()
 def test_stopped_refuses_unchanged_intent(self):
  self.qmp.side_effect=FileNotFoundError('absent')
  (self.vm/'power-intent.json').write_text('{"running":false}')
  with self.assertRaisesRegex(ValueError,'off'):self.main()
  self.assertEqual((self.vm/'power-intent.json').read_text(),'{"running":false}');self.launch.assert_not_called()
if __name__=='__main__':unittest.main(verbosity=2)
