#!/usr/bin/python3
"""Guest-local scoped bridge to the pinned Cua MCP process. No model/HTTP server.

The caller owns desktop/human-control authorization. Model arguments never choose
PID, profile, native window, display, raw MCP tools or browser target IDs.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import select
import signal
import shlex
import socket
import subprocess
import sys
import time

LIMIT = 2 * 1024 * 1024


class MCP:
    def __init__(self, binary, display, root):
        env = {**os.environ, 'DISPLAY': display, 'DO_NOT_TRACK': '1',
               'CUA_TELEMETRY': '0', 'CUA_DRIVER_RS_UPDATE_CHECK': 'false',
               'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_CACHE_HOME': str(root / 'cache'),
               'XDG_DATA_HOME': str(root / 'data'), 'CUA_HOME': str(root / 'cua'),
               'XDG_SESSION_TYPE': 'x11', 'GDK_BACKEND': 'x11'}
        # Input stays on this bot's Xvfb, never an inherited Wayland/portal seat.
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('WAYLAND_SOCKET', None)
        self.child = subprocess.Popen([binary, 'mcp', '--direct', '--no-overlay', '--grant', 'existing-profile'],
                                     env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, bufsize=0)
        self.sequence = 0
        try:
            self.request('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                                       'clientInfo': {'name': 'kindred-guest', 'version': '1'}})
        except Exception:
            self.close()
            raise

    def request(self, method, params):
        self.sequence += 1
        message = {'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}
        self.child.stdin.write(json.dumps(message).encode() + b'\n')
        buffer = bytearray()
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            ready = select.select([self.child.stdout], [], [], max(0, deadline-time.monotonic()))[0]
            if not ready:
                break
            byte = os.read(self.child.stdout.fileno(), 1)
            if not byte:
                raise EOFError('Page connection ended')
            buffer.extend(byte)
            if len(buffer) > LIMIT:
                raise ValueError('Page observation too large')
            if byte == b'\n':
                value = json.loads(buffer)
                buffer.clear()
                if value.get('id') == self.sequence:
                    if 'error' in value:
                        raise ValueError('Page request rejected')
                    return value['result']
        raise TimeoutError('Page response timed out')

    def call(self, tool, arguments):
        return self.request('tools/call', {'name': tool, 'arguments': arguments})

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()


def browser_arguments(entry):
    args = [arg for arg in (entry/'cmdline').read_bytes().split(b'\0') if arg]
    # Chromium can move argv/environment and expose one joined process title.
    # Trusted managed profiles have no whitespace; ambiguous titles fail closed.
    if len(args) == 1:
        args = [part.encode() for part in shlex.split(args[0].decode())]
    return args


def browser_pid(profile, display):
    """Exactly one browser parent whose argv/profile and environment match."""
    matches = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            args = browser_arguments(entry)
            env = (entry/'environ').read_bytes().split(b'\0')
            if ([a for a in args if a.startswith(b'--user-data-dir=')] == [f'--user-data-dir={profile}'.encode()] and
                not any(a.startswith((b'--type=', b'--app=')) for a in args) and
                bool(args) and os.path.abspath(os.fsdecode(args[0])) == os.path.abspath(os.readlink(entry/'exe')) and
                Path(os.readlink(entry/'exe')).name in ('chrome', 'chromium', 'chromium-browser') and
                [e for e in env if e.startswith(b'DISPLAY=')] in ([], [f'DISPLAY={display}'.encode()]) and
                entry.stat().st_uid == os.getuid()):
                matches.append(int(entry.name))
        except (OSError, PermissionError, ValueError):
            pass
    if len(matches) != 1:
        raise ValueError('The current browser is not available')
    return matches[0]


def structured(result):
    data = result.get('structuredContent', {})
    if result.get('isError') or data.get('status') == 'refused' or data.get('effect') == 'refused':
        raise ValueError('The page could not be observed')
    return data


class Adapter:
    def __init__(self, mcp, profile, display):
        self.mcp, self.profile, self.display = mcp, profile, display
        self.session = None
        self.targets = {}
        self.binding = None
        self.anchor = None

    def invalidate(self):
        self.targets.clear()
        self.binding = None

    def attach(self, session):
        self.invalidate()
        self.anchor = None
        pid = browser_pid(self.profile, self.display)
        args = browser_arguments(Path('/proc')/str(pid))
        port_file = Path(self.profile)/'DevToolsActivePort'
        if (b'--remote-debugging-port=0' not in args or
            b'--remote-debugging-address=127.0.0.1' not in args or
            not port_file.is_file() or port_file.is_symlink()):
            return {'unavailable': True}
        if self.session != session:
            if self.session:
                self.mcp.call('end_session', {'session': self.session})
            self.session = session
            structured(self.mcp.call('start_session', {'session': session}))
        # Exact PID/window binding on this driver's X11 display is mandatory,
        # including when Chromium cleared its original /proc environment block.
        windows = structured(self.mcp.call('list_windows', {'session': session, 'pid': pid}))['windows']
        if len(windows) != 1:
            return {'unavailable': True}
        window = windows[0]['window_id']
        prepared = structured(self.mcp.call('browser_prepare', {'session': session, 'pid': pid,
                            'window_id': window, 'strategy': {'kind': 'existing_profile'}}))
        if not prepared.get('prepared'):
            return {'unavailable': True}
        # Only this explicit acting path can prepare an endpoint. Screenshots
        # never call browser_prepare or its native browser-settings fallback.
        self.anchor = (pid, window)
        return {'attached': True}

    def observe(self, session):
        self.invalidate()  # Previous observations cannot survive a failed capture.
        if self.session != session or self.anchor is None:
            return {'unavailable': True}
        pid, window = self.anchor
        if browser_pid(self.profile, self.display) != pid:
            self.anchor = None
            return {'unavailable': True}
        bound = structured(self.mcp.call('get_browser_state', {'session': session, 'pid': pid, 'window_id': window}))
        if bound.get('binding_quality') != 'exact' or not bound.get('mutation_allowed'):
            raise ValueError('The current page could not be identified')
        active = [t for t in bound['tabs'] if t.get('active')]
        if len(active) != 1:
            raise ValueError('Choose one browser tab before continuing')
        self.binding = {'session': session, 'target_id': bound['target_id'], 'tab_id': active[0]['tab_id']}
        page = structured(self.mcp.call('get_browser_state', {**self.binding, 'snapshot_format': 'semantic_v2'}))
        elements = []
        for ref in page.get('refs', [])[:160]:
            token = secrets.token_hex(16)
            self.targets[token] = ref
            elements.append({'target': token, **{k: ref.get(k) for k in ('role', 'name', 'value', 'actions', 'states')}})
        # Grounding strings are data, never executable page instructions.
        return {'page': page.get('page'), 'elements': elements,
                'content': [{k: r.get(k) for k in ('role', 'name', 'value')} for r in page.get('content_refs', [])[:160]], 'complete': page.get('snapshot', {}).get('complete', False)}

    def action(self, session, operation, target, text=None):
        ref = self.targets.get(target)
        if session != self.session or not ref or not self.binding:
            return {'failed': True, 'action_applied': False, 'uncertain_effect': False,
                    'text': 'The page changed. Take a fresh screenshot before trying again.'}
        if operation not in ('click', 'type') or operation not in ref.get('actions', []):
            return {'failed': True, 'action_applied': False, 'uncertain_effect': False,
                    'text': 'This page item does not support that action. Inspect the page again.'}
        if operation == 'type' and (not isinstance(text, str) or len(text.encode()) > 16000):
            return {'failed': True, 'action_applied': False, 'text': 'Text is too long or missing.'}
        binding = dict(self.binding)
        self.invalidate()  # One-use targets, even if the reply is lost.
        arguments = {**binding, 'ref': ref['ref']}
        if operation == 'click':
            arguments['delivery_mode'] = 'foreground'  # Never retry a background refusal.
        else:
            arguments['text'] = text
        try:
            result = self.mcp.call('browser_'+operation, arguments)
            data = result.get('structuredContent', {})
            # Upstream normalizes both pre- and post-delivery failures to refused;
            # it drops attempted=false. Fail closed without code-based fallback.
            if result.get('isError') or data.get('effect') not in ('applied', 'unverifiable'):
                return {'failed': True, 'uncertain_effect': True, 'action_applied': False,
                        'page_action_receipt': result,
                        'text': 'The action is not confirmed. Do not repeat it. Inspect the page to check what happened.'}
            return {'action_applied': True, 'text': 'Input delivered. Inspect the fresh image and page values to verify the result.',
                    'delivery_confirmed': data.get('effect') == 'applied', 'page_action_receipt': result}
        except Exception:
            self.mcp.close()
            return {'failed': True, 'uncertain_effect': True, 'timed_out': True, 'action_applied': False,
                    'text': 'The action is not confirmed. Do not repeat it. Inspect the page to check what happened.'}

    def handle(self, request):
        session = request.get('session')
        if not isinstance(session, str) or not session or len(session) > 100:
            return {'failed': True, 'text': 'Page session unavailable.'}
        op = request.get('op')
        if op == 'invalidate':
            self.invalidate()
            return {'invalidated': True}
        if op in ('observe', 'attach'):
            try:
                return self.observe(session) if op == 'observe' else self.attach(session)
            except Exception:
                self.invalidate()
                return {'unavailable': True}
        return self.action(session, op, request.get('target'), request.get('text'))


def receive(connection):
    data = bytearray()
    while not data.endswith(b'\n'):
        part = connection.recv(65536)
        if not part:
            raise EOFError('No page request')
        data.extend(part)
        if len(data) > 65536:
            raise ValueError('Page request too large')
    return json.loads(data)


def verified_binary(arguments):
    try:
        binary = Path(arguments.binary)
        return (not binary.is_symlink() and binary.is_file() and
                hashlib.sha256(binary.read_bytes()).hexdigest() == arguments.expected_sha256)
    except OSError:
        return False


def daemon(arguments):
    if not verified_binary(arguments):
        return
    def terminate(signum, frame):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, terminate)
    root = Path(arguments.socket).parent
    with open(root/'startup.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = Path(arguments.socket)
        path.unlink(missing_ok=True)
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(path)); path.chmod(0o600); listener.listen(4); listener.settimeout(300)
            mcp = None
            try:
                mcp = MCP(arguments.binary, arguments.display, root)
                adapter = Adapter(mcp, arguments.profile, arguments.display)
                while True:
                    try:
                        connection, _ = listener.accept()
                    except socket.timeout:
                        break
                    with connection:
                        connection.settimeout(20)
                        try:
                            request = receive(connection)
                            # Recovery is explicit and read-only: only a newly
                            # approved page-open attachment can replace a dead
                            # driver. Never replay the action that lost a receipt.
                            if request.get('op') == 'attach' and mcp.child.poll() is not None:
                                mcp = MCP(arguments.binary, arguments.display, root)
                                adapter = Adapter(mcp, arguments.profile, arguments.display)
                            result = adapter.handle(request)
                            connection.sendall(json.dumps(result).encode()+b'\n')
                        except (OSError, ValueError, EOFError):
                            adapter.invalidate()
            finally:
                if mcp is not None:mcp.close()
                path.unlink(missing_ok=True)


def client(arguments, request):
    root = Path(arguments.socket).parent
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        return {'unavailable': True}
    if request.get('op') != 'invalidate' and not verified_binary(arguments):
        return {'unavailable': True}
    connection = None
    # Reconnect only before dispatch; once sent, a lost receipt is never retried.
    for attempt in range(30):
        try:
            connection = socket.socket(socket.AF_UNIX); connection.connect(arguments.socket); break
        except OSError:
            connection.close(); connection = None
            if attempt == 0 and verified_binary(arguments):
                subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--daemon',
                                  '--socket', arguments.socket, '--binary', arguments.binary,
                                  '--display', arguments.display, '--profile', arguments.profile,
                                  '--expected-sha256', arguments.expected_sha256],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)
            time.sleep(.05)
    if connection is None:
        return {'unavailable': True}
    with connection:
        connection.settimeout(20)
        try:
            connection.sendall(json.dumps(request).encode()+b'\n')
            data = bytearray()
            while not data.endswith(b'\n'):
                chunk = connection.recv(65536)
                if not chunk:raise EOFError('Page response ended')
                data.extend(chunk)
                if len(data) > LIMIT:raise ValueError('Page response too large')
            return json.loads(data)
        except Exception:
            if request.get('op') in ('click', 'type'):
                return {'failed': True, 'uncertain_effect': True, 'timed_out': True,
                        'text': 'The action is not confirmed. Do not repeat it. Inspect the page to check what happened.'}
            return {'unavailable': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--daemon', action='store_true')
    parser.add_argument('--socket', required=True)
    parser.add_argument('--binary', default='/usr/local/lib/kindred/cua-driver')
    parser.add_argument('--expected-sha256', required=True)
    parser.add_argument('--display', required=True)
    parser.add_argument('--profile', required=True)
    args = parser.parse_args()
    if args.daemon:
        daemon(args)
    else:
        request = json.load(sys.stdin)
        print(json.dumps(client(args, request)))
