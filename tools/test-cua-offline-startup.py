"""Verify the selected driver initializes without an Internet connection.

strace observes connect calls from this driver tree only. No browser, display or
human profile is attached; telemetry/update flags and private state match bridge.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import tempfile

root=Path(__file__).resolve().parents[1]
binary=Path(os.environ['KINDRED_TEST_CUA_BINARY']).resolve()
proof=Path(os.environ['KINDRED_TEST_ARTIFACTS']);proof.mkdir(parents=True,exist_ok=True)
manifest=json.loads((root/'deploy/cua-driver-manifest.json').read_text())
assert hashlib.sha256(binary.read_bytes()).hexdigest()==manifest['binary_sha256']
with tempfile.TemporaryDirectory(prefix='kindred-cua-offline-') as temporary:
    env={**os.environ,'DISPLAY':':2099','XDG_SESSION_TYPE':'x11','DO_NOT_TRACK':'1','CUA_TELEMETRY':'0','CUA_DRIVER_RS_UPDATE_CHECK':'false','CUA_HOME':temporary+'/cua','XDG_CONFIG_HOME':temporary+'/config','XDG_DATA_HOME':temporary+'/data','XDG_CACHE_HOME':temporary+'/cache'}
    env.pop('WAYLAND_DISPLAY',None);env.pop('WAYLAND_SOCKET',None)
    with (proof/'driver-stderr.log').open('w') as stderr:
        child=subprocess.Popen(['strace','-f','-e','trace=connect','-o',str(proof/'network.log'),str(binary),'mcp','--direct','--no-overlay'],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr,text=True,start_new_session=True)
        try:
            child.stdin.write(json.dumps({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'kindred-offline-check','version':'1'}}})+'\n');child.stdin.flush()
            assert select.select([child.stdout],[],[],15)[0], 'Driver initialization timed out'
            response=json.loads(child.stdout.readline());assert response.get('id')==1 and 'result' in response,response
            (proof/'initialize.json').write_text(json.dumps(response,indent=2))
            child.stdin.close();child.wait(timeout=15);assert child.returncode==0
        finally:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
trace=(proof/'network.log').read_text()
network=[line for line in trace.splitlines() if re.search(r'sa_family=AF_INET6?[,}]',line)]
external=[line for line in network if not ('inet_addr("127.' in line or '"::1"' in line)]
assert not external, 'Initialization attempted an external IP connection: '+repr(external)
(proof/'result.json').write_text(json.dumps({'passed':True,'driver_sha256':manifest['binary_sha256'],'stdio_initialization':True,'external_ip_connect_attempts':0,'loopback_connect_attempts':len(network),'loopback_trace':network,'limits':'Observed connect syscalls for driver tree only; no browser/profile attached and no live guest or provider claim'},indent=2))
print('Selected driver stdio initialized; zero external IP connect attempts')
