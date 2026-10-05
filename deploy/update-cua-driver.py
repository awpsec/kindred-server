#!/usr/bin/python3
"""Receive an exact bundled driver during the existing idle guest maintenance."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile


def install(stream, target, expected_hash, expected_size):
    target = Path(target)
    if target.is_symlink():
        raise ValueError('Customized driver target')
    with tempfile.NamedTemporaryFile(dir=target.parent, prefix='.cua-driver-', delete=False) as output:
        staged = Path(output.name)
        try:
            digest = hashlib.sha256();size = 0
            while block := stream.read(1024*1024):
                size += len(block)
                if size > expected_size or size > 64*1024*1024:
                    raise ValueError('Driver transfer size mismatch')
                digest.update(block);output.write(block)
            output.flush();os.fsync(output.fileno())
            if size != expected_size or digest.hexdigest() != expected_hash:
                raise ValueError('Driver transfer did not verify')
            if target.is_file() and hashlib.sha256(target.read_bytes()).hexdigest() == expected_hash:
                return {'sha256':expected_hash,'updated':False}
            if target.exists():
                shutil.copy2(target, target.with_name(target.name+'.previous'))
            staged.chmod(0o755);os.replace(staged,target)
            return {'sha256':expected_hash,'updated':True}
        finally:
            staged.unlink(missing_ok=True)


if __name__ == '__main__':
    if os.geteuid() != 0:
        raise RuntimeError('Driver maintenance requires root inside the guest')
    print(json.dumps(install(sys.stdin.buffer, '/usr/local/lib/kindred/cua-driver', sys.argv[1], int(sys.argv[2]))))
