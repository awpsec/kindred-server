import importlib.util
import unittest
from unittest.mock import patch
from pathlib import Path
spec = importlib.util.spec_from_file_location('screen_state', Path(__file__).with_name('screen-state.py'))
m = importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class ScreenState(unittest.TestCase):
    def props(self, active, sub='', result='success'):
        return dict(LoadState='loaded', ActiveState=active, SubState=sub, Result=result)
    def test_inactive_is_asleep_and_not_a_failure(self):
        self.assertEqual(m.classify(self.props('inactive')), 'asleep')
    def test_start_and_stop_are_transitions(self):
        self.assertEqual(m.classify(self.props('activating', 'start-post')), 'waking')
        self.assertEqual(m.classify(self.props('deactivating')), 'sleeping')
    def test_failed_restart_is_not_sleep_or_wake(self):
        self.assertEqual(m.classify(self.props('failed', result='exit-code')), 'failed')
        self.assertEqual(m.classify(self.props('activating', 'auto-restart')), 'failed')
    def test_ready_without_any_browser_window(self):
        self.assertEqual(m.classify(self.props('active'), True, True, True), 'ready')
    def test_missing_screen_components_are_failures(self):
        for checks in [(False, True, True), (True, False, True), (True, True, False)]:
            self.assertEqual(m.classify(self.props('active'), *checks), 'failed')
    def test_unknown_never_claims_sleep(self):
        self.assertEqual(m.classify({}), 'unavailable')
        self.assertEqual(m.classify({'ActiveState':'inactive','LoadState':'not-found'}), 'unavailable')
    def test_observing_stopped_screen_only_reads_systemd(self):
        result = type('Result', (), {'returncode':0, 'stdout':'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\n'})()
        with patch.object(m, 'command', return_value=result) as command:
            self.assertEqual(m.observe(5), {'state':'asleep'})
            self.assertEqual(command.call_count, 1)
            self.assertEqual(command.call_args.args[0], ['systemctl','--user','show','kindred-screen@5.service','--property=LoadState,ActiveState,SubState,Result'])
    def test_unknown_service_manager_is_unavailable(self):
        with patch.object(m, 'command', return_value=type('Result', (), {'returncode':1})()):
            self.assertEqual(m.observe(5), {'state':'unavailable'})
    def test_invalid_slot_never_probes(self):
        with patch.object(m, 'command') as command:
            with self.assertRaises(ValueError): m.observe(0)
            command.assert_not_called()
if __name__ == '__main__':unittest.main()
