import hashlib,json,os,shutil,tarfile,stat
from contextlib import contextmanager
from pathlib import Path
from guards import real_ancestors
@contextmanager
def bound_file(path,limit):
    real_ancestors(path.parent)
    before=path.lstat()
    assert stat.S_ISREG(before.st_mode) and before.st_nlink==1 and not before.st_mode&0o111 and before.st_size<=limit
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
    with os.fdopen(fd,'rb') as stream:
        opened=os.fstat(stream.fileno())
        identity=lambda s:(s.st_dev,s.st_ino,s.st_mode,s.st_nlink,s.st_size,s.st_mtime_ns,s.st_ctime_ns)
        assert identity(before)==identity(opened)
        yield stream
        assert identity(before)==identity(os.fstat(stream.fileno()))==identity(path.lstat())
stage=Path('recipe'); out=Path('export'); archive=stage/'image-inputs/server-image.tar.gz'
real_ancestors(archive.parent);real_ancestors(out)
assert not list(out.iterdir())
assert archive.is_file() and not archive.is_symlink() and archive.stat().st_size<=2*1024**3
# Do not unpack image layers. Preserve actual archive config descriptor bytes.
with bound_file(archive,2*1024**3) as image_stream, tarfile.open(fileobj=image_stream,mode='r:gz') as tar:
    member=tar.getmember('manifest.json'); assert member.isfile() and member.size<=1024*1024
    manifest=json.load(tar.extractfile(member)); assert len(manifest)==1
    config=manifest[0]['Config']; assert '..' not in Path(config).parts and not Path(config).is_absolute()
    m=tar.getmember(config); assert m.isfile() and m.size<=1024*1024
    raw=tar.extractfile(m).read(); parsed=json.loads(raw)
    labels=parsed['config']['Labels']; assert labels['org.opencontainers.image.revision']==os.environ['SOURCE'] and labels['org.opencontainers.image.version']==os.environ['VERSION']
    (out/'archive-config.json').write_bytes(raw)
    (out/'archive-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
names=['bundle-verification.json','current-harness-custody.json','standalone-context-custody.json','image-documentation-manifest.json','standalone-image-bytecheck.log','standalone-image-server-smoke.log','publication-image-stage.log','image-documentation-check.json','image-provider-helper-check.json','image-current-harness-check.json','image-sdk-check.json','publication-image-identity.json','remote-bundle-check.log','remote-harness-check.log','remote-context-check.log','remote-build.log','remote-image-checks.log']
for name in names:
    p=stage/'proof'/name; real_ancestors(p.parent); assert p.is_file() and not p.is_symlink() and p.stat().st_size<=8*1024**2
    with bound_file(p,8*1024**2) as stream:
        data=stream.read(8*1024**2+1)
    assert len(data)<=8*1024**2
    fd=os.open(out/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,'wb') as stream:stream.write(data)
with bound_file(archive,2*1024**3):pass
assert not (out/'server-image.tar.gz').exists()
shutil.move(archive,out/'server-image.tar.gz')
rows={}
for p in out.iterdir():
    with bound_file(p,2*1024**3) as stream:
        rows[p.name]={'sha256':hashlib.file_digest(stream,'sha256').hexdigest(),'size':os.fstat(stream.fileno()).st_size}
assert sum(v['size'] for v in rows.values())<=2304*1024**2
(out/'custody.json').write_text(json.dumps({'source':os.environ['SOURCE'],'version':os.environ['VERSION'],'run_id':os.environ['GITHUB_RUN_ID'],'attempt':os.environ['GITHUB_RUN_ATTEMPT'],'files':rows,'tool':os.environ['GITHUB_SHA'],'registry_published':False},indent=2)+'\n')
