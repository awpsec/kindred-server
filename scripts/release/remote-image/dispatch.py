import os,json,subprocess,re,secrets,datetime
from pathlib import Path
from guards import bound,load,sha,REPO
if __name__=='__main__':
 import sys
 spec=bound(load(Path(sys.argv[1]).read_bytes()));selection=load(Path(sys.argv[2]).read_bytes())
 assert selection['selected'] is True and selection['minutes']==45 and selection['attempts']==1 and selection['source']==spec['source'] and selection['tool']==spec['tool']
 assert selection['spec_sha256']==sha(Path(sys.argv[1]).read_bytes())
 assert sha(Path(selection['authority_path']).read_bytes())==spec['authority_sha256'] and sha(Path(selection['pin_path']).read_bytes())==spec['pin_sha256']
 pin=load(Path(selection['pin_path']).read_bytes());assert pin['version']==spec['version'] and pin['repositories']['server']['commit']==spec['source']
 assert selection['source_review_accepted'] is True
 remote=subprocess.check_output(['git','ls-remote','https://github.com/'+REPO+'.git','refs/heads/'+spec['tool_ref']],text=True,timeout=30).split()
 assert remote==[spec['tool'],'refs/heads/'+spec['tool_ref']]
 asset=selection['spec_asset'];assert asset['sha256']==selection['spec_sha256'] and asset['size']==Path(sys.argv[1]).stat().st_size
 # Read-only input artifact guards before the one irreversible paid-dispatch intent.
 from input import download
 assert download(asset)==Path(sys.argv[1]).read_bytes()
 for key in ['recipe','bundle']:
  m=load(subprocess.check_output(['gh','api',f"repos/{REPO}/releases/assets/{spec[key]['asset_id']}"],timeout=30));assert m['size']==spec[key]['size'] and m['digest']=='sha256:'+spec[key]['sha256'] and m['name']==spec[key]['name']
 nonce=secrets.token_hex(16);intent=Path(sys.argv[3]);assert intent.absolute()==Path(selection['intent_path']).absolute();fd=os.open(intent,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 with os.fdopen(fd,'w') as f:json.dump({'at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'source':spec['source'],'tool':spec['tool'],'nonce':nonce,'spec_sha256':asset['sha256'],'selected_minutes':45,'no_retry':True},f);f.flush();os.fsync(f.fileno())
 args=['gh','workflow','run','remote-image.yml','--repo',REPO,'--ref',spec['tool_ref']]
 fields={'approve':'true','source':spec['source'],'tool':spec['tool'],'version':spec['version'],'spec_id':str(asset['asset_id']),'spec_release':str(asset['release_id']),'spec_sha':asset['sha256'],'spec_size':str(asset['size']),'nonce':nonce}
 for k,v in fields.items():args+=['-f',k+'='+v]
 subprocess.run(args,check=True,timeout=60)
 print(json.dumps({'dispatched':True,'nonce':nonce,'run_id':'UNRESOLVED; bind unique displayTitle remote-image-'+nonce+' and actual tool SHA/event/workflow before collection; never redispatch on unknown'}))
