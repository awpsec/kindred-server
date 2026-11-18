#!/usr/bin/python3
"""Run official Grok Build account operations in an account-specific VM home."""
import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tomllib
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


def verify_inference_configuration(binary, env, cwd):
    # This account is subscription-only. Reject custom models, API keys, token
    # helpers and provider plugins even if configured manually in the VM.
    config = Path(env['GROK_HOME']) / 'config.toml'
    if config.exists():
        with config.open('rb') as stream:
            settings = tomllib.load(stream)
        if set(settings) - {'marketplace'} or set(settings.get('marketplace', {})) - {'default_skills_installs_purged'}:
            raise ValueError('Unsupported settings in this subscription account')
    inspection = subprocess.run([binary, 'inspect', '--json'], env=env, cwd=cwd,
                                capture_output=True, timeout=15, check=True)
    data = json.loads(inspection.stdout)
    if (data['grokVersion'] != '1.0.88' or
            any(data[key] for key in ('projectInstructions', 'hooks', 'skills', 'plugins', 'mcpServers', 'lspServers')) or
            any(entry.get('role') != 'user' for entry in data['configSources']['layers'])):
        raise ValueError('Grok must use an isolated subscription configuration')


def infer_text(binary, env, cwd, model, prompt):
    """One text request. All native tools and indirect dispatch are disabled."""
    if not isinstance(prompt, str) or not prompt.strip() or len(prompt.encode()) > 1024 * 1024:
        raise ValueError('Invalid Grok prompt')
    if not isinstance(model, str) or not model.startswith('grok-') or len(model) > 128:
        raise ValueError('Invalid Grok model')
    prompt_file = Path(cwd) / 'prompt.txt'
    prompt_file.write_text(prompt, encoding='utf-8')
    # 1.0.88 treats an empty --tools list as unrestricted. Select one known
    # tool and exclude it explicitly along with both implicit MCP meta-tools.
    args = [binary, '--tools', 'read_file', '--disallowed-tools', 'read_file,search_tool,use_tool',
            '--permission-mode', 'dontAsk', '--disable-web-search', '--no-subagents', '--no-plan',
            '--no-memory', '--max-turns', '1', '--output-format', 'streaming-messages-json',
            '--model', model, '--prompt-file', str(prompt_file)]
    result = subprocess.run(args, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=240, check=True)
    if len(result.stdout) > 8 * 1024 * 1024:
        raise ValueError('Grok response exceeds size limit')
    frames = [json.loads(line) for line in result.stdout.splitlines()]
    if len(frames) < 2:
        raise ValueError('Grok did not complete the turn')
    init, final = frames[0], frames[-1]
    if (init.get('type') != 'system' or init.get('subtype') != 'init' or init.get('model') != model or
            init.get('tools') != [] or init.get('mcp_servers') != [] or init.get('skills') != [] or
            init.get('permissionMode') != 'dontAsk' or not init.get('session_id')):
        raise ValueError('Grok offered unexpected execution capabilities')
    if (final.get('type') != 'result' or final.get('subtype') != 'success' or
            final.get('is_error') is not False or final.get('num_turns') != 1 or
            final.get('session_id') != init['session_id'] or not isinstance(final.get('result'), str)):
        raise ValueError('Grok did not return a successful text response')
    return {'text': final['result'], 'model': model, 'usage': final.get('usage')}


@contextmanager
def auth_lock(home, exclusive):
    flags = os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW
    with os.fdopen(os.open(home / 'auth.lock', flags, 0o600), 'r+') as lock:
        mode = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        fcntl.flock(lock, mode | fcntl.LOCK_NB)
        yield


def perform(binary, home, operation):
    env = environment(home)
    marker = home / 'last-device-login'
    if operation == 'account':
        try:
            last_login = int(marker.stat().st_mtime)
        except FileNotFoundError:
            last_login = None
        return {'installed': Path(binary).is_file(), 'sign_in_saved': last_login is not None,
                'last_login_at': last_login}
    if operation == 'open-login':
        request = json.loads(sys.stdin.buffer.readline(65536))
        slot = request.get('slot')
        if not Path(binary).is_file() or type(slot) != int or not 1 <= slot <= 32:
            raise ValueError('A configured bot display and Grok installation are required')
        display = {'DISPLAY': ':' + str(slot), 'PATH': '/usr/bin:/bin', 'HOME': str(Path.home())}
        if 'XAUTHORITY' in os.environ:
            display['XAUTHORITY'] = os.environ['XAUTHORITY']
        subprocess.Popen(['/usr/bin/xterm', '-T', 'Kindred · Grok sign in', '-e', sys.executable,
                          str(Path(__file__).resolve()), 'login', home.name],
                         cwd=home, env=display, start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return {'opened': True, 'message': 'Complete Grok device sign-in on the bot computer.'}
    with auth_lock(home, operation in ('login', 'logout')), tempfile.TemporaryDirectory(prefix='kindred-grok-work-') as work:
        if operation == 'infer':
            verify_inference_configuration(binary, env, work)
            request = json.loads(sys.stdin.buffer.readline(2 * 1024 * 1024))
            model = request['model']
            if model not in {item['model'] for item in model_catalogue(binary, env, work)}:
                raise ValueError('Model is not in the account catalogue')
            return infer_text(binary, env, work, model, request['prompt'])
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
        args = ['login', '--device-auth'] if operation == 'login' else ['logout']
        subprocess.run([binary, *args], env=env, cwd=work,
                       stdout=None if operation == 'login' else subprocess.DEVNULL,
                       timeout=600 if operation == 'login' else 30, check=True)
        if operation == 'login':
            marker.touch(mode=0o600)
        else:
            marker.unlink(missing_ok=True)
        return {'operation': operation, 'completed': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('login', 'open-login', 'logout', 'account', 'usage', 'models', 'infer'))
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
