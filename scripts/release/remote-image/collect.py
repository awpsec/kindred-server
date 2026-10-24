# Pure completed-artifact validator. Download is a separate admitted, once-only operation.
import io,zipfile,stat,re,json,hashlib,tarfile
from pathlib import Path
from guards import load,sha
LOGS={'bundle-verification.json','current-harness-custody.json','standalone-context-custody.json','image-documentation-manifest.json','standalone-image-bytecheck.log','standalone-image-server-smoke.log','publication-image-stage.log','image-documentation-check.json','image-provider-helper-check.json','image-current-harness-check.json','image-sdk-check.json','publication-image-identity.json','remote-bundle-check.log','remote-harness-check.log','remote-context-check.log','remote-build.log','remote-image-checks.log'}
NAMES=LOGS|{'archive-config.json','archive-manifest.json','server-image.tar.gz','custody.json'}
def validate(data,api,run,s,runid,nonce):
 assert type(runid) is int and runid>0 and run['repository']['full_name']=='awpsec/kindred-server'
 association=api['workflow_run'];assert type(association['id']) is int and association['id']==runid and association['head_sha']==s['tool']
 assert type(association['repository_id']) is int and association['repository_id']==run['repository']['id']
 assert run['id']==runid and run['run_attempt']==1 and run['head_sha']==s['tool'] and run['event']=='workflow_dispatch'
 assert run['path']=='.github/workflows/remote-image.yml' and run['status']=='completed' and run['conclusion']=='success' and run['display_title']=='remote-image-'+nonce
 assert api['name']=='remote-image-'+s['version']+'-'+str(runid) and api['expired'] is False
 assert type(api['size_in_bytes']) is int and len(data)==api['size_in_bytes']<=2304*1024**2 and api['digest']=='sha256:'+sha(data)
 with zipfile.ZipFile(io.BytesIO(data)) as z:
  members=z.infolist();assert len(members)==len(NAMES) and set(z.namelist())==NAMES
  assert sum(i.file_size for i in members)<=2304*1024**2
  for i in members:
   assert not i.is_dir() and not i.flag_bits&1 and stat.S_IFMT(i.external_attr>>16) in (0,stat.S_IFREG)
   assert i.file_size <= (2*1024**3 if i.filename=='server-image.tar.gz' else 8*1024**2)
  assert z.testzip() is None;c=load(z.read('custody.json'));assert c['source']==s['source'] and c['version']==s['version'] and c['run_id']==str(runid) and c['attempt']=='1' and c['tool']==s['tool']
  assert set(c['files'])==NAMES-{'custody.json'}
  for name,row in c['files'].items():
   d=z.read(name);assert len(d)==row['size'] and sha(d)==row['sha256']
  config=z.read('archive-config.json');labels=load(config)['config']['Labels'];assert labels['org.opencontainers.image.revision']==s['source'] and labels['org.opencontainers.image.version']==s['version']
  manifest=load(z.read('archive-manifest.json'));assert len(manifest)==1 and Path(manifest[0]['Config']).name.split('.')[0]==sha(config)
  with tarfile.open(fileobj=io.BytesIO(z.read('server-image.tar.gz')),mode='r:gz') as tar:
   m=tar.getmember('manifest.json');assert m.isfile() and m.size<=1024**2 and load(tar.extractfile(m).read())==manifest
   m=tar.getmember(manifest[0]['Config']);assert m.isfile() and m.size<=1024**2 and tar.extractfile(m).read()==config
 return {'ready_for_server_technical_review':True,'source':s['source'],'archive_config_sha256':sha(config),'registry_outcome':False,'required_next':'Full raw registry digest/descriptor must equal this config; archive bytes and manifests remain immutable.'}
