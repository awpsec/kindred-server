#!/usr/bin/python3
"""Observe a managed screen without starting services or restoring applications."""
import json
import os
import socket
import subprocess
import sys


def classify(properties, display_ready=False, dock_ready=False, ports_ready=False):
    active = properties.get('ActiveState')
    sub = properties.get('SubState')
    result = properties.get('Result', 'success')
    if active == 'failed' or sub == 'auto-restart' or result not in ('', 'success'):
        return 'failed'
    if active == 'activating':
        return 'waking'
    if active == 'deactivating':
        return 'sleeping'
    if active == 'inactive' and properties.get('LoadState') == 'loaded':
        return 'asleep'
    if active == 'active':
        return 'ready' if display_ready and dock_ready and ports_ready else 'failed'
    return 'unavailable'


def command(args, env):
    return subprocess.run(args, env=env, capture_output=True, text=True, timeout=3)


def observe(screen, control_port=5900, view_port=5901):
    if not 1 <= screen <= 32:
        raise ValueError('Invalid screen')
    env = {**os.environ, 'DISPLAY': ':' + str(screen), 'XDG_RUNTIME_DIR': '/run/user/' + str(os.getuid())}
    args = ['systemctl'] + ([] if screen == 1 else ['--user'])
    unit = 'kindred-guest-desktop.service' if screen == 1 else 'kindred-screen@' + str(screen) + '.service'
    result = command(args + ['show', unit, '--property=LoadState,ActiveState,SubState,Result'], env)
    if result.returncode:
        return {'state': 'unavailable'}
    properties = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    if properties.get('ActiveState') != 'active':
        return {'state': classify(properties)}
    display = command(['xdpyinfo', '-display', env['DISPLAY']], env).returncode == 0
    dock = display and command(['xdotool', 'search', '--onlyvisible', '--name', '^Kindred Dock$'], env).returncode == 0
    ports = True
    for port in (control_port + (screen - 1) * 2, view_port + (screen - 1) * 2):
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=0.5) as connection:
                ports = ports and connection.recv(12).startswith(b'RFB ')
        except OSError:
            ports = False
    return {'state': classify(properties, display, dock, ports)}


if __name__ == '__main__':
    try:
        print(json.dumps(observe(*map(int, sys.argv[1:]))))
    except (OSError, ValueError, subprocess.SubprocessError):
        print(json.dumps({'state': 'unavailable'}))
