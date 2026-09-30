"""Exercise the real browser driver on an isolated local assessment form.

Usage: python3 tools/test-browser-worker.py --chrome /path/to/chrome --playwright-module /path/to/playwright
The developer fixture uses an installed Node/Playwright browser launcher. The
guest driver under test still uses only Python's standard library and CDP.
No account, VM, scan, external website or Decisions model is used.
"""
import argparse
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('browser_driver', ROOT / 'deploy/browser-driver.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

PAGE = b'''<!doctype html><title>Local assessment fixture</title>
<label><input id="safe" type="checkbox">Safe checks</label>
<label><input id="other" type="checkbox" checked>Preserve this setting</label>
<label>Targets<textarea id="targets">old target</textarea></label>
<label>Policy<select id="policy"><option value="basic">Basic</option><option value="advanced">Advanced</option></select></label>
<label hidden>Password<input type="password" id="secret" value="private-fixture"></label>
<label hidden>Code<input autocomplete="one-time-code" value="654321"></label>
<button id="save" onclick="document.getElementById('result').textContent='Saved locally'">Save configuration</button><p id="result"></p>
<button disabled>Disabled action</button><input hidden value="hidden-fixture">
<script>document.getElementById('targets').addEventListener('input',()=>window.inputReceived=true)</script>'''

BOOTSTRAP = '''const {chromium} = require(process.argv[1]);
(async () => {
  const context = await chromium.launchPersistentContext(process.argv[2], {
    executablePath: process.argv[3], headless: true, timeout: 10000,
    args: ['--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0']
  });
  const stop = async () => { await context.close(); process.exit(0); };
  process.once('SIGTERM', stop);
  process.once('SIGINT', stop);
  process.stdin.resume();
  process.stdin.once('end', stop);
  await context.pages()[0].goto(process.argv[4], {timeout: 10000});
  console.log('ready');
})().catch(error => { console.error(error.message); process.exitCode = 1; });'''


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.end_headers()
        self.wfile.write(PAGE)

    def log_message(self, *args):
        pass


class BrowserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory(prefix='kindred-browser-worker-')
        cls.addClassCleanup(cls.directory.cleanup)
        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.addClassCleanup(cls.server.server_close)
        cls.addClassCleanup(cls.server.shutdown)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.origin = f'http://127.0.0.1:{cls.server.server_port}'
        # Prepare an already-loaded browser, the driver's production entry
        # condition after normal computer tools open the site.
        cls.browser = subprocess.Popen([NODE, '-e', BOOTSTRAP, PLAYWRIGHT_MODULE,
            cls.directory.name, CHROME, cls.origin + '/settings'], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, text=True)
        cls.addClassCleanup(cls.stop_browser)
        ready = queue.Queue()
        threading.Thread(target=lambda: ready.put(cls.browser.stdout.readline()), daemon=True).start()
        if ready.get(timeout=25).strip() != 'ready':
            raise RuntimeError('Isolated Chromium did not start')
        cls.driver = module.Driver(cls.directory.name, (ROOT / 'deploy/browser-observer.js').read_text())
        cls.addClassCleanup(cls.driver.ws.socket.close)
        observation = cls.driver.observe(cls.origin, {})
        if 'Safe checks' not in observation['text']:
            raise RuntimeError('Fixture tab did not become observable')

    @classmethod
    def stop_browser(cls):
        cls.browser.terminate()
        try:
            cls.browser.wait(timeout=5)
        except subprocess.TimeoutExpired:
            cls.browser.kill()
            cls.browser.wait()
        cls.browser.stdin.close()
        cls.browser.stdout.close()

    def setUp(self):
        self.driver.call('Page.navigate', {'url': self.origin + '/settings'}, self.driver.session)
        deadline = time.monotonic() + 5
        while True:
            try:
                observation = self.driver.observe(self.origin, {})
                if 'Safe checks' in observation['text']:
                    break
            except Exception:
                pass
            if time.monotonic() > deadline:
                raise RuntimeError('Fixture did not reload')
            time.sleep(0.05)

    def action(self, observation, label):
        choice = next(c for c in observation['candidates'] if label in c['label'])
        return self.driver.act(observation['snapshot_id'], choice['id'])

    def test_precise_settings_and_exact_multiline_target_list(self):
        targets = '192.0.2.1\n198.51.100.0/24\nexample.test\n\u00e9valuation.test'
        observation = self.driver.observe(self.origin, {'targets': targets})
        self.assertTrue(observation['image'].startswith('data:image/png;base64,'))
        self.assertNotIn('private-fixture', json.dumps(observation))
        self.assertNotIn('654321', json.dumps(observation))
        self.assertNotIn('hidden-fixture', json.dumps(observation))
        self.assertFalse(any('Disabled action' in c['label'] for c in observation['candidates']))
        self.assertTrue(self.action(observation, 'Set checkbox \u201cSafe checks\u201d to checked')['applied'])
        observation = self.driver.observe(self.origin, {'targets': targets})
        self.assertTrue(self.action(observation, 'Fill \u201cTargets\u201d')['applied'])
        self.assertEqual(self.driver.evaluate('document.getElementById("targets").value'), targets)
        self.assertTrue(self.driver.evaluate('window.inputReceived'))
        observation = self.driver.observe(self.origin, {})
        self.assertTrue(self.action(observation, 'to \u201cAdvanced\u201d')['applied'])
        self.assertEqual(self.driver.evaluate('document.getElementById("policy").value'), 'advanced')
        self.assertTrue(self.driver.evaluate('document.getElementById("other").checked'))
        observation = self.driver.observe(self.origin, {})
        self.assertTrue(self.action(observation, 'Click \u201cSave configuration\u201d')['applied'])
        self.assertEqual(self.driver.evaluate('document.getElementById("result").textContent'), 'Saved locally')

    def test_stale_page_and_replayed_choices_are_rejected(self):
        observation = self.driver.observe(self.origin, {})
        self.driver.evaluate('document.getElementById("safe").disabled=true')
        receipt = self.action(observation, 'Set checkbox \u201cSafe checks\u201d')
        self.assertFalse(receipt['applied'])
        self.assertFalse(receipt['uncertain'])
        self.assertFalse(self.driver.evaluate('document.getElementById("safe").checked'))
        self.driver.evaluate('document.getElementById("safe").disabled=false')
        observation = self.driver.observe(self.origin, {})
        self.assertTrue(self.action(observation, 'Set checkbox \u201cSafe checks\u201d')['applied'])
        receipt = self.action(observation, 'Set checkbox \u201cSafe checks\u201d')
        self.assertFalse(receipt['applied'])
        self.assertTrue(self.driver.evaluate('document.getElementById("safe").checked'))

    def test_covered_elements_and_frames_require_fallback(self):
        observation = self.driver.observe(self.origin, {})
        self.driver.evaluate('document.body.insertAdjacentHTML("beforeend", `<div style="position:fixed;inset:0;z-index:9999"></div>`)')
        receipt = self.action(observation, 'Set checkbox \u201cSafe checks\u201d')
        self.assertFalse(receipt['applied'])
        self.assertFalse(self.driver.evaluate('document.getElementById("safe").checked'))
        self.driver.evaluate('document.body.insertAdjacentHTML("beforeend", `<iframe src="about:blank"></iframe>`)')
        with self.assertRaises(RuntimeError):
            self.driver.observe(self.origin, {})

    def test_worker_wire_protocol_and_unavailable_profile(self):
        request = json.dumps({'op': 'hello', 'protocol': 1, 'observer': (ROOT / 'deploy/browser-observer.js').read_text()}) + '\n'
        result = subprocess.run(['python3', str(ROOT / 'deploy/browser-driver.py'), self.directory.name], input=request,
            text=True, capture_output=True, timeout=15, check=True)
        self.assertEqual(json.loads(result.stdout), {'protocol': 1})
        with tempfile.TemporaryDirectory() as absent:
            result = subprocess.run(['python3', str(ROOT / 'deploy/browser-driver.py'), absent], input=request,
                text=True, capture_output=True, timeout=15, check=True)
            self.assertIn('error', json.loads(result.stdout))
            self.assertEqual(result.stderr, '')

    def test_authentication_views_do_not_produce_model_observations(self):
        observation = self.driver.observe(self.origin, {})
        self.driver.evaluate('document.getElementById("secret").parentElement.hidden=false')
        receipt = self.action(observation, 'Set checkbox \u201cSafe checks\u201d')
        self.assertFalse(receipt['applied'])
        self.assertFalse(receipt['uncertain'])
        with self.assertRaises(RuntimeError):
            self.driver.observe(self.origin, {})
        self.assertIsNone(self.driver.snapshot)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--chrome', required=True)
    parser.add_argument('--node', default='node')
    parser.add_argument('--playwright-module', default='playwright')
    args = parser.parse_args()
    CHROME = args.chrome
    NODE = args.node
    PLAYWRIGHT_MODULE = args.playwright_module
    unittest.main(argv=[__file__], verbosity=2)
