"""Persistent, guest-local CDP driver. Attaches to an existing browser only.

The JSONL wire protocol is Kindred-owned. No API authentication or model calls.
Uses the standard library so existing guests need no new Python dependencies.
"""
import base64
import hashlib
import json
from pathlib import Path
import secrets
import socket
import struct
import sys
import time
from urllib.parse import urlsplit

LIMIT = 8 * 1024 * 1024


class WebSocket:
    def __init__(self, url):
        parts = urlsplit(url)
        if parts.scheme != 'ws' or parts.hostname != '127.0.0.1' or not parts.port or parts.username or parts.password:
            raise ValueError('Only guest-loopback browser sockets are supported')
        self.socket = socket.create_connection(('127.0.0.1', parts.port), timeout=10)
        self.socket.settimeout(10)
        nonce = base64.b64encode(secrets.token_bytes(16)).decode()
        request = (f'GET {parts.path} HTTP/1.1\r\nHost: 127.0.0.1:{parts.port}\r\nUpgrade: websocket\r\n'
                   f'Connection: Upgrade\r\nSec-WebSocket-Key: {nonce}\r\nSec-WebSocket-Version: 13\r\n\r\n')
        self.socket.sendall(request.encode())
        header = bytearray()
        while not header.endswith(b'\r\n\r\n'):
            header.extend(self.read(1))
            if len(header) > 16384:
                raise ValueError('Browser handshake too large')
        expected = base64.b64encode(hashlib.sha1((nonce + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        headers = dict(line.split(':', 1) for line in header.decode().split('\r\n')[1:] if ':' in line)
        if header.split(b'\r\n')[0] != b'HTTP/1.1 101 WebSocket Protocol Handshake' and not header.startswith(b'HTTP/1.1 101 '):
            raise ValueError('Browser rejected WebSocket handshake')
        if {k.lower(): v.strip() for k, v in headers.items()}.get('sec-websocket-accept') != expected:
            raise ValueError('Invalid browser WebSocket handshake')

    def read(self, count):
        result = bytearray()
        while len(result) < count:
            chunk = self.socket.recv(count - len(result))
            if not chunk:
                raise EOFError('Browser disconnected')
            result.extend(chunk)
        return bytes(result)

    def send(self, payload, opcode=1):
        mask = secrets.token_bytes(4)
        size = len(payload)
        if size > LIMIT:
            raise ValueError('Browser message too large')
        prefix = bytes([0x80 | opcode])
        if size < 126:
            prefix += bytes([0x80 | size])
        elif size < 65536:
            prefix += bytes([0x80 | 126]) + struct.pack('!H', size)
        else:
            prefix += bytes([0x80 | 127]) + struct.pack('!Q', size)
        self.socket.sendall(prefix + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def receive(self):
        result = bytearray()
        started = False
        while True:
            first, second = self.read(2)
            opcode, finished = first & 15, bool(first & 0x80)
            if first & 0x70 or second & 0x80:
                raise ValueError('Unsupported browser frame')
            size = second & 127
            if size == 126:
                size = struct.unpack('!H', self.read(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.read(8))[0]
            if len(result) + size > LIMIT:
                raise ValueError('Browser message too large')
            payload = self.read(size)
            if opcode == 8:
                raise EOFError('Browser closed connection')
            if opcode in (9, 10):
                if not finished or size > 125:
                    raise ValueError('Invalid control frame')
                if opcode == 9:
                    self.send(payload, 10)
                continue
            if opcode not in (0, 1) or (opcode == 0) != started:
                raise ValueError('Invalid browser text frame')
            started = True
            result.extend(payload)
            if finished:
                return json.loads(result)


class Driver:
    def __init__(self, profile, observer):
        lines = (Path(profile) / 'DevToolsActivePort').read_text().splitlines()
        if len(lines) != 2 or not lines[0].isdigit() or not lines[1].startswith('/devtools/browser/'):
            raise ValueError('No supported debugging connection for this browser profile')
        port = int(lines[0])
        if not 1 <= port <= 65535:
            raise ValueError('Invalid browser port')
        self.ws = WebSocket(f'ws://127.0.0.1:{port}{lines[1]}')
        self.next_id = 0
        self.session = None
        self.context = None
        self.observer = observer
        self.snapshot = None
        self.origin = None

    def call(self, method, params=None, session=None):
        self.next_id += 1
        request_id = self.next_id
        request = {'id': request_id, 'method': method, 'params': params or {}}
        if session:
            request['sessionId'] = session
        self.ws.send(json.dumps(request).encode())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            response = self.ws.receive()
            if response.get('id') != request_id:
                continue
            if 'error' in response:
                raise RuntimeError('Browser protocol request failed')
            return response['result']
        raise TimeoutError('Browser protocol timed out')

    def evaluate(self, expression, context=None, session=None):
        params = {'expression': expression, 'returnByValue': True, 'awaitPromise': True}
        if context is not None:
            params['contextId'] = context
        result = self.call('Runtime.evaluate', params, session or self.session)
        if result.get('exceptionDetails'):
            raise RuntimeError('Browser observation or element operation failed')
        return result['result'].get('value')

    def observe(self, origin, values):
        self.snapshot = None
        # Identify the focused visible tab from this browser profile. Never
        # switch tabs or select another bot's browser to obtain an observation.
        tabs = self.call('Target.getTargets')['targetInfos']
        visible = []
        for tab in tabs:
            if tab['type'] != 'page' or not tab['url'].startswith(origin + '/'):
                continue
            session = self.call('Target.attachToTarget', {'targetId': tab['targetId'], 'flatten': True})['sessionId']
            self.call('Page.enable', session=session)
            self.call('Runtime.enable', session=session)
            frame = self.call('Page.getFrameTree', session=session)['frameTree']['frame']['id']
            context = self.call('Page.createIsolatedWorld', {'frameId': frame, 'worldName': 'kindred-browser-worker'}, session)['executionContextId']
            if self.evaluate('document.visibilityState === "visible" && document.hasFocus()', context, session):
                visible.append((session, context))
            else:
                self.call('Target.detachFromTarget', {'sessionId': session})
        if len(visible) != 1:
            for session, _ in visible:
                self.call('Target.detachFromTarget', {'sessionId': session})
            raise RuntimeError('Need exactly one focused browser tab at the requested origin')
        if self.session:
            try:
                self.call('Target.detachFromTarget', {'sessionId': self.session})
            except Exception:
                pass
        self.session, self.context = visible[0]
        self.origin = origin
        self.evaluate('globalThis.kindredBrowser = ' + self.observer, self.context)
        result = self.evaluate('kindredBrowser.observe(' + json.dumps(values) + ',' + json.dumps(secrets.token_hex(24)) + ')', self.context)
        capture_check = 'kindredBrowser.safeToCapture(' + json.dumps(origin) + ')'
        if not self.evaluate(capture_check, self.context):
            raise RuntimeError('Browser changed or authentication view requires human input')
        image = self.call('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': False}, self.session)['data']
        if not self.evaluate(capture_check, self.context):
            raise RuntimeError('Browser changed while capturing its observation')
        result['image'] = 'data:image/png;base64,' + image
        self.snapshot = result['snapshot_id']
        return result

    def act(self, snapshot, choice):
        dispatched = False
        if not self.snapshot or snapshot != self.snapshot:
            return {'applied': False, 'uncertain': False, 'detail': 'Stale browser snapshot; no action attempted'}
        args = json.dumps(snapshot) + ',' + json.dumps(choice)
        try:
            if not self.evaluate('document.hasFocus() && document.visibilityState === "visible"', self.context):
                return {'applied': False, 'uncertain': False, 'detail': 'Browser tab lost focus; no action attempted'}
            action = self.evaluate('kindredBrowser.prepare(' + args + ')', self.context)
            if 'rejected' in action:
                return {'applied': False, 'uncertain': False, 'detail': action['rejected']}
            kind = action['kind']
            dispatched = True
            if kind in ('click', 'check', 'fill'):
                for event in ('mousePressed', 'mouseReleased'):
                    self.call('Input.dispatchMouseEvent', {'type': event, 'x': action['x'], 'y': action['y'], 'button': 'left', 'clickCount': 1}, self.session)
                if kind == 'fill':
                    value = self.evaluate('kindredBrowser.fill(' + args + ')', self.context)
                    self.call('Input.insertText', {'text': value}, self.session)
            elif kind == 'select':
                self.evaluate('kindredBrowser.select(' + args + ')', self.context)
            elif kind == 'scroll':
                self.call('Input.dispatchMouseEvent', {'type': 'mouseWheel', 'x': 100, 'y': 100, 'deltaX': 0, 'deltaY': action['amount']}, self.session)
            elif kind == 'wait':
                time.sleep(0.25)
            else:
                raise RuntimeError('Unsupported action')
            if kind in ('check', 'fill', 'select') and not self.evaluate('kindredBrowser.verify(' + args + ')', self.context):
                raise RuntimeError('Field or checkbox verification failed')
            return {'applied': True, 'uncertain': False, 'detail': 'Input dispatched; field state verified' if kind in ('check', 'fill', 'select') else 'Input dispatched; inspect the next observation'}
        except Exception:
            return {'applied': False, 'uncertain': dispatched, 'detail': 'Action may have changed the page; inspect current state before continuing' if dispatched else 'Page changed or browser unavailable; no action attempted'}
        finally:
            # A choice can be used only once, including failed dispatch attempts.
            self.snapshot = None


def serve(profile):
    driver = None
    for line in iter(lambda: sys.stdin.buffer.readline(65537), b''):
        try:
            if len(line) > 65536 or not line.endswith(b'\n'):
                raise ValueError('Browser request exceeds its limit')
            request = json.loads(line)
            if request['op'] == 'hello':
                if driver or request['protocol'] != 1 or not isinstance(request['observer'], str) or len(request['observer']) > 32000:
                    raise ValueError('Invalid browser handshake')
                driver = Driver(profile, request['observer'])
                response = {'protocol': 1}
            elif driver and request['op'] == 'observe':
                response = driver.observe(request['origin'], request['values'])
            elif driver and request['op'] == 'act':
                response = driver.act(request['snapshot_id'], request['choice_id'])
            else:
                raise ValueError('Unsupported browser request')
        except Exception:
            # Never include page HTML, supplied text, credentials or protocol
            # response bodies in an error. Runtime records action uncertainty.
            response = {'error': 'Existing browser connection or observation unavailable'}
        data = json.dumps(response, ensure_ascii=False).encode()
        if len(data) > LIMIT:
            data = b'{"error":"Browser response exceeds its limit"}'
        sys.stdout.buffer.write(data + b'\n')
        sys.stdout.buffer.flush()


if __name__ == '__main__':
    serve(sys.argv[1])
