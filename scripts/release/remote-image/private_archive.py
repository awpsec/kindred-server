"""Remote same-job upload, after unchanged image QA/export. No dispatch or retries."""
import os,sys,json,re,subprocess,hashlib
from pathlib import Path
from guards import bound,load
import stream_protocol as p

def api(endpoint,method='GET',body=None,stdin=None):
 args=['gh','api',endpoint,'--method',method]
 if stdin is not None:
  size=os.fstat(stdin.fileno()).st_size
  assert type(size)is int and 0<size<=p.MAX_ARCHIVE and stdin.tell()==0
  args+=['--input','-','-H','Content-Type: application/gzip','-H','Content-Length: '+str(size)]
 elif body is not None:args+=['--input','-']
 result=subprocess.run(args,input=None if body is None else json.dumps(body).encode(),stdin=stdin,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=900)
 if result.returncode!=0:
  # Retain only numeric HTTP status and fixed operation identity; never raw stderr.
  statuses=re.findall(rb'HTTP ([1-5][0-9]{2})',result.stderr[:65536])
  diagnostic={'phase':'private_archive_api','operation':'archive_upload' if stdin is not None else 'release_metadata','returncode':result.returncode,'http_statuses':[int(x) for x in statuses[:4]],'stderr_bytes':len(result.stderr),'automatic_retry':False}
  print(json.dumps(diagnostic,sort_keys=True),file=sys.stderr)
  raise AssertionError('Private upload API failed; no automatic retry')
 assert len(result.stdout)<=8*1024**2
 return load(result.stdout)

def main():
 s=bound(load(Path('spec-consumed.json').read_bytes()))
 assert os.environ['GITHUB_REPOSITORY']==p.REPO and os.environ['GITHUB_EVENT_NAME']=='workflow_dispatch' and os.environ['GITHUB_RUN_ATTEMPT']=='1'
 runid=int(os.environ['GITHUB_RUN_ID']);nonce=load(Path(os.environ['GITHUB_EVENT_PATH']).read_bytes())['inputs']['nonce'];tag=p.context(s,runid,nonce)
 assert s['tool']==os.environ['GITHUB_SHA']==os.environ['TOOL'] and s['source']==os.environ['SOURCE'] and s['version']==os.environ['VERSION']
 spec_sha=os.environ['SPEC_SHA'];assert p.hexstr(spec_sha,64)
 out=Path('export');archive=out/'server-image.tar.gz';custody=load((out/'custody.json').read_bytes())
 assert custody['source']==s['source'] and custody['tool']==s['tool'] and custody['run_id']==str(runid) and custody['attempt']=='1' and custody['version']==s['version'] and custody['registry_published'] is False
 row=custody['files']['server-image.tar.gz'];assert type(row['size'])is int and 0<row['size']<=p.MAX_ARCHIVE and p.hexstr(row['sha256'],64)
 # Intent is exclusive and precedes any mutating API request. A failure consumes this path.
 with open('private-archive-upload-intent.json','x') as f:json.dump({'run_id':runid,'nonce':nonce,'tag':tag,'source':s['source'],'tool':s['tool'],'archive':row,'spec_sha256':spec_sha,'automatic_retry':False},f)
 with p.bound_file(archive,p.MAX_ARCHIVE) as held:
  assert os.fstat(held.fileno()).st_size==row['size'] and hashlib.file_digest(held,'sha256').hexdigest()==row['sha256'];held.seek(0)
  release=api('repos/'+p.REPO+'/releases','POST',{'tag_name':tag,'target_commitish':s['source'],'name':'Private exact image '+str(runid),'draft':True,'body':'Source-bound image archive from the selected image QA job; registry publication remains separate.'})
  assert release['draft'] is True and release['tag_name']==tag and release['target_commitish']==s['source']
  endpoint='https://uploads.github.com/repos/'+p.REPO+'/releases/'+str(release['id'])+'/assets?name=server-image.tar.gz'
  asset=api(endpoint,'POST',stdin=held)
  receipt={'schema':1,'source':s['source'],'tool':s['tool'],'version':s['version'],'run_id':runid,'attempt':1,'nonce':nonce,'pin_sha256':s['pin_sha256'],'authority_sha256':s['authority_sha256'],'release_id':release['id'],'asset_id':asset['id'],'name':'server-image.tar.gz','size':row['size'],'sha256':row['sha256'],'tag':tag,'private_draft':True,'spec_sha256':spec_sha}
  release=api('repos/'+p.REPO+'/releases/'+str(release['id']));p.asset_join(asset,release,receipt,s,runid,nonce)
 # Keep the only archive on the remote runner, outside the small metadata artifact. No local download/copy.
 retained=Path('remote-private-archive');retained.mkdir(exist_ok=False);os.rename(archive,retained/archive.name)
 with open(out/'private-archive.json','x') as f:json.dump(receipt,f,indent=2);f.write('\n')
 assert set(x.name for x in out.iterdir())==p.SMALL
if __name__=='__main__':main()
