import importlib.util
import hashlib
import io
import json
from pathlib import Path
import socket
import tempfile
import threading
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).parent

def load(name, file):
    spec=importlib.util.spec_from_file_location(name,ROOT/file);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

bridge=load('cua_adapter','cua-adapter.py')
installer=load('cua_install','install-cua-driver.py')
updater=load('cua_update','update-cua-driver.py')


class MCP:
    def __init__(self):self.calls=[];self.response={'structuredContent':{'effect':'unverifiable'}};self.effects=0;self.closed=False
    def call(self,tool,args):
        self.calls.append((tool,args))
        self.effects+=1
        if self.response=='lost':raise EOFError('Applied but reply lost')
        return self.response
    def close(self):self.closed=True


class Tests(unittest.TestCase):
    def adapter(self):
        m=MCP();a=bridge.Adapter(m,'/managed/profile',':2');a.session='run-A';a.anchor=(123,456);a.binding={'session':'run-A','target_id':'private','tab_id':'private-tab'};a.targets={'one-use':{'ref':'p1:2','actions':['click','type']}};return a,m
    def test_foreground_once_no_background_retry(self):
        a,m=self.adapter();r=a.action('run-A','click','one-use');self.assertTrue(r['action_applied']);self.assertEqual(m.calls[0][1]['delivery_mode'],'foreground');self.assertEqual(m.effects,1);self.assertFalse(a.action('run-A','click','one-use')['uncertain_effect']);self.assertEqual(m.effects,1)
    def test_foreign_session_rejected_before_dispatch(self):
        a,m=self.adapter();r=a.action('run-B','click','one-use');self.assertFalse(r['action_applied']);self.assertEqual(m.effects,0)
    def test_normalized_refusal_never_retries_by_code(self):
        a,m=self.adapter();m.response={'structuredContent':{'effect':'refused','error':{'code':'browser_input_trust_unavailable'}}};r=a.action('run-A','click','one-use');self.assertTrue(r['uncertain_effect']);self.assertEqual(r['page_action_receipt'],m.response);self.assertEqual(m.effects,1);self.assertFalse(a.targets)
    def test_lost_receipt_preserves_uncertainty_no_retry(self):
        a,m=self.adapter();m.response='lost';r=a.action('run-A','type','one-use','partial');self.assertTrue(r['uncertain_effect']);self.assertTrue(r['timed_out']);self.assertTrue(m.closed);self.assertEqual(m.effects,1);self.assertFalse(a.targets)
    def test_invalidate_refuses_old_input(self):
        a,m=self.adapter();a.handle({'op':'invalidate','session':'run-A'});self.assertFalse(a.action('run-A','click','one-use')['action_applied']);self.assertEqual(m.effects,0)
    def test_failing_observation_clears_old_reference(self):
        a,m=self.adapter();r=a.handle({'op':'observe','session':'run-A'});self.assertTrue(r['unavailable']);self.assertFalse(a.targets)
    def test_unsupported_action_no_effect(self):
        a,m=self.adapter();self.assertFalse(a.action('run-A','scroll','one-use')['action_applied']);self.assertEqual(m.effects,0)
    def test_lost_bridge_reply_does_not_reconnect_or_replay(self):
        with tempfile.TemporaryDirectory() as root:
            path=str(Path(root)/'page.sock');listener=socket.socket(socket.AF_UNIX);listener.bind(path);listener.listen(1);effects=[]
            def serve():
                connection,_=listener.accept()
                with connection:effects.append(bridge.receive(connection))
                listener.close()
            thread=threading.Thread(target=serve);thread.start();args=types.SimpleNamespace(socket=path,binary=__file__,expected_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest());result=bridge.client(args,{'op':'click','session':'A','target':'one-use'});thread.join();self.assertTrue(result['uncertain_effect']);self.assertEqual(len(effects),1)
    def test_missing_driver_unavailable_without_fetch(self):
        with tempfile.TemporaryDirectory() as root:
            result=bridge.client(types.SimpleNamespace(socket=str(Path(root)/'page.sock'),binary='/missing-kindred-test-binary',expected_sha256='0'*64),{'op':'observe','session':'A'});self.assertEqual(result,{'unavailable':True})
    def test_observation_new_run_stays_native_without_attachment(self):
        a,m=self.adapter();self.assertEqual(a.handle({'op':'observe','session':'new-run'}),{'unavailable':True});self.assertEqual(m.calls,[]);self.assertFalse(a.targets)
    def test_observation_stale_browser_never_prepares_new_process(self):
        a,m=self.adapter()
        with patch.object(bridge,'browser_pid',return_value=999):
            self.assertEqual(a.handle({'op':'observe','session':'run-A'}),{'unavailable':True})
        self.assertEqual(m.calls,[]);self.assertIsNone(a.anchor);self.assertFalse(a.targets)
    def test_observation_refused_reconnect_stops_at_read_only_bind(self):
        a,m=self.adapter();m.response={'isError':True,'structuredContent':{'status':'refused'}}
        with patch.object(bridge,'browser_pid',return_value=123):
            self.assertEqual(a.handle({'op':'observe','session':'run-A'}),{'unavailable':True})
        self.assertEqual([c[0] for c in m.calls],['get_browser_state']);self.assertFalse(a.targets)
    def test_attachment_missing_endpoint_never_prepares(self):
        a,m=self.adapter()
        with tempfile.TemporaryDirectory() as root:
            a.profile=root
            with patch.object(bridge,'browser_pid',return_value=__import__('os').getpid()):
                self.assertEqual(a.handle({'op':'attach','session':'run-A'}),{'unavailable':True})
        self.assertEqual(m.calls,[]);self.assertIsNone(a.anchor)
    def test_wrong_driver_bytes_never_connect_or_start(self):
        with tempfile.TemporaryDirectory() as root:
            result=bridge.client(types.SimpleNamespace(socket=str(Path(root)/'page.sock'),binary=__file__,expected_sha256='0'*64),{'op':'observe','session':'A'})
            self.assertEqual(result,{'unavailable':True})
            self.assertFalse((Path(root)/'page.sock').exists())
    def test_unattached_observation_never_prepares_or_sends_input(self):
        m=MCP();a=bridge.Adapter(m,'/managed/profile',':2')
        self.assertEqual(a.handle({'op':'observe','session':'A'}),{'unavailable':True})
        self.assertEqual(m.calls,[])
    def test_chromium_joined_process_title_and_empty_environment(self):
        with tempfile.TemporaryDirectory() as root:
            proc=Path(root);entry=proc/'123';entry.mkdir();(entry/'cmdline').write_bytes(b'/opt/chrome --remote-debugging-port=0 --remote-debugging-address=127.0.0.1 --user-data-dir=/managed/profile\0')
            (entry/'environ').write_bytes(b'');(entry/'exe').symlink_to('/opt/chrome')
            original=bridge.Path
            with patch.object(bridge,'Path',side_effect=lambda value:proc if value=='/proc' else original(value)):
                self.assertEqual(bridge.browser_pid('/managed/profile',':2'),123)
            (entry/'environ').write_bytes(b'DISPLAY=:3\0')
            with patch.object(bridge,'Path',side_effect=lambda value:proc if value=='/proc' else original(value)):
                with self.assertRaises(ValueError):bridge.browser_pid('/managed/profile',':2')
            (entry/'environ').write_bytes(b'DISPLAY=:3\0DISPLAY=:2\0')
            with patch.object(bridge,'Path',side_effect=lambda value:proc if value=='/proc' else original(value)):
                with self.assertRaises(ValueError):bridge.browser_pid('/managed/profile',':2')
    def test_wrong_profile_renderer_duplicate_parent_and_embedded_flags_refused(self):
        with tempfile.TemporaryDirectory() as root:
            proc=Path(root);entry=proc/'123';entry.mkdir();(entry/'environ').write_bytes(b'DISPLAY=:2\0');(entry/'exe').symlink_to('/opt/chrome')
            original=bridge.Path
            def attempt(argv):
                (entry/'cmdline').write_bytes(argv)
                with patch.object(bridge,'Path',side_effect=lambda value:proc if value=='/proc' else original(value)):
                    return bridge.browser_pid('/managed/profile',':2')
            cases=[b'/opt/chrome\0--user-data-dir=/foreign/profile\0',
                   b'/opt/chrome\0--user-data-dir=/managed/profile\0--type=renderer\0',
                   b'/opt/chrome\0--user-data-dir=/managed/profile\0--user-data-dir=/foreign/profile\0',
                   b'/opt/chrome\0--app=title --user-data-dir=/managed/profile\0',
                   b'/opt/chrome --app=title --user-data-dir=/managed/profile\0',
                   b'/opt/chrome-link\0--user-data-dir=/managed/profile\0']
            for case in cases:
                with self.subTest(argv=case),self.assertRaises(ValueError):attempt(case)
            (entry/'cmdline').write_bytes(b'/opt/chrome\0--user-data-dir=/managed/profile\0')
            second=proc/'124';second.mkdir();(second/'cmdline').write_bytes((entry/'cmdline').read_bytes());(second/'environ').write_bytes(b'DISPLAY=:2\0');(second/'exe').symlink_to('/opt/chrome')
            with patch.object(bridge,'Path',side_effect=lambda value:proc if value=='/proc' else original(value)),self.assertRaises(ValueError):bridge.browser_pid('/managed/profile',':2')
    def test_duplicate_tab_titles_never_guess_or_keep_old_targets(self):
        a,m=self.adapter();m.response={'structuredContent':{'binding_quality':'exact','mutation_allowed':True,'target_id':'window','tabs':[{'tab_id':'a','active':None},{'tab_id':'b','active':None}]}}
        with patch.object(bridge,'browser_pid',return_value=123):
            self.assertEqual(a.handle({'op':'observe','session':'run-A'}),{'unavailable':True})
        self.assertFalse(a.targets);self.assertIsNone(a.binding)
        self.assertFalse(a.action('run-A','click','one-use')['action_applied'])
        self.assertEqual([c[0] for c in m.calls],['get_browser_state'])
    def test_failed_driver_start_removes_dead_socket(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'page.sock'
            args=types.SimpleNamespace(socket=str(path),binary=__file__,expected_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),display=':2')
            with patch.object(bridge.signal,'signal'),patch.object(bridge,'MCP',side_effect=OSError('Loader could not start selected driver')):
                with self.assertRaises(OSError):bridge.daemon(args)
            self.assertFalse(path.exists(),'A failed loader must not leave a connection target for later screenshots')
    def test_corrupt_payload_keeps_previous_driver(self):
        manifest=json.loads((ROOT/'cua-driver-manifest.json').read_text())
        with tempfile.TemporaryDirectory() as root:
            source=Path(root)/'bad';target=Path(root)/'driver';source.write_bytes(b'wrong');target.write_bytes(b'previous')
            with self.assertRaises(ValueError):installer.install(source,target,manifest)
            self.assertEqual(target.read_bytes(),b'previous')
            with self.assertRaises(ValueError):updater.install(io.BytesIO(b'wrong'),target,manifest['binary_sha256'],manifest['binary_bytes'])
            self.assertEqual(target.read_bytes(),b'previous')

if __name__=='__main__':unittest.main()
