"""Real subprocess login relay, with a keyless stand-in for the vendor CLI."""
import concurrent.futures
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

FAKE = r'''#!/usr/bin/python3
import json,os,sys,time
from pathlib import Path
home=Path.home()
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':(home/'fixture-connected').exists(),'authMethod':'claude.ai','email':'fixture@example.invalid','orgId':'fixture-org','accessToken':'secret-never-return'}))
elif sys.argv[1:3]==['auth','logout']:
 (home/'fixture-connected').unlink(missing_ok=True)
elif sys.argv[1:3]==['auth','login']:
 assert '--claudeai' in sys.argv and 'DISPLAY' not in os.environ and os.environ['BROWSER']=='/bin/true'
 with (home/'fixture-starts').open('a') as f:f.write('started\n')
 (home/'fixture-child').write_text(str(os.getpid()))
 url='https://claude.com/cai/oauth/authorize?response_type=code&client_id=official-fixture&state=fixture&code_challenge=fixture'
 sys.stdout.write('\x1b]8;;'+url[:55]);sys.stdout.flush();time.sleep(.03)
 print(url[55:]+'\x1b\\Open Claude\x1b]8;;\x1b\\\nPaste code here if prompted:',flush=True)
 code=sys.stdin.readline().strip()
 if code=='fixture-code#state':
  (home/'fixture-connected').write_text('yes')
  print('Login successful',flush=True)
 else:
  print('Rejected '+code+' secret-never-return',flush=True);sys.exit(1)
'''


class Login(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='kl-')
        self.root = Path(self.temp.name)
        self.home = self.root / 'u'
        self.home.mkdir()
        self.store = self.home / '.kindred-providers/claude-code'
        self.binary = self.root / 'claude'
        self.binary.write_text(FAKE)
        self.binary.chmod(0o755)
        self.helper = self.root / 'provider-cli.py'
        source = Path(__file__).with_name('provider-cli.py').read_text()
        source, replaced = re.subn(r'^CLAUDE = .+$', 'CLAUDE = ' + repr(str(self.binary)), source, count=1, flags=re.M)
        self.assertEqual(replaced, 1, 'Fixture must replace the CLI resolver, never run a real provider')
        self.helper.write_text(source)
        shutil.copy2(Path(__file__).with_name('provider-login.py'), self.root / 'provider-login.py')
        shutil.copy2(Path(__file__).with_name('provider-connectors.py'), self.root / 'provider-connectors.py')
        self.env = {**os.environ, 'HOME': str(self.home)}

    def call(self, action, body=None):
        result = subprocess.run([sys.executable, str(self.helper), action, 'claude-code'],
                                input=json.dumps(body or {}), text=True, capture_output=True,
                                env=self.env, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('secret-never-return', result.stdout + result.stderr)
        self.assertNotIn('fixture-code#state', result.stdout + result.stderr)
        return json.loads(result.stdout)

    def until(self, states):
        for _ in range(60):
            value = self.call('login-status')
            if value['state'] in states:
                return value
            time.sleep(.05)
        self.fail('Login did not reach ' + repr(states))

    def tearDown(self):
        try:
            state = self.call('login-status')
            if state['state'] in {'starting', 'waiting', 'verifying'}:
                self.call('login-cancel', {'attempt': state['attempt']})
            for _ in range(40):
                pidfile = self.store / 'browser-login.pid'
                if not pidfile.exists():
                    break
                pid = json.loads(pidfile.read_text())['pid']
                try:
                    if not Path(f'/proc/{pid}/cmdline').read_bytes():
                        break
                except OSError:
                    break
                time.sleep(.05)
        finally:
            self.temp.cleanup()

    def test_concurrent_login_reuses_one_flow_and_success_is_verified(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: self.call('login'), range(2)))
        self.assertEqual(results[0]['attempt'], results[1]['attempt'])
        self.assertEqual((self.store / 'fixture-starts').read_text().count('started'), 1)
        value = self.until({'waiting'})
        self.assertTrue(value['url'].startswith('https://claude.com/cai/oauth/authorize?'))
        self.assertEqual((self.store / 'browser-login.json').stat().st_mode & 0o777, 0o600)
        self.assertEqual((self.store / 'browser-login.sock').stat().st_mode & 0o777, 0o600)
        stale = self.call('login-complete', {'attempt': 'wrong', 'code': 'fixture-code#state'})
        self.assertEqual(stale['state'], 'expired')
        self.assertFalse((self.store / 'fixture-connected').exists())
        self.call('login-complete', {'attempt': value['attempt'], 'code': 'fixture-code#state'})
        success = self.until({'connected'})
        self.assertTrue(success['connected'])
        self.assertNotIn('url', success)
        self.assertEqual(self.call('login')['state'], 'connected')
        self.assertEqual((self.store / 'fixture-starts').read_text().count('started'), 1)
        for name in ['browser-login.json', 'browser-login.pid']:
            text = (self.store / name).read_text()
            self.assertNotIn('fixture-code#state', text)
            self.assertNotIn('secret-never-return', text)

    def test_cancel_stops_only_pending_flow_and_allows_retry(self):
        first = self.call('login')
        self.call('login-cancel', {'attempt': first['attempt']})
        self.assertEqual(self.until({'cancelled'})['state'], 'cancelled')
        for _ in range(40):
            second = self.call('login')
            if second['state'] == 'waiting':
                break
            time.sleep(.05)
        self.assertEqual(second['state'], 'waiting')
        self.assertNotEqual(first['attempt'], second['attempt'])
        self.assertEqual(self.call('login-complete', {'attempt': first['attempt'], 'code': 'old'})['state'], 'expired')
        self.assertEqual(self.call('login-status')['attempt'], second['attempt'])

    def test_invalid_input_and_vendor_failure_do_not_leak_output(self):
        value = self.call('login')
        invalid = self.call('login-complete', {'attempt': value['attempt'], 'code': 'line1\nline2'})
        self.assertEqual(invalid['state'], 'waiting')
        self.call('login-complete', {'attempt': value['attempt'], 'code': 'bad-code'})
        failed = self.until({'failed'})
        self.assertFalse(failed['connected'])
        self.assertNotIn('bad-code', json.dumps(failed))
        self.assertNotIn('url', failed)

    def test_expiry_removes_link_and_logout_cancels_pending_flow(self):
        login = self.root / 'provider-login.py'
        login.write_text(login.read_text().replace('TTL = 600', 'TTL = 1'))
        first = self.call('login')
        expired = self.until({'expired'})
        self.assertNotIn('url', expired)
        self.assertEqual(self.call('login-complete', {'attempt': first['attempt'], 'code': 'late'})['state'], 'expired')
        login.write_text(login.read_text().replace('TTL = 1', 'TTL = 600'))
        for _ in range(40):
            value = self.call('login')
            if value['state'] == 'waiting':
                break
            time.sleep(.05)
        self.call('logout')
        self.assertEqual(self.until({'cancelled'})['state'], 'cancelled')
        self.assertFalse(self.call('account')['connected'])


if __name__ == '__main__':
    unittest.main()
