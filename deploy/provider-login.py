#!/usr/bin/python3
"""Relay the official Claude CLI's login link and one-use return code.

The vendor process owns OAuth and its credential files. This bridge neither reads
those files nor implements token exchange. Terminal output and submitted codes
are never saved or returned. One bounded login runs per profile.
"""
import fcntl
import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import subprocess
import sys
import tempfile
import time
from urllib.parse import parse_qs, urlsplit
import uuid

TTL = 600
ACTIVE = {'starting', 'waiting', 'verifying'}
ENDPOINTS = {('claude.com', '/cai/oauth/authorize'),
             ('claude.ai', '/oauth/authorize')}


def valid_url(value):
    try:
        u = urlsplit(value)
        q = parse_qs(u.query)
        return (len(value) <= 16384 and u.scheme == 'https' and
                not u.username and not u.password and not u.port and not u.fragment and
                (u.hostname, u.path) in ENDPOINTS and q.get('response_type') == ['code'] and
                all(q.get(k) for k in ('client_id', 'state', 'code_challenge')))
    except (TypeError, ValueError):
        return False


def write(home, name, value):
    with tempfile.NamedTemporaryFile(mode='w', dir=home, delete=False) as f:
        json.dump(value, f)
        temporary = f.name
    os.replace(temporary, home / name)


def read(home, name):
    try:
        value = json.loads((home / name).read_text())
        return value if isinstance(value, dict) else {}
    except (OSError, ValueError):
        return {}


def public(value):
    state = value.get('state', 'idle')
    result = {k: value[k] for k in ('attempt', 'state', 'expires_at', 'message') if k in value}
    result['state'] = state
    result['connected'] = state == 'connected'
    if state == 'waiting' and valid_url(value.get('url', '')):
        result['url'] = value['url']
    return result


def alive(home, attempt):
    pid = read(home, 'browser-login.pid')
    try:
        if pid.get('attempt') != attempt:
            return False
        process = Path('/proc') / str(int(pid['pid']))
        args = (process / 'cmdline').read_bytes().split(b'\0')
        return (process.stat().st_uid == os.getuid() and
                str(Path(__file__).resolve()).encode() in args and
                b'run' in args and attempt.encode() in args)
    except (KeyError, OSError, ValueError, AttributeError):
        return False


def status(home):
    value = read(home, 'browser-login.json')
    if value.get('state') in ACTIVE:
        if time.time() >= value.get('expires_at', 0):
            value.update(state='expired', message='Sign-in expired. Choose Sign in to try again.')
        elif not alive(home, value.get('attempt', '')):
            value.update(state='failed', message='Sign-in stopped. Choose Sign in to try again.')
    return public(value)


def stop_legacy_terminal(home, binary):
    # A new user-initiated sign-in replaces only this profile's old login window.
    try:
        pid = int((home / 'login.pid').read_text())
        process = Path('/proc') / str(pid)
        args = (process / 'cmdline').read_bytes().split(b'\0')
        env = (process / 'environ').read_bytes().split(b'\0')
        if (process.stat().st_uid == os.getuid() and args[0].endswith(b'xterm') and
                binary.encode() in args and b'auth' in args and b'login' in args and
                ('HOME=' + str(home)).encode() in env):
            os.killpg(pid, signal.SIGTERM)
    except (OSError, ValueError, IndexError):
        pass


def request(binary, env, action, payload):
    home = Path(env['HOME'])
    if action == 'login-status':
        return status(home)
    with open(home / 'login.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        current = status(home)
        if action == 'login':
            if current['state'] in ACTIVE:
                return current
            if alive(home, current.get('attempt', '')):
                return {**current, 'message': 'The previous sign-in is closing. Choose Sign in again in a moment.'}
            stop_legacy_terminal(home, binary)
            attempt = uuid.uuid4().hex
            write(home, 'browser-login.json', {'attempt': attempt, 'state': 'starting',
                  'expires_at': time.time() + TTL, 'message': 'Preparing Claude sign-in…'})
            child_env = {**env, 'BROWSER': '/bin/true'}
            child_env.pop('DISPLAY', None)
            child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                      'run', binary, attempt], env=child_env, cwd=home,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True)
            write(home, 'browser-login.pid', {'pid': child.pid, 'attempt': attempt})
            for _ in range(80):
                current = status(home)
                if current['state'] != 'starting':
                    break
                time.sleep(.1)
            return current
        if action not in ('login-complete', 'login-cancel'):
            raise ValueError('Unsupported login action')
        if payload.get('attempt') != current.get('attempt') or current['state'] not in ACTIVE:
            return {'state': 'expired', 'connected': False,
                    'message': 'This sign-in is no longer active. Choose Sign in to start again.'}
        if action == 'login-complete':
            code = payload.get('code', '')
            code = code.strip() if isinstance(code, str) else ''
            if not (isinstance(code, str) and 1 <= len(code) <= 4096 and
                    all(33 <= ord(c) <= 126 for c in code) and '://' not in code):
                return {**current, 'message': 'Paste only the complete code shown by Claude.'}
        packet = {'action': action, 'attempt': current['attempt']}
        if action == 'login-complete':
            packet['code'] = code
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(str(home / 'browser-login.sock'))
            client.sendall(json.dumps(packet).encode() + b'\n')
            with client.makefile('rb') as reply:
                return json.loads(reply.readline(32768))


def run(binary, attempt):
    home = Path.home()
    value = read(home, 'browser-login.json')
    if value.get('attempt') != attempt:
        return
    child = None
    selector = selectors.DefaultSelector()
    listener = socket.socket(socket.AF_UNIX)
    address = home / 'browser-login.sock'
    terminal = False

    def update(state, message, url=None):
        value.update(state=state, message=message)
        value.pop('url', None)
        if url:
            value['url'] = url
        write(home, 'browser-login.json', value)

    try:
        address.unlink(missing_ok=True)
        listener.bind(str(address))
        address.chmod(0o600)
        listener.listen(4)
        selector.register(listener, selectors.EVENT_READ)
        child = subprocess.Popen([binary, 'auth', 'login', '--claudeai'],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        selector.register(child.stdout, selectors.EVENT_READ)
        output = b''
        while time.time() < value['expires_at']:
            for key, _ in selector.select(.2):
                if key.fileobj is listener:
                    connection, _ = listener.accept()
                    with connection:
                        connection.settimeout(2)
                        with connection.makefile('rb') as stream:
                            raw = stream.readline(16384)
                        try:
                            packet = json.loads(raw)
                            assert packet.get('attempt') == attempt
                            if packet.get('action') == 'login-cancel':
                                # A caller may sign out immediately after this reply.
                                # Stop token exchange before acknowledging cancellation.
                                if child.poll() is None:
                                    os.killpg(child.pid, signal.SIGTERM)
                                    try:
                                        child.wait(timeout=3)
                                    except subprocess.TimeoutExpired:
                                        os.killpg(child.pid, signal.SIGKILL)
                                        child.wait()
                                update('cancelled', 'Sign-in cancelled. Your saved account was not signed out.')
                                terminal = True
                            elif packet.get('action') == 'login-complete' and value['state'] == 'waiting':
                                code = packet['code']
                                assert isinstance(code, str) and 1 <= len(code) <= 4096
                                assert all(33 <= ord(c) <= 126 for c in code) and '://' not in code
                                child.stdin.write(code.encode() + b'\n')
                                child.stdin.flush()
                                update('verifying', 'Claude is verifying your sign-in…')
                                code = ''
                            else:
                                raise ValueError('Stale login')
                            connection.sendall(json.dumps(public(value)).encode() + b'\n')
                        except (ValueError, KeyError, AssertionError, BrokenPipeError):
                            connection.sendall(json.dumps({'state': value['state'], 'attempt': attempt,
                                'message': 'The code could not be submitted. Check sign-in status and try again.'}).encode() + b'\n')
                else:
                    chunk = os.read(key.fd, 8192)
                    if not chunk:
                        selector.unregister(child.stdout)
                        continue
                    output += chunk
                    if len(output) > 256 * 1024:
                        raise ValueError('Login output limit')
                    text = output.decode(errors='replace')
                    if value['state'] == 'starting':
                        for match in re.finditer(r'https://[^\s\x00-\x20\x7f<>"\x1b]+', text):
                            url = match.group()
                            if match.end() < len(text) and valid_url(url):
                                update('waiting', 'Finish signing in in your browser.', url)
                                break
            if terminal:
                break
            if child.poll() is not None:
                result = subprocess.run([binary, 'auth', 'status', '--json'],
                                        capture_output=True, timeout=25)
                try:
                    connected = child.returncode == 0 and json.loads(result.stdout).get('loggedIn') is True
                except ValueError:
                    connected = False
                update('connected' if connected else 'failed',
                       'Claude is connected for this profile’s bots.' if connected else
                       'Claude did not complete sign-in. Choose Sign in and use the newest link and code.')
                terminal = True
                break
        if not terminal:
            update('expired', 'Sign-in expired. Choose Sign in to try again.')
    except Exception:
        update('failed', 'Claude sign-in stopped. Choose Sign in to try again.')
    finally:
        if child and child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        selector.close()
        listener.close()
        address.unlink(missing_ok=True)


if __name__ == '__main__':
    if len(sys.argv) == 4 and sys.argv[1] == 'run':
        run(sys.argv[2], sys.argv[3])
