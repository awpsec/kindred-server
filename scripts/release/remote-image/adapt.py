# Materialize source templates only, never build or package an input archive.
import json,hashlib,re
from pathlib import Path
from guards import SCRIPTS,sha
OLD_STAGE='/root/.openrig/workspace/projects/kindred/releases/complete-fixes-0.87.0-preparation'
OLD_SOURCE='/root/.openrig/workspace/projects/kindred/checkouts/builder/release-0870-server'
REPLACEMENTS={OLD_STAGE:'@STAGE@',OLD_SOURCE:'@PRODUCT@','/root/.openrig/workspace/projects/kindred/releases/cua-preparation-20261006/docker-config':'@DOCKER_CONFIG@','3b6db7dedc081821303abdf218fb24b21cd32054':'@SOURCE@','0870-7a27c6a':'@IMAGE_TAG@','0.87.0':'@VERSION@'}
def transform(data):
 text=data.decode(); spans=[]
 pattern=re.compile('|'.join(re.escape(x) for x in sorted(REPLACEMENTS,key=len,reverse=True)))
 chunks=[];last=0;offset=0
 for m in pattern.finditer(text):
  prefix=text[last:m.start()];chunks.append(prefix);offset+=len(prefix);value=REPLACEMENTS[m.group()];spans.append({'offset':offset,'before':m.group(),'after':value});chunks.append(value);offset+=len(value);last=m.end()
 chunks.append(text[last:]);out=''.join(chunks);inverse=out
 for r in reversed(spans):assert inverse[r['offset']:r['offset']+len(r['after'])]==r['after'];inverse=inverse[:r['offset']]+r['before']+inverse[r['offset']+len(r['after']):]
 assert inverse==text
 return out.encode(),spans
if __name__=='__main__':
 import sys
 parent=Path(sys.argv[1]);bindings=json.loads(Path(sys.argv[2]).read_text());out=Path(sys.argv[3]);assert not out.exists();out.mkdir()
 manifest={}
 for name in sorted(SCRIPTS):
  data=(parent/name).read_bytes();assert sha(data)==bindings[name]['sha256'];child,spans=transform(data);p=out/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(child);manifest[name]={'parent_sha256':sha(data),'sha256':sha(child),'size':len(child),'spans':spans,'inverse_exact':True}
 (out/'adaptation.json').write_text(json.dumps(manifest,indent=2)+'\n')
