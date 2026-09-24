#!/usr/bin/env python3
"""Source-only coordination regression gate. No packages, services, or real connector writes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
BACKEND = [
    'desktop_sessions::', 'team_chats::', 'team_restart_tests::', 'profiles::server_chats::',
    'chat_tests::', 'chats::delivery_tests::', 'continuity::tests::', 'user_tasks::tests::',
    'conversation_updates::', 'command_jobs::', 'connector_policy::',
    'connector_artifacts::', 'connector_edits::', 'connector_records::',
    'codex_connectors::tests::', 'quiet_output::', 'chat_edit::',
    'instructions::tests', 'provider_retry::',
]
UI = [
    'group-activity', 'sidebar-quiet-routing', 'assigned-group-activity', 'mention-contrast', 'group-presentation', 'group-command-waits',
    'group-sequences', 'team-chat-controls', 'group-description',
    'collaboration-notifications', 'connector-stacks', 'connector-artifacts',
    'connector-edit-workflows', 'connector-stack-accessibility',
    'chat-edit-review', 'floating-composer-read', 'unfocused-message-motion', 'server-chats',
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--ui-only', action='store_true')
    mode.add_argument('--backend-only', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    output = (args.output or Path(tempfile.mkdtemp(prefix='kindred-coordination-'))).resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, WEBKIT='1', KINDRED_TEST_ARTIFACTS=str(output), ARTIFACTS=str(output / 'activity'))
    results = []
    metadata = {
        'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'working_changes': subprocess.check_output(['git', 'status', '--short'], cwd=ROOT, text=True),
        'engine': 'webkit', 'scope': 'ui' if args.ui_only else 'backend' if args.backend_only else 'full',
        'ui_sha256': {name: hashlib.sha256((ROOT / 'ui' / name).read_bytes()).hexdigest()
                      for name in ['app.js', 'group-activity.js', 'style.css']},
    }
    (output / 'source.json').write_text(json.dumps(metadata, indent=2))

    def run(name, command, timeout=120, nonempty=False):
        print(f'Running {name}', flush=True)
        with subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, start_new_session=True) as process:
            try:
                stdout, stderr = process.communicate(timeout=timeout)
                text = stdout + stderr
                passed = process.returncode == 0 and (not nonempty or bool(re.search(r'test result: ok\. [1-9]\d* passed', text)))
                code = process.returncode
            except subprocess.TimeoutExpired:
                # Stop the fixture/browser children too; timed-out checks must not
                # accumulate hidden processes while the remaining suite runs.
                if os.name == 'posix':
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                else:
                    process.kill()
                stdout, stderr = process.communicate()
                text = f'Timed out after {timeout}s\n' + stdout + stderr
                passed, code = False, 'timeout'
        (output / f'{name}.log').write_text(text)
        results.append(dict(check=name, passed=passed, exit=code))
        (output / 'results.json').write_text(json.dumps(results, indent=2))
        print(f'{"PASS" if passed else "FAIL"} {name}', flush=True)
        return passed, text

    if not args.ui_only:
        cargo = shutil.which('cargo') or str(Path.home() / '.cargo/bin/cargo')
        ok, log = run('compile-tests', [cargo, 'test', '--bin', 'kindred', '--no-run', '--message-format=json'], timeout=900)
        executable = None
        if ok:
            for line in log.splitlines():
                try:
                    item = json.loads(line)
                except ValueError:
                    continue
                if item.get('reason') == 'compiler-artifact' and item.get('profile', {}).get('test') and item.get('executable'):
                    executable = item['executable']
        if not executable:
            print(f'No current test executable. See {output}', file=sys.stderr)
            return 1
        for name in BACKEND:
            run('backend-' + name.replace(':', '_'), [executable, name, '--test-threads=2'], nonempty=True)
    if not args.backend_only:
        for name in UI:
            run('ui-' + name, ['node', f'tools/frontend/test-{name}.cjs'])
    print(f'Results: {output}')
    return 0 if all(result['passed'] for result in results) else 1

if __name__ == '__main__':
    sys.exit(main())
