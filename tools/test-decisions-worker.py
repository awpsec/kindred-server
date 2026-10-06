"""Actual production Rust/HTTP/browser loop on a disposable local form.

Default is an explicitly MOCKED local Decisions wire server. --live uses the
fixed official production transport and requires KINDRED_DECISIONS_API_KEY.
No selector replacement is made in live mode. Playwright only launches the
browser; input and choice execution are performed by Kindred.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('browser_fixture',ROOT/'tools/test-browser-worker.py')
fixture=importlib.util.module_from_spec(spec);spec.loader.exec_module(fixture)
TARGETS='192.0.2.1\n198.51.100.0/24\nfixture.example'
fixture.PAGE=b'''<!doctype html><title>Disposable Decisions form</title>
<label><input id="safe" type="checkbox">Safe checks</label>
<label><input id="other" type="checkbox" checked>Preserve this setting</label>
<label>Targets<textarea id="targets">old target</textarea></label>
<label>Policy<select id="policy"><option value="basic">Basic</option><option value="advanced">Advanced</option></select></label>
<button id="review" onclick="this.disabled=true;document.getElementById('confirm').hidden=false;document.getElementById('result').textContent='Review before confirming'">Review configuration</button>
<button id="confirm" hidden onclick="this.disabled=true;fetch('/confirm',{method:'POST'}).then(()=>document.getElementById('result').textContent='Configuration confirmed locally')">Confirm configuration</button><p id="result"></p>
<input hidden value="hidden-sentinel"><input hidden type="password" value="password-sentinel">'''
EFFECTS=[];REQUESTS=[]
class PageHandler(fixture.Handler):
    def do_POST(self):
        assert self.path=='/confirm';EFFECTS.append(time.monotonic());self.send_response(200);self.end_headers();self.wfile.write(b'ok')
fixture.Handler=PageHandler
class DecisionsHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        assert self.path=='/v1/decisions'
        assert self.headers['Authorization']=='Bearer sk-fixture-only-not-valid-live'
        request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert request['model']=='gpt-6-luna'
        text=json.loads(request['input'][0]['content'][0]['text']);controls=json.loads(text['observed_controls'])['controls']
        choices=request['questions'][0]['choices']; name=request['questions'][0]['name']
        assert request['input'][0]['content'][1]['image_url'].startswith('data:image/png;base64,')
        serialized=json.dumps(request);assert 'password-sentinel' not in serialized and 'hidden-sentinel' not in serialized
        states={c['name']:c for c in controls}; labels=[c['description'] for c in choices]
        if not states['Safe checks']['checked']:wanted='Set checkbox “Safe checks” to checked'
        elif states['Targets']['value']!=TARGETS:wanted='Fill “Targets”'
        elif states['Policy']['value']!='advanced':wanted='to “Advanced”'
        elif not states['Review configuration']['disabled']:wanted='Click “Review configuration”'
        elif 'Confirm configuration' in states and not states['Confirm configuration']['disabled']:wanted='Click “Confirm configuration”'
        elif EFFECTS:wanted='Finish:'
        else:wanted='Wait briefly'
        selected=next(c['value'] for c in choices if wanted in c['description'])
        REQUESTS.append({'question':name,'choice':selected,'choice_count':len(choices),'mocked':True})
        response={'answers':[{'type':'choice','name':name,'choice':selected,'confidence':1,
            'probabilities':[{'value':c['value'],'probability':int(c['value']==selected)} for c in choices]}]}
        data=json.dumps(response).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args):pass

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--chrome',required=True);parser.add_argument('--playwright-module',required=True);parser.add_argument('--test-binary',required=True);parser.add_argument('--proof',required=True);parser.add_argument('--live',action='store_true');parser.add_argument('--x11',action='store_true');args=parser.parse_args()
    live_key=os.environ.get('KINDRED_DECISIONS_API_KEY') if args.live else None
    if args.live and not live_key and os.environ.get('KINDRED_DECISIONS_API_KEY_FILE'):
        private=Path(os.environ['KINDRED_DECISIONS_API_KEY_FILE'])
        import stat
        metadata=private.stat()
        if private.is_symlink() or not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=os.getuid() or metadata.st_mode&0o077:
            raise RuntimeError('Live key file must be an owned private regular file; no request made')
        if metadata.st_size>4096:raise RuntimeError('Live key file exceeds limit; no request made')
        live_key=private.read_text().strip()
    if args.live and not live_key:raise RuntimeError('Live API key is not provisioned; no request made')
    if args.x11:
        if not os.environ.get('DISPLAY'):raise RuntimeError('Disposable X11 display required')
        os.environ['KINDRED_BROWSER_FIXTURE_DISPLAY']=os.environ['DISPLAY']
    proof=Path(args.proof);proof.mkdir(parents=True,exist_ok=True)
    fixture.NODE='node';fixture.CHROME=args.chrome;fixture.PLAYWRIGHT_MODULE=args.playwright_module
    # unittest fixture owns the exact temporary profile, HTTP server and cleanup.
    import unittest
    class Run(fixture.BrowserTests):
        def test_production_loop(self):
            env={**os.environ,'KINDRED_BROWSER_FIXTURE_PROFILE':self.directory.name,
                'KINDRED_BROWSER_FIXTURE_TASK':json.dumps({'origin':self.origin,'goal':'Enable Safe checks, preserve the other setting, enter the exact supplied targets, choose Advanced policy, review and confirm configuration exactly once. Finish only after Configuration confirmed locally is visible.','values':{'targets':TARGETS},'max_actions':8}),
                'KINDRED_BROWSER_FIXTURE_RESULT':str(proof/'worker-result.json')}
            server=None
            if not args.live:
                server=ThreadingHTTPServer(('127.0.0.1',0),DecisionsHandler);threading.Thread(target=server.serve_forever,daemon=True).start();env['KINDRED_BROWSER_FIXTURE_ENDPOINT']=f'http://127.0.0.1:{server.server_port}/v1/decisions'
            else:
                env.pop('KINDRED_BROWSER_FIXTURE_ENDPOINT',None);env['KINDRED_DECISIONS_API_KEY']=live_key
            try:
                with (proof/'worker.log').open('w') as log:
                    run=subprocess.run([args.test_binary,'browser_use::tests::actual_browser_decisions_worker','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=subprocess.STDOUT,timeout=150)
                result=json.loads((proof/'worker-result.json').read_text());body=json.loads(result['text'])
                oracle={key:self.driver.evaluate(expression) for key,expression in {
                    'safe':'document.getElementById("safe").checked','preserved':'document.getElementById("other").checked','targets':'document.getElementById("targets").value','policy':'document.getElementById("policy").value','confirmation':'document.getElementById("result").textContent'}.items()}
                receipt={'live_api':args.live,'test_exit':run.returncode,'requests':REQUESTS if not args.live else result['fixture_metrics']['requests'],
                    'metrics':result['fixture_metrics'],'oracle':oracle,'effects':len(EFFECTS),'status':body['browser_worker']['status']}
                (proof/'summary.json').write_text(json.dumps(receipt,indent=2)+'\n')
                self.assertEqual(run.returncode,0);self.assertEqual(oracle,{'safe':True,'preserved':True,'targets':TARGETS,'policy':'advanced','confirmation':'Configuration confirmed locally'});self.assertEqual(len(EFFECTS),1);self.assertEqual(body['browser_worker']['status'],'reported_finished')
                self.assertLessEqual(len(result['fixture_metrics']['requests']),9)
            finally:
                if server:server.shutdown();server.server_close()
    # This fixture does not use the original Safe checks readiness string check
    # differently; the form still contains it and all setup tests are unchanged.
    suite=unittest.TestSuite([Run('test_production_loop')]);result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
if __name__=='__main__':main()
