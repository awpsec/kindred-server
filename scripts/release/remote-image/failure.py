"""Preserve bounded diagnostic logs after a failed remote image phase; never archive payloads."""
import os,sys,stat,json,hashlib,re
from pathlib import Path
PHASES=('setup','bundle','harness','context','build','checks','provider','current_harness','sdk','export')
LOGS=('remote-bundle-check.log','remote-harness-check.log','remote-context-check.log','remote-build.log','remote-image-checks.log')
def collect(root,phase,rc):
 assert phase in PHASES and isinstance(rc,int) and 1<=rc<=255
 root=Path(root).absolute();proof=root/'recipe/proof';out=root/'failure'
 for p in [proof,*proof.parents]:assert stat.S_ISDIR(p.lstat().st_mode) and not p.is_symlink()
 assert not out.exists() and not out.is_symlink();out.mkdir(mode=0o700)
 rows={}
 for name in LOGS:
  p=proof/name
  try:before=p.lstat()
  except FileNotFoundError:continue
  assert stat.S_ISREG(before.st_mode) and before.st_nlink==1 and not before.st_mode&0o111
  fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
  with os.fdopen(fd,'rb') as f:
   opened=os.fstat(f.fileno());assert (opened.st_dev,opened.st_ino,opened.st_size)==(before.st_dev,before.st_ino,before.st_size)
   f.seek(max(0,opened.st_size-65536));data=f.read(65537);after=os.fstat(f.fileno())
  assert len(data)<=65536 and (after.st_size,after.st_mtime_ns)==(opened.st_size,opened.st_mtime_ns)
  assert not data.startswith((b'\x7fELF',b'MZ',b'PK\x03\x04'))
  (out/name).write_bytes(data);rows[name]={'source_size':before.st_size,'tail_size':len(data),'sha256':hashlib.sha256(data).hexdigest()}
 receipt={'phase':phase,'exit_code':rc,'failed':True,'logs':rows,'max_log_tail_bytes':65536,'payload_or_source_exported':False}
 (out/'failure.json').write_text(json.dumps(receipt,indent=2)+'\n')
 print(json.dumps({'remote_image_failed_phase':phase,'exit_code':rc,'bounded_log_count':len(rows)}),file=sys.stderr)
 return receipt
if __name__=='__main__':collect(Path.cwd(),sys.argv[1],int(sys.argv[2]))
