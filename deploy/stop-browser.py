#!/usr/bin/env python3
"""Save the assigned Chromium profile before systemd stops its desktop.

No credential reads, cookie edits, encryption overrides or forced process kills.
"""
import argparse
import ctypes
import importlib.util
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location('browser_launch', Path(__file__).with_name('browser-launch.py'))
launch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launch)
ROOT = launch.ROOT
BrowserContextError = launch.BrowserContextError
private_directory = launch.private_directory
desktop_context = launch.desktop_context

def browser_processes(profile, proc=Path('/proc')):
    """Only exact owned Chromium parents; never inspect browser databases."""
    parents = []
    for entry in proc.iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            args = [os.fsdecode(a) for a in (entry / 'cmdline').read_bytes().split(b'\0') if a]
            if len(args) == 1:
                args = shlex.split(args[0])
            executable = os.readlink(entry / 'exe')
            if (entry.stat().st_uid != os.getuid() or not args
                    or Path(executable).name not in ('chrome', 'chromium', 'chromium-browser')
                    or os.path.abspath(args[0]) != os.path.abspath(executable)
                    or [a for a in args if a.startswith('--user-data-dir=')] != ['--user-data-dir=' + str(profile)]
                    or any(a.startswith(('--type=', '--app=')) for a in args)):
                continue
            parents.append((int(entry.name), (entry / 'stat').read_bytes().rsplit(b') ', 1)[1].split()[19]))
        except (OSError, ValueError, IndexError):
            continue
    return parents


def process_current(binding, proc=Path('/proc')):
    try:
        fields = (proc / str(binding[0]) / 'stat').read_bytes().rsplit(b') ', 1)[1].split()
        return fields[0] != b'Z' and fields[19] == binding[1]
    except (OSError, IndexError):
        return False


def close_browser(screen, root=ROOT, timeout=20):
    """Flush the assigned browser before systemd stops X11 and its session bus."""
    if not 1 <= screen <= 32:
        raise BrowserContextError('Invalid assigned desktop.')
    profile = root / ('browser' if screen == 1 else 'browser-' + str(screen))
    if not profile.exists():
        return
    private_directory(root, os.getuid())
    private_directory(profile, os.getuid())
    parents = browser_processes(profile)
    if not parents:
        return
    if len(parents) != 1:
        raise BrowserContextError('Browser shutdown is ambiguous; preserve the profile.')
    binding = parents[0]
    context = desktop_context(':' + str(screen))
    env = {**os.environ, **context, 'DISPLAY': ':' + str(screen)}
    windows = subprocess.run(['xdotool', 'search', '--all', '--pid', str(binding[0])],
                             env=env, capture_output=True, text=True, timeout=3)
    if windows.returncode or not windows.stdout.strip():
        raise BrowserContextError('Assigned browser windows are unavailable for a clean shutdown.')
    # WM_DELETE_WINDOW uses Chromium's ordinary close/save path. No SIGKILL,
    # database edits, password-store overrides, synthetic cookie renewal or CDP.
    class Message(ctypes.Structure):
        _fields_ = [('type', ctypes.c_int), ('serial', ctypes.c_ulong), ('send_event', ctypes.c_int),
                    ('display', ctypes.c_void_p), ('window', ctypes.c_ulong), ('message_type', ctypes.c_ulong),
                    ('format', ctypes.c_int), ('data', ctypes.c_long * 5)]
    class Event(ctypes.Union):
        _fields_ = [('message', Message), ('padding', ctypes.c_long * 24)]
    x = ctypes.CDLL('libX11.so.6')
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]; x.XOpenDisplay.restype = ctypes.c_void_p
    x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]; x.XInternAtom.restype = ctypes.c_ulong
    x.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(Event)]
    x.XFlush.argtypes = [ctypes.c_void_p]; x.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = x.XOpenDisplay(env['DISPLAY'].encode())
    if not display:
        raise BrowserContextError('Assigned display is unavailable for a clean shutdown.')
    try:
        for window in windows.stdout.splitlines():
            if not process_current(binding):
                return
            protocols = subprocess.run(['xprop', '-id', window, 'WM_PROTOCOLS'], env=env,
                                       capture_output=True, text=True, timeout=3)
            if 'WM_DELETE_WINDOW' not in protocols.stdout:
                continue
            event = Event()
            event.message.type = 33; event.message.display = display; event.message.window = int(window)
            event.message.message_type = x.XInternAtom(display, b'WM_PROTOCOLS', False)
            event.message.format = 32
            event.message.data[0] = x.XInternAtom(display, b'WM_DELETE_WINDOW', False)
            if not x.XSendEvent(display, int(window), False, 0, ctypes.byref(event)):
                raise BrowserContextError('Browser close request failed.')
            x.XFlush(display)
    finally:
        x.XCloseDisplay(display)
    deadline = time.monotonic() + timeout
    while process_current(binding) and time.monotonic() < deadline:
        time.sleep(.1)
    if process_current(binding):
        raise BrowserContextError('Browser did not finish saving before the shutdown deadline.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--screen', type=int, required=True)
    args = parser.parse_args()
    try:
        close_browser(args.screen)
    except BrowserContextError as error:
        print(str(error), file=sys.stderr)
        return 2
    except (OSError, subprocess.SubprocessError):
        print('Browser shutdown failed; preserve its profile and inspect the desktop.', file=sys.stderr)
        return 2
    return 0

if __name__ == '__main__':
    sys.exit(main())
