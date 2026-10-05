#!/usr/bin/python3
"""Stage the exact reviewed Linux driver. Downloads are build-time opt-in only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import urllib.request

MANIFEST = Path(__file__).with_name('cua-driver-manifest.json')


def verify(data, manifest):
    if len(data) != manifest['binary_bytes'] or hashlib.sha256(data).hexdigest() != manifest['binary_sha256']:
        raise ValueError('Cua driver payload does not match the pinned manifest')
    if data[:5] != b'\x7fELF\x02' or data[18:20] != b'\x3e\x00':
        raise ValueError('Expected a Linux x86_64 driver')


def install(source, target, manifest):
    data = Path(source).read_bytes()
    verify(data, manifest)
    target = Path(target)
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.is_symlink():
        raise ValueError('Driver target must not be a symlink')
    if target.exists() and target.read_bytes() == data:
        target.chmod(0o755)
        return {'version':manifest['version'], 'sha256':manifest['binary_sha256'], 'updated':False}
    with tempfile.NamedTemporaryFile(dir=target.parent, prefix='.cua-driver-', delete=False) as stream:
        staged = Path(stream.name)
        stream.write(data);stream.flush();os.fsync(stream.fileno())
    try:
        staged.chmod(0o755)
        if target.exists():
            os.replace(target, target.with_name(target.name+'.previous'))
        os.replace(staged, target)
    finally:
        staged.unlink(missing_ok=True)
    return {'version':manifest['version'], 'sha256':manifest['binary_sha256'], 'updated':True}


def unpack(archive, output, manifest):
    data = Path(archive).read_bytes()
    if hashlib.sha256(data).hexdigest() != manifest['archive_sha256']:
        raise ValueError('Cua driver archive does not match the pinned manifest')
    with tarfile.open(archive, 'r:gz') as bundle:
        member = bundle.getmember(manifest['archive_member'])
        if not member.isfile() or member.size != manifest['binary_bytes']:
            raise ValueError('Invalid driver archive member')
        binary = bundle.extractfile(member).read()
    verify(binary, manifest)
    Path(output).write_bytes(binary)
    Path(output).chmod(0o755)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=MANIFEST)
    parser.add_argument('--payload', type=Path)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--download', action='store_true')
    parser.add_argument('--target', type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    with tempfile.TemporaryDirectory(prefix='kindred-cua-stage-') as temporary:
        archive = args.archive
        if args.download:
            if manifest.get('kindred_policy'):
                raise ValueError('The selected policy build requires its verified local payload or candidate archive; upstream download is not a substitute')
            if args.payload or args.archive:
                parser.error('Choose download, archive or payload')
            archive = Path(temporary)/'driver.tar.gz'
            with urllib.request.urlopen(manifest['archive_url'], timeout=60) as response:
                with archive.open('wb') as output:
                    size = 0
                    while block := response.read(1024*1024):
                        size += len(block)
                        if size > 64*1024*1024:
                            raise ValueError('Driver archive too large')
                        output.write(block)
        payload = args.payload
        if archive:
            payload = Path(temporary)/'cua-driver'
            unpack(archive, payload, manifest)
        if payload is None:
            parser.error('Choose --payload, --archive or explicit build-time --download')
        receipt = install(payload, args.target, manifest)
        if archive:
            receipt['archive_sha256'] = manifest['archive_sha256']
        receipt['source_commit'] = manifest['source_commit']
        print(json.dumps(receipt))
