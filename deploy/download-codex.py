#!/usr/bin/env python3
"""Stage the complete pinned Codex runtime, or install a staged runtime offline.

download-codex.py TARGET stages a verified official package and its CLI symlink.
download-codex.py --install SOURCE TARGET installs it under TARGET's prefix.
"""
import hashlib,json,os,re,runpy,shutil,sys,tarfile,tempfile,urllib.request,uuid
from pathlib import Path, PurePosixPath

VERSION='0.153.4'
NAME='codex-package-x86_64-unknown-linux-musl.tar.gz'
CHECKER=runpy.run_path(str(Path(__file__).with_name('check-codex.py')))
FILES=CHECKER['FILES']

def digest(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()

def validate(root):
    metadata=CHECKER['validate'](root)
    if metadata.get('version')!=VERSION:raise ValueError('Unexpected Codex package version')

def extract(archive,root):
    with tarfile.open(archive) as tar:
        members={}
        for member in tar.getmembers():
            if member.isdir():continue
            path=PurePosixPath(member.name)
            safe=not path.is_absolute() and '..' not in path.parts and str(path)==member.name
            allowed=member.name in FILES or member.name.startswith('codex-resources/')
            if not safe or not allowed or not member.isfile() or member.name in members:
                raise ValueError('Unexpected Codex package member: '+member.name)
            if not 0<member.size<=512*1024*1024:raise ValueError('Invalid Codex package member size')
            members[member.name]=member
        if not set(FILES).issubset(members):raise ValueError('Official Codex package is incomplete')
        for name,member in members.items():
            target=root/name;target.parent.mkdir(parents=True,exist_ok=True)
            with tar.extractfile(member) as source,target.open('wb') as out:shutil.copyfileobj(source,out)
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
    validate(root)

def publish(source,destination,target):
    validate(source);destination.parent.mkdir(parents=True,exist_ok=True)
    if destination.exists():
        validate(destination)
        source_files={p.relative_to(source) for p in source.rglob('*') if p.is_file()}
        destination_files={p.relative_to(destination) for p in destination.rglob('*') if p.is_file()}
        if source_files!=destination_files or any(digest(source/name)!=digest(destination/name) for name in source_files):
            raise ValueError('Existing Codex runtime differs from the verified package')
    else:
        with tempfile.TemporaryDirectory(prefix='.codex-stage-',dir=destination.parent) as temporary:
            staged=Path(temporary)/'runtime';shutil.copytree(source,staged)
            os.replace(staged,destination)
    # Test the actual destination filesystem/runtime before switching the CLI.
    # This catches noexec mounts, damaged binaries and broken tool transport.
    CHECKER['check'](destination/'bin/codex')
    target.parent.mkdir(parents=True,exist_ok=True)
    link=target.with_name('.'+target.name+'-'+uuid.uuid4().hex)
    try:
        link.symlink_to(os.path.relpath(destination/'bin/codex',target.parent))
        os.replace(link,target)
    finally:link.unlink(missing_ok=True)

def download(target):
    request=urllib.request.Request(f'https://api.github.com/repos/openai/codex/releases/tags/rust-v{VERSION}',headers={'User-Agent':'Kindred-build'})
    with urllib.request.urlopen(request,timeout=60) as response:release=json.load(response)
    asset=next(a for a in release['assets'] if a['name']==NAME)
    with tempfile.TemporaryDirectory() as temporary:
        archive=Path(temporary)/NAME;root=Path(temporary)/'runtime'
        with urllib.request.urlopen(asset['browser_download_url'],timeout=120) as response,archive.open('wb') as out:shutil.copyfileobj(response,out)
        if 'sha256:'+digest(archive)!=asset.get('digest'):raise ValueError('Official Codex asset digest did not match')
        extract(archive,root)
        publish(root,target.parent/('codex-runtime-'+VERSION),target)

def install(source,target):
    # Keep the manifest, sibling code-mode host, PATH tools and sandbox resources.
    root=source.resolve().parent.parent
    destination=target.parent.parent/'lib/kindred'/('codex-runtime-'+VERSION)
    publish(root,destination,target)
    # Existing app-server processes may still locate their sibling through the
    # old flat entrypoint. Keep that path working without interrupting a turn.
    helper=target.with_name('codex-code-mode-host');link=helper.with_name('.'+helper.name+'-'+uuid.uuid4().hex)
    try:
        link.symlink_to(os.path.relpath(destination/'bin/codex-code-mode-host',helper.parent))
        os.replace(link,helper)
    finally:link.unlink(missing_ok=True)

def update(target):
    global VERSION
    request=urllib.request.Request('https://api.github.com/repos/openai/codex/releases/latest',headers={'User-Agent':'Kindred-update'})
    with urllib.request.urlopen(request,timeout=60) as response:release=json.load(response)
    tag=release.get('tag_name','')
    if release.get('prerelease') or release.get('draft') or not re.fullmatch(r'rust-v[0-9]+\.[0-9]+\.[0-9]+',tag):
        raise ValueError('Expected an official stable Codex release')
    VERSION=tag.removeprefix('rust-v')
    with tempfile.TemporaryDirectory() as directory:
        staged=Path(directory)/'codex'
        download(staged)
        install(staged,target)

if __name__=='__main__':
    if len(sys.argv)==3 and sys.argv[1]=='--update':update(Path(sys.argv[2]))
    elif len(sys.argv)==4 and sys.argv[1]=='--install':install(Path(sys.argv[2]),Path(sys.argv[3]))
    elif len(sys.argv)==2:download(Path(sys.argv[1]))
    else:raise SystemExit(__doc__)
    print('Verified complete official Codex '+VERSION+' runtime')
