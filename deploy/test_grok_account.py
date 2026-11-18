import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
import uuid

spec = importlib.util.spec_from_file_location('grok', Path(__file__).with_name('grok-account.py'))
grok = importlib.util.module_from_spec(spec)
spec.loader.exec_module(grok)


class Isolation(unittest.TestCase):
    def test_private_account_homes_and_environment(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'providers'
            a, b = [grok.account_directory(root, str(uuid.uuid4())) for _ in range(2)]
            self.assertNotEqual(a, b)
            for path in (a, b):
                self.assertEqual(path.stat().st_mode & 0o777, 0o700)
                env = grok.environment(path)
                self.assertEqual(env['HOME'], str(path))
                self.assertEqual(env['GROK_HOME'], str(path / '.grok'))
                self.assertNotIn('XAI_API_KEY', env)

    def test_reject_symlink_and_path_account(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'providers'
            home = grok.account_directory(root, str(uuid.uuid4()))
            link = home.with_name(str(uuid.uuid4()))
            link.symlink_to(home)
            for account in (link.name, '../elsewhere', home.name.upper()):
                with self.assertRaises(ValueError):
                    grok.account_directory(root, account)


class NativeInference(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('GROK_TEST_BINARY'), 'set GROK_TEST_BINARY to the pinned CLI')
    def test_native_wire_has_no_tools_and_returns_fixture_reply(self):
        import json
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        model_requests = []
        class Fixture(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                tools = [tool['function']['name'] for tool in request.get('tools', [])]
                # Grok may issue one separate request for an internal session title.
                if tools != ['session_title']:
                    model_requests.append(tools)
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                for delta, stop in [({'role': 'assistant', 'content': 'Fixture reply'}, None), ({}, 'stop')]:
                    frame = {'id': 'fixture', 'model': 'grok-fixture', 'object': 'chat.completion.chunk',
                             'created': 1, 'choices': [{'index': 0, 'delta': delta, 'finish_reason': stop}]}
                    self.wfile.write(('data: ' + json.dumps(frame) + '\n\n').encode())
                self.wfile.write(b'data: [DONE]\n\n')

        server = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            home = grok.account_directory(root / 'providers', str(uuid.uuid4()))
            env = grok.environment(home)
            cwd = root / 'work'
            cwd.mkdir()
            binary = os.environ['GROK_TEST_BINARY']
            grok.verify_inference_configuration(binary, env, cwd)
            # Test provider override only exists in this disposable home.
            cli_home = Path(env['GROK_HOME'])
            cli_home.mkdir(exist_ok=True)
            (cli_home / 'config.toml').write_text('[model.grok-fixture]\nmodel="fixture"\n'
                f'base_url="http://127.0.0.1:{server.server_port}/v1"\nenv_key="FIXTURE_KEY"\n')
            with self.assertRaises(ValueError):
                grok.verify_inference_configuration(binary, env, cwd)
            env['FIXTURE_KEY'] = 'synthetic'
            self.assertEqual(grok.infer_text(binary, env, cwd, 'grok-fixture', 'Hello')['text'], 'Fixture reply')
            self.assertTrue(model_requests)
            self.assertTrue(all(tools == [] for tools in model_requests))
