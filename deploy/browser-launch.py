#!/usr/bin/env python3
"""Launch in the selected desktop's existing browser profile and session bus.

No password-store overrides, credential reads, browser reset or bus creation.
The guest embeds this file so a binary-only update uses the same policy.
"""
import argparse
import json
import os
from pathlib import Path
import re
import stat
import socket as sockets
import shutil
import struct
import sys
import time
from urllib.parse import urlsplit, unquote

ROOT = Path('/home/bot/.local/share/kindred')

class BrowserContextError(RuntimeError):
    pass


def private_directory(path, uid):
    # Check ancestors too: an otherwise owned child under a symlink could select
    # another account's profile. Never chmod or replace an existing directory.
    path = Path(path)
    for part in [path, *path.parents]:
        if part.is_symlink():
            raise BrowserContextError('Browser profile path is redirected; preserve it and ask for help.')
    if not path.exists():
        path.mkdir(mode=0o700, parents=True)
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o077:
        raise BrowserContextError('Browser profile is unavailable or not private; do not reset it.')
    return path


def bus_context(env, uid):
    address = env.get('DBUS_SESSION_BUS_ADDRESS', '')
    match = re.fullmatch(r'unix:(path|abstract)=([^,;]+)(?:,guid=[a-fA-F0-9]+)?', address)
    if not match:
        raise BrowserContextError('Desktop session bus is unavailable; do not start a replacement browser profile.')
    try:
        kind, name = match[1], unquote(match[2])
        if '\0' in name or len(name.encode()) > 107:
            raise OSError()
        if kind == 'path':
            socket = Path(name)
            info = socket.lstat()
            if not socket.is_absolute() or not stat.S_ISSOCK(info.st_mode) or info.st_uid != uid:
                raise OSError()
            endpoint = name
        else:
            endpoint = '\0' + name
        with sockets.socket(sockets.AF_UNIX, sockets.SOCK_STREAM) as connection:
            connection.settimeout(0.25)
            connection.connect(endpoint)
            # Abstract sockets have no filesystem owner; peer credentials also
            # prevent a replaced path from redirecting to another UID's bus.
            _, peer_uid, _ = struct.unpack('3i', connection.getsockopt(sockets.SOL_SOCKET, sockets.SO_PEERCRED, 12))
            if peer_uid != uid:
                raise OSError()
        runtime = env.get('XDG_RUNTIME_DIR')
        if not runtime:
            raise OSError()
        if runtime:
            path = Path(runtime)
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o077:
                raise OSError()
    except OSError:
        raise BrowserContextError('Desktop session bus is unavailable; preserve the browser and ask for help.') from None
    context = {key: env[key] for key in ('DBUS_SESSION_BUS_ADDRESS', 'XDG_RUNTIME_DIR')}
    # Chromium's backend detection must see the selected desktop, rather than
    # unrelated SSH/user-service values. Empty values clear an absent marker.
    for key in ('XDG_CURRENT_DESKTOP', 'DESKTOP_SESSION', 'KDE_SESSION_VERSION', 'KDE_FULL_SESSION'):
        value = env.get(key, '')
        if len(value) > 128 or not re.fullmatch(r'[A-Za-z0-9_:.+-]*', value):
            raise BrowserContextError('Desktop environment marker is invalid; preserve the browser.')
        context[key] = value
    return context


def desktop_context(display, environ=None, proc=Path('/proc'), uid=None):
    environ = os.environ if environ is None else environ
    uid = os.getuid() if uid is None else uid
    matches = []
    executable = shutil.which('openbox')
    if not executable:
        raise BrowserContextError('Assigned desktop executable is unavailable.')
    executable = os.path.realpath(executable)
    for entry in proc.iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            identity = (entry / 'stat').read_bytes().rsplit(b') ', 1)[1].split()[19]
            if entry.stat().st_uid != uid or os.path.realpath(entry / 'exe') != executable:
                continue
            raw = (entry / 'environ').read_bytes()
            env = dict(item.decode().split('=', 1) for item in raw.split(b'\0') if b'=' in item)
            if env.get('DISPLAY', '').split('.')[0] != display:
                continue
            context = bus_context(env, uid)
            if (entry / 'stat').read_bytes().rsplit(b') ', 1)[1].split()[19] != identity or os.path.realpath(entry / 'exe') != executable:
                raise BrowserContextError('Desktop changed during browser launch; inspect it before trying again.')
            matches.append(context)
        except (OSError, UnicodeError, ValueError):
            continue
    if matches:
        if any(context != matches[0] for context in matches[1:]):
            raise BrowserContextError('Desktop session is ambiguous; preserve the browser and ask for help.')
        return matches[0]
    raise BrowserContextError('Assigned desktop is unavailable; do not create another browser profile.')


def browser_arguments(profile, mode='restore', url=None):
    if mode not in ('restore', 'new', 'url'):
        raise BrowserContextError('Invalid browser launch action.')
    args = ['--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
            '--user-data-dir=' + str(profile), '--no-first-run', '--window-size=1120,680',
            '--restore-last-session']
    if mode == 'new':
        args.append('--new-window')
    if mode == 'url':
        try:
            value = urlsplit(url or '')
            valid = value.scheme == 'https' and value.hostname and not value.username and value.password is None and len(url) <= 2000
        except ValueError:
            valid = False
        if not valid:
            raise BrowserContextError('Expected HTTPS address without embedded credentials.')
        args.append(url)
    return args


def prepare(screen, mode='restore', url=None, root=ROOT, environ=None, proc=Path('/proc')):
    if not 1 <= screen <= 32:
        raise BrowserContextError('Invalid assigned desktop.')
    display = ':' + str(screen)
    # Resolve the desktop first; an unavailable desktop must not provision state.
    context_env = os.environ if environ is None else environ
    deadline = time.monotonic() + (5 if context_env.get('KINDRED_DESKTOP_STARTING') == '1' else 0)
    while True:
        try:
            context = desktop_context(display, context_env, proc)
            break
        except BrowserContextError as error:
            if not str(error).startswith('Assigned desktop is unavailable') or time.monotonic() >= deadline:
                raise
            # Startup waits for Openbox's actual environment, including its
            # configured desktop markers; it never invents a replacement bus.
            time.sleep(0.05)
    private_directory(root, os.getuid())
    profile = private_directory(root / ('browser' if screen == 1 else 'browser-' + str(screen)), os.getuid())
    return {'args': browser_arguments(profile, mode, url), 'env': dict(context, DISPLAY=display)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--screen', type=int, required=True)
    parser.add_argument('--mode', choices=('restore', 'new', 'url'), default='restore')
    parser.add_argument('--url')
    parser.add_argument('--prepare', action='store_true')
    args = parser.parse_args()
    try:
        launch = prepare(args.screen, args.mode, args.url)
        if args.prepare:
            print(json.dumps(launch))
        else:
            env = dict(os.environ)
            env.pop('KINDRED_DESKTOP_STARTING', None)
            env.update(launch['env'])
            os.execvpe('chromium', ['chromium', *launch['args']], env)
    except BrowserContextError as error:
        print(str(error), file=sys.stderr)
        return 2
    return 0

if __name__ == '__main__':
    sys.exit(main())
