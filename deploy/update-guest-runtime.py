#!/usr/bin/python3
"""Atomically install the server's matching guest executable during idle maintenance."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

LIMIT = 256 * 1024 * 1024


def install(stream, target, expected_hash, version):
    target = Path(target)
    if not target.is_file() or target.is_symlink():
        raise RuntimeError('Managed Kindred guest executable is missing or customized.')
    temporary = None
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
        result = subprocess.run([str(temporary), 'guest-rpc'], input=json.dumps({'tool':'command_poll','args':{'commands':[]},'screen':1}), capture_output=True, text=True, timeout=15, check=True)
        if json.loads(result.stdout) != {'commands': []}:
            raise RuntimeError('Guest executable does not support managed commands.')
        if hashlib.sha256(target.read_bytes()).hexdigest() == expected_hash:
            return {'updated': False, 'version': version}
        # Preserve a working rollback copy before switching the executable.
        backup = target.with_name(target.name + '.previous')
        shutil.copy2(target, str(backup) + '.new')
        os.replace(str(backup) + '.new', backup)
        os.replace(temporary, target)
        temporary = None
        return {'updated': True, 'version': version}
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    try:
        if os.geteuid() != 0:
            raise RuntimeError('Guest runtime maintenance requires root inside the VM.')
        print(json.dumps(install(sys.stdin.buffer, '/usr/local/lib/kindred/kindred-bin', sys.argv[1], sys.argv[2])))
    except Exception:
        # Executable output may contain local details; leave those inside the VM.
        print(json.dumps({'error':'Guest runtime update failed verification. The previous executable was retained.'}))
        sys.exit(1)
