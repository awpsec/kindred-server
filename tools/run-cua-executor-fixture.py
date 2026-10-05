"""Exercise the production Rust guest executor on a disposable X11 display.

Only VM lifecycle, browser/profile path and binary path are supplied by the
existing opt-in Rust test seam. Capture, bridge, driver and input are real.
"""
import hashlib
import base64
import http.server
import json
import os
from pathlib import Path
import socket
import ssl
import shlex
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(os.environ['KINDRED_TEST_ARTIFACTS']);ARTIFACTS.mkdir(parents=True,exist_ok=True)
BINARY = os.environ['KINDRED_EXECUTOR_TEST_BINARY'];CHROME = os.environ['KINDRED_TEST_CHROME']
COMPARE=os.environ.get('KINDRED_CUA_COMPARISON_MODE')
SOURCE_ROOT=Path(os.environ.get('KINDRED_EXECUTOR_SOURCE_ROOT',str(ROOT)))
HTML = b'''<title>Kindred Cua Executor</title><style>body{padding:80px;min-height:1400px}</style><label>Recipient<input id="recipient"></label><label>Note<input id="note" maxlength="4" oninput="localStorage.note=this.value;const r=new XMLHttpRequest();r.open('POST','/partial',false);r.send(this.value)"></label><button id="review" onmousedown="applyFixtureEffect()">Review fixture</button><p id="result">No effect</p><script>function applyFixtureEffect(){const r=new XMLHttpRequest();r.open('POST','/effect',false);r.send('fixture');window.count++;localStorage.count=window.count;document.querySelector('#result').textContent=window.count;}window.count=Number(localStorage.count||0);document.querySelector('#recipient').value=localStorage.recipient||'';document.querySelector('#recipient').oninput=e=>localStorage.recipient=e.target.value;onload=()=>fetch('/ready',{method:'POST'});</script>'''
if COMPARE:
    HTML=b'''<title>Kindred Comparison</title><style>body{padding:80px;font:18px sans-serif}label{display:block;margin-bottom:24px}input{display:block;width:360px;height:32px}button{height:40px}</style><label>Recipient<input id="recipient"></label><label>Amount<input id="amount"></label><label>Note<input id="note"></label><button id="review" disabled onmousedown="review()">Review fixture</button><p id="result">No effect</p><script>let seq=0;function observe(){const fields={};for(const e of document.querySelectorAll('input,button')){const r=e.getBoundingClientRect();fields[e.id]={value:e.value||'',x:Math.round(screenX+(outerWidth-innerWidth)/2+r.x+r.width/2),y:Math.round(screenY+outerHeight-innerHeight+r.y+r.height/2)};}fetch('/observe',{method:'POST',body:JSON.stringify({seq:++seq,fields,enabled:!document.querySelector('#review').disabled})});}for(const e of document.querySelectorAll('input'))e.oninput=()=>{observe();if([...document.querySelectorAll('input')].every(x=>x.value))setTimeout(()=>{document.querySelector('#review').disabled=false;observe();},250);};function review(){const r=new XMLHttpRequest();r.open('POST','/effect',false);r.send(JSON.stringify([...document.querySelectorAll('input')].map(x=>x.value)));document.querySelector('#result').textContent='Reviewed once';}onload=observe;onresize=observe;</script>'''
oracle={'effects':0,'fault':False,'state':None,'cookie_issued':False,'authenticated_requests':0,'seq':0,'fields':{},'enabled':False,'submitted':None,'partial_value':None,'ready':0,'ready_pages':[],'http_requests':[]}
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        oracle['http_requests'].append({'method':'GET','path':self.path,'at':time.monotonic()})
        if 'kindred_fixture_session=synthetic_session' in self.headers.get('Cookie',''):
            oracle['authenticated_requests']+=1
        self.send_response(200)
        self.send_header('Content-Type','text/html; charset=utf-8')
        if not oracle['cookie_issued']:
            self.send_header('Set-Cookie','kindred_fixture_session=synthetic_session; Path=/; Max-Age=86400; Secure; HttpOnly; SameSite=Lax')
            oracle['cookie_issued']=True
        # Distinct fixture pages let the typed native-title proof select a tab.
        # Duplicate titles are exercised separately and must remain native-only.
        title_suffix=hashlib.sha256(self.path.encode()).hexdigest()[:10]
        self.end_headers();self.wfile.write(HTML.replace(b'</title>',(' ['+title_suffix+']</title>').encode())+b"<script>addEventListener('load',()=>fetch('/page-ready',{method:'POST',body:location.pathname+location.search}));</script>")
    def do_POST(self):
        oracle['http_requests'].append({'method':'POST','path':self.path,'at':time.monotonic()})
        body=self.rfile.read(int(self.headers.get('Content-Length','0')))
        if self.path=='/page-ready':
            oracle['ready_pages'].append(body.decode());self.send_response(200);self.end_headers();return
        if self.path=='/ready':
            oracle['ready']+=1;self.send_response(200);self.end_headers();return
        if self.path=='/observe':
            observation=json.loads(body)
            if observation['seq']>=oracle['seq']:oracle.update(observation)
            self.send_response(200);self.end_headers();return
        if self.path=='/partial':
            oracle['partial_value']=body.decode()
        else:
            oracle['effects']+=1
        if COMPARE:oracle['submitted']=json.loads(body)
        if oracle['fault'] or self.path=='/partial':
            for entry in Path('/proc').iterdir():
                if not entry.name.isdecimal():continue
                try:
                    args=(entry/'cmdline').read_bytes().split(b'\0');env=(entry/'environ').read_bytes().split(b'\0')
                    if args[:2]==[str(driver).encode(),b'mcp'] and ('CUA_HOME='+str(oracle['state']/'cua')).encode() in env:
                        os.kill(int(entry.name),9)
                except OSError:pass
        self.send_response(200);self.end_headers();self.wfile.write(b'ok')
    def log_message(self,*a):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
screen=next(n for n in range(32,1,-1) if not Path(f'/tmp/.X11-unix/X{n}').exists() and not Path(f'/tmp/.X{n}-lock').exists())
with socket.socket() as listener:listener.bind(('127.0.0.1',0));address=listener.getsockname()
processes=[];receipts=[]
with tempfile.TemporaryDirectory(prefix='kindred-cua-executor-') as temporary:
    root=Path(temporary);profile=root/'profile';state=root/'bridge';state.mkdir(mode=0o700);oracle['state']=state
    subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-keyout',str(root/'key.pem'),'-out',str(root/'cert.pem')],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(root/'cert.pem',root/'key.pem');server.socket=context.wrap_socket(server.socket,server_side=True)
    threading.Thread(target=server.serve_forever,daemon=True).start();url=f'https://127.0.0.1:{server.server_port}'
    commands=root/'bin';commands.mkdir();capture_fault=root/'fail-capture';capture=commands/'import'
    capture.write_text('#!/bin/sh\nif [ -e '+shlex.quote(str(capture_fault))+' ]; then exit 19; fi\nexec '+shlex.quote(os.environ['KINDRED_TEST_IMPORT'])+' "$@"\n');capture.chmod(0o755)
    launcher=commands/'chromium';launcher.write_text('#!/usr/bin/python3\nimport os,sys\nargs=[a for a in sys.argv[1:] if not a.startswith("--user-data-dir=")]\nos.execv('+repr(CHROME)+',['+repr(CHROME)+',"--disable-extensions","--no-sandbox","--ozone-platform=x11","--ignore-certificate-errors","--disable-dev-shm-usage",'+repr('--user-data-dir='+str(profile))+']+args)\n');launcher.chmod(0o755)
    driver=root/'cua-driver';driver.write_bytes(Path(os.environ['KINDRED_TEST_CUA_BINARY']).read_bytes());driver.chmod(0o755)
    env={**os.environ,'DISPLAY':f':{screen}','WAYLAND_DISPLAY':'kindred-fixture-unavailable','WAYLAND_SOCKET':'9876','XDG_SESSION_TYPE':'wayland','XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),
         'KINDRED_EXECUTOR_FIXTURE_SCREEN':str(screen),'KINDRED_EXECUTOR_FIXTURE_ADDRESS':f'{address[0]}:{address[1]}',
         'PATH':str(commands)+':'+os.environ['PATH'],'KINDRED_TEST_CUA_PROFILE':str(profile),'KINDRED_TEST_CUA_STATE_ROOT':str(state),'KINDRED_TEST_CUA_BINARY':str(driver)}
    log=(ARTIFACTS/'process.log').open('w')
    def spawn(args):
        process=subprocess.Popen(list(map(str,args)),env=env,stdout=log,stderr=log);processes.append(process);return process
    opened_pages=0
    def open_page(session='fixture-run'):
        global opened_pages
        opened_pages+=1
        path='/?page='+str(opened_pages)
        result=call('computer_open_url',{'url':url+path,'_page_session':session})
        deadline=time.monotonic()+20
        while time.monotonic()<deadline and path not in oracle['ready_pages']:time.sleep(.02)
        assert path in oracle['ready_pages'],('Navigation did not finish',path,oracle['http_requests'])
        return result
    def call(tool,args):
        start=time.monotonic()
        with socket.create_connection(address,timeout=30) as connection:
            connection.settimeout(60);connection.sendall(json.dumps({'tool':tool,'args':{'_page_session':'fixture-run',**args}}).encode()+b'\n');data=bytearray()
            while not data.endswith(b'\n'):
                block=connection.recv(65536)
                if not block:raise EOFError('Executor response ended')
                data.extend(block)
        value=json.loads(data);saved={k:v for k,v in value.items() if k!='image'}
        if value.get('image'):
            image=base64.b64decode(value['image'].split(',',1)[-1]);saved['image_sha256']=hashlib.sha256(image).hexdigest();saved['image_bytes']=len(image)
            (ARTIFACTS/f'{len(receipts):02}-{tool}.png').write_bytes(image)
        receipts.append({'tool':tool,'args':args,'elapsed_ms':round((time.monotonic()-start)*1000,2),'result':saved});return value
    try:
        spawn(['Xvfb',f':{screen}','-screen','0','1440x900x24','-nolisten','tcp','-noreset']);time.sleep(.3);spawn(['openbox']);time.sleep(.3)
        chrome=spawn(['node',ROOT/'tools/launch-cua-fixture-chrome.cjs',profile,url+'/?page=initial',root/'chrome-ready']);time.sleep(.2)
        deadline=time.monotonic()+35
        while time.monotonic()<deadline and not (oracle['fields'] if COMPARE else oracle['ready']):
            if chrome.poll() is not None:raise RuntimeError('Chrome exited before fixture ready')
            time.sleep(.05)
        assert oracle['fields'] if COMPARE else oracle['ready'], 'Chrome did not render disposable fixture'
        executor=spawn([BINARY,'--exact','guest::browser_tests::local_executor_fixture','--ignored','--nocapture'])
        port_hex=f'{address[1]:04X}'
        for _ in range(100):
            if executor.poll() is not None:raise RuntimeError('Executor ended early')
            if any(line.split()[1].endswith(':'+port_hex) for line in Path('/proc/net/tcp').read_text().splitlines()[1:]):break
            time.sleep(.05)
        if COMPARE:
            assert COMPARE in ('native','target')
            started=time.monotonic();open_page();time.sleep(.3)
            if COMPARE=='target':assert (state/'adapter.py').read_bytes()==(ROOT/'deploy/cua-adapter.py').read_bytes(),'Stale adapter embedded in executor'
            observation=call('computer_screenshot',{});assert observation.get('image')
            assert ('page_observation' in observation)==(COMPARE=='target'), 'Wrong executor or unexpected attachment'
            expected={'recipient':'Fixture recipient','amount':'12.34','note':'Fixture review only'}
            for name,value in expected.items():
                if COMPARE=='target':
                    target=next(x['target'] for x in observation['page_observation']['elements'] if x['name'].lower()==name)
                    observation=call('computer_type',{'target':target,'text':value,'observe':True})
                else:
                    point=oracle['fields'][name];clicked=call('computer_click',{'x':point['x'],'y':point['y'],'observe':True});assert clicked['action_applied'] and clicked.get('image')
                    observation=call('computer_type',{'text':value,'observe':True})
                assert observation['action_applied'] and observation.get('image')
                deadline=time.monotonic()+2
                while time.monotonic()<deadline and oracle['fields'].get(name,{}).get('value')!=value:time.sleep(.02)
                assert oracle['fields'].get(name,{}).get('value')==value, (name,oracle)
            deadline=time.monotonic()+2
            while time.monotonic()<deadline and not oracle['enabled']:time.sleep(.02)
            assert oracle['enabled']
            # Fresh observation before the delayed review button, same policy in both modes.
            observation=call('computer_screenshot',{})
            if COMPARE=='target':
                target=next(x['target'] for x in observation['page_observation']['elements'] if x['name']=='Review fixture')
                review=call('computer_click',{'target':target,'observe':True})
            else:
                point=oracle['fields']['review'];review=call('computer_click',{'x':point['x'],'y':point['y'],'observe':True})
            assert review['action_applied'] and review.get('image');assert oracle['effects']==1 and oracle['submitted']==list(expected.values()),oracle
            elapsed=time.monotonic()-started
            with socket.create_connection(address) as connection:connection.sendall(b'{"tool":"fixture_stop","args":{}}\n')
            executor.wait(timeout=5);assert executor.returncode==0
            (ARTIFACTS/'results.json').write_text(json.dumps({'passed':True,'mode':COMPARE,'source_head':subprocess.check_output(['git','-C',str(SOURCE_ROOT),'rev-parse','HEAD'],text=True).strip(),'source_dirty':bool(subprocess.check_output(['git','-C',str(SOURCE_ROOT),'status','--porcelain'],text=True)),'driver_binary_sha256':hashlib.sha256(driver.read_bytes()).hexdigest(),'guest_source_sha256':hashlib.sha256((SOURCE_ROOT/'src/guest.rs').read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256(Path(BINARY).read_bytes()).hexdigest(),'tool_requests':len(receipts),'images':sum(bool(r['result'].get('image_sha256')) for r in receipts),'image_bytes':sum(r['result'].get('image_bytes',0) for r in receipts),'elapsed_seconds':elapsed,'incorrect_target':0,'duplicate_writes':0,'effects':oracle['effects'],'submitted_values':oracle['submitted'],'provider_requests':None,'model_tokens':None,'limits':'Scripted executor policy with fixture value/geometry oracle, not live model evidence; includes fixture page-load settling delay'},indent=2))
        else:
            cold=call('computer_screenshot',{});assert cold.get('image') and 'page_observation' not in cold
            call('computer_open_url',{'url':url+'/?page=initial'});time.sleep(.3)
            ambiguous=call('computer_screenshot',{});assert ambiguous.get('image') and 'page_observation' not in ambiguous;assert oracle['effects']==0
            open_page();time.sleep(.3)
            assert (state/'adapter.py').read_bytes()==(ROOT/'deploy/cua-adapter.py').read_bytes(),'Stale adapter embedded in executor'
            observation=call('computer_screenshot',{});assert (observation['width'],observation['height'])==(1440,900);assert observation.get('image')
            if 'page_observation' not in observation:
                import importlib.util
                spec=importlib.util.spec_from_file_location('diagnostic_bridge',ROOT/'deploy/cua-adapter.py');bridge=importlib.util.module_from_spec(spec);spec.loader.exec_module(bridge)
                diagnostic=bridge.MCP(str(driver),env['DISPLAY'],root/'diagnostic');driver_trace=[]
                original=diagnostic.call
                def record(tool,arguments):
                    result=original(tool,arguments);driver_trace.append({'tool':tool,'args':arguments,'response':result});return result
                diagnostic.call=record
                try:
                    a=bridge.Adapter(diagnostic,str(profile),env['DISPLAY']);a.attach('diagnostic-run');a.observe('diagnostic-run')
                finally:
                    (ARTIFACTS/'direct-driver-diagnostic.json').write_text(json.dumps(driver_trace,indent=2));diagnostic.close()
            page=observation['page_observation'];field=next(x['target'] for x in page['elements'] if x['name']=='Recipient')
            rejected=call('computer_type',{'target':field,'text':'Wrong','_page_session':'foreign-run'});assert rejected['failed'] and not rejected['uncertain_effect']
            typed=call('computer_type',{'target':field,'text':'Correct fixture recipient','observe':True});assert typed['action_applied'] and typed.get('image');assert any(x['value']=='Correct fixture recipient' for x in typed['page_observation']['elements'])
            button=next(x['target'] for x in typed['page_observation']['elements'] if x['name']=='Review fixture')
            clicked=call('computer_click',{'target':button,'observe':True});assert clicked['action_applied'] and clicked.get('image');assert any(x['name']=='1' for x in clicked['page_observation']['content'])
            stale=call('computer_click',{'target':button,'observe':True});assert stale['failed'] and not stale['uncertain_effect']
            # The real field accepts only a four-character prefix (maxlength);
            # kill the actual driver on that input event before its receipt.
            # This is not a claim of character-by-character driver delivery.
            note=next(x['target'] for x in clicked['page_observation']['elements'] if x['name']=='Note')
            partial=call('computer_type',{'target':note,'text':'Partial fixture text','observe':True})
            assert partial['uncertain_effect'] and partial['timed_out'],partial
            assert oracle['partial_value'] and len(oracle['partial_value'])<len('Partial fixture text'),oracle
            replay=call('computer_type',{'target':note,'text':'Partial fixture text'});assert replay['failed'] and not replay['uncertain_effect']
            open_page();time.sleep(.3)
            recovered=call('computer_screenshot',{});assert recovered.get('page_observation')
            # Native scrolling and fresh semantic observation remain usable.
            scrolled=call('computer_scroll',{'direction':'down','clicks':2,'observe':True});assert scrolled['action_applied'] and scrolled.get('image')
            scrolled=call('computer_scroll',{'direction':'up','clicks':2,'observe':True});assert scrolled['action_applied'] and scrolled.get('image')
            # Coordinate input remains the original native implementation and clears refs.
            current=call('computer_screenshot',{});old=next(x['target'] for x in current['page_observation']['elements'] if x['name']=='Review fixture')
            native=call('computer_key',{'key':'Tab','observe':True});assert native['action_applied'] and native.get('image')
            stale=call('computer_click',{'target':old});assert stale['failed'] and not stale['uncertain_effect']
            # Temporarily unavailable payload: screenshot/native methods still work,
            # and restoring the payload cannot revive an earlier page target.
            observation=call('computer_screenshot',{});before_missing=next(x['target'] for x in observation['page_observation']['elements'] if x['name']=='Review fixture')
            driver.rename(root/'driver-unavailable')
            missing=call('computer_screenshot',{});assert missing.get('image') and 'page_observation' not in missing
            native=call('computer_key',{'key':'Tab','observe':True});assert native['action_applied'] and native.get('image')
            (root/'driver-unavailable').rename(driver)
            refused=call('computer_click',{'target':before_missing});assert refused['failed'] and not refused['uncertain_effect']
            # Capture loss is distinct from lost input receipt: preserve applied effect.
            current=call('computer_screenshot',{});target=next(x['target'] for x in current['page_observation']['elements'] if x['name']=='Review fixture')
            capture_fault.touch()
            capture_lost=call('computer_click',{'target':target,'observe':True});assert capture_lost['action_applied'] and capture_lost['observation_error'] and not capture_lost.get('uncertain_effect');assert oracle['effects']==2
            reused=call('computer_click',{'target':target});assert reused['failed'] and not reused['uncertain_effect'];assert oracle['effects']==2
            capture_fault.unlink()
            fresh=call('computer_screenshot',{});assert fresh.get('image') and fresh.get('page_observation');assert oracle['effects']==2
            # Actual page-applied write while the driver is killed before receipt.
            current=call('computer_screenshot',{});target=next(x['target'] for x in current['page_observation']['elements'] if x['name']=='Review fixture')
            oracle['fault']=True
            fault=call('computer_click',{'target':target,'observe':True});assert fault['failed'] and fault['uncertain_effect'] and fault['timed_out'],fault
            assert oracle['effects']==3
            replay=call('computer_click',{'target':target,'observe':True});assert replay['failed'] and not replay['uncertain_effect'];assert oracle['effects']==3
            dead=call('computer_screenshot',{});assert dead.get('image') and 'page_observation' not in dead
            oracle['fault']=False
            # Explicit new authorized page-open restores driver observations, not the write.
            open_page('fixture-after-fault');time.sleep(.3)
            recovered=call('computer_screenshot',{'_page_session':'fixture-after-fault'});assert recovered.get('page_observation');assert oracle['effects']==3
            # A graceful browser restart retains this exact synthetic profile marker.
            authenticated_before_restart=oracle['authenticated_requests']
            chrome.terminate();chrome.wait(timeout=10)
            (root/'chrome-ready').unlink()
            restarted=spawn(['node',ROOT/'tools/launch-cua-fixture-chrome.cjs',profile,url+'/?page=initial',root/'chrome-ready'])
            deadline=time.monotonic()+35
            while time.monotonic()<deadline and not (root/'chrome-ready').exists():time.sleep(.05)
            assert (root/'chrome-ready').exists(),'Restarted fixture Chrome did not load'
            assert oracle['authenticated_requests']>authenticated_before_restart,'Synthetic session cookie was lost on restart'
            restarted_cold=call('computer_screenshot',{});assert restarted_cold.get('image') and 'page_observation' not in restarted_cold
            open_page();time.sleep(.3)
            state_after=call('computer_screenshot',{});assert any(x['value']=='Correct fixture recipient' for x in state_after['page_observation']['elements'])
            with socket.create_connection(address) as connection:connection.sendall(b'{"tool":"fixture_stop","args":{}}\n')
            executor.wait(timeout=5);assert executor.returncode==0
            (ARTIFACTS/'results.json').write_text(json.dumps({'passed':True,'production_guest_executor':True,'actual_png':True,'partial_typed_prefix':oracle['partial_value'],'partial_input_uncertain_no_replay':True,'partial_input_boundary':'Chromium maxlength accepts four-character prefix before actual driver kill; not character-by-character driver delivery','native_scroll_with_post_observation':True,'element_type_readback':True,'effect_count':3,'lost_capture_preserves_applied_effect_no_replay':True,'lost_input_receipt_one_effect_no_replay':True,'dead_driver_native_capture':True,'explicit_driver_recovery_without_replay':True,'foreign_run_refused':True,'one_use_refused':True,'native_input_invalidates_refs':True,'persistent_profile_restart':True,'synthetic_http_only_session_cookie_persists':True,'cold_and_restart_observation_native_only':True,'explicit_approved_open_attachment':True,'duplicate_title_native_fallback_without_input':True,'unavailable_native_fallback':True,'unavailable_does_not_revive_refs':True,'source_dirty':bool(subprocess.check_output(['git','-C',str(SOURCE_ROOT),'status','--porcelain'],text=True)),'driver_binary_sha256':hashlib.sha256(driver.read_bytes()).hexdigest(),'guest_source_sha256':hashlib.sha256((ROOT/'src/guest.rs').read_bytes()).hexdigest(),'adapter_source_sha256':hashlib.sha256((ROOT/'deploy/cua-adapter.py').read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256(Path(BINARY).read_bytes()).hexdigest()},indent=2))
    finally:
        (ARTIFACTS/'oracle.json').write_text(json.dumps({k:v for k,v in oracle.items() if k!='state'},indent=2))
        (ARTIFACTS/'receipts.json').write_text(json.dumps(receipts,indent=2))
        candidates=[]
        for entry in Path('/proc').iterdir():
            if not entry.name.isdecimal():continue
            try:
                args=(entry/'cmdline').read_bytes().split(b'\0');envs=(entry/'environ').read_bytes().split(b'\0')
                if str(profile).encode() in b' '.join(args):candidates.append({'pid':entry.name,'uid':entry.stat().st_uid,'args':[a.decode(errors='replace') for a in args],'display':[e.decode(errors='replace') for e in envs if e.startswith(b'DISPLAY=')]})
            except OSError:pass
        (ARTIFACTS/'browser-candidates.json').write_text(json.dumps(candidates,indent=2))
        # Only fixture-owned children and the bridge launched with this private socket.
        for entry in Path('/proc').iterdir():
            if entry.name.isdecimal():
                try:
                    cmd=(entry/'cmdline').read_bytes()
                    if str(state/'page.sock').encode() in cmd and b'--daemon' in cmd:os.kill(int(entry.name),15)
                except (OSError,PermissionError):pass
        for process in reversed(processes):
            if process.poll() is None:process.terminate()
        for process in reversed(processes):
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown();server.server_close();log.close()
