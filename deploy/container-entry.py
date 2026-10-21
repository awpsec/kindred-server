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
import network_settings
child=None
network=None
stopping=False
def launch():
    global child, network
    if stopping: return
    if os.environ.get('KINDRED_DEPLOYMENT') != 'standalone':
        network=network_settings.load(root,os.environ)
        settings=network['settings']
        listen,additional=network_settings.listeners(settings)
    else:
        network=None
        settings={'public_url':os.environ.get('KINDRED_PUBLIC_URL','http://127.0.0.1:9444').rstrip('/'),
                  'allowed_origins':json.loads(os.environ.get('KINDRED_ALLOWED_ORIGINS','[]')),
                  'confirmed_http_origins':json.loads(os.environ.get('KINDRED_CONFIRMED_HTTP_ORIGINS','[]'))}
        listen,additional='0.0.0.0:9444',[]
    url=settings['public_url']
    config=f'''listen = {json.dumps(listen)}
    additional_listen = {json.dumps(additional)}
    public_url = {json.dumps(url)}
    allowed_origins = {json.dumps(settings["allowed_origins"])}
    confirmed_http_origins = {json.dumps(settings["confirmed_http_origins"])}
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
    child_env=dict(os.environ)
    if network: child_env['KINDRED_WEB_NETWORK_SETTINGS']=str(root/'network-applied.json')
    else: child_env.pop('KINDRED_WEB_NETWORK_SETTINGS',None)
    mask=signal.pthread_sigmask(signal.SIG_BLOCK,{signal.SIGTERM,signal.SIGINT})
    try:
        if not stopping:
            child=subprocess.Popen(['/usr/local/bin/kindred','--config',str(path),'serve'],env=child_env,
                preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK,mask))
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK,mask)
recovery=None
stopping=False
def stop(signum,frame):
    global stopping
    if stopping:return
    stopping=True
    # Stop scheduling before powering down each dedicated computer. QMP quit at
    # the deadline flushes the disk; no volume or VM disk is ever deleted here.
    if child is not None and child.poll() is None: child.send_signal(signal.SIGINT)
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
def healthy(revision):
    import urllib.request
    try:
        with urllib.request.urlopen('http://127.0.0.1:9444/identity/meta',timeout=1) as response:
            return json.load(response).get('docker_network',{}).get('revision')==revision
    except Exception: return False

def stop_server():
    if child.poll() is None:
        child.send_signal(signal.SIGINT)
        try: child.wait(timeout=15)
        except subprocess.TimeoutExpired:
            child.kill();child.wait()

signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
last_good=root/'network-last-good.json'
def restore_last_good():
    if os.environ.get('KINDRED_DEPLOYMENT')=='standalone' or not last_good.exists(): return False
    if child is not None: stop_server()
    network_settings.atomic_json(root/'network-settings.json',network_settings.read_state(last_good))
    launch()
    return True

def verify_start():
    deadline=time.monotonic()+120
    while not stopping and child.poll() is None and time.monotonic()<deadline:
        if healthy(network['revision']): return
        time.sleep(.25)
    if not stopping: raise RuntimeError('The new network configuration did not become ready')

try:
    launch()
    if network and last_good.exists() and network_settings.read_state(last_good)['revision']!=network['revision']:
        verify_start()
except Exception:
    # Invalid Compose input never overwrites an established web configuration.
    # Only a staged web generation with the same environment can roll back.
    if (network is None or child is None or not last_good.exists()
            or network["baseline"]!=network_settings.read_state(last_good)["baseline"]
            or not restore_last_good()): raise
previous_mask=signal.pthread_sigmask(signal.SIG_BLOCK,{signal.SIGTERM,signal.SIGINT})
try:
    if not stopping:
        recovery=subprocess.Popen([sys.executable,'/usr/local/lib/kindred/vm-manager.py','restore'],start_new_session=True,preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK,previous_mask))
finally:
    signal.pthread_sigmask(signal.SIG_SETMASK,previous_mask)
# A saved generation is the restart request. It survives a lost HTTP response.
# Keep the VM supervisor and recovery process alive during this child restart.
while child.poll() is None and not stopping:
    time.sleep(.25)
    if not network: continue
    if healthy(network['revision']):
        current={key:network[key] for key in ('revision','settings','baseline')}
        if not last_good.exists() or network_settings.read_state(last_good)['revision']!=network['revision']:
            network_settings.atomic_json(last_good,current)
    try: pending=network_settings.read_state(root/'network-settings.json')
    except Exception as error:
        print(f'Could not read pending network settings: {error}',file=sys.stderr)
        time.sleep(2);continue
    if pending['revision']==network['revision']: continue
    previous={key:network[key] for key in ('revision','settings','baseline')}
    time.sleep(2) # Let the Save response reach the browser before disconnecting.
    if stopping: break
    stop_server()
    if stopping: break
    try:
        launch()
        verify_start()
        if not stopping:
            network_settings.atomic_json(last_good,{key:network[key] for key in ("revision","settings","baseline")})
    except Exception as error:
        if stopping: break
        print(f'Network change failed; restoring the previous settings: {error}',file=sys.stderr)
        stop_server()
        network_settings.atomic_json(root/'network-settings.json',previous)
        launch()
result=child.wait()
if not stopping:stop(signal.SIGTERM,None)
sys.exit(result)
