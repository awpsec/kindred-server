#!/usr/bin/python3
"""Run official Grok Build account operations in an account-specific VM home."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import uuid

BINARY = Path('/opt/kindred/providers/grok-1.0.88')


def account_directory(root, account_id):
    # Reject alternate spellings as well as paths; an account has one namespace.
    if str(uuid.UUID(account_id)) != account_id:
        raise ValueError('Invalid Grok account ID')
    for directory in (root, root / 'grok', root / 'grok' / account_id):
        directory.mkdir(mode=0o700, exist_ok=True)
        info = directory.lstat()
        forbidden = 0o022 if directory == root else 0o077
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & forbidden:
            raise ValueError('Grok account storage is not private')
    return directory


def environment(home):
    # In particular, never inherit XAI_API_KEY, external IdPs or another CLI home.
    return {'HOME': str(home), 'GROK_HOME': str(home / '.grok'),
            'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_DATA_HOME': str(home / '.local/share'),
            'XDG_CACHE_HOME': str(home / '.cache'), 'PATH': '/usr/bin:/bin',
            'LANG': 'C.UTF-8', 'TERM': 'xterm-256color', 'NO_COLOR': '1'}


def model_catalogue(binary, env, cwd):
    request = {'jsonrpc': '2.0', 'method': 'initialize', 'id': 1, 'params': {
        'protocolVersion': 1, 'clientCapabilities': {}, 'clientInfo': {'name': 'Kindred', 'version': '1'}}}
    result = subprocess.run([binary, 'agent', 'stdio'], cwd=cwd, env=env,
                            input=json.dumps(request) + '\n', text=True,
                            capture_output=True, timeout=15, check=True)
    for line in result.stdout.splitlines():
        frame = json.loads(line)
        if frame.get('id') != 1:
            continue
        agent = frame['result']
        if agent['protocolVersion'] != 1 or not any(a['id'] == 'grok.com' for a in agent['authMethods']):
            raise ValueError('The installed CLI does not offer Grok sign-in')
        entries = agent.get('_meta', {}).get('modelState', {}).get('availableModels', [])
        return [{'model': item['modelId'], 'displayName': item.get('name', item['modelId'])}
                for item in entries if isinstance(item.get('modelId'), str)]
    raise ValueError('Missing Grok agent initialization')


def perform(binary, home, operation):
    env = environment(home)
    with tempfile.TemporaryDirectory(prefix='kindred-grok-work-') as work:
        if operation == 'models':
            return {'data': model_catalogue(binary, env, work)}
        if operation == 'usage':
            result = subprocess.run([binary, 'usage', '--json'], env=env, cwd=work,
                                    capture_output=True, timeout=15)
            if result.returncode:
                return {'available': False, 'usage_url': 'https://grok.com/dashboard'}
            data = json.loads(result.stdout)
            if not isinstance(data, dict) or not data:
                return {'available': False, 'usage_url': 'https://grok.com/dashboard'}
            return {'available': True, 'usage': data}
        # Interactive device code belongs in the selected account's terminal.
        # The vendor handles expiry, browser authorization and token refresh.
        if operation not in {'login', 'logout'}:
            raise ValueError('Unsupported Grok account action')
        lock_fd = os.open(home / 'auth.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(lock_fd, 'r+') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            args = ['login', '--device-auth'] if operation == 'login' else ['logout']
            subprocess.run([binary, *args], env=env, cwd=work,
                           timeout=600 if operation == 'login' else 30, check=True)
            return {'operation': operation, 'completed': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('login', 'logout', 'usage', 'models'))
    parser.add_argument('account_id')
    args = parser.parse_args()
    root = Path.home() / '.kindred-providers'
    try:
        home = account_directory(root, args.account_id)
        print(json.dumps(perform(BINARY, home, args.operation)))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        parser.exit(1, 'Grok could not complete the selected account operation.\n')


if __name__ == '__main__':
    main()
