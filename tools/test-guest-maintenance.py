"""Maintenance state-machine tests. Never run host package commands."""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch, Mock
if sys.platform == 'win32':
    sys.modules['fcntl'] = Mock(LOCK_EX=2, LOCK_NB=4)
spec = importlib.util.spec_from_file_location('maintenance', Path(__file__).parents[1] / 'deploy/guest-maintenance.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Maintenance(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name, value in [('ROOT', self.root), ('guest_check', Mock()), ('update_harnesses', Mock()), ('unit_active', Mock(return_value=False)), ('boot_id', Mock(return_value='boot-one'))]:
            context = patch.object(m, name, value); context.start(); self.addCleanup(context.stop)
        # All subprocesses are forbidden unless a test explicitly supplies a fake.
        context = patch.object(m.subprocess, 'run', side_effect=AssertionError('Unexpected command'))
        context.start(); self.addCleanup(context.stop)
        self.job = 'cae84a0a-8eb4-41bc-bca4-d9f0954e9bfa'
    def save(self, **fields):
        value = dict(job_id=self.job, phase='starting', attempted=int(m.time.time()), boot_id='boot-one')
        value.update(fields); m.atomic(self.root / 'state.json', value)
    def test_manual_update_bypasses_cooldown_and_reboots_only_after_success(self):
        self.save(phase='completed', finished=int(m.time.time()))
        self.job='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        with patch.object(m.subprocess,'run',return_value=m.subprocess.CompletedProcess([],0)):
            result=m.start(self.job,manual=True)
            self.assertEqual(result['phase'],'starting')
            with patch.object(m,'apt',return_value=0),patch.object(m,'package_digest',return_value='same'):
                m.worker(self.job)
        self.assertEqual(m.read_state()['phase'],'rebooting')
        self.assertEqual(m.status()['phase'],'rebooting')
        m.boot_id.return_value='boot-two'
        self.assertEqual(m.status()['phase'],'completed')
        self.assertFalse(m.status()['reboot_recommended'])
    def test_harness_failure_does_not_reboot_or_report_success(self):
        self.save(manual=True)
        m.update_harnesses.side_effect=RuntimeError('Harness update failed')
        with patch.object(m,'package_digest',return_value='same'):
            m.worker(self.job)
        self.assertEqual(m.read_state()['phase'],'failed')
        self.assertIn('Harness',m.read_state()['error'])
    def test_same_job_is_never_dispatched_twice(self):
        for phase in ('starting','updating','completed','failed'):
            self.save(phase=phase)
            self.assertEqual(m.start(self.job)['phase'], phase)
    def test_different_job_cannot_bypass_three_day_cooldown(self):
        self.save(phase='completed', attempted=int(m.time.time()) - 86400*2)
        result=m.start('cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb')
        self.assertEqual(result['phase'],'deferred')
        self.assertGreater(result['retry_at'],m.time.time()+86000)
    def test_active_service_holds_computer_until_process_exit(self):
        self.save(phase='completed');m.unit_active.return_value=True
        self.assertEqual(m.status()['phase'],'updating')
        m.unit_active.return_value=False
        self.assertEqual(m.status()['phase'],'completed')
    def test_interrupted_install_is_failed_and_reboot_clears_hint(self):
        self.save(attempted=int(m.time.time())-60,reboot_recommended=True,boot_id='old')
        result=m.status();self.assertEqual(result['phase'],'failed')
        self.assertIn('restarted',result['error']);self.assertFalse(result['reboot_recommended'])
    def test_worker_updates_then_upgrades_without_reboot(self):
        self.save()
        with patch.object(m,'apt',return_value=0) as apt,patch.object(m,'package_digest',side_effect=['old','new','new']):
            m.worker(self.job)
        self.assertEqual([c.args[0] for c in apt.call_args_list],[['-o','APT::Update::Error-Mode=any','update'],['--yes','--with-new-pkgs','upgrade']])
        value=m.read_state();self.assertEqual(value['phase'],'completed');self.assertTrue(value['reboot_recommended'])
    def test_failed_update_does_not_upgrade_or_retry(self):
        self.save()
        with patch.object(m,'apt',return_value=100) as apt,patch.object(m,'package_digest',return_value='old'):
            m.worker(self.job)
        self.assertEqual(apt.call_count,1);self.assertEqual(m.read_state()['phase'],'failed')
        self.assertNotIn('http',m.read_state()['error'])
    def test_partial_upgrade_failure_still_recommends_reboot(self):
        self.save()
        with patch.object(m,'apt',side_effect=[0,100]),patch.object(m,'package_digest',side_effect=['old','partial']):
            m.worker(self.job)
        self.assertEqual(m.read_state()['phase'],'failed');self.assertTrue(m.read_state()['reboot_recommended'])
    def test_unknown_dispatch_is_fenced_against_a_late_start(self):
        self.save(phase='completed', attempted=int(m.time.time())-4*86400)
        unknown='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        failed=m.reconcile(unknown);self.assertEqual(failed['job_id'],unknown);self.assertEqual(failed['phase'],'failed')
        self.assertEqual(m.start(unknown),failed)
        self.assertEqual(m.reconcile(unknown),failed)
        # Even after a later job replaced the current state, the late ID stays fenced.
        self.save(job_id='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfc',phase='completed',attempted=int(m.time.time())-4*86400)
        self.assertEqual(m.start(unknown),failed)
    def test_an_interrupted_start_cannot_launch_a_delayed_worker(self):
        self.save(attempted=int(m.time.time())-60)
        result=m.reconcile(self.job);self.assertEqual(result['phase'],'failed')
        self.assertEqual(m.read_state()['phase'],'failed')
        with self.assertRaisesRegex(RuntimeError,'no longer current'):
            m.worker(self.job)
    def test_recovery_adopts_an_active_job_instead_of_cancelling_it(self):
        self.save(phase='updating');m.unit_active.return_value=True
        unknown='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        result=m.reconcile(unknown)
        self.assertEqual(result['job_id'],self.job);self.assertEqual(result['phase'],'updating')
        self.assertEqual(m.read_state()['phase'],'updating')
        m.unit_active.return_value=False
        self.save(phase='completed',attempted=int(m.time.time())-4*86400)
        self.assertEqual(m.start(unknown)['phase'],'failed')
    @unittest.skipIf(sys.platform=='win32','Requires real flock semantics')
    def test_reconciliation_waits_for_the_dispatch_lock(self):
        from concurrent.futures import ThreadPoolExecutor
        import threading,time
        began=threading.Event()
        def recover():
            began.set();return m.reconcile(self.job)
        with ThreadPoolExecutor() as pool:
            with (self.root/'dispatch.lock').open('a') as lock:
                m.fcntl.flock(lock,m.fcntl.LOCK_EX)
                future=pool.submit(recover);self.assertTrue(began.wait(2));time.sleep(.05)
                self.assertFalse(future.done())
            self.assertEqual(future.result(timeout=2)['phase'],'failed')
    def launch(self, result, free=2*1024**3, uptime=1000):
        from types import SimpleNamespace
        original=Path.read_text
        def read(path,*args,**kwargs):
            return f'{uptime}.0 {uptime}.0' if str(path).replace('\\','/')=='/proc/uptime' else original(path,*args,**kwargs)
        with patch.object(Path,'read_text',read),patch.object(m.shutil,'disk_usage',return_value=SimpleNamespace(free=free)),patch.object(m.subprocess,'run',side_effect=result) as run:
            value=m.start(self.job)
        return value,run
    def test_low_disk_failure_is_durable_and_cannot_start_after_space_is_freed(self):
        failed,run=self.launch(AssertionError('Must not dispatch'),free=100)
        self.assertEqual(failed['phase'],'failed');self.assertEqual(run.call_count,0)
        self.assertEqual(m.read_state()['job_id'],self.job)
        self.assertEqual(m.start('cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb')['phase'],'deferred')
        replay,run=self.launch(AssertionError('Must not redispatch'))
        self.assertEqual(replay,failed);self.assertEqual(run.call_count,0)
    def test_startup_deferral_remains_terminal_for_its_request_after_guest_settles(self):
        deferred,run=self.launch(AssertionError('Must not dispatch'),uptime=10)
        self.assertEqual(deferred['phase'],'deferred');self.assertEqual(run.call_count,0)
        replay,run=self.launch(AssertionError('Must not redispatch'),uptime=1000)
        self.assertEqual(replay,deferred);self.assertEqual(run.call_count,0)
        self.assertEqual(m.read_state()['phase'],'idle')
    def test_cooldown_after_failure_is_parseable_and_cannot_turn_into_a_late_attempt(self):
        old_time=int(m.time.time())-100
        self.save(phase='failed',attempted=old_time,finished=old_time+10,error='Package installation failed (exit 100).')
        self.job='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        deferred=m.start(self.job)
        self.assertEqual(deferred['phase'],'deferred');self.assertEqual(deferred['error'],'')
        self.assertEqual(deferred['attempted'],old_time)
        with patch.object(m.time,'time',return_value=old_time+4*86400):
            replay,run=self.launch(AssertionError('Must not redispatch'))
        self.assertEqual(replay,deferred);self.assertEqual(run.call_count,0)
    def test_setup_deferral_remains_closed_after_setup_finishes(self):
        original=Path.exists
        def exists(path):
            name=str(path).replace('\\','/')
            if name=='/var/lib/cloud/instance':return True
            if name=='/var/lib/cloud/instance/boot-finished':return False
            return original(path)
        with patch.object(Path,'exists',exists):
            deferred,run=self.launch(AssertionError('Must not dispatch'))
        self.assertEqual(deferred['phase'],'deferred');self.assertIn('setup',deferred['detail'])
        replay,run=self.launch(AssertionError('Must not redispatch'))
        self.assertEqual(replay,deferred);self.assertEqual(run.call_count,0)
    def test_start_while_another_job_is_active_fences_only_the_new_request(self):
        self.save(phase='updating');m.unit_active.return_value=True
        requested='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        self.assertEqual(m.start(requested)['job_id'],self.job)
        self.assertEqual(m.read_state()['phase'],'updating')
        m.unit_active.return_value=False
        self.save(phase='completed',attempted=int(m.time.time())-4*86400)
        self.assertEqual(m.start(requested)['phase'],'failed')
    def test_legacy_cancellation_is_still_honored(self):
        (self.root/'cancelled').mkdir()
        now=int(m.time.time())
        record=dict(job_id=self.job,phase='failed',attempted=now,finished=now,error='Cancelled before dispatch.')
        m.atomic(self.root/'cancelled'/(self.job+'.json'),record)
        self.assertEqual(m.start(self.job),record)
    def test_replaying_an_archived_result_after_reboot_does_not_restore_the_reminder(self):
        self.save(phase='completed',attempted=int(m.time.time())-4*86400,reboot_recommended=True)
        m.start(self.job)
        self.save(job_id='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb',phase='completed',reboot_recommended=False)
        m.boot_id.return_value='boot-two'
        self.assertFalse(m.start(self.job)['reboot_recommended'])
    def test_closed_job_replay_cannot_dispatch_after_a_later_job_replaces_it(self):
        old=self.job
        self.save(phase='completed',attempted=int(m.time.time())-4*86400,finished=int(m.time.time())-4*86400+10)
        self.job='cae84a0a-8eb4-41bc-bca4-d9f0954e9bfb'
        self.launch(m.subprocess.TimeoutExpired('systemd-run',20))
        self.save(phase='completed',attempted=int(m.time.time())-4*86400,finished=int(m.time.time())-4*86400+10)
        self.job=old
        replay,run=self.launch(AssertionError('Must not redispatch'))
        self.assertEqual(replay['phase'],'completed');self.assertEqual(replay['job_id'],old);self.assertEqual(run.call_count,0)
    def test_dispatch_timeout_is_persisted_and_never_relaunched(self):
        value,run=self.launch(m.subprocess.TimeoutExpired('systemd-run',20))
        self.assertEqual(value['phase'],'starting');self.assertEqual(m.read_state()['job_id'],self.job)
        self.assertEqual(m.start(self.job)['phase'],'starting');self.assertEqual(run.call_count,1)
        with patch.object(m.time,'time',return_value=value['attempted']+31):
            self.assertEqual(m.reconcile(self.job)['phase'],'failed')
        with self.assertRaisesRegex(RuntimeError,'no longer current'):m.worker(self.job)
    def test_dispatch_failure_is_terminal_and_reports_no_raw_output(self):
        result=m.subprocess.CompletedProcess(['systemd-run'],1,stdout='private output',stderr='private stderr')
        value,run=self.launch(lambda *args,**kwargs:result)
        self.assertEqual(value['phase'],'failed');self.assertNotIn('private',value['error']);self.assertEqual(m.start(self.job)['phase'],'failed')
        self.assertEqual(run.call_count,1)
    def test_package_command_keeps_configs_and_avoids_service_restart(self):
        process=Mock();process.stdout=io.BytesIO(b'ok');process.wait.return_value=0
        context=Mock();context.__enter__=Mock(return_value=process);context.__exit__=Mock(return_value=False)
        with patch.object(m.subprocess,'Popen',return_value=context) as popen:
            self.assertEqual(m.apt(['--yes','--with-new-pkgs','upgrade'],io.BytesIO()),0)
        args=popen.call_args;self.assertIn('Dpkg::Options::=--force-confold',args.args[0]);self.assertEqual(args.kwargs['env']['NEEDRESTART_MODE'],'l');self.assertNotIn('dist-upgrade',args.args[0])

if __name__=='__main__': unittest.main()
