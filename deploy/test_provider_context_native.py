"""Opt-in real Claude CLI, loopback fake Messages API, disposable auth-free home.

Run inside a network-none container with KINDRED_CLAUDE_NATIVE_FIXTURE set to the
verified pinned CLI. No provider credentials, subscriptions, or real tools.
"""
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import unittest
import uuid

@unittest.skipUnless(os.environ.get('KINDRED_CLAUDE_NATIVE_FIXTURE'),'Set pinned Claude fixture binary; run network-isolated')
class NativeClaudeContext(unittest.TestCase):
    def test_explicit_session_resume_restores_history_without_repeating_input(self):
        requests=[]
        class API(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_POST(self):
                body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
                if self.path.startswith('/v1/messages/count_tokens'):
                    self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(b'{"input_tokens":20}');return
                if not self.path.startswith('/v1/messages'):
                    self.send_response(404);self.end_headers();return
                requests.append(body)
                message={'id':'msg_fixture_'+str(len(requests)),'type':'message','role':'assistant','model':body['model'],
                         'content':[],'stop_reason':None,'stop_sequence':None,'usage':{'input_tokens':20,'output_tokens':0,'cache_creation_input_tokens':0,'cache_read_input_tokens':0}}
                events=[{'type':'message_start','message':message},
                        {'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}},
                        {'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'Fixture completed.'}},
                        {'type':'content_block_stop','index':0},
                        {'type':'message_delta','delta':{'stop_reason':'end_turn','stop_sequence':None},'usage':{'output_tokens':3}},
                        {'type':'message_stop'}]
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
                for event in events:self.wfile.write(('event: '+event['type']+'\ndata: '+json.dumps(event)+'\n\n').encode())
                self.wfile.flush()
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),API)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix='kindred-native-context-') as directory:
                root=Path(directory);home=root/'home';home.mkdir();cwd=root/'conversation';cwd.mkdir();config=home/'.claude';sid=str(uuid.uuid4())
                env={'HOME':str(home),'CLAUDE_CONFIG_DIR':str(config),'PATH':'/usr/local/bin:/usr/bin:/bin',
                     'ANTHROPIC_API_KEY':'kindred-local-fixture-not-a-key','ANTHROPIC_BASE_URL':f'http://127.0.0.1:{server.server_port}',
                     'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC':'1','DISABLE_AUTOUPDATER':'1','NO_COLOR':'1'}
                binary=os.environ['KINDRED_CLAUDE_NATIVE_FIXTURE']
                version=subprocess.check_output([binary,'--version'],env=env,text=True,timeout=10).strip()
                common=[binary,'-p','--verbose','--output-format','stream-json','--tools','','--strict-mcp-config',
                        '--setting-sources','','--restricted','--disable-slash-commands','--no-chrome',
                        '--model','claude-sonnet-4-6','--system-prompt','Stable Kindred fixture policy.',
                        '--system-prompt-snapshot','on','--max-turns','1']
                def run(flags,prompt):
                    result=subprocess.run(common+flags,input=prompt,env=env,cwd=cwd,capture_output=True,text=True,timeout=25)
                    self.assertEqual(result.returncode,0,result.stderr[-1500:]+result.stdout[-1500:])
                    rows=[json.loads(line)for line in result.stdout.splitlines() if line.startswith('{')]
                    final=next(row for row in reversed(rows) if row.get('type')=='result')
                    self.assertEqual(final['subtype'],'success');self.assertEqual(final['session_id'],sid)
                    return rows
                first='FIRST_PRIVATE_CONTEXT_ALPHA'
                second='SECOND_CURRENT_REQUEST_BETA'
                first_rows=run(['--session-id',sid],first)
                transcript=config/'projects'/re.sub(r'[^a-zA-Z0-9]','-',str(cwd))/(sid+'.jsonl')
                self.assertTrue(transcript.is_file());self.assertGreater(transcript.stat().st_size,0)
                first_count=len(requests)
                second_rows=run(['--resume',sid],second)
                self.assertGreater(len(requests),first_count)
                prompt=json.dumps(requests[-1]['messages'])
                self.assertEqual(prompt.count(first),1);self.assertEqual(prompt.count(second),1)
                self.assertIn('Fixture completed.',prompt)
                self.assertTrue(any(row.get('type')=='system' and row.get('subtype')=='init' and row.get('session_id')==sid for row in first_rows))
                self.assertTrue(any(row.get('type')=='system' and row.get('subtype')=='init' and row.get('session_id')==sid for row in second_rows))
                before_missing=len(requests)
                missing=subprocess.run(common+['--resume',str(uuid.uuid4())],input='MUST_NOT_SEND',env=env,cwd=cwd,capture_output=True,text=True,timeout=15)
                self.assertNotEqual(missing.returncode,0);self.assertEqual(len(requests),before_missing)
                print(json.dumps({'native_version':version,'explicit_session_id':True,'resume_id_verified':True,'native_transcript_path_verified':True,
                                  'first_input_occurrences_on_resume':1,'new_input_occurrences_on_resume':1,'missing_resume_no_inference':True,
                                  'mock_requests':len(requests),'provider_traffic':False}),flush=True)
        finally:server.shutdown();server.server_close();thread.join(timeout=2)

if __name__=='__main__':unittest.main()
