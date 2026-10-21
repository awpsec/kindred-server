"""Run the production supervisor with isolated paths, a tiny HTTP child and VM stub."""
import copy
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
import network_settings as network


class Supervisor(unittest.TestCase):
    def setUp(self):
        self.folder=tempfile.TemporaryDirectory();self.addCleanup(self.folder.cleanup)
        self.root=Path(self.folder.name);self.data=self.root/'data';self.data.mkdir()
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0));self.port=sock.getsockname()[1]
        source=Path(__file__).parent
        (self.root/'network_settings.py').write_text((source/'network_settings.py').read_text().replace('9444',str(self.port)))
        self.child=self.root/'kindred'
        self.child.write_text('''#!/usr/bin/env python3
import http.server,json,os,sys,tomllib
from pathlib import Path
config=tomllib.loads(Path(sys.argv[2]).read_text())
state=json.loads(Path(os.environ['KINDRED_WEB_NETWORK_SETTINGS']).read_text())
with open(Path(sys.argv[2]).parent/'children','a') as f:f.write(str(os.getpid())+'\\n')
if config['public_url']=='https://fail.example':sys.exit(2)
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200);self.end_headers();self.wfile.write(json.dumps({'docker_network':{'revision':state['revision']},'pid':os.getpid()}).encode())
    def log_message(self,*args):pass
http.server.HTTPServer.allow_reuse_address=True
server=http.server.HTTPServer(('127.0.0.1',PORT),Handler)
try:server.serve_forever()
except KeyboardInterrupt:pass
finally:server.server_close()
'''.replace('PORT',str(self.port)))
        self.child.chmod(0o700)
        manager=self.root/'manager.py'
        manager.write_text(f'''from pathlib import Path
log=Path({str(self.data/'vm-events')!r})
def qmp(*args):
    with log.open('a') as f:f.write('shutdown\\n')
def running(*args):return False
if __name__=='__main__':
    with log.open('a') as f:f.write('restore\\n')
''')
        computer=self.data/'profiles'/'fixture'/'computer';computer.mkdir(parents=True);(computer/'computer.json').write_text('{}')
        entry=(source/'container-entry.py').read_text().replace("'/data'",repr(str(self.data))).replace('/data/',str(self.data)+'/')
        entry=entry.replace('if os.getuid()==0:','if False:') # Keep fixture ownership; UID behavior has its own container test.
        entry=entry.replace('/usr/local/bin/kindred',str(self.child)).replace('/usr/local/lib/kindred/vm-manager.py',str(manager)).replace('9444',str(self.port))
        (self.root/'entry.py').write_text(entry)
        self.env={**os.environ,'KINDRED_NETWORK_MODE':'host'};self.env.pop('KINDRED_DEPLOYMENT',None)
        for key in ['KINDRED_PUBLIC_URL','KINDRED_ACCESS','KINDRED_ALLOWED_ORIGINS','KINDRED_CONFIRMED_HTTP_ORIGINS','KINDRED_TAILNET_ADDRESS']:self.env.pop(key,None)
        self.env['KINDRED_PUBLIC_URL']='http://127.0.0.1:9444'
        self.processes=[];self.addCleanup(self.stop_all)

    def start(self,env=None):
        log=open(self.root/f'process-{len(self.processes)}.log','w');self.addCleanup(log.close)
        proc=subprocess.Popen([sys.executable,str(self.root/'entry.py')],env=env or self.env,stdout=log,stderr=log,start_new_session=True)
        self.processes.append(proc);return proc

    def stop_all(self):
        for proc in self.processes:
            if proc.poll() is None:
                proc.terminate()
                try:proc.wait(timeout=8)
                except subprocess.TimeoutExpired:os.killpg(proc.pid,signal.SIGKILL);proc.wait()

    def wait(self,revision=None):
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{self.port}/identity/meta',timeout=.3) as response:value=json.load(response)
                if revision is None or value['docker_network']['revision']==revision:return value
            except Exception:pass
            time.sleep(.1)
        self.fail('Supervisor did not serve the expected generation: '+''.join(p.read_text() for p in self.root.glob('process-*.log')))

    def change(self,revision,url):
        before=network.read_state(self.data/'network-settings.json')
        network.atomic_json(self.data/'network-last-good.json',before)
        updated=copy.deepcopy(before);updated['revision']=revision;updated['settings']['public_url']=url
        network.atomic_json(self.data/'network-settings.json',updated)
        return before

    def test_restart_and_failed_change_keep_vm_supervisor_running(self):
        proc=self.start();first=self.wait()
        self.change('second','https://second.example');second=self.wait('second')
        self.assertNotEqual(first['pid'],second['pid']);self.assertIsNone(proc.poll())
        self.assertEqual((self.data/'vm-events').read_text(),'restore\n')
        self.change('failed','https://fail.example')
        # Wait for the failed child and the restored child, rather than mistaking
        # the old process's final response for rollback success.
        deadline=time.monotonic()+15
        while time.monotonic()<deadline and len((self.data/'children').read_text().splitlines())<4:time.sleep(.1)
        restored=self.wait('second');self.assertNotEqual(restored['pid'],second['pid'])
        self.assertEqual(network.read_state(self.data/'network-settings.json')['revision'],'second')
        self.assertEqual((self.data/'vm-events').read_text(),'restore\n')
        self.assertTrue((self.data/'profiles/fixture/computer/computer.json').exists())

    def test_bad_compose_input_does_not_roll_back_successful_saved_settings(self):
        original=network.load(self.data,{'KINDRED_NETWORK_MODE':'host'})
        network.atomic_json(self.data/'network-last-good.json',{key:original[key] for key in ('revision','settings','baseline')})
        self.change('successful','https://saved.example')
        before=(self.data/'network-settings.json').read_bytes()
        proc=self.start({**self.env,'KINDRED_PUBLIC_URL':'not-a-url'});self.assertNotEqual(proc.wait(timeout=5),0)
        self.assertEqual((self.data/'network-settings.json').read_bytes(),before)

    def test_cold_start_recovers_pending_failed_web_generation(self):
        original=network.load(self.data,{'KINDRED_NETWORK_MODE':'host'})
        self.change('failed-cold-start','https://fail.example')
        self.start();self.wait(original['revision'])
        self.assertEqual(network.read_state(self.data/'network-settings.json')['revision'],original['revision'])


if __name__=='__main__':unittest.main()
