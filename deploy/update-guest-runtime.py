#!/usr/bin/python3
"""Atomically install the server's matching guest executable during idle maintenance."""
import hashlib
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid
import stat
from contextlib import contextmanager

LIMIT = 256 * 1024 * 1024
# Filled by the server from its compiled, reviewed deployment sources.
LAUNCHERS = None


def stage_launchers(root, payload):
    if payload is None:
        return []
    names = ('browser-launch.py', 'desktop-launch', 'start-desktop.sh', 'ensure-screen', 'screen-ownership.py', 'wait-screen-ready')
    if set(payload) != set(names):
        raise RuntimeError('Incomplete managed browser launcher set.')
    staged = []
    try:
        for name in names:
            value = payload[name]
            source = value['source'].encode('utf-8')
            if not source or len(source) > 64 * 1024:
                raise RuntimeError('Invalid managed browser launcher source.')
            target = root / name
            old = None
            if target.is_symlink():
                raise RuntimeError('Customized browser launcher was retained.')
            if target.exists():
                if not target.is_file() or target.stat().st_uid != os.geteuid():
                    raise RuntimeError('Customized browser launcher was retained.')
                old = target.read_bytes()
                digest = hashlib.sha256(old).hexdigest()
                candidate = hashlib.sha256(source).hexdigest()
                if digest == candidate and target.stat().st_mode & 0o777 == 0o755:
                    continue
                previous=value['previous_sha256']
                accepted=previous if isinstance(previous,list) else [previous]
                if digest not in [candidate,*accepted]:
                    raise RuntimeError('Customized browser launcher was retained.')
            elif value['previous_sha256'] is not None and not (isinstance(value['previous_sha256'],list) and None in value['previous_sha256']):
                raise RuntimeError('Managed browser launcher is missing.')
            with tempfile.NamedTemporaryFile(dir=root, prefix='.kindred-launcher-', delete=False) as out:
                temporary = Path(out.name)
                staged.append((target, temporary, old, target.stat().st_mode & 0o777 if old is not None else None))
                out.write(source); out.flush(); os.fsync(out.fileno())
            temporary.chmod(0o755)
        return staged
    except Exception:
        for _, temporary, _, _ in staged:
            temporary.unlink(missing_ok=True)
        raise


def restore_launcher(target, old, mode):
    if old is None:
        target.unlink(missing_ok=True)
        return
    with tempfile.NamedTemporaryFile(dir=target.parent, prefix='.kindred-launcher-rollback-', delete=False) as out:
        rollback = Path(out.name)
        out.write(old); out.flush(); os.fsync(out.fileno())
    try:
        rollback.chmod(mode)
        os.replace(rollback, target)
    finally:
        rollback.unlink(missing_ok=True)



@contextmanager
def runtime_lock(target):
    # Distinct server/profile callers may target the same VM runtime.
    lock_path = Path(target).parent / '.kindred-runtime.lock'
    descriptor = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'a+b') as lock:
        if os.fstat(lock.fileno()).st_uid != os.geteuid():
            raise RuntimeError('Managed runtime lock is not owned.')
        os.fchmod(lock.fileno(), 0o600)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another guest runtime installation is in progress.') from None
        yield


def install(stream, target, expected_hash, version, launchers=None):
    with runtime_lock(target):
        return _install(stream, target, expected_hash, version, launchers)


def _install(stream, target, expected_hash, version, launchers=None, before_switch=None):
    target = Path(target)
    if not target.is_file() or target.is_symlink():
        raise RuntimeError('Managed Kindred guest executable is missing or customized.')
    temporary = None
    staged = []
    switched = []
    try:
        with tempfile.NamedTemporaryFile(dir=target.parent, prefix='.kindred-runtime-', delete=False) as out:
            temporary = Path(out.name)
            digest = hashlib.sha256()
            size = 0
            while chunk := stream.read(1024 * 1024):
                size += len(chunk)
                if size > LIMIT:
                    raise RuntimeError('Guest executable exceeds the transfer limit.')
                out.write(chunk)
                digest.update(chunk)
            out.flush()
            os.fsync(out.fileno())
        if digest.hexdigest() != expected_hash or not size:
            raise RuntimeError('Guest executable transfer did not verify.')
        temporary.chmod(0o755)
        result = subprocess.run([str(temporary), '--version'], capture_output=True, text=True, timeout=15, check=True)
        if result.stdout.strip() != 'kindred ' + version:
            raise RuntimeError('Guest executable version did not match the server.')
        result = subprocess.run([str(temporary), 'guest-rpc'], env={**os.environ, 'KINDRED_GUEST': '1'}, input=json.dumps({'tool':'command_poll','args':{'commands':[]},'screen':1}), capture_output=True, text=True, timeout=15, check=True)
        if json.loads(result.stdout) != {'commands': []}:
            raise RuntimeError('Guest executable does not support managed commands.')
        staged = stage_launchers(target.parent, launchers)
        updated = hashlib.sha256(target.read_bytes()).hexdigest() != expected_hash
        if before_switch is not None:
            before_switch()
        # Prepare rollback copies before the first switch. No browser process,
        # profile, cookie or credential path is part of this installation.
        if updated:
            backup = target.with_name(target.name + '.previous')
            shutil.copy2(target, str(backup) + '.new')
            os.replace(str(backup) + '.new', backup)
        for path, _, old, _ in staged:
            if old is not None:
                backup = path.with_name(path.name + '.previous')
                shutil.copy2(path, str(backup) + '.new')
                os.replace(str(backup) + '.new', backup)
        launcher_hashes = {}
        try:
            # The shared helper is installed first; dependent entrypoints follow.
            for path, replacement, old, mode in staged:
                os.replace(replacement, path)
                switched.append((path, old, mode))
            if launchers is not None:
                for name, value in launchers.items():
                    digest = hashlib.sha256((target.parent / name).read_bytes()).hexdigest()
                    if digest != hashlib.sha256(value['source'].encode('utf-8')).hexdigest():
                        raise RuntimeError('Browser launcher installation did not verify.')
                    launcher_hashes[name] = digest
            if updated:
                os.replace(temporary, target)
                temporary = None
        except Exception:
            for path, old, mode in reversed(switched):
                restore_launcher(path, old, mode)
            raise
        return {'updated': updated, 'version': version, 'launchers_updated': bool(staged), 'launcher_hashes': launcher_hashes}
    finally:
        for _, replacement, _, _ in staged:
            replacement.unlink(missing_ok=True)
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def private_directory(path):
    path.mkdir(mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise RuntimeError('Runtime operation directory is not private and owned.')


def save_journal(directory, value):
    with tempfile.NamedTemporaryFile(dir=directory, prefix='.journal-', delete=False) as out:
        temporary = Path(out.name)
        out.write(json.dumps(value).encode()); out.flush(); os.fsync(out.fileno())
    try:
        os.replace(temporary, directory / 'journal.json')
        fd = os.open(directory, os.O_DIRECTORY | os.O_NOFOLLOW)
        try: os.fsync(fd)
        finally: os.close(fd)
    finally:
        temporary.unlink(missing_ok=True)


def file_identity(path):
    try: info = path.lstat()
    except FileNotFoundError: return {'exists': False}
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
        raise RuntimeError('Runtime operation file is not a regular owned file.')
    return {'exists': True, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': stat.S_IMODE(info.st_mode)}


def settle_delivery(directory, record, target):
    current = {name: file_identity(target.parent / name) for name in record['target']}
    matching = all(current[name] == {'exists': True, 'sha256': digest, 'mode': 0o755}
                   for name, digest in record['target'].items())
    if matching:
        receipt = {'job_id': record['job_id'], 'phase': 'completed', 'sha256': record['target'][target.name],
                   'version': record['version'], 'launcher_hashes': {n: h for n, h in record['target'].items() if n != target.name},
                   'updated': record.get('previous', {}).get(target.name, {}).get('sha256') != record['target'][target.name],
                   'launchers_updated': any(record.get('previous', {}).get(n, {}).get('sha256') != h for n,h in record['target'].items() if n != target.name)}
    elif 'previous' not in record or current == record['previous']:
        receipt = {'job_id': record['job_id'], 'phase': 'failed', 'sha256': record['target'][target.name],
                   'version': record['version'], 'launcher_hashes': {n:h for n,h in record['target'].items() if n != target.name},
                   'error': 'Runtime delivery did not switch the selected files. This operation is closed.'}
    else:
        receipt = {'job_id': record['job_id'], 'phase': 'checking', 'sha256': record['target'][target.name],
                   'version': record['version'], 'launcher_hashes': {n:h for n,h in record['target'].items() if n != target.name},
                   'error': 'Runtime delivery needs recovery: installed files do not match a complete selected or predecessor set.'}
    record.update(phase=receipt['phase'], receipt=receipt)
    save_journal(directory, record)
    return receipt


@contextmanager
def package_installation_lock(root):
    # Existing guest package workers own this lock. Runtime-only delivery must
    # not overlap an installer started by another controller; it never starts it.
    if not root.exists():
        yield
        return
    private_directory(root)
    descriptor = os.open(root / 'upgrade.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'a+b') as lock:
        info = os.fstat(lock.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
            raise RuntimeError('Package installation lock is not owned.')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def delivery(operation, job_id, stream, target, expected_hash, version, launchers, package_root=Path('/var/lib/kindred/maintenance')):
    # Both install and status own the same lock. Status closes an absent ID,
    # preventing a delayed SSH install from starting after work has resumed.
    if operation not in ('install', 'status') or str(uuid.UUID(job_id)) != job_id:
        raise RuntimeError('Invalid runtime delivery operation.')
    target = Path(target)
    if not isinstance(launchers, dict) or len(launchers) != 6:
        raise RuntimeError('Missing exact runtime helper manifest.')
    expected = {target.name: expected_hash, **{name: hashlib.sha256(value['source'].encode()).hexdigest() for name,value in launchers.items()}}
    # Only the existing fixed helper set can introduce target paths.
    if set(launchers) != {'browser-launch.py','desktop-launch','start-desktop.sh','ensure-screen','screen-ownership.py','wait-screen-ready'}:
        raise RuntimeError('Invalid managed helper set.')
    if len(expected_hash) != 64 or any(c not in '0123456789abcdef' for c in expected_hash):
        raise RuntimeError('Invalid runtime hash.')
    with runtime_lock(target), package_installation_lock(package_root):
        root = target.parent / '.kindred-runtime-operations'; private_directory(root)
        directory = root / job_id; private_directory(directory)
        journal = directory / 'journal.json'
        if journal.exists() or journal.is_symlink():
            file_identity(journal)
            record = json.loads(journal.read_text())
            if record['target'] != expected or record['job_id'] != job_id or record['version'] != version:
                raise RuntimeError('Runtime operation target changed.')
            # A repeated install is reconciliation only, never re-execution.
            return settle_delivery(directory, record, target)
        record = {'job_id': job_id, 'target': expected, 'version': version, 'phase': 'prepared'}
        save_journal(directory, record)
        if operation == 'status':
            return settle_delivery(directory, record, target)
        def preserve_predecessors():
            previous = directory / 'previous'; private_directory(previous)
            identities = {}
            for name in expected:
                path = target.parent / name; identities[name] = file_identity(path)
                if identities[name]['exists']:
                    with (previous / name).open('xb') as out:
                        os.fchmod(out.fileno(), 0o600)
                        data = path.read_bytes()
                        if hashlib.sha256(data).hexdigest() != identities[name]['sha256']:
                            raise RuntimeError('Runtime predecessor changed during capture.')
                        out.write(data); out.flush(); os.fsync(out.fileno())
            fd = os.open(previous, os.O_DIRECTORY | os.O_NOFOLLOW)
            try: os.fsync(fd)
            finally: os.close(fd)
            record.update(previous=identities, phase='installing')
            save_journal(directory, record)
        try:
            _install(stream, target, expected_hash, version, launchers, preserve_predecessors)
        except Exception:
            # Files, not the transport result, determine whether work can resume.
            pass
        return settle_delivery(directory, record, target)


if __name__ == '__main__':
    try:
        if os.geteuid() != 0:
            raise RuntimeError('Guest runtime maintenance requires root inside the VM.')
        if len(sys.argv) == 5:
            result = delivery(sys.argv[3], sys.argv[4], sys.stdin.buffer,
                              '/usr/local/lib/kindred/kindred-bin', sys.argv[1], sys.argv[2], LAUNCHERS)
        else:
            result = install(sys.stdin.buffer, '/usr/local/lib/kindred/kindred-bin', sys.argv[1], sys.argv[2], LAUNCHERS)
        print(json.dumps(result))
    except Exception:
        print(json.dumps({'error':'Guest runtime delivery could not be verified. Keep the reservation and reconcile its operation ID.'}))
        sys.exit(1)
