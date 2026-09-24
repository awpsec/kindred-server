#!/usr/bin/env python3
"""Offline Codex package/tool transport check. No account, model or bot data.

Protocol V1 from openai/codex rust-v0.153.4 code-mode-protocol/src/host.
Keep this probe in step with the pinned package when upgrading Codex.
"""
import json
import os
from pathlib import Path
import select
import shutil
import struct
import subprocess
import sys
import time

FILES = ('bin/codex', 'bin/codex-code-mode-host', 'codex-package.json',
         'codex-path/rg', 'codex-resources/bwrap', 'codex-resources/zsh/bin/zsh')


def validate(root):
    for name in FILES:
        path = root / name
        if not path.is_file() or path.is_symlink():
            raise ValueError('Codex runtime is missing a regular file: ' + name)
        if name != 'codex-package.json':
            with path.open('rb') as stream:
                header = stream.read(64)
            if len(header) != 64 or header[:6] != b'\x7fELF\x02\x01' or header[18:20] != b'\x3e\x00':
                raise ValueError('Expected an x86_64 Linux executable: ' + name)
            if not os.access(path, os.X_OK):
                raise ValueError('Codex runtime file is not executable: ' + name)
    metadata = json.loads((root / 'codex-package.json').read_text())
    for key, value in {'layoutVersion': 1, 'target': 'x86_64-unknown-linux-musl',
                       'entrypoint': 'bin/codex', 'resourcesDir': 'codex-resources',
                       'pathDir': 'codex-path'}.items():
        if metadata.get(key) != value:
            raise ValueError('Unexpected Codex package ' + key)
    return metadata


def probe(host, timeout=4):
    """Execute JS -> fixture callback -> JS result, bounded in time and bytes."""
    deadline = time.monotonic() + timeout
    with subprocess.Popen([str(host), '--listen', 'stdio'], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0) as child:
        def send(value):
            body = json.dumps(value).encode()
            child.stdin.write(struct.pack('<I', len(body)) + body)

        def read(size):
            data = bytearray()
            while len(data) < size:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([child.stdout], [], [], remaining)[0]:
                    raise ValueError('Codex tool helper check timed out')
                block = os.read(child.stdout.fileno(), size - len(data))
                if not block:
                    raise ValueError('Codex tool helper stopped before completing its check')
                data.extend(block)
            return data

        def receive():
            size, = struct.unpack('<I', read(4))
            if not 0 < size <= 65536:
                raise ValueError('Invalid Codex tool helper response size')
            return json.loads(read(size))

        def require(condition):
            if not condition:
                raise ValueError('Codex tool helper failed its offline tool round trip')

        try:
            send({'type': 'connection/hello', 'supportedVersions': [1],
                  'requiredCapabilities': [], 'optionalCapabilities': []})
            hello = receive()
            require(hello.get('type') == 'connection/ready' and hello.get('selectedVersion') == 1)
            send({'type': 'operation/request', 'id': 1,
                  'request': {'method': 'session/open', 'sessionId': 'kindred-check'}})
            opened = receive()
            require(opened.get('id') == 1 and opened.get('result', {}).get('value', {}).get('type') == 'session/ready')
            send({'type': 'operation/request', 'id': 2, 'request': {
                'method': 'session/execute', 'sessionId': 'kindred-check', 'request': {
                    'tool_call_id': 'kindred-check', 'enabled_tools': [{
                        'name': 'kindred_check', 'tool_name': {'name': 'kindred_check', 'namespace': None},
                        'description': 'Isolated health check', 'kind': 'function',
                        'input_schema': {'type': 'object', 'properties': {'value': {'type': 'integer'}}},
                        'output_schema': None}],
                    'source': 'const r = await tools.kindred_check({value: 41}); text(r.value + 1);',
                    'yield_time_ms': 1000, 'max_output_tokens': 100}}})
            called = 0
            # No model can choose these calls. Accept only this fixed fixture.
            for _ in range(12):
                frame = receive()
                if frame.get('type') == 'delegate/request':
                    request = frame['request']
                    invocation = request.get('invocation', {})
                    require(request.get('type') == 'tool/invoke' and called == 0
                            and invocation.get('tool_name') == {'name': 'kindred_check', 'namespace': None}
                            and invocation.get('input') == {'value': 41})
                    called += 1
                    send({'type': 'delegate/response', 'id': frame['id'], 'result': {
                        'status': 'ok', 'value': {'type': 'tool/result', 'result': {'value': 41}}}})
                elif frame.get('type') == 'execute/initialResponse':
                    result = frame.get('result', {}).get('value', {}).get('Result', {})
                    require(called == 1 and result.get('error_text') is None
                            and result.get('content_items') == [{'type': 'input_text', 'text': '42'}])
                    return
                else:
                    require(frame.get('type') in ('operation/response', 'cell/closed'))
            raise ValueError('Codex tool helper did not complete its check')
        finally:
            # Also reap hung/crashed helpers; never leave health-check workers.
            child.kill()
            child.wait()


def check(entry, execute=True):
    entry = Path(shutil.which(str(entry)) or entry).resolve(strict=True)
    root = entry.parent.parent
    metadata = validate(root)
    if entry != root / 'bin/codex':
        raise ValueError('Codex entrypoint is outside its complete runtime package')
    if execute:
        result = subprocess.run([str(entry), '--version'], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=3, check=True)
        if result.stdout.decode().strip() != 'codex-cli ' + metadata.get('version', ''):
            raise ValueError('Codex executable and package versions differ')
        probe(root / 'bin/codex-code-mode-host')


if __name__ == '__main__':
    try:
        check(sys.argv[1], execute='--files-only' not in sys.argv[2:])
    except Exception:
        # Never expose provider output, environment values or private paths.
        print('Codex tool runtime is incomplete or failed its offline check. Repair the complete guest Codex package; sign-in alone cannot fix this.', file=sys.stderr)
        sys.exit(78)
