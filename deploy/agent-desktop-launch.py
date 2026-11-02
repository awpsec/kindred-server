#!/usr/bin/python3
"""Run the pinned accessibility CLI only in Kindred's assigned desktop session."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

LIB = Path('/usr/local/lib/kindred')


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def prepare(environ, context, root, generation):
    display = environ.get('DISPLAY', '')
    if not re.fullmatch(r':(?:[1-9]|[12][0-9]|3[0-2])(?:\.0)?', display):
        raise RuntimeError('Use guest_exec with use_desktop=true on your assigned Bot Computer.')
    # Discard caller/session overrides before using the verified Openbox session.
    env = dict(environ)
    for key in list(env):
        if key.startswith('AGENT_DESKTOP_') or key in ('AT_SPI_BUS_ADDRESS', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XDG_RUNTIME_DIR'):
            env.pop(key)
    env.update(context)
    screen = display[1:].split('.')[0]
    if environ.get('KINDRED_SCREEN_SLOT', screen) != screen:
        raise RuntimeError('Assigned screen differs from requested display')
    env.update(DISPLAY=':'+screen, XDG_SESSION_TYPE='x11', NO_AT_BRIDGE='0',
               AGENT_DESKTOP_HOME=str(root / ('agent-desktop-'+screen+'-'+generation)))
    return env


def main():
    display = os.environ.get('DISPLAY', '')
    if not re.fullmatch(r':(?:[1-9]|[12][0-9]|3[0-2])(?:\.0)?', display):
        raise RuntimeError('Use guest_exec with use_desktop=true on your assigned Bot Computer.')
    screen = int(display[1:].split('.')[0])
    browser = module('kindred_browser_context', LIB/'browser-launch.py')
    ownership = module('kindred_screen_ownership', LIB/'screen-ownership.py')
    context = browser.desktop_context(':'+str(screen))
    with ownership.launch_guard(screen, desktop_context=context) as identity:
        if identity and any(context.get('KINDRED_SCREEN_'+key.upper()) != identity[key] for key in ('owner','generation')):
            raise RuntimeError('Desktop session ownership changed')
        generation = identity['generation'] if identity else 'legacy'
        env = prepare(os.environ, context, ownership.ROOT, generation)
        ownership.private(Path(env['AGENT_DESKTOP_HOME']), True)
        base = Path(__file__).resolve().parent
        if sys.argv[1:] == ['skills', 'get', 'kindred']:
            print(json.dumps({'ok':True,'data':{'content':(base/'SKILL.md').read_text()}}))
            return 0
        # Keep the supervisor alive so the ownership lock covers the entire action.
        try:
            return subprocess.run([str(base/'agent-desktop'), *sys.argv[1:]], env=env).returncode
        except OSError:
            print(json.dumps({'ok':False,'error':{'code':'KINDRED_DESKTOP_RECEIPT_UNAVAILABLE',
                'message':'The action receipt is unavailable. Inspect the screen before deciding whether to repeat the action.',
                'disposition':{'delivery':'unknown','retry':'unsafe'}}}))
            return 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception:
        # No exception text: it can contain a private path or command argument.
        print(json.dumps({'ok':False,'error':{'code':'KINDRED_DESKTOP_UNAVAILABLE',
            'message':'Assigned desktop is unavailable or ownership changed. Use guest_exec with use_desktop=true; do not override DISPLAY or create a replacement session.',
            'disposition':{'delivery':'not_delivered','retry':'safe'}}}))
        sys.exit(1)
