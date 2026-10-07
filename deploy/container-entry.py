#!/usr/bin/env python3
"""Initialize the persistent server directory, then run as the unprivileged user."""
import importlib.util,json,os,signal,subprocess,sys,time
from pathlib import Path
root=Path('/data');root.mkdir(exist_ok=True)
profiles=root/'profiles'
if os.getuid()==0:
    os.chown(root,1000,1000)
    os.setgroups(os.getgroups());os.setgid(1000);os.setuid(1000)
profiles.mkdir(exist_ok=True,mode=0o700)
os.umask(0o077)
url=os.environ.get('KINDRED_PUBLIC_URL','http://127.0.0.1:9444').rstrip('/')
config=f'''listen = "0.0.0.0:9444"
public_url = {json.dumps(url)}
allowed_origins = {json.dumps(json.loads(os.environ.get("KINDRED_ALLOWED_ORIGINS","[]")))}
confirmed_http_origins = {json.dumps(json.loads(os.environ.get("KINDRED_CONFIRMED_HTTP_ORIGINS","[]")))}
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
recovery=None
stopping=False
def stop(signum,frame):
    global stopping
    if stopping:return
    stopping=True
    # Stop scheduling before powering down each dedicated computer. QMP quit at
    # the deadline flushes the disk; no volume or VM disk is ever deleted here.
    child.send_signal(signal.SIGINT)
    # Stop recovery and its foreground helpers before enumerating computers.
    # QEMU daemonizes into its own session and is stopped through QMP below.
    if recovery is not None and recovery.poll() is None:
        try: os.killpg(recovery.pid,signal.SIGTERM)
        except ProcessLookupError: pass
        try: recovery.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try: os.killpg(recovery.pid,signal.SIGKILL)
            except ProcessLookupError: pass
            recovery.wait()
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
previous_mask=signal.pthread_sigmask(signal.SIG_BLOCK,{signal.SIGTERM,signal.SIGINT})
try:
    if not stopping:
        recovery=subprocess.Popen([sys.executable,'/usr/local/lib/kindred/vm-manager.py','restore'],start_new_session=True,preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK,previous_mask))
finally:
    signal.pthread_sigmask(signal.SIG_SETMASK,previous_mask)
result=child.wait()
if not stopping:stop(signal.SIGTERM,None)
sys.exit(result)
