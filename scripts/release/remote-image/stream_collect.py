"""Admitted metadata collection and independent actual archive streaming; no full image copy."""
import os,sys,json,time,select,subprocess,zipfile,hashlib,stat,argparse,threading
from pathlib import Path
import stream_protocol as p
from guards import bound,real_ancestors

def api(endpoint):
 r=subprocess.run(['gh','api','repos/'+p.REPO+'/'+endpoint],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=60)
 assert r.returncode==0 and len(r.stdout)<=8*1024**2
 return p.load(r.stdout)
class Pipe:
 def __init__(self,stream,deadline):self.stream=stream;self.deadline=deadline
 def read(self,n):
  assert 0<=n<=1024**2
  if n==0:return b''
  chunks=[];remaining=n
  # gzip expects read(n) to fill n bytes unless true EOF. OS pipes may return
  # arbitrarily short fragments; retain the original absolute deadline per read.
  while remaining:
   left=self.deadline-time.monotonic();assert left>0,'Stream deadline'
   ready,_,_=select.select([self.stream],[],[],left);assert ready,'Stream deadline'
   chunk=os.read(self.stream.fileno(),remaining)
   if not chunk:break
   chunks.append(chunk);remaining-=len(chunk)
  return b''.join(chunks)
def download(assetid,consume):
 process=subprocess.Popen(['gh','api','repos/'+p.REPO+'/releases/assets/'+str(assetid),'-H','Accept: application/octet-stream'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
 timer=threading.Timer(1800,process.kill);timer.start()
 try:
  result=consume(Pipe(process.stdout,time.monotonic()+1800));assert process.wait(timeout=5)==0;return result
 finally:
  timer.cancel()
  if process.poll() is None:process.kill();process.wait(timeout=5)
  process.stdout.close()
def fp(st):return tuple(getattr(st,k) for k in ('st_dev','st_ino','st_mode','st_uid','st_gid','st_nlink','st_size','st_mtime_ns','st_ctime_ns'))
def metadata_file(path,artifact,s,runid,nonce):
 before=path.lstat();assert stat.S_ISREG(before.st_mode) and before.st_nlink==1 and not before.st_mode&0o111
 with os.fdopen(os.open(path,os.O_RDONLY|os.O_NOFOLLOW),'rb') as held:
  assert fp(before)==fp(os.fstat(held.fileno())) and before.st_size==artifact['size_in_bytes']<=p.MAX_METADATA
  assert 'sha256:'+hashlib.file_digest(held,'sha256').hexdigest()==artifact['digest'];held.seek(0)
  with zipfile.ZipFile(held) as z:rows,receipt=p.metadata(z,s,runid,nonce)
  assert fp(before)==fp(os.fstat(held.fileno()))==fp(path.lstat())
 return rows,receipt
def selected(s,selection,rawspec):
 assert set(selection)=={'approved','kind','minutes','run_id','nonce','spec_sha256','source','tool','pin_sha256','authority_sha256'}
 assert selection['approved'] is True and selection['kind']=='FIRST_IMAGE45' and selection['minutes']==45 and selection['spec_sha256']==p.sha(rawspec)
 for k in ('source','tool','pin_sha256','authority_sha256'):assert selection[k]==s[k]
 p.context(s,selection['run_id'],selection['nonce'])
def admitted(a,size,out):
 assert a['ordinary_uid']==223514 and type(a['at_unix'])in (int,float) and 0<=time.time()-a['at_unix']<=120
 assert a['artifact_bytes']==size and type(a['concurrent_remaining_bytes'])is int and a['concurrent_remaining_bytes']>=0
 assert a['peak_new_bytes']>=size+192*1024**2 and a['reserve_bytes']==50*1024**3
 actual=os.statvfs(out.parent);available=actual.f_bavail*actual.f_frsize
 assert min(available,a['available_bytes'])-a['peak_new_bytes']-a['concurrent_remaining_bytes']>=a['reserve_bytes']
def registry_asset(s,selection,result,receipt,asset,release):
 selected_fields=('source','tool','pin_sha256','authority_sha256')
 assert result['independent_archive_stream_verified'] is True and result['local_archive_copy'] is False
 for k in selected_fields:assert result[k]==s[k]
 assert result['run_id']==selection['run_id'] and result['nonce']==selection['nonce'] and result['spec_sha256']==selection['spec_sha256']==receipt['spec_sha256']
 assert result['receipt_sha256']==p.sha(json.dumps(receipt,sort_keys=True,separators=(',',':')).encode())
 assert result['archive_sha256']==receipt['sha256'] and result['archive_bytes']==receipt['size'] and result['all_layer_hashes_verified'] is True and result['layer_count']>0
 p.asset_join(asset,release,receipt,s,selection['run_id'],selection['nonce'])
 return {'release_id':receipt['release_id'],'asset_id':receipt['asset_id'],'name':receipt['name'],'size':receipt['size'],'sha256':receipt['sha256'],'config_sha256':result['config_sha256'],'source':s['source'],'version':s['version']}
def main():
 parser=argparse.ArgumentParser();parser.add_argument('--spec',required=True);parser.add_argument('--selection',required=True);parser.add_argument('--admission',required=True);parser.add_argument('--output',required=True);args=parser.parse_args()
 rawspec=Path(args.spec).read_bytes();s=bound(p.load(rawspec));selection=p.load(Path(args.selection).read_bytes());selected(s,selection,rawspec)
 runid=selection['run_id'];nonce=selection['nonce'];out=Path(args.output);real_ancestors(out.parent);assert not out.exists() and not out.is_symlink()
 run=api('actions/runs/'+str(runid));jobs=api('actions/runs/'+str(runid)+'/jobs?per_page=100');artifacts=api('actions/runs/'+str(runid)+'/artifacts?per_page=100');assert artifacts['total_count']==1 and len(artifacts['artifacts'])==1
 artifact=artifacts['artifacts'][0];p.run_join(run,jobs,artifact,s,runid,nonce);admitted(p.load(Path(args.admission).read_bytes()),artifact['size_in_bytes'],out)
 out.mkdir()
 with open(out/'collection-intent.json','x') as f:json.dump({'selected':selection,'artifact_id':artifact['id'],'automatic_retry':False,'local_archive_copy':False},f)
 def receive(stream):
  with open(out/'metadata.zip','xb') as f:
   h=hashlib.sha256();size=0
   while data:=stream.read(1024**2):
    size+=len(data);assert size<=artifact['size_in_bytes'];h.update(data);f.write(data)
   assert size==artifact['size_in_bytes'] and artifact['digest']=='sha256:'+h.hexdigest()
 download_artifact=artifact['id']
 # Artifact archive endpoint differs from private release asset endpoint.
 def fetch_artifact():
  process=subprocess.Popen(['gh','api','repos/'+p.REPO+'/actions/artifacts/'+str(download_artifact)+'/zip'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
  timer=threading.Timer(1800,process.kill);timer.start()
  try:receive(Pipe(process.stdout,time.monotonic()+1800));assert process.wait(timeout=5)==0
  finally:
   timer.cancel()
   if process.poll() is None:process.kill();process.wait(timeout=5)
   process.stdout.close()
 fetch_artifact();rows,receipt=metadata_file(out/'metadata.zip',artifact,s,runid,nonce)
 assert receipt['spec_sha256']==selection['spec_sha256']
 asset=api('releases/assets/'+str(receipt['asset_id']));release=api('releases/'+str(receipt['release_id']));p.asset_join(asset,release,receipt,s,runid,nonce)
 result=download(receipt['asset_id'],lambda stream:p.verify_archive(stream,receipt,rows['archive-config.json'],rows['archive-manifest.json'],s))
 # Repeat association/readback after the actual body stream; no uploader-only trust.
 asset2=api('releases/assets/'+str(receipt['asset_id']));release2=api('releases/'+str(receipt['release_id']));p.asset_join(asset2,release2,receipt,s,runid,nonce)
 result.update({k:s[k] for k in ('tool','pin_sha256','authority_sha256')});result.update({'run_id':runid,'nonce':nonce,'spec_sha256':selection['spec_sha256'],'receipt_sha256':p.sha(json.dumps(receipt,sort_keys=True,separators=(',',':')).encode()),'independent_archive_stream_verified':True})
 for n,data in rows.items():
  with open(out/n,'xb') as f:f.write(data)
 result['registry_asset']=registry_asset(s,selection,result,receipt,asset2,release2)
 for name,data in [('run.json',run),('jobs.json',jobs),('artifact.json',artifact),('release.json',release2),('asset.json',asset2),('validation.json',result)]:
  with open(out/name,'x') as f:json.dump(data,f,indent=2);f.write('\n')
if __name__=='__main__':main()
