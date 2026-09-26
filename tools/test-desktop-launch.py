#!/usr/bin/python3
"""Launcher argument contract, with stub apps on PATH; runs nothing real.

/workspace rarely exists on a development host, so a temporary copy of
deploy/desktop-launch changes only its working directory; the actions are
byte-identical to the shipped script.
"""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / 'deploy/desktop-launch').read_text()


def test():
    with tempfile.TemporaryDirectory(prefix='kindred-launch-') as tmp:
        tmp = Path(tmp)
        assert SOURCE.count('cd /workspace\n') == 1
        launcher = tmp / 'desktop-launch'
        launcher.write_text(SOURCE.replace('cd /workspace\n', f'cd {tmp}\n'))
        launcher.chmod(0o755)
        bin_dir = tmp / 'bin'
        bin_dir.mkdir()
        record = tmp / 'argv'
        for app in ['chromium', 'pcmanfm', 'konsole', 'rofi']:
            stub = bin_dir / app
            stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "$0" "$@" > {record}\n')
            stub.chmod(0o755)

        def run(action, display=':1'):
            record.unlink(missing_ok=True)
            env = {**os.environ, 'PATH': f'{bin_dir}:/usr/bin:/bin', 'DISPLAY': display}
            code = subprocess.run([str(launcher), action], env=env).returncode
            argv = record.read_text().splitlines() if record.exists() else []
            return code, [Path(argv[0]).name, *argv[1:]] if argv else []

        code, argv = run('browser')
        assert code == 0 and argv[0] == 'chromium', argv
        assert '--restore-last-session' in argv and '--new-window' not in argv, argv
        code, new = run('browser-new')
        assert code == 0 and new[0] == 'chromium', new
        assert '--new-window' in new and '--restore-last-session' not in new, new
        profile = '--user-data-dir=/home/bot/.local/share/kindred/browser'
        assert profile in argv and profile in new, 'a new window joins the running profile'
        assert run('browser-new', ':3')[1][1] == profile + '-3'
        assert '--new-win' in run('files')[1] and '--separate' in run('terminal')[1]
        assert run('applications')[1][0] == 'rofi'
        assert run('nonsense')[0] == 2 and run('browser', ':0')[0] == 2

        # Staging an older guest launcher adds exactly the browser-new action.
        loader = importlib.machinery.SourceFileLoader('refresh', str(ROOT / 'deploy/refresh-desktop.py'))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        refresh = importlib.util.module_from_spec(spec)
        loader.exec_module(refresh)
        old = ''.join(line for line in SOURCE.splitlines(True) if 'browser-new)' not in line)
        merged = refresh.merge_browser_new(old + '# local edit\n', SOURCE)
        assert merged == SOURCE + '# local edit\n', merged
        assert refresh.merge_browser_new(SOURCE, SOURCE) == SOURCE
        try:
            refresh.merge_browser_new('#!/bin/sh\nexec custom\n', SOURCE)
            raise AssertionError('custom launcher accepted')
        except SystemExit:
            pass
        print('PASS: browser restores, browser-new opens a new window in the same profile, '
              'files/terminal/applications flags, invalid actions/screens, staging merge')


if __name__ == '__main__':
    test()
