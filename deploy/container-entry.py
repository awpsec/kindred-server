#!/usr/bin/env python3
"""Initialize the persistent server directory, then run as the unprivileged user."""
import importlib.util,json,os,signal,subprocess,sys,time
from pathlib import Path
root=Path('/data');root.mkdir(exist_ok=True)
profiles=root/'profiles';profiles.mkdir(exist_ok=True,mode=0o700)
if os.getuid()==0:
    os.chown(root,1000,1000);os.chown(profiles,1000,1000)
    os.setgroups(os.getgroups());os.setgid(1000);os.setuid(1000)
os.umask(0o077)
url=os.environ.get('KINDRED_PUBLIC_URL','http://127.0.0.1:9444').rstrip('/')
config=f'''listen = "0.0.0.0:9444"
public_url = {json.dumps(url)}
database = "/data/kindred.db"
max_parallel_runs = {int(os.environ.get('KINDRED_MAX_PARALLEL_RUNS','4'))}
[profiles]
enabled = true
directory = "/data/profiles"
open_registration = {str(os.environ.get('KINDRED_OPEN_REGISTRATION','true').lower()=='true').lower()}
max_users = {int(os.environ.get('KINDRED_MAX_USERS','128'))}
max_profiles_per_user = {int(os.environ.get('KINDRED_MAX_PROFILES_PER_USER','8'))}
'''
path=root/'server.toml';path.write_text(config)
child=subprocess.Popen(['/usr/local/bin/kindred','--config',str(path),'serve'])
stopping=False
def stop(signum,frame):
    global stopping
    if stopping:return
    stopping=True
    # Stop scheduling before powering down each dedicated computer. QMP quit at
    # the deadline flushes the disk; no volume or VM disk is ever deleted here.
    child.send_signal(signal.SIGINT)
    spec=importlib.util.spec_from_file_location('manager','/usr/local/lib/kindred/vm-manager.py');manager=importlib.util.module_from_spec(spec);spec.loader.exec_module(manager)
    computers=[p.parent for p in profiles.glob('*/computer/computer.json')]
    for computer in computers:
        try:manager.qmp(computer,'system_powerdown')
        except Exception:pass
    deadline=time.monotonic()+60
    while time.monotonic()<deadline and any(manager.running(p) for p in computers):time.sleep(.5)
    for computer in computers:
        if manager.running(computer):
            try:manager.qmp(computer,'quit')
            except Exception:pass
signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
result=child.wait()
if not stopping:stop(signal.SIGTERM,None)
sys.exit(result)
