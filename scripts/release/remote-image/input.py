import os,subprocess,json,hashlib,re
from pathlib import Path
from guards import *
def api(path,raw=False):
 args=['gh','api',path]
 if raw:args+=['-H','Accept: application/octet-stream']
 return subprocess.check_output(args,timeout=120)
def download(a):
 m=load(api(f"repos/{REPO}/releases/assets/{a['asset_id']}"));r=load(api(f"repos/{REPO}/releases/{a['release_id']}"))
 assert r['draft'] is True and any(x['id']==a['asset_id'] for x in r['assets'])
 assert m['name']==a['name'] and m['size']==a['size'] and m['state']=='uploaded' and m['digest']=='sha256:'+a['sha256']
 d=api(f"repos/{REPO}/releases/assets/{a['asset_id']}",True);assert len(d)==a['size'] and sha(d)==a['sha256'];return d
if __name__=='__main__':
 assert os.environ['GITHUB_REPOSITORY']==REPO
 assert os.environ['APPROVE']=='true' and os.environ['GITHUB_RUN_ATTEMPT']=='1'
 for k in ['SPEC_ID','SPEC_RELEASE','SPEC_SIZE']:assert re.fullmatch('[1-9][0-9]*',os.environ[k])
 assert int(os.environ['SPEC_SIZE'])<=1024**2 and re.fullmatch('[0-9a-f]{64}',os.environ['SPEC_SHA'])
 a={'asset_id':int(os.environ['SPEC_ID']),'release_id':int(os.environ['SPEC_RELEASE']),'size':int(os.environ['SPEC_SIZE']),'sha256':os.environ['SPEC_SHA'],'name':'remote-image-spec.json'}
 s=bound(load(download(a)));assert s['tool']==os.environ['GITHUB_SHA']==os.environ['TOOL'] and s['source']==os.environ['SOURCE'] and s['version']==os.environ['VERSION']
 assert subprocess.check_output(['git','-C','product','rev-parse','HEAD'],text=True).strip()==s['source']
 rows=recipe_members(download(s['recipe']),s);write_recipe(rows,Path.cwd()/'recipe')
 bundle=download(s['bundle']);m=load(rows['server/SOURCE.json']);assert m['version']==s['version'] and m['source_commit']==s['source'] and m['standalone_sha256']==sha(bundle)
 import io,zipfile
 with zipfile.ZipFile(io.BytesIO(bundle)) as z:
  assert len(z.infolist())==len(set(z.namelist())) and sum(i.file_size for i in z.infolist())<=256*1024**2
  for i in z.infolist():
   assert not Path(i.filename).is_absolute() and '..' not in Path(i.filename).parts and chr(92) not in i.filename
   assert stat.S_IFMT(i.external_attr>>16) in (0,stat.S_IFREG,stat.S_IFDIR)
  assert z.testzip() is None;bm=load(z.read('bundle.json'));assert bm['version']==s['version'] and bm['source_commit']==s['source'];assert sha(z.read('kindred'))==m['server_sha256']
 Path('recipe/server/kindred-standalone-0.87.8.zip').write_bytes(bundle)
 # Only explicit reviewed placeholders can vary; embedded code remains bound by recipe hashes.
 values={'@STAGE@':str(Path('recipe').resolve()),'@PRODUCT@':str(Path('product').resolve()),'@SOURCE@':s['source'],'@VERSION@':s['version'],'@IMAGE_TAG@':'0875-'+s['source'][:12],'@DOCKER_CONFIG@':str(Path('docker-config').resolve())}
 Path('docker-config').mkdir(exist_ok=False)
 for n in SCRIPTS:
  p=Path('recipe')/n;text=p.read_text()
  for token,value in values.items():text=text.replace(token,value)
  assert not re.search(r'@[A-Z_]+@',text);p.write_text(text)
 Path('spec-consumed.json').write_text(json.dumps(s,indent=2)+'\n')
