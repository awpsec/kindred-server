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


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parent)
    refresh(parser.parse_args().source)
