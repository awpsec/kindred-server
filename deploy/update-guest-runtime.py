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

LIMIT = 256 * 1024 * 1024
# Filled by the server from its compiled, reviewed deployment sources.
LAUNCHERS = None


def stage_launchers(root, payload):
    if payload is None:
        return []
    names = ('browser-launch.py', 'desktop-launch', 'start-desktop.sh')
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
                if digest not in (candidate, value['previous_sha256']):
                    raise RuntimeError('Customized browser launcher was retained.')
            elif value['previous_sha256'] is not None:
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



def install(stream, target, expected_hash, version, launchers=None):
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
        return _install(stream, target, expected_hash, version, launchers)


def _install(stream, target, expected_hash, version, launchers=None):
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


if __name__ == '__main__':
    try:
        if os.geteuid() != 0:
            raise RuntimeError('Guest runtime maintenance requires root inside the VM.')
        print(json.dumps(install(sys.stdin.buffer, '/usr/local/lib/kindred/kindred-bin', sys.argv[1], sys.argv[2], LAUNCHERS)))
    except Exception:
        # Executable output may contain local details; leave those inside the VM.
        print(json.dumps({'error':'Guest runtime update failed verification. The previous executable was retained.'}))
        sys.exit(1)
