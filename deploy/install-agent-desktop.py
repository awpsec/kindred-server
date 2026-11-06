#!/usr/bin/python3
"""Verify/stage at build time; install the bundled guest capability offline."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import urllib.request

# Embedded by the server for existing guest maintenance. No network in that path.
RESOURCES = None
LIMIT = 32 * 1024 * 1024


def resources():
    if RESOURCES is not None:
        return RESOURCES
    base = Path(__file__).resolve().parent
    # Source checkouts keep notices beside deploy/. Guest media flattens deploy/
    # into its root and keeps third-party/ inside that same root.
    license_path=base/'third-party/agent-desktop/LICENSE.txt'
    if not license_path.is_file(): license_path=base.parent/'third-party/agent-desktop/LICENSE.txt'
    return {'manifest': json.loads((base/'agent-desktop-manifest.json').read_text()),
            'launcher': (base/'agent-desktop-launch.py').read_text(),
            'skill': (base/'agent-desktop-skill.md').read_text(),
            'license': license_path.read_text()}


def verified(data, manifest):
    if len(data) != manifest['archive_bytes'] or hashlib.sha256(data).hexdigest() != manifest['archive_sha256']:
        raise ValueError('Agent desktop archive does not match the pinned release')
    payload = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        members = archive.getmembers()
        if len(members) != len(manifest['files']) or {m.name for m in members} != set(manifest['files']):
            raise ValueError('Unexpected agent desktop archive contents')
        for member in members:
            expected = manifest['files'][member.name]
            if not member.isfile() or member.size != expected['bytes'] or member.size > LIMIT:
                raise ValueError('Invalid agent desktop archive member')
            value = archive.extractfile(member).read()
            if hashlib.sha256(value).hexdigest() != expected['sha256'] or value[:5] != b'\x7fELF\x02' or value[18:20] != b'\x3e\x00':
                raise ValueError('Agent desktop executable does not match')
            payload[member.name] = value
    return payload


def safe_parent(path):
    for parent in [path, *path.parents]:
        if parent.is_symlink():
            raise ValueError('Agent desktop installation path is redirected')
        if parent.exists() and (parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o022):
            raise ValueError('Agent desktop installation path is not owned')


def install(data, resource, prefix=Path('/usr/local')):
    manifest = resource['manifest']
    payload = verified(data, manifest)
    payload.update({'kindred-agent-desktop': resource['launcher'].encode(),
                    'SKILL.md': resource['skill'].encode(), 'LICENSE.txt': resource['license'].encode(),
                    'SOURCE.json': (json.dumps(manifest, indent=2)+'\n').encode()})
    # Content-address the entire install, including Kindred's launcher and guidance.
    identity = hashlib.sha256(b''.join(k.encode()+b'\0'+v for k,v in sorted(payload.items()))).hexdigest()
    root = prefix/'lib/kindred/agent-desktop'
    link = prefix/'bin/agent-desktop'
    safe_parent(root); safe_parent(link.parent)
    root.mkdir(parents=True, exist_ok=True); link.parent.mkdir(parents=True, exist_ok=True)
    target = root/identity
    if os.path.lexists(link):
        if not link.is_symlink() or link.resolve().parent.parent != root or link.resolve().name != 'kindred-agent-desktop':
            raise ValueError('Custom agent-desktop installation retained')
    updated = not link.is_symlink() or link.resolve() != target/'kindred-agent-desktop'
    if target.exists():
        safe_parent(target)
        if {p.name for p in target.iterdir()} != set(payload):
            raise ValueError('Existing agent desktop payload changed')
        for name, value in payload.items():
            p=target/name
            if p.is_symlink() or not p.is_file() or p.read_bytes()!=value or p.stat().st_mode & 0o777 != (0o755 if name in manifest['files'] or name=='kindred-agent-desktop' else 0o644):
                raise ValueError('Existing agent desktop payload changed')
    else:
        staged=Path(tempfile.mkdtemp(prefix='.stage-',dir=root))
        try:
            for name,value in payload.items():
                p=staged/name;p.write_bytes(value);p.chmod(0o755 if name in manifest['files'] or name=='kindred-agent-desktop' else 0o644)
            staged.chmod(0o755);os.rename(staged,target)
        finally:
            if staged.exists():shutil.rmtree(staged)
    if updated:
        fd,name=tempfile.mkstemp(prefix='.agent-desktop-',dir=link.parent);os.close(fd);os.unlink(name)
        try:
            os.symlink(target/'kindred-agent-desktop',name);os.replace(name,link)
        finally:
            if os.path.lexists(name):os.unlink(name)
    return {'version':manifest['version'],'archive_sha256':manifest['archive_sha256'],'updated':updated,'installation':identity}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive',type=Path)
    parser.add_argument('--download',action='store_true',help='Explicit build-time download only')
    parser.add_argument('--stage',type=Path,help='Verify and retain archive without installing')
    parser.add_argument('--receive',action='store_true',help='Offline maintenance input on stdin')
    args=parser.parse_args();resource=resources();manifest=resource['manifest']
    if sum([bool(args.archive),args.download,args.receive])!=1:parser.error('Choose archive, download or receive')
    if args.download:
        if not args.stage:parser.error('Downloads require a build-time --stage target')
        with urllib.request.urlopen(manifest['archive_url'],timeout=60) as response:data=response.read(LIMIT+1)
    elif args.receive:data=sys.stdin.buffer.read(LIMIT+1)
    else:data=args.archive.read_bytes()
    verified(data,manifest)
    if args.stage:
        args.stage.parent.mkdir(parents=True,exist_ok=True)
        if args.stage.is_symlink():raise ValueError('Archive stage is redirected')
        args.stage.write_bytes(data)
        result={'version':manifest['version'],'archive_sha256':manifest['archive_sha256'],'staged':True}
    else:
        if os.geteuid()!=0:raise RuntimeError('Install only as root inside the dedicated guest')
        result=install(data,resource)
    print(json.dumps(result))


if __name__=='__main__':main()
