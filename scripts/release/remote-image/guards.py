import hashlib,json,os,re,stat,zipfile,io
from pathlib import Path
REPO='awpsec/kindred-server'
FIXTURES={'proof/email-fixtures/mcp__claude_ai_Gmail__send_email.json','proof/email-fixtures/mcp__claude_ai_Gmail__send_message.json'}
SCRIPTS={'build-image.sh','stage-server-image.sh','standalone-image-tmpfs.Dockerfile','server-image-publication.Dockerfile','proof/verify-bundle.py','proof/verify-current-harness.py','proof/stage-context.py','proof/run-image-checks.sh','proof/check-image.py','proof/smoke-image-server.py','proof/check-image-documentation.py','proof/check-provider-helper.py','proof/check-current-image-harness.py','proof/check-image-sdk.py'}
ALLOWED=SCRIPTS|FIXTURES|{'proof/email-fixture-source-binding.json','server/SOURCE.json'}
def sha(b):return hashlib.sha256(b).hexdigest()
def load(b):
 def pairs(rows):
  d={}
  for k,v in rows:
   assert k not in d,'Duplicate JSON key';d[k]=v
  return d
 return json.loads(b,object_pairs_hook=pairs,parse_constant=lambda _: (_ for _ in ()).throw(AssertionError('Nonfinite JSON')))
def bound(s):
 assert s['bound'] is True and s['version']=='0.87.3' and s['repo']==REPO
 for key,n in [('source',40),('tool',40),('pin_sha256',64),('authority_sha256',64)]:assert re.fullmatch('[0-9a-f]{'+str(n)+'}',s[key])
 assert re.fullmatch(r'codex/[A-Za-z0-9._/-]+',s['tool_ref']) and '..' not in s['tool_ref']
 for key in ['recipe','bundle']:
  a=s[key];assert type(a['asset_id']) is int and a['asset_id']>0 and type(a['release_id']) is int and a['release_id']>0
  assert type(a['size']) is int and 0<a['size']<=128*1024**2 and re.fullmatch('[0-9a-f]{64}',a['sha256'])
 assert s['recipe']['name']=='remote-image-recipe.zip' and s['bundle']['name']=='standalone.zip'
 assert set(s['files'])==ALLOWED
 for n,r in s['files'].items():assert re.fullmatch('[0-9a-f]{64}',r['sha256']) and type(r['size']) is int and 0<=r['size']<=8*1024**2
 return s
def real_ancestors(p):
 p=Path(p).absolute()
 for a in [p,*p.parents]:assert stat.S_ISDIR(a.lstat().st_mode) and not a.is_symlink(),'Unsafe ancestor'
 return p

def recipe_members(data,s):
 assert len(data)==s['recipe']['size'] and sha(data)==s['recipe']['sha256']
 with zipfile.ZipFile(io.BytesIO(data)) as z:
  infos=z.infolist();names=[x.filename for x in infos];assert len(names)==len(set(names)) and set(names)==ALLOWED
  assert sum(x.file_size for x in infos)<=256*1024**2
  rows={}
  for i in infos:
   assert not i.is_dir() and i.file_size==s['files'][i.filename]['size'] and not i.flag_bits&1
   mode=i.external_attr>>16;assert stat.S_IFMT(mode) in (0,stat.S_IFREG) and not mode&0o111
   b=z.read(i);assert sha(b)==s['files'][i.filename]['sha256'];rows[i.filename]=b
  assert z.testzip() is None
 return rows

def write_recipe(rows,target):
 target=Path(target);assert not target.exists() and not target.is_symlink();real_ancestors(target.parent);target.mkdir()
 for name,data in rows.items():
  assert name in ALLOWED;p=target/name;p.parent.mkdir(parents=True,exist_ok=True);real_ancestors(p.parent)
  fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
  with os.fdopen(fd,'wb') as f:f.write(data)
