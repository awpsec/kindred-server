"""Bounded pure protocol; no product execution, extraction, or credential access."""
import hashlib,io,json,re,tarfile,gzip,stat,os
from pathlib import PurePosixPath,Path
REPO='awpsec/kindred-server'
MAX_ARCHIVE=2*1024**3
MAX_TAR=16*1024**3
MAX_METADATA=160*1024**2
LOGS={'bundle-verification.json','current-harness-custody.json','standalone-context-custody.json','image-documentation-manifest.json','standalone-image-bytecheck.log','standalone-image-server-smoke.log','publication-image-stage.log','image-documentation-check.json','image-provider-helper-check.json','image-current-harness-check.json','image-sdk-check.json','publication-image-identity.json','remote-bundle-check.log','remote-harness-check.log','remote-context-check.log','remote-build.log','remote-image-checks.log'}
SMALL=LOGS|{'archive-config.json','archive-manifest.json','custody.json','private-archive.json'}
def sha(b):return hashlib.sha256(b).hexdigest()
def load(b):
 def pairs(rows):
  d={}
  for k,v in rows:assert k not in d;d[k]=v
  return d
 return json.loads(b,object_pairs_hook=pairs,parse_constant=lambda _:(_ for _ in ()).throw(AssertionError('Nonfinite JSON')))
def hexstr(x,n):return type(x)is str and re.fullmatch('[0-9a-f]{'+str(n)+'}',x) is not None
def safe(n):return type(n)is str and bool(n) and not n.startswith('/') and '\\' not in n and all(p not in ('','.','..') for p in n.split('/'))
def context(s,runid,nonce):
 assert s['bound'] is True and s['stream_mode']=='private-archive-v1' and s['version']=='0.87.6' and s['repo']==REPO
 for k,n in [('source',40),('tool',40),('pin_sha256',64),('authority_sha256',64)]:assert hexstr(s[k],n)
 assert type(runid)is int and runid>0 and re.fullmatch('[A-Za-z0-9_-]{16,64}',nonce)
 assert re.fullmatch('codex/[A-Za-z0-9._/-]+',s['tool_ref']) and '..' not in s['tool_ref']
 return 'image-private-'+s['version']+'-'+str(runid)+'-'+nonce

def asset_join(asset,release,receipt,s,runid,nonce):
 tag=context(s,runid,nonce)
 assert set(receipt)=={'schema','source','tool','version','run_id','attempt','nonce','pin_sha256','authority_sha256','release_id','asset_id','name','size','sha256','tag','private_draft','spec_sha256'}
 assert type(receipt['schema'])is int and type(receipt['attempt'])is int and type(receipt['run_id'])is int
 assert receipt['schema']==1 and receipt['source']==s['source'] and receipt['tool']==s['tool'] and receipt['version']==s['version'] and receipt['run_id']==runid and receipt['attempt']==1 and receipt['nonce']==nonce
 assert receipt['pin_sha256']==s['pin_sha256'] and receipt['authority_sha256']==s['authority_sha256']
 assert receipt['private_draft'] is True and receipt['tag']==tag and receipt['name']=='server-image.tar.gz' and hexstr(receipt['sha256'],64)
 assert type(receipt['size'])is int and 0<receipt['size']<=MAX_ARCHIVE
 assert type(receipt['asset_id'])is int and receipt['asset_id']>0 and type(receipt['release_id'])is int and receipt['release_id']>0
 assert release['id']==receipt['release_id'] and release['draft'] is True and release['tag_name']==tag and release['target_commitish']==s['source']
 assert asset['id']==receipt['asset_id'] and asset['state']=='uploaded' and asset['name']==receipt['name'] and asset['size']==receipt['size'] and asset['digest']=='sha256:'+receipt['sha256']
 matches=[a for a in release['assets'] if a['id']==asset['id']];assert len(matches)==1 and all(matches[0][k]==asset[k] for k in ('id','name','size','digest','state'))
 return True

def run_join(run,jobs,api,s,runid,nonce):
 context(s,runid,nonce)
 assert type(run['id'])is int and type(run['run_attempt'])is int
 assert run['id']==runid and run['run_attempt']==1 and run['head_sha']==s['tool'] and run['head_branch']==s['tool_ref'] and run['event']=='workflow_dispatch' and run['path']=='.github/workflows/remote-image.yml'
 assert run['status']=='completed' and run['conclusion']=='success' and run['display_title']=='remote-image-'+nonce and run['repository']['full_name']==REPO
 assert len(jobs['jobs'])==1 and jobs['jobs'][0]['conclusion']=='success'
 required={'Verify exact private inputs and final product/tool binding','Build and run the existing isolated image QA','Upload exact archive to private draft without local archive copy','Upload bounded verified archive and named receipts'}
 steps=jobs['jobs'][0]['steps'];assert all(sum(x['name']==n and x['conclusion']=='success' for x in steps)==1 for n in required)
 association=api['workflow_run'];assert type(association['id'])is int and type(association['repository_id'])is int and association['id']==runid and association['head_sha']==s['tool'] and association['repository_id']==run['repository']['id']
 assert api['name']=='remote-image-'+s['version']+'-'+str(runid) and api['expired'] is False and type(api['size_in_bytes'])is int and 0<api['size_in_bytes']<=MAX_METADATA and hexstr(api['digest'].removeprefix('sha256:'),64) and api['digest'].startswith('sha256:')

def metadata(z,s,runid,nonce):
 infos=z.infolist();assert len(infos)==len(SMALL) and set(z.namelist())==SMALL and sum(i.file_size for i in infos)<=MAX_METADATA
 for i in infos:assert not i.is_dir() and not i.flag_bits&1 and stat.S_IFMT(i.external_attr>>16) in (0,stat.S_IFREG) and not (i.external_attr>>16)&0o111 and i.file_size<=8*1024**2
 rows={n:z.read(n) for n in SMALL};assert z.testzip() is None
 c=load(rows['custody.json']);assert set(c)=={'source','tool','version','run_id','attempt','registry_published','files'}
 assert c['source']==s['source'] and c['version']==s['version'] and c['run_id']==str(runid) and c['attempt']=='1' and c['tool']==s['tool'] and c['registry_published'] is False
 assert set(c['files'])==LOGS|{'archive-config.json','archive-manifest.json','server-image.tar.gz'}
 for n in LOGS|{'archive-config.json','archive-manifest.json'}:assert c['files'][n]=={'size':len(rows[n]),'sha256':sha(rows[n])}
 receipt=load(rows['private-archive.json']);assert c['files']['server-image.tar.gz']=={'size':receipt['size'],'sha256':receipt['sha256']}
 return rows,receipt

class Meter:
 def __init__(self,raw,limit):self.raw=raw;self.limit=limit;self.count=0;self.hash=hashlib.sha256()
 def read(self,n=-1):
  assert 0<=n<=1024**2,'Bound every stream read'
  b=self.raw.read(n);self.count+=len(b);assert self.count<=self.limit;self.hash.update(b);return b

def verify_archive(raw,receipt,config_raw,manifest_raw,s):
 """Consume the actual gzip stream once; retain no layer or archive bytes."""
 manifest=load(manifest_raw);assert type(manifest)is list and len(manifest)==1
 m=manifest[0];config=load(config_raw);assert config['architecture']=='amd64' and config['os']=='linux'
 labels=config['config']['Labels'];assert labels['org.opencontainers.image.revision']==s['source'] and labels['org.opencontainers.image.version']==s['version']
 assert m['Config']=='blobs/sha256/'+sha(config_raw) and m['RepoTags']==['ghcr.io/awpsec/kindred-server:'+s['version']]
 layers=m['Layers'];assert 0<len(layers)<=128 and len(set(layers))==len(layers) and all(re.fullmatch('blobs/sha256/[0-9a-f]{64}',n) for n in layers)
 assert config['rootfs']['type']=='layers' and config['rootfs']['diff_ids']==['sha256:'+n.rsplit('/',1)[1] for n in layers]
 compressed=Meter(raw,receipt['size']);decompressed=Meter(gzip.GzipFile(fileobj=compressed,mode='rb'),MAX_TAR);seen=set();found=set();small={}
 with tarfile.open(fileobj=decompressed,mode='r|') as tar:
  for member in tar:
   name=member.name.rstrip('/');assert safe(name) and name not in seen and len(seen)<2048;seen.add(name)
   if member.isdir():assert member.size==0;continue
   assert member.isfile() and 0<=member.size<=MAX_TAR
   source=tar.extractfile(member)
   if name in layers:
    h=hashlib.sha256();size=0
    while chunk:=source.read(1024**2):h.update(chunk);size+=len(chunk)
    assert size==member.size and h.hexdigest()==name.rsplit('/',1)[1];found.add(name)
   else:
    assert member.size<=1024**2
    data=source.read(1024**2+1);assert len(data)==member.size
    if name.startswith('blobs/sha256/'):assert sha(data)==name.rsplit('/',1)[1]
    assert sum(len(v) for v in small.values())+len(data)<=8*1024**2
    small[name]=data
 # Validate gzip trailer and consume tar padding/trailing bytes under the same total bounds.
 while decompressed.read(1024**2):pass
 while compressed.read(1024**2):pass
 assert compressed.count==receipt['size'] and compressed.hash.hexdigest()==receipt['sha256']
 assert found==set(layers) and small[m['Config']]==config_raw and load(small['manifest.json'])==manifest
 return {'archive_sha256':receipt['sha256'],'archive_bytes':compressed.count,'config_sha256':sha(config_raw),'layer_count':len(found),'all_layer_hashes_verified':True,'local_archive_copy':False,'source':s['source'],'version':s['version']}

from contextlib import contextmanager
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
