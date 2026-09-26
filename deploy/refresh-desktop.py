#!/usr/bin/env python3
"""Add the Applications dock item to an existing guest without closing its apps.

Run inside the guest as root, from an extracted matching deploy directory.
Existing dock settings are retained. Only the dock reloads; X sessions stay up.
"""
import argparse
import datetime
import os
from pathlib import Path
import shutil
import subprocess


def refresh(source):
    if os.geteuid() != 0:
        raise SystemExit('Run as root inside the bot computer')
    desktop = Path('/usr/local/share/kindred/desktop')
    config = desktop / 'tint2rc'
    launcher = Path('/usr/local/lib/kindred/desktop-launch')
    # Read/preflight before installing anything; never substitute user customizations.
    text = config.read_text()
    script = launcher.read_text()
    if 'applications)' not in script:
        marker = '  browser)'
        if marker not in script:
            raise SystemExit('Custom desktop launcher needs manual integration')
        script = script.replace(marker, '  applications) exec rofi -show drun -show-icons -display-drun Applications -theme /usr/local/share/kindred/desktop/applications.rasi;;\n' + marker, 1)
    entry = 'launcher_item_app = /usr/local/share/kindred/desktop/applications.desktop'
    if entry not in text:
        marker = 'launcher_item_app = '
        if marker not in text:
            raise SystemExit('Custom dock has no launcher section; manual integration needed')
        index = text.index(marker)
        text = text[:index] + entry + '\n' + text[index:]
    assets = ['applications.desktop', 'applications.rasi', 'applications.svg']
    for name in assets:
        assert (source / 'desktop' / name).is_file(), name
    if not shutil.which('rofi'):
        env = {**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'}
        subprocess.run(['apt-get', 'update', '-qq'], env=env, check=True)
        subprocess.run(['apt-get', 'install', '-y', '--no-install-recommends', 'rofi'], env=env, check=True)
    assert shutil.which('rsvg-convert'), 'SVG renderer missing'
    backup = Path('/var/backups/kindred-desktop') / datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    backup.mkdir(parents=True, mode=0o700)
    for path in [config, launcher, *(desktop / name for name in assets), desktop / 'applications.png']:
        if path.exists():
            shutil.copy2(path, backup / path.name)
    for name in assets:
        shutil.copy2(source / 'desktop' / name, desktop / name)
        (desktop / name).chmod(0o644)
    subprocess.run(['rsvg-convert', str(desktop / 'applications.svg'), '-o', str(desktop / 'applications.png')], check=True)
    (desktop / 'applications.png').chmod(0o644)
    launcher.write_text(script)
    config.write_text(text)
    # SIGUSR1 is tint2's documented configuration reload, not an X/server restart.
    result = subprocess.run(['pkill', '-USR1', '-u', 'bot', '-x', 'tint2'])
    assert result.returncode in (0, 1), 'Could not reload dock'
    print('Applications added; existing desktop sessions preserved. Backup: ' + str(backup))


def merge_browser_new(script, source_script):
    """The panel's New window uses browser-new; add only that action, keeping local launcher edits."""
    if 'browser-new)' in script:
        return script
    action = next(line for line in source_script.splitlines() if line.startswith('  browser-new)'))
    lines = script.splitlines(True)
    index = next((i for i, line in enumerate(lines) if line.startswith('  browser)')), None)
    if index is None or 'profile=' not in script:
        raise SystemExit('Custom desktop launcher needs manual integration')
    lines.insert(index + 1, action + '\n')
    return ''.join(lines)


def stage_panel(source):
    """Install the new managed panel for the next computer restart; preserve live apps."""
    if os.geteuid() != 0:
        raise SystemExit('Run as root inside the bot computer')
    target = Path('/usr/local/lib/kindred')
    shell = target / 'start-desktop-shell'
    current = shell.read_text()
    if 'tint2 -c /usr/local/share/kindred/desktop/tint2rc' not in current and '/usr/local/lib/kindred/kindred-dock' not in current:
        raise SystemExit('Custom desktop shell detected; manual integration needed')
    for name in ['kindred-dock', 'start-desktop-shell', 'desktop-launch']:
        if not (source / name).is_file():
            raise SystemExit('Missing panel source: ' + name)
    launcher = target / 'desktop-launch'
    script = merge_browser_new(launcher.read_text(), (source / 'desktop-launch').read_text())
    env = {**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'}
    subprocess.run(['apt-get', 'update', '-qq'], env=env, check=True)
    subprocess.run(['apt-get', 'install', '-y', '--no-install-recommends',
                    'python3-gi', 'gir1.2-gtk-3.0', 'gir1.2-wnck-3.0', 'x11-utils'], env=env, check=True)
    backup = Path('/var/backups/kindred-desktop') / datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    backup.mkdir(parents=True, mode=0o700)
    for name in ['start-desktop-shell', 'kindred-dock', 'desktop-launch']:
        if (target / name).exists():
            shutil.copy2(target / name, backup / name)
    if script != launcher.read_text():
        temporary = target / 'desktop-launch.new'
        temporary.write_text(script)
        temporary.chmod(0o755)
        temporary.replace(launcher)
    # Activate the supervisor last. The running supervisor and all apps stay untouched.
    for name in ['kindred-dock', 'start-desktop-shell']:
        temporary = target / (name + '.new')
        shutil.copy2(source / name, temporary)
        temporary.chmod(0o755)
        temporary.replace(target / name)
    print('Panel staged for the next computer restart. Live sessions unchanged. Backup: ' + str(backup))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--panel', action='store_true', help='Stage the full-width panel for the next computer restart')
    args = parser.parse_args()
    (stage_panel if args.panel else refresh)(args.source)
