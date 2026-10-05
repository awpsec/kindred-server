"""Actual two-display driver isolation; Playwright supplies fixture lifecycle only."""
import hashlib,http.server,importlib.util,json,os
from pathlib import Path
import subprocess,tempfile,threading,time
ROOT=Path(__file__).resolve().parents[1]
PROOF=Path(os.environ['KINDRED_TEST_ARTIFACTS']);PROOF.mkdir(parents=True,exist_ok=True)
BINARY=Path(os.environ['KINDRED_TEST_CUA_BINARY']);manifest=json.loads((ROOT/'deploy/cua-driver-manifest.json').read_text())
assert hashlib.sha256(BINARY.read_bytes()).hexdigest()==manifest['binary_sha256']
spec=importlib.util.spec_from_file_location('bridge',ROOT/'deploy/cua-adapter.py');bridge=importlib.util.module_from_spec(spec);spec.loader.exec_module(bridge)
effects=[]
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        seat=self.path.strip('/')
        self.send_response(200);self.end_headers();self.wfile.write(f'<title>Seat {seat}</title><button onmousedown="if(event.isTrusted)fetch(\'/effect\',{{method:\'POST\',body:\'{seat}\'}})">Write {seat}</button>'.encode())
    def do_POST(self):
        effects.append(self.rfile.read(int(self.headers['Content-Length'])).decode());self.send_response(200);self.end_headers()
    def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
processes=[];drivers=[];trace=[]
with tempfile.TemporaryDirectory(prefix='kindred-cua-displays-') as temporary:
    root=Path(temporary);log=(PROOF/'process.log').open('w')
    try:
        seats={};screens=[n for n in range(31,1,-1) if not Path(f'/tmp/.X11-unix/X{n}').exists() and not Path(f'/tmp/.X{n}-lock').exists()][:2];assert len(screens)==2
        for seat,screen in zip(('A','B'),screens):
            path=root/seat;path.mkdir();profile=path/'profile';display=f':{screen}'
            env={**os.environ,'DISPLAY':display,'XDG_CONFIG_HOME':str(path/'config'),'XDG_CACHE_HOME':str(path/'cache'),'XDG_DATA_HOME':str(path/'data')}
            for command in (['Xvfb',display,'-screen','0','1440x900x24','-nolisten','tcp','-noreset'],['openbox']):
                process=subprocess.Popen(command,env=env,stdout=log,stderr=log);processes.append(process);time.sleep(.3)
            process=subprocess.Popen(['node',str(ROOT/'tools/launch-cua-fixture-chrome.cjs'),str(profile),f'http://127.0.0.1:{server.server_port}/{seat}',str(path/'ready')],env=env,stdout=log,stderr=log);processes.append(process)
            deadline=time.monotonic()+15
            while time.monotonic()<deadline and not (path/'ready').exists():
                assert process.poll() is None,'Fixture browser ended';time.sleep(.05)
            assert (path/'ready').exists()
            mcp=bridge.MCP(str(BINARY),display,path);drivers.append(mcp)
            original=mcp.call
            def record(tool,args,original=original,seat=seat):
                response=original(tool,args);trace.append({'seat':seat,'tool':tool,'args':args,'response':response});return response
            mcp.call=record
            adapter=bridge.Adapter(mcp,str(profile),display)
            assert adapter.handle({'op':'attach','session':seat}).get('attached')
            observation=adapter.handle({'op':'observe','session':seat});assert observation.get('elements'),observation
            seats[seat]=(adapter,observation,bridge.browser_pid(str(profile),display),display)
        a,oa,pida,displaya=seats['A'];b,ob,pidb,displayb=seats['B']
        assert pida!=pidb and displaya!=displayb
        windows=[a.anchor[1],b.anchor[1]]
        assert windows[0]==windows[1],f'Fixture must challenge colliding native window IDs: {windows}'
        targeta=next(e['target'] for e in oa['elements'] if e['name']=='Write A')
        targetb=next(e['target'] for e in ob['elements'] if e['name']=='Write B')
        foreign=b.handle({'op':'click','session':'B','target':targeta});assert foreign['failed'] and not foreign['uncertain_effect']
        before=len(trace)
        wrong=bridge.Adapter(b.mcp,a.profile,displayb).handle({'op':'attach','session':'wrong-display'})
        assert wrong.get('unavailable') and not effects,wrong
        assert all(r['tool']!='browser_prepare' for r in trace[before:]),'Wrong display must stop before endpoint preparation'
        # Refresh B after the refused extra session, then make one trusted write per seat.
        assert b.handle({'op':'attach','session':'B'}).get('attached')
        ob=b.handle({'op':'observe','session':'B'});targetb=next(e['target'] for e in ob['elements'] if e['name']=='Write B')
        assert a.handle({'op':'click','session':'A','target':targeta})['action_applied']
        assert b.handle({'op':'click','session':'B','target':targetb})['action_applied']
        deadline=time.monotonic()+2
        while time.monotonic()<deadline and len(effects)<2:time.sleep(.02)
        assert sorted(effects)==['A','B'],effects
        (PROOF/'results.json').write_text(json.dumps({'passed':True,'driver_sha256':manifest['binary_sha256'],'adapter_sha256':hashlib.sha256((ROOT/'deploy/cua-adapter.py').read_bytes()).hexdigest(),'pids':[pida,pidb],'displays':[displaya,displayb],'colliding_window_ids':windows,'wrong_display_before_prepare':True,'foreign_target_zero_effects':True,'trusted_effects':effects,'limits':'Two disposable real X11 displays and Chromium profiles; no production bot, AT-SPI or live model claim'},indent=2))
    finally:
        (PROOF/'trace.json').write_text(json.dumps(trace,indent=2))
        for driver in drivers:driver.close()
        for process in reversed(processes):
            if process.poll() is None:process.terminate()
        for process in reversed(processes):
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown();server.server_close();log.close()
