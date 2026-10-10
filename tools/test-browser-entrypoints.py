"""Execute real launch scripts in an owned network-none disposable container.

Chromium/desktop commands are capture stand-ins; this proves argv/environment
at the shell/embedded-helper boundary, not authentication or native attachment.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
ROOT=Path(__file__).resolve().parents[1]
IMAGE='kindred-builder-ubuntu22:0855'

class Entrypoints(unittest.TestCase):
    def test_startup_desktop_and_embedded_guest_share_context_and_policy(self):
        with tempfile.TemporaryDirectory(prefix='kindred-entrypoints-') as temporary:
            fixture=Path(temporary);fixture.chmod(0o755);(fixture/'passwd').write_text('root:x:0:0:root:/root:/bin/sh\nbot:x:1000:1000:fixture:/home/bot:/bin/sh\n');(fixture/'group').write_text('root:x:0:\nbot:x:1000:\n');lib=fixture/'lib';lib.mkdir();home=fixture/'home';home.mkdir();workspace=fixture/'workspace';workspace.mkdir();commands=fixture/'bin';commands.mkdir();runtime=fixture/'runtime';runtime.mkdir(mode=0o700)
            profile=home/'.local/share/kindred/browser';profile.mkdir(parents=True,mode=0o700);profile.parent.chmod(0o700);(profile/'Local State').write_text('{}')
            for name in ['browser-launch.py','start-desktop.sh','desktop-launch']:
                shutil.copyfile(ROOT/'deploy'/name,lib/name);(lib/name).chmod(0o755)
            if os.environ.get('KINDRED_TEST_BROWSER_BASELINE') == '1':
                (lib/'desktop-launch').write_bytes(subprocess.check_output(['git','show','d724df072e47b828cc355ed77c55762968c6739f:deploy/desktop-launch'],cwd=ROOT))
            scripts={
                'chromium':'''#!/usr/bin/python3
import json,os,sys
with open('/tmp/kindred-captures.jsonl','a') as out:out.write(json.dumps({'args':sys.argv[1:],'display':os.environ.get('DISPLAY'),'bus':os.environ.get('DBUS_SESSION_BUS_ADDRESS'),'runtime':os.environ.get('XDG_RUNTIME_DIR')})+'\\n')
''',
                'xdpyinfo':'#!/bin/sh\nexit 0\n',
                'openbox-session':'#!/bin/sh\nexec /fixture/bin/openbox 60\n',
            }
            for name,value in scripts.items():(commands/name).write_text(value);(commands/name).chmod(0o755)
            (lib/'start-desktop-shell').write_text('#!/bin/sh\nexit 0\n');(lib/'start-desktop-shell').chmod(0o755)
            # This native binary stand-in gives /proc an exact selected executable
            # identity, unlike a shell wrapper whose exe is the shell interpreter.
            shutil.copyfile('/bin/sleep',commands/'openbox');(commands/'openbox').chmod(0o755)
            runner='''import json,os,signal,subprocess,time
from pathlib import Path
Path('/tmp/kindred-entry-runtime').mkdir(mode=0o700)
profile=Path('/home/bot/.local/share/kindred/browser');profile.mkdir(parents=True,mode=0o700);profile.parent.chmod(0o700);(profile/'Local State').write_text('{}')
os.environ.update(PATH='/fixture/bin:'+os.environ['PATH'],XDG_RUNTIME_DIR='/tmp/kindred-entry-runtime',DISPLAY=':1')
start=subprocess.Popen(['/usr/local/lib/kindred/start-desktop.sh','1'],start_new_session=True)
try:
 deadline=time.monotonic()+8
 while not Path('/tmp/kindred-captures.jsonl').exists():
  assert start.poll() is None,'Startup exited before browser request'
  assert time.monotonic()<deadline,'Startup request deadline'
  time.sleep(.05)
finally:
 os.killpg(start.pid,signal.SIGTERM)
 try:start.wait(timeout=3)
 except subprocess.TimeoutExpired:os.killpg(start.pid,signal.SIGKILL);start.wait(timeout=3)
script="""import os,subprocess,time,json
box=subprocess.Popen(['/fixture/bin/openbox','60'])
try:
 time.sleep(.1)
 for mode in ['browser','browser-new']:subprocess.run(['/usr/local/lib/kindred/desktop-launch',mode],check=True)
 # This is the same embedded-source Python preparation invoked by the guest.
 source=open('/usr/local/lib/kindred/browser-launch.py').read()
 result=subprocess.run(['python3','-c',source,'--prepare','--screen','1','--mode','url','--url','https://example.test'],capture_output=True,text=True,check=True)
 launch=json.loads(result.stdout)
 subprocess.run(['chromium',*launch['args']],env={**os.environ,**launch['env']},check=True)
finally:box.terminate();box.wait(timeout=3)
"""
subprocess.run(['dbus-run-session','--','python3','-c',script],check=True)
rows=[json.loads(line) for line in Path('/tmp/kindred-captures.jsonl').read_text().splitlines()]
assert len(rows)==4,rows
print(json.dumps({'captured':4,'loopbackCDPByEntry':[('--remote-debugging-port=0' in row['args']) for row in rows]}),flush=True)
for row in rows:
 assert row['display']==':1' and row['bus'].startswith(('unix:path=','unix:abstract=')) and row['runtime']=='/tmp/kindred-entry-runtime'
 assert '--user-data-dir=/home/bot/.local/share/kindred/browser' in row['args']
 for flag in ['--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--restore-last-session','--window-size=1120,680']:assert flag in row['args']
assert len({row['bus'] for row in rows[1:]})==1
assert '--new-window' in rows[2]['args'] and rows[3]['args'][-1]=='https://example.test'
assert profile.stat().st_uid==1000 and profile.stat().st_mode&0o777==0o700
assert (profile/'Local State').read_text()=='{}'
print(json.dumps({'uid':os.getuid(),'entrypoints':4,'sameProfile':True,'desktopAndGuestSameBus':True,'restoreAndLoopbackCDP':True,'chromiumStandIn':True,'noBrowserRestartOrAuthClaim':True}))
'''
            (fixture/'run.py').write_text(runner)
            result=subprocess.run(['docker','run','--rm','--network','none','--user','1000:1000','--tmpfs','/home/bot:rw,noexec,uid=1000,gid=1000,mode=0700','--entrypoint','python3','-v',f'{fixture}/passwd:/etc/passwd:ro','-v',f'{fixture}/group:/etc/group:ro','-v',f'{fixture}:/fixture:ro','-v',f'{lib}:/usr/local/lib/kindred:ro','-v',f'{workspace}:/workspace',IMAGE,'/fixture/run.py'],capture_output=True,text=True,timeout=35)
            print(result.stdout,flush=True)
            self.assertEqual(result.returncode,0,result.stderr)
            receipt=json.loads(result.stdout.strip().splitlines()[-1]);self.assertEqual(receipt['entrypoints'],4)
            self.assertEqual((profile/'Local State').read_text(),'{}')

if __name__=='__main__':unittest.main()
