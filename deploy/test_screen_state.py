import importlib.util
import unittest
import socket
import threading
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
class BannerStream:
    def __init__(self, chunks):
        self.chunks = list(chunks)
        self.timeouts = []
        self.read_sizes = []
    def __enter__(self): return self
    def __exit__(self, *_): pass
    def settimeout(self, value): self.timeouts.append(value)
    def recv(self, size):
        self.read_sizes.append(size)
        return self.chunks.pop(0) if self.chunks else b''

class RfbReadiness(unittest.TestCase):
    def ready(self, chunks):
        stream = BannerStream(chunks)
        with patch.object(m.socket, 'create_connection', return_value=stream) as connect:
            result = m.rfb_ready(5908)
            connect.assert_called_once_with(('127.0.0.1', 5908), timeout=0.5)
        return result, stream
    def test_fragmented_and_one_byte_banners(self):
        for chunks in [[b'RFB 003.008\n'], [b'R', b'FB 003.008\n'],
                       [bytes([b]) for b in b'RFB 003.008\n']]:
            with self.subTest(chunks=chunks):
                result, stream = self.ready(chunks)
                self.assertTrue(result)
                self.assertTrue(all(0 < t <= 0.5 for t in stream.timeouts))
                self.assertEqual(stream.read_sizes[-1], len(chunks[-1]))
    def test_invalid_or_truncated_banners(self):
        for banner in [b'', b'RFB ', b'RFB 003.008', b'RFB 003x008\n',
                       b'RFB abc.008\n', b'RFB 003.008X', b'HTTP/1.1 200']:
            with self.subTest(banner=banner): self.assertFalse(self.ready([banner])[0])
    def test_connection_and_receive_failures(self):
        for error in [ConnectionRefusedError(), socket.timeout()]:
            with patch.object(m.socket, 'create_connection', side_effect=error):
                self.assertFalse(m.rfb_ready(5908))
        stream = BannerStream([])
        with patch.object(m.socket, 'create_connection', return_value=stream), \
             patch.object(stream, 'recv', side_effect=socket.timeout()):
            self.assertFalse(m.rfb_ready(5908))
    def test_deadline_is_shared_across_fragments(self):
        stream = BannerStream([b'R', b'F', b'B'])
        with patch.object(m.socket, 'create_connection', return_value=stream), \
             patch.object(m.time, 'monotonic', side_effect=[10, 10.1, 10.3, 10.51]):
            self.assertFalse(m.rfb_ready(5908))
        self.assertEqual(stream.chunks, [b'B'])
        self.assertAlmostEqual(stream.timeouts[0], 0.4)
        self.assertAlmostEqual(stream.timeouts[1], 0.2)
    def test_connect_time_uses_the_same_deadline(self):
        stream = BannerStream([b'RFB 003.008\n'])
        with patch.object(m.socket, 'create_connection', return_value=stream), \
             patch.object(m.time, 'monotonic', side_effect=[10, 10.51]):
            self.assertFalse(m.rfb_ready(5908))
        self.assertEqual(stream.read_sizes, [])
    def test_late_complete_banner_is_not_ready(self):
        stream = BannerStream([b'RFB 003.008\n'])
        with patch.object(m.socket, 'create_connection', return_value=stream), \
             patch.object(m.time, 'monotonic', side_effect=[10, 10.1, 10.51]):
            self.assertFalse(m.rfb_ready(5908))
    def test_real_socket_fragmentation(self):
        reader, writer = socket.socketpair()
        requested = threading.Event()
        class SocketReader:
            def __enter__(self): return self
            def __exit__(self, *_): reader.close()
            def settimeout(self, value): reader.settimeout(value)
            def recv(self, size):
                if not requested.is_set():
                    chunk = reader.recv(size)
                    requested.set()
                    return chunk
                return reader.recv(size)
        def send():
            with writer:
                writer.sendall(b'R')
                if requested.wait(2): writer.sendall(b'FB 003.008\n')
        thread = threading.Thread(target=send)
        thread.start()
        try:
            with patch.object(m.socket, 'create_connection', return_value=SocketReader()):
                self.assertTrue(m.rfb_ready(5908))
        finally:
            requested.set(); thread.join(2)
        self.assertFalse(thread.is_alive())
    def test_both_listeners_are_checked_and_required(self):
        result = type('Result', (), {'returncode':0, 'stdout':'LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\n'})()
        for states in [(True, True), (True, False), (False, True), (False, False)]:
            with self.subTest(states=states), patch.object(m, 'command', return_value=result) as command, \
                 patch.object(m, 'rfb_ready', side_effect=states) as probe:
                self.assertEqual(m.observe(5), {'state':'ready' if all(states) else 'failed'})
                self.assertEqual([call.args[0] for call in probe.call_args_list], [5908, 5909])
                self.assertEqual([call.args[0][0] for call in command.call_args_list], ['systemctl', 'xdpyinfo', 'xdotool'])

if __name__ == '__main__':unittest.main()
