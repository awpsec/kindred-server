"""Installed guest Chromium, disposable display/profile, synthetic loopback login.

Run as bot from a directory containing browser-launch.py, stop-browser.py and
browser-driver.py. Does not restart the VM, any bot screen or an owner browser.
"""
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
import time

source = Path(__file__).resolve().parent
if not (source / 'browser-launch.py').exists():
    source = source.parent / 'deploy'


def module(name):
    spec = importlib.util.spec_from_file_location(name, source / (name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


launch = module('browser-launch')
stop = module('stop-browser')
driver_module = module('browser-driver')


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        if self.path == '/seed':
            self.send_header('Set-Cookie', 'persistent=synthetic; Max-Age=3600; HttpOnly; Path=/')
            self.send_header('Set-Cookie', 'session=synthetic; HttpOnly; Path=/')
        self.end_headers()
        cookie = self.headers.get('Cookie', '')
        flags = {name: name + '=synthetic' in cookie for name in ('persistent', 'session')}
        self.wfile.write(('<title>Synthetic persistence fixture</title><pre>' + json.dumps(flags) + '</pre>').encode())

    def log_message(self, *args):
        pass


def wait_for(check, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(.1)
    raise RuntimeError('Disposable browser fixture deadline')


with tempfile.TemporaryDirectory(prefix='kindred-installed-browser-') as directory:
    root = Path(directory)
    runtime = root / 'runtime'; runtime.mkdir(mode=0o700)
    env = {**os.environ, 'XDG_RUNTIME_DIR': str(runtime), 'XDG_DATA_HOME': str(root / 'data'),
           'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_CACHE_HOME': str(root / 'cache')}
    screen = next(n for n in range(32, 19, -1) if not Path(f'/tmp/.X11-unix/X{n}').exists() and not Path(f'/tmp/.X{n}-lock').exists())
    env['DISPLAY'] = ':' + str(screen)
    display = subprocess.Popen(['Xvfb', env['DISPLAY'], '-screen', '0', '1440x900x24', '-nolisten', 'tcp'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f'http://127.0.0.1:{server.server_port}'
    bus = browser = driver = None

    def desktop():
        global bus
        bus = subprocess.Popen(['dbus-run-session', '--', 'openbox', '--sm-disable'], env=env,
                               start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        def ready():
            try:
                launch.desktop_context(env['DISPLAY']);return True
            except launch.BrowserContextError:
                return False
        wait_for(ready)

    def start_browser(url):
        global browser, driver
        policy = launch.prepare(screen, root=root / 'profiles')
        profile = root / 'profiles' / ('browser-' + str(screen))
        (profile / 'DevToolsActivePort').unlink(missing_ok=True)
        browser = subprocess.Popen([shutil.which('chromium'), *policy['args'], url], env={**env, **policy['env']},
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        def attach():
            global driver
            try:
                driver = driver_module.Driver(profile, '', env['DISPLAY'])
                return True
            except (OSError, ValueError):
                return False
        wait_for(attach)
        def loaded():
            try:
                return evaluate("!!document.querySelector('pre')",page_session())
            except (StopIteration,RuntimeError):
                return False
        wait_for(loaded)
        return profile, policy

    def page_session():
        targets = driver.call('Target.getTargets')['targetInfos']
        page = next(t for t in targets if t['type'] == 'page' and t['url'].startswith(origin))
        return driver.call('Target.attachToTarget', {'targetId': page['targetId'], 'flatten': True})['sessionId']

    def evaluate(expression, session):
        value = driver.call('Runtime.evaluate', {'expression': expression, 'returnByValue': True, 'awaitPromise': True}, session)
        if 'exceptionDetails' in value:
            raise RuntimeError('Synthetic page evaluation failed')
        return value['result'].get('value')

    def close():
        global browser, driver
        driver.ws.socket.close(); driver = None
        stop.close_browser(screen, root=root / 'profiles')
        assert browser.wait(timeout=5) == 0
        prefs = json.loads((root / 'profiles' / ('browser-' + str(screen)) / 'Default/Preferences').read_text())
        assert prefs['profile']['exit_type'] == 'Normal'
        browser = None

    try:
        wait_for(lambda: subprocess.run(['xdpyinfo', '-display', env['DISPLAY']], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0)
        desktop()
        profile, first = start_browser(origin + '/seed')
        inode = profile.stat().st_ino
        session = page_session()
        evaluate("localStorage.setItem('synthetic','present'); new Promise((resolve,reject)=>{let q=indexedDB.open('synthetic',1);q.onupgradeneeded=()=>q.result.createObjectStore('state');q.onsuccess=()=>{let db=q.result,tx=db.transaction('state','readwrite');tx.objectStore('state').put('present','auth');tx.oncomplete=()=>{db.close();resolve(true)};tx.onerror=reject};q.onerror=reject})", session)
        close()
        os.killpg(bus.pid, signal.SIGTERM); bus.wait(timeout=5); bus = None
        display.terminate(); display.wait(timeout=5)
        # Recreate both X11 and DBus using the same synthetic state directories.
        display = subprocess.Popen(['Xvfb', env['DISPLAY'], '-screen', '0', '1440x900x24', '-nolisten', 'tcp'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_for(lambda: subprocess.run(['xdpyinfo', '-display', env['DISPLAY']], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0)
        desktop()
        profile, second = start_browser(origin + '/check')
        assert first['env']['DBUS_SESSION_BUS_ADDRESS'] != second['env']['DBUS_SESSION_BUS_ADDRESS']
        session = page_session()
        state = evaluate("(async()=>({cookies:JSON.parse(document.querySelector('pre').textContent),localStorage:localStorage.getItem('synthetic')==='present',indexedDB:await new Promise((resolve,reject)=>{let q=indexedDB.open('synthetic',1);q.onsuccess=()=>{let db=q.result,r=db.transaction('state').objectStore('state').get('auth');r.onsuccess=()=>{db.close();resolve(r.result==='present')};r.onerror=reject};q.onerror=reject})}))()", session)
        assert state == {'cookies': {'persistent': True, 'session': True}, 'localStorage': True, 'indexedDB': True}, state
        assert profile.stat().st_ino == inode
        close()
        print(json.dumps({'installedChromium': subprocess.check_output(['chromium', '--version'], text=True).strip(),
                          'boundary': 'clean-browser-close-and-X11-DBus-restart', 'state': state,
                          'profileInodePreserved': True, 'cleanExit': True, 'ownerBrowsersTouched': False}))
    finally:
        if driver:
            driver.ws.socket.close()
        if browser and browser.poll() is None:
            browser.terminate(); browser.wait(timeout=5)
        if bus and bus.poll() is None:
            os.killpg(bus.pid, signal.SIGTERM); bus.wait(timeout=5)
        if display.poll() is None:
            display.terminate(); display.wait(timeout=5)
        server.shutdown(); server.server_close()
