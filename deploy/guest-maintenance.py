#!/usr/bin/env python3
"""Durable guest updates; successful manual updates reboot the bot computer.

The server holds every screen while this service runs. systemd owns the package
process so losing SSH or restarting the server cannot kill dpkg halfway through.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import tempfile
import uuid

ROOT = Path('/var/lib/kindred/maintenance')
UNIT = 'kindred-maintenance.service'
INTERVAL = 3 * 86400
IDLE = 900
ACTIVE = ('starting', 'updating', 'rebooting')


def atomic(path, value):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex)
    temporary.write_text(json.dumps(value), encoding='utf-8')
    temporary.chmod(0o600)
    temporary.replace(path)


def read_state():
    try:
        return json.loads((ROOT / 'state.json').read_text())
    except FileNotFoundError:
        return {'phase': 'idle', 'job_id': '', 'attempted': 0}


def boot_id():
    return Path('/proc/sys/kernel/random/boot_id').read_text().strip()


def unit_active():
    value = subprocess.run(['systemctl', 'show', UNIT, '--property=LoadState,ActiveState', '--no-pager'],
                           capture_output=True, text=True, timeout=10)
    fields = dict(line.split('=', 1) for line in value.stdout.splitlines() if '=' in line)
    if fields.get('LoadState') == 'not-found':
        return False
    if value.returncode or fields.get('ActiveState') not in ('active', 'activating', 'deactivating', 'inactive', 'failed'):
        raise RuntimeError('Could not verify the update service state.')
    return fields['ActiveState'] in ('active', 'activating', 'deactivating')


def status():
    value = read_state()
    if value['phase'] == 'rebooting':
        if value.get('boot_id') != boot_id():
            value.update(phase='completed', reboot_recommended=False)
        elif time.time() - value['finished'] > 300:
            value.update(phase='failed', error='Updates installed, but the computer did not reboot. Reboot it manually.', reboot_recommended=True)
        return value
    active = unit_active()
    if active:
        if value['phase'] not in ACTIVE:
            value = dict(value, phase='updating')
    elif value['phase'] in ACTIVE and time.time() - value['attempted'] > 30:
        reason = 'Computer restarted during updates.' if value.get('boot_id') != boot_id() else 'Update service exited before completion.'
        value = dict(value, phase='failed', error=reason, finished=int(time.time()))
    if value.get('boot_id') != boot_id():
        value['reboot_recommended'] = False
    return value


def guest_check():
    if os.geteuid() != 0 or not Path('/etc/debian_version').is_file():
        raise RuntimeError('Automatic updates require a Debian-family bot VM.')
    if not Path('/usr/local/lib/kindred/kindred-bin').is_file() or not Path('/etc/systemd/system/kindred-guest-desktop.service').is_file():
        raise RuntimeError('This is not a configured Kindred guest.')
    virtual = subprocess.run(['systemd-detect-virt', '--vm'], capture_output=True, timeout=5)
    if virtual.returncode:
        raise RuntimeError('Automatic updates are restricted to the virtual bot computer.')


def saved_result(job_id):
    uuid.UUID(job_id)
    # Read legacy cancellation records too. Closed IDs must never become fresh
    # requests, even after a later job has replaced state.json.
    for directory in ('results', 'cancelled'):
        record = ROOT / directory / (job_id + '.json')
        if record.exists():
            value = json.loads(record.read_text())
            if value.get('boot_id') != boot_id() and 'reboot_recommended' in value:
                value['reboot_recommended'] = False
            return value
    return None


def remember(job_id, value):
    uuid.UUID(job_id)
    directory = ROOT / 'results'
    directory.mkdir(exist_ok=True, mode=0o700)
    value = dict(value, boot_id=boot_id())
    atomic(directory / (job_id + '.json'), value)
    return value


def reject_unknown(job_id, current):
    result = saved_result(job_id)
    if result is not None:
        return result
    now = int(time.time())
    return remember(job_id, dict(job_id=job_id, phase='failed', attempted=now, finished=now,
                    reboot_recommended=current.get('reboot_recommended', False),
                    error='The update request did not start. No packages were installed by this attempt.'))


def archive_current(value):
    if value.get('job_id') and value['phase'] in ('completed', 'failed'):
        remember(value['job_id'], value)


def reconcile(job_id):
    """Fence an uncertain dispatch before the server releases its reservation."""
    uuid.UUID(job_id)
    guest_check()
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (ROOT / 'dispatch.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        value = status()
        if value['phase'] in ACTIVE:
            if value.get('job_id') != job_id:
                reject_unknown(job_id, value)
            return value
        archive_current(value)
        if value.get('job_id') == job_id:
            if value['phase'] == 'failed':
                # A delayed worker must see the terminal phase before tasks resume.
                atomic(ROOT / 'state.json', value)
            return value
        # No matching service started. Keep a durable rejection so a delayed
        # SSH process with this ID cannot begin installing after reconciliation.
        return reject_unknown(job_id, value)


def start(job_id, manual=False):
    uuid.UUID(job_id)
    guest_check()
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (ROOT / 'dispatch.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        previous = status()
        # A repeated request returns the same job, including its terminal result.
        if previous['phase'] in ACTIVE:
            if previous.get('job_id') != job_id:
                reject_unknown(job_id, previous)
            return previous
        archive_current(previous)
        if previous.get('job_id') == job_id:
            return previous
        result = saved_result(job_id)
        if result is not None:
            return result
        now = int(time.time())
        if not manual and previous.get('attempted', 0) > now - INTERVAL:
            return remember(job_id, dict(previous, job_id=job_id, phase='deferred', error='', retry_at=previous['attempted'] + INTERVAL))
        uptime = float(Path('/proc/uptime').read_text().split()[0])
        if not manual and uptime < IDLE:
            return remember(job_id, {'phase': 'deferred', 'job_id': job_id, 'retry_at': now + IDLE - int(uptime), 'detail': 'Waiting for the computer to settle after startup.'})
        if Path('/var/lib/cloud/instance').exists() and not Path('/var/lib/cloud/instance/boot-finished').exists():
            return remember(job_id, {'phase': 'deferred', 'job_id': job_id, 'retry_at': now + 300, 'detail': 'Waiting for computer setup to finish.'})
        if shutil.disk_usage('/').free < 1024 ** 3:
            value = remember(job_id, {'phase': 'failed', 'job_id': job_id, 'attempted': now, 'finished': now,
                             'reboot_recommended': previous.get('reboot_recommended', False),
                             'error': 'Free at least 1 GB on the bot computer before updating.'})
            atomic(ROOT / 'state.json', value)
            return value
        source = globals().get('SOURCE') or Path(__file__).read_text()
        worker = ROOT / 'worker.py'
        temporary = ROOT / ('worker.' + uuid.uuid4().hex)
        temporary.write_text(source, encoding='utf-8')
        temporary.chmod(0o700)
        temporary.replace(worker)
        value = {'job_id': job_id, 'phase': 'starting', 'attempted': now, 'boot_id': boot_id(),
                 'reboot_recommended': previous.get('reboot_recommended', False), 'manual': manual}
        atomic(ROOT / 'state.json', value)
        try:
            launched = subprocess.run(['systemd-run', '--quiet', '--collect', '--unit=' + UNIT,
                '--property=Type=exec', '--property=Nice=10', '--property=IOSchedulingClass=idle',
                '/usr/bin/python3', str(worker), 'worker', job_id],
                capture_output=True, timeout=20)
            if launched.returncode:
                raise RuntimeError('The package update service could not start.')
        except subprocess.TimeoutExpired:
            # Dispatch outcome is unknown. Keep the durable starting state and
            # reconcile the service; never launch a second job on a timeout.
            return value
        except Exception:
            value.update(phase='failed', finished=int(time.time()), error='The package update service could not start.')
            atomic(ROOT / 'state.json', value)
        return value


def package_digest():
    with Path('/var/lib/dpkg/status').open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def apt(arguments, log):
    environment = dict(os.environ, DEBIAN_FRONTEND='noninteractive', NEEDRESTART_MODE='l')
    # Keep existing configuration; install new dependencies without removals or
    # a distribution release change. Bound network waits, not package installs.
    options = ['-o', 'DPkg::Lock::Timeout=60', '-o', 'Acquire::Retries=0',
               '-o', 'Acquire::http::Timeout=30', '-o', 'Acquire::https::Timeout=30',
               '-o', 'Dpkg::Options::=--force-confdef', '-o', 'Dpkg::Options::=--force-confold']
    command = ['/usr/bin/apt-get', *options, *arguments]
    with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, env=environment) as process:
        while chunk := process.stdout.read(8192):
            if log.tell() > 1024 * 1024:
                log.seek(0)
                log.truncate()
            log.write(chunk)
            log.flush()
        return process.wait()


def update_harnesses(log):
    # Use the complete verified runtime installer, preserving account/profile data.
    with tempfile.TemporaryDirectory(prefix='kindred-codex-') as directory:
        root = Path(directory)
        (root / 'download-codex.py').write_text(CODEX_DOWNLOADER)
        (root / 'check-codex.py').write_text(CODEX_CHECKER)
        (root / 'update-harnesses.py').write_text(HARNESS_UPDATER)
        result = subprocess.run(['/usr/bin/python3', str(root / 'download-codex.py'),
                                 '--update', '/usr/local/bin/codex'],
                                stdin=subprocess.DEVNULL, stdout=log, stderr=log, timeout=600)
        if result.returncode:
            raise RuntimeError('Codex update failed. The previous verified runtime is retained; inspect the computer update log.')

        result = subprocess.run(['/usr/bin/python3', str(root / 'update-harnesses.py'), 'guest'],
                                stdin=subprocess.DEVNULL, stdout=log, stderr=log, timeout=1200)
        if result.returncode:
            raise RuntimeError('Provider harness update failed. Inspect the computer update log.')
        bridge = Path('/usr/local/lib/kindred/provider-cli.py')
        temporary = bridge.with_suffix('.update')
        temporary.write_text(PROVIDER_BRIDGE)
        temporary.chmod(0o755)
        os.replace(temporary, bridge)


def worker(job_id):
    guest_check()
    uuid.UUID(job_id)
    with (ROOT / 'upgrade.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with (ROOT / 'dispatch.lock').open('a') as dispatch:
            fcntl.flock(dispatch, fcntl.LOCK_EX)
            value = read_state()
            if value.get('job_id') != job_id or value['phase'] != 'starting' or saved_result(job_id) is not None:
                raise RuntimeError('This update job is no longer current.')
            value.update(phase='updating')
            atomic(ROOT / 'state.json', value)
        before = None
        try:
            before = package_digest()
            with (ROOT / 'packages.log').open('wb') as log:
                update_harnesses(log)
                code = apt(['-o', 'APT::Update::Error-Mode=any', 'update'], log)
                if code:
                    raise RuntimeError(f'Package index update failed (exit {code}).')
                code = apt(['--yes', '--with-new-pkgs', 'upgrade'], log)
                if code:
                    raise RuntimeError(f'Package installation failed (exit {code}).')
            value.update(phase='completed', changed=before != package_digest(), error='')
            value['reboot_recommended'] = value.get('reboot_recommended', False) or value['changed'] or Path('/run/reboot-required').exists()
        except Exception as error:
            # No subprocess details or repository credentials leave the guest.
            safe = str(error) if isinstance(error, RuntimeError) else 'The package update did not complete. Inspect the update log on the bot computer.'
            value.update(phase='failed', error=safe)
        if before is not None:
            try:
                value['reboot_recommended'] = value.get('reboot_recommended', False) or before != package_digest() or Path('/run/reboot-required').exists()
            except Exception:
                value['reboot_recommended'] = True
        value['finished'] = int(time.time())
        if value['phase'] == 'completed' and value.get('manual'):
            value['phase'] = 'rebooting'
        atomic(ROOT / 'state.json', value)
        if value['phase'] == 'rebooting':
            try:
                subprocess.run(['systemctl', 'reboot', '--no-block'], check=True, timeout=15)
            except Exception:
                value.update(phase='failed', error='Updates installed, but reboot failed. Reboot the computer manually.', reboot_recommended=True)
                atomic(ROOT / 'state.json', value)


if __name__ == '__main__':
    try:
        action = sys.argv[1]
        if action == 'worker':
            worker(sys.argv[2])
        elif action in ('status', 'start', 'manual', 'reconcile'):
            guest_check()
            print(json.dumps(status() if action == 'status' else reconcile(sys.argv[2]) if action == 'reconcile' else start(sys.argv[2], manual=action == 'manual')))
        else:
            raise RuntimeError('Unsupported maintenance operation.')
    except Exception as error:
        safe = str(error) if isinstance(error, RuntimeError) else 'Could not verify automatic updates on this bot computer.'
        print(json.dumps({'error': safe}))
        sys.exit(1)
