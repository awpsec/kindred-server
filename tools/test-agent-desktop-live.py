"""Real, disposable Debian/X11/AT-SPI fixture. No production VM or owner account.

Requires Xvfb, Openbox, dbus-run-session, python3-gi, GTK3, AT-SPI, and Chromium.
Pass the exact verified upstream CLI and a Chromium executable explicitly.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[1]
FIXTURE=ROOT/'tools/fixtures/agent-desktop'

def session():
    root=Path(os.environ['FIXTURE_ROOT']); processes=[]
    try:
        processes.append(subprocess.Popen(['openbox'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
        processes.append(subprocess.Popen(['/usr/bin/python3',str(FIXTURE/'gtk.py')]))
        processes.append(subprocess.Popen([os.environ['FIXTURE_CHROME'],'--no-sandbox','--no-first-run','--disable-gpu',
            '--disable-dev-shm-usage','--force-renderer-accessibility','--user-data-dir='+str(root/'chrome'),
            'file://'+str(FIXTURE/'browser.html')],stdout=(root/'browser.log').open('w'),stderr=subprocess.STDOUT))
        (root/'environment.json').write_text(json.dumps({k:os.environ[k] for k in ['DISPLAY','DBUS_SESSION_BUS_ADDRESS','XDG_RUNTIME_DIR']}))
        signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
        while True:time.sleep(1)
    finally:
        for p in reversed(processes):
            if p.poll() is None:p.terminate()
        for p in processes:
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait()

def wait_for(test,seconds=15):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if test():return
        time.sleep(.05)
    raise AssertionError('Fixture did not become ready')

def nodes(value):
    if isinstance(value,dict):
        yield value
        for v in value.values():yield from nodes(v)
    elif isinstance(value,list):
        for v in value:yield from nodes(v)

def ref(value,name):
    matches={v['ref_id'] for v in nodes(value) if v.get('name')==name and v.get('ref_id')}
    assert len(matches)==1,(name,value)
    return next(iter(matches))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--chrome',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True);args=parser.parse_args()
    manifest=json.loads((ROOT/'deploy/agent-desktop-manifest.json').read_text())
    assert hashlib.sha256(args.binary.read_bytes()).hexdigest()==manifest['files']['agent-desktop']['sha256']
    args.output.mkdir(mode=0o700,parents=True,exist_ok=False)
    processes=[];logs=[];environments={};receipts=[]
    def call(name,*cmd):
        start=time.monotonic();p=subprocess.run([str(args.binary.resolve()),*cmd],env=environments[name],capture_output=True,text=True,timeout=30)
        value=json.loads(p.stdout);receipts.append({'screen':name,'args':cmd,'ms':round((time.monotonic()-start)*1000),'result':value})
        (args.output/'actions.json').write_text(json.dumps(receipts,indent=2,ensure_ascii=False))
        return value
    def state(name):return json.loads((args.output/name/'state.json').read_text())
    try:
        available=[n for n in range(32,1,-1) if not Path(f'/tmp/.X11-unix/X{n}').exists() and not Path(f'/tmp/.X{n}-lock').exists()]
        assert len(available)>=2,'Need two unused fixture displays'
        for name,screen in zip(['alpha','beta'],available):
            folder=args.output/name;folder.mkdir(mode=0o700);(folder/'runtime').mkdir(mode=0o700)
            env=dict(os.environ,DISPLAY=f':{screen}',XDG_RUNTIME_DIR=str(folder/'runtime'),FIXTURE_ROOT=str(folder),
                     FIXTURE_NAME=name,FIXTURE_CHROME=str(args.chrome.resolve()),NO_AT_BRIDGE='0',GTK_MODULES='gail:atk-bridge')
            for k in ['DBUS_SESSION_BUS_ADDRESS','AT_SPI_BUS_ADDRESS','WAYLAND_DISPLAY']:env.pop(k,None)
            log=(folder/'session.log').open('w');logs.append(log)
            x=subprocess.Popen(['Xvfb',f':{screen}','-screen','0','1440x900x24','-nolisten','tcp'],stdout=log,stderr=log,start_new_session=True);processes.append(x)
            wait_for(lambda:subprocess.run(['xdpyinfo','-display',f':{screen}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0)
            p=subprocess.Popen(['dbus-run-session','--','/usr/bin/python3',str(Path(__file__).resolve()),'--session'],env=env,stdout=log,stderr=log,start_new_session=True);processes.append(p)
            wait_for(lambda:(folder/'environment.json').exists() and (folder/'state.json').exists())
            def load(mod,path):
                spec=importlib.util.spec_from_file_location(mod,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
            browser=load('browser_context',ROOT/'deploy/browser-launch.py');launcher=load('agent_launcher',ROOT/'deploy/agent-desktop-launch.py')
            # Exercise the production session selector from an intentionally wrong bus.
            verified=browser.desktop_context(f':{screen}')
            environments[name]=launcher.prepare(dict(os.environ,DISPLAY=f':{screen}',DBUS_SESSION_BUS_ADDRESS='wrong',AT_SPI_BUS_ADDRESS='wrong'),verified,folder,'fixture')
            assert call(name,'status')['data']['supported_surfaces']
        gtk=call('alpha','snapshot','--app','python3.13','-i','--compact');assert gtk['ok'],gtk
        text='Hello – résumé 日本語 🐾';typed=call('alpha','type',ref(gtk,'Draft'),text);assert typed['ok'],typed
        assert state('alpha')['text']==text and state('beta')['text']==''
        cross=call('beta','click',ref(gtk,'Submit once'));assert not cross['ok'] and cross['error']['disposition']['delivery']=='not_delivered',cross
        assert state('alpha')['count']==state('beta')['count']==0
        gtk=call('alpha','snapshot','--app','python3.13','-i','--compact');submit=ref(gtk,'Submit once')
        clicked=call('alpha','click',submit);assert clicked['ok'],clicked
        assert state('alpha')['count']==1
        stale=call('alpha','click',submit);assert not stale['ok'],stale
        assert state('alpha')['count']==1
        fresh=call('alpha','snapshot','--app','python3.13','-i','--compact');assert ref(fresh,'Submitted')
        toggle=call('alpha','click',ref(fresh,'Enabled'));assert toggle['ok'],toggle
        assert state('alpha')['enabled'] and not state('beta')['enabled']
        browser_apps={}
        for name in ('alpha','beta'):
            apps=call(name,'list-apps');assert apps['ok'],apps
            matches=[app['name'] for app in apps['data']['apps'] if app['name'].lower() in ('chrome','chromium','google chrome','google chrome for testing')]
            assert len(matches)==1,apps
            browser_apps[name]=matches[0]
        ready=call('alpha','wait','--text','Save draft','--app',browser_apps['alpha'],'--timeout','10000');assert ready['ok'],ready
        field=call('alpha','find','--app',browser_apps['alpha'],'--name','Message','--exact');assert field['ok'],field
        typed=call('alpha','type',ref(field,'Message'),'Browser draft')
        # Document the actual browser's capability; never silently switch to headed input.
        if not typed['ok']:
            assert typed['error']['code']=='POLICY_DENIED' and typed['error']['disposition']['delivery']=='not_delivered',typed
        save=call('alpha','find','--app',browser_apps['alpha'],'--name','Save draft','--exact');assert save['ok'],save
        clicked=call('alpha','click',ref(save,'Save draft'));assert clicked['ok'],clicked
        result=call('alpha','find','--app',browser_apps['alpha'],'--text','Saved 1:');assert result['ok'],result
        assert any(str(n.get('name','')).startswith('Saved 1:') for n in nodes(result)),result
        other=call('beta','find','--app',browser_apps['beta'],'--text','Saved 1:');assert not any(str(n.get('name','')).startswith('Saved 1:') for n in nodes(other)),other
        receipt={'passed':True,'commands':len(receipts),'binary_sha256':manifest['files']['agent-desktop']['sha256'],
                 'browser_semantic_typing':typed['ok'],'total_command_ms':sum(r['ms'] for r in receipts),
                 'checks':['native Unicode typing','native click and readback','checkbox readback','cross-screen reference rejection',
                           'stale reference no duplicate action','fresh reference recovery','verified desktop bus selection',
                           'browser control discovery','browser click and readback','browser screen isolation']}
        (args.output/'receipt.json').write_text(json.dumps(receipt,indent=2));print(json.dumps(receipt))
    finally:
        for p in reversed(processes):
            if p.poll() is None:
                try:os.killpg(p.pid,signal.SIGTERM)
                except ProcessLookupError:pass
                try:p.wait(timeout=6)
                except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait()
        for log in logs:log.close()

if __name__=='__main__':
    if sys.argv[1:]==['--session']:session()
    else:main()
