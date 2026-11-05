"""Durable Claude bridge tests. Fake native CLI, isolated homes, no model/account."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import queue
import unittest
from unittest.mock import patch

HELPER=Path(__file__).with_name('provider-cli.py').resolve()
spec=importlib.util.spec_from_file_location('context_bridge',HELPER)
bridge=importlib.util.module_from_spec(spec);spec.loader.exec_module(bridge)
FIRST='11111111-1111-4111-8111-111111111111'
SECOND='22222222-2222-4222-8222-222222222222'
LAUNCH=r'''
import asyncio,importlib.util,json,sys,os
spec=importlib.util.spec_from_file_location('bridge',sys.argv[1]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.BIN['claude-code']=sys.argv[2]
m.account=lambda *a:{'connected':True,'account_key':sys.argv[4],'data':[{'model':'sonnet','supportedReasoningEfforts':[{'reasoningEffort':'low'}]}]}
m.environment=lambda p:{'HOME':sys.argv[3],'CLAUDE_CONFIG_DIR':sys.argv[3]+'/.claude','PATH':'/usr/bin:/bin'}
load=m.connector_module
def connectors():
 x=load();original=x.initialize
 async def initialize(child,*args,**kwargs):return await original(child,settle=0)
 x.initialize=initialize;return x
m.connector_module=connectors
try:asyncio.run(m.worker('claude-code'))
except m.ContextUnavailable as e:
 m.emit({'type':'error','code':'context_unavailable','context_protocol':1,'context_pre_input':True,'message':str(e)});sys.exit(1)
'''
FAKE=r'''#!/usr/bin/python3
import json,sys,os,re
from pathlib import Path
root=Path(__file__).parent
fault=(root/'fault').read_text() if (root/'fault').exists() else ''
args=sys.argv
def emit(v):print(json.dumps(v),flush=True)
def read():return json.loads(sys.stdin.readline())
ephemeral='--no-session-persistence' in args
assert not ephemeral or fault=='allow-ephemeral'
assert '--continue' not in args
assert args[args.index('--system-prompt-snapshot')+1]=='on'
assert args[args.index('--tools')+1]=='ToolSearch' and '--restricted' in args
assert args[args.index('--permission-prompt-tool')+1]=='stdio'
resume='--resume' in args;sid=args[args.index('--resume' if resume else '--session-id')+1]
assert Path.cwd().name==sid or (ephemeral and Path.cwd().name.startswith('kindred-context-fallback-'))
instructions=Path(args[args.index('--system-prompt-file')+1]).read_text()
assert 'CURRENT-STATE' not in instructions
with (root/'launches.jsonl').open('a') as f:f.write(json.dumps({'sid':sid,'resume':resume,'cwd':str(Path.cwd()),'instructions':instructions})+'\n')
packet=read();assert packet['request']['subtype']=='initialize'
emit({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{}}})
packet=read();assert packet['request']['subtype']=='mcp_status'
emit({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{'mcpServers':[]}}})
packet=read();assert packet['type']=='user'
assert 'CURRENT-STATE' in packet['message']['content']
with (root/'inputs.jsonl').open('a') as f:f.write(json.dumps(packet)+'\n')
native='33333333-3333-4333-8333-333333333333' if fault=='wrong-id' else sid
emit({'type':'system','subtype':'init','session_id':native,'mcp_servers':[{'name':'kindred','status':'connected'}],'tools':['mcp__kindred__remember']})
if fault in ('two-usage','latest-missing-usage'):
 emit({'type':'assistant','message':{'id':'earlier-message','usage':{'input_tokens':1000,'output_tokens':10},'content':[{'type':'text','text':'Earlier request.'}]}})
emit({'type':'assistant','message':{'id':'message','usage':None if fault in ('missing-usage','latest-missing-usage') else {'input_tokens':10,'cache_read_input_tokens':100,'cache_creation_input_tokens':5,'output_tokens':4},'content':[{'type':'text','text':'Completed once.'}]}})
project=Path(os.environ['CLAUDE_CONFIG_DIR'])/'projects'/re.sub(r'[^a-zA-Z0-9]','-',str(Path.cwd()))
project.mkdir(parents=True,exist_ok=True)
transcript=project/(sid+'.jsonl')
if fault!='missing-transcript' and not ephemeral:
 with transcript.open('a') as f:f.write(json.dumps({'sessionId':sid,'type':'user','message':packet['message']})+'\n')
emit({'type':'result','subtype':'error_during_execution' if fault=='failed' else 'success','is_error':fault=='failed','session_id':native,'uuid':'native-result-1'})
if fault=='result-only-followup':
 packet=read();assert packet['type']=='user' and packet['message']['content']=='New follow-up only.'
 emit({'type':'result','subtype':'success','is_error':False,'session_id':native,'uuid':'native-result-2'})
assert sys.stdin.read()==''
if fault=='bad-exit':sys.exit(7)
'''

class ContextTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)
        self.home=self.root/'home';self.home.mkdir()
        self.binary=self.root/'fake';self.binary.write_text(FAKE);self.binary.chmod(0o755)
        self.previous_binary=bridge.BIN['claude-code'];bridge.BIN['claude-code']=str(self.binary)
    def tearDown(self):bridge.BIN['claude-code']=self.previous_binary;self.tmp.cleanup()
    def request(self,sid=FIRST,resume=False):
        return {'protocol':1,'provider':'claude-code','model':'sonnet','reasoning_effort':'','instructions':'Stable Kindred instructions.',
                'prompt':'Only the current request.','live_context':'CURRENT-STATE: previous actions completed; old question dismissed.',
                'context_protocol':1,'context_session_id':sid,'context_resume':resume,'max_steps':4,
                'tools':[{'name':'remember','description':'Save memory','inputSchema':{'type':'object','properties':{}}}]}
    def run_worker(self,request,account='a'*64):
        child=subprocess.Popen([sys.executable,'-c',LAUNCH,str(HELPER),str(self.binary),str(self.home),account],
                               stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        packets=queue.Queue()
        def read():
            for line in child.stdout:packets.put(line)
            packets.put(None)
        reader=threading.Thread(target=read,daemon=True);reader.start()
        events=[]
        try:
            child.stdin.write(json.dumps(request)+'\n');child.stdin.flush()
            for _ in range(20):
                line=packets.get(timeout=15)
                if line is None:break
                frame=json.loads(line);events.append(frame)
                if frame['type']=='connector_catalogue':
                    child.stdin.write(json.dumps({'type':'tool_result','id':'connector-context','result':{}})+'\n');child.stdin.flush()
                if frame['type']=='turn_complete':
                    decision={'type':'turn_decision','id':'turn-'+frame['input_id']}
                    decision.update({'offer_id':'followup','prompt':'New follow-up only.'} if frame['input_id']=='initial' else {'finish':True})
                    child.stdin.write(json.dumps(decision)+'\n');child.stdin.flush()
                if frame['type'] in ('complete','error'):break
            child.stdin.close();child.wait(timeout=10);reader.join(timeout=1)
            errors=child.stderr.read()
            return subprocess.CompletedProcess(child.args,child.returncode,stderr=errors),events
        finally:
            if child.poll() is None:child.kill();child.wait()
            if not child.stdin.closed:child.stdin.close()
            child.stdout.close();child.stderr.close()
    def transcript(self,sid=FIRST):
        context=self.home/'contexts'/sid
        project=bridge.re.sub(r'[^a-zA-Z0-9]','-',str(context))
        return self.home/'.claude/projects'/project/(sid+'.jsonl')
    def lines(self,name):
        p=self.root/name;return [json.loads(x)for x in p.read_text().splitlines()] if p.exists() else []
    def test_new_then_explicit_resume_preserves_native_id_and_only_sends_new_input(self):
        p,events=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['context_checkpoint'],{'schema':1,'protocol':1,'session_id':FIRST,'estimated_tokens':119})
        ready=next(e for e in events if e['type']=='ready');self.assertEqual(ready['context_protocol'],1)
        second=self.request(resume=True);second['prompt']='A different new request.'
        p,events=self.run_worker(second);self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['context_checkpoint']['session_id'],FIRST)
        launches=self.lines('launches.jsonl');self.assertEqual([x['resume']for x in launches],[False,True])
        self.assertEqual(launches[0]['cwd'],launches[1]['cwd']);self.assertEqual(launches[0]['instructions'],launches[1]['instructions'])
        inputs=self.lines('inputs.jsonl');self.assertEqual(len(inputs),2)
        self.assertTrue(inputs[1]['message']['content'].startswith(second['prompt']))
        self.assertNotIn('Only the current request.',inputs[1]['message']['content'])
    def test_missing_native_transcript_fails_before_any_input_or_new_process(self):
        p,_=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        self.transcript().unlink()
        p,events=self.run_worker(self.request(resume=True));self.assertNotEqual(p.returncode,0)
        self.assertEqual(events,[{'type':'error','code':'context_unavailable','context_protocol':1,'context_pre_input':True,'message':'Saved provider context cannot be restored. No prompt was sent.'}])
        self.assertEqual(len(self.lines('launches.jsonl')),1);self.assertEqual(len(self.lines('inputs.jsonl')),1)
    def test_new_uuid_isolates_conversation_and_old_uuid_cannot_be_created_twice(self):
        for sid in [FIRST,SECOND]:
            p,_=self.run_worker(self.request(sid));self.assertEqual(p.returncode,0,p.stderr)
        p,events=self.run_worker(self.request());self.assertNotEqual(p.returncode,0)
        self.assertEqual(events[0]['code'],'context_unavailable')
        self.assertNotEqual(*[x['cwd']for x in self.lines('launches.jsonl')])
    def test_account_model_policy_and_tools_changes_refuse_resume_before_input(self):
        p,_=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        for kind in ['account','model','instructions','tools']:
            request=self.request(resume=True);account='a'*64
            if kind=='account':account='b'*64
            elif kind=='model':request['reasoning_effort']='low';request['model']='sonnet'
            elif kind=='instructions':request['instructions']='Changed authorization.'
            else:request['tools'][0]['description']='Changed tool policy.'
            p,events=self.run_worker(request,account)
            self.assertNotEqual(p.returncode,0)
            self.assertFalse(any(e['type']=='complete'for e in events))
        self.assertEqual(len(self.lines('inputs.jsonl')),1)
    def test_failed_turn_invalidates_previous_checkpoint(self):
        p,_=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        (self.root/'fault').write_text('failed')
        p,events=self.run_worker(self.request(resume=True));self.assertNotEqual(p.returncode,0)
        self.assertFalse((self.home/'contexts'/FIRST/'checkpoint.json').exists())
        (self.root/'fault').unlink()
        p,events=self.run_worker(self.request(resume=True));self.assertNotEqual(p.returncode,0)
        self.assertEqual(events[0]['code'],'context_unavailable');self.assertEqual(len(self.lines('inputs.jsonl')),2)
    def test_missing_checkpoint_or_bad_exit_does_not_fail_completed_work(self):
        for fault,sid in [('missing-transcript',FIRST),('bad-exit',SECOND),('missing-usage','33333333-3333-4333-8333-333333333333')]:
            (self.root/'fault').write_text(fault)
            p,events=self.run_worker(self.request(sid));self.assertEqual(p.returncode,0,p.stderr)
            self.assertEqual(events[-1]['output'],'Completed once.');self.assertNotIn('context_checkpoint',events[-1])
    def test_native_session_mismatch_fails_and_has_no_checkpoint(self):
        (self.root/'fault').write_text('wrong-id')
        p,events=self.run_worker(self.request());self.assertNotEqual(p.returncode,0)
        self.assertFalse(any(e['type']=='complete'for e in events));self.assertFalse((self.home/'contexts'/FIRST/'checkpoint.json').exists())
    def test_context_size_uses_latest_request_not_cumulative_usage(self):
        (self.root/'fault').write_text('two-usage')
        p,events=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['context_checkpoint']['estimated_tokens'],119)
        (self.root/'fault').write_text('latest-missing-usage')
        p,events=self.run_worker(self.request(SECOND));self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['type'],'complete');self.assertNotIn('context_checkpoint',events[-1])
    def test_result_only_followup_does_not_checkpoint_previous_turn_usage(self):
        (self.root/'fault').write_text('result-only-followup')
        request=self.request();request['followup_protocol']=1
        p,events=self.run_worker(request);self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(len([e for e in events if e['type']=='turn_complete']),2)
        self.assertEqual(events[-1]['type'],'complete');self.assertNotIn('context_checkpoint',events[-1])
    def test_unsafe_uuid_and_symlink_cannot_escape_provider_home(self):
        for identifier in ['../escape','LATEST',FIRST.upper(),'']:
            request=self.request(identifier)
            if identifier==FIRST.upper():request['context_resume']='true'
            p,events=self.run_worker(request);self.assertNotEqual(p.returncode,0);self.assertEqual(events[0]['code'],'context_unavailable')
        (self.home/'contexts').mkdir(exist_ok=True)
        (self.home/'contexts'/FIRST).symlink_to(self.root,target_is_directory=True)
        p,events=self.run_worker(self.request());self.assertNotEqual(p.returncode,0);self.assertEqual(events[0]['code'],'context_unavailable')
        self.assertEqual(self.lines('inputs.jsonl'),[])

    def test_capability_probe_needs_no_provider_account_or_home(self):
        result=subprocess.run([sys.executable,str(HELPER),'context-capabilities'],capture_output=True,text=True,check=True,timeout=3,
                              env={'PATH':'/usr/bin:/bin','HOME':str(self.root/'does-not-exist')})
        self.assertEqual(json.loads(result.stdout),{'context_protocol':1,'providers':['claude-code']})
        self.assertFalse((self.root/'does-not-exist').exists())

    def test_pruning_removes_only_old_generated_context_and_native_project(self):
        for sid in [FIRST,SECOND]:
            p,_=self.run_worker(self.request(sid));self.assertEqual(p.returncode,0,p.stderr)
        auth=self.home/'.claude/.credentials.json';auth.write_text('fixture credentials remain unchanged')
        old=self.home/'contexts'/FIRST
        os.utime(old/'started',(1,1))
        bridge.prune_claude_contexts(self.home/'contexts',self.home/'.claude',SECOND,maximum=2)
        self.assertFalse(old.exists());self.assertFalse(self.transcript(FIRST).parent.exists())
        self.assertTrue(self.transcript(SECOND).exists());self.assertEqual(auth.read_text(),'fixture credentials remain unchanged')

    def test_pruning_cannot_remove_an_active_context(self):
        p,_=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        env={'HOME':str(self.home),'CLAUDE_CONFIG_DIR':str(self.home/'.claude')}
        context=bridge.ClaudeContext(self.request(resume=True),env,'a'*64)
        try:
            with self.assertRaises(bridge.ContextUnavailable):
                bridge.prune_claude_contexts(self.home/'contexts',self.home/'.claude',SECOND,maximum=1,ttl=0)
            self.assertTrue(self.transcript().exists())
        finally:context.close()

    def test_busy_cache_admission_uses_safe_ephemeral_bootstrap_without_checkpoint(self):
        base=self.home/'contexts';base.mkdir()
        (self.root/'fault').write_text('allow-ephemeral')
        with bridge.context_admission(base):
            p,events=self.run_worker(self.request());self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['output'],'Completed once.');self.assertNotIn('context_checkpoint',events[-1])
        self.assertFalse(next(e for e in events if e['type']=='ready')['context_persistent'])
        self.assertFalse((base/FIRST).exists())

    def test_unknown_cache_account_bootstraps_ephemerally_but_resume_requires_reset(self):
        (self.root/'fault').write_text('allow-ephemeral')
        p,events=self.run_worker(self.request(),account='')
        self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(events[-1]['type'],'complete');self.assertNotIn('context_checkpoint',events[-1])
        self.assertFalse(next(e for e in events if e['type']=='ready')['context_persistent'])
        p,events=self.run_worker(self.request(resume=True),account='')
        self.assertNotEqual(p.returncode,0);self.assertTrue(events[0]['context_pre_input'])
        self.assertEqual(len(self.lines('inputs.jsonl')),1)

    def test_readonly_cache_uses_ephemeral_only_before_new_input(self):
        env={'HOME':str(self.home),'CLAUDE_CONFIG_DIR':str(self.home/'.claude')}
        with patch.object(bridge,'private_directory',side_effect=PermissionError('read-only fixture')):
            with bridge.claude_context('claude-code',self.request(),env,'a'*64) as context:
                self.assertFalse(context.persistent);self.assertEqual(context.identifier,FIRST)
                self.assertIsNone(context.checkpoint(100))
            with self.assertRaises(bridge.ContextUnavailable):
                with bridge.claude_context('claude-code',self.request(resume=True),env,'a'*64):pass

    def test_missing_provider_binary_is_not_hidden_by_cache_fallback(self):
        env={'HOME':str(self.home),'CLAUDE_CONFIG_DIR':str(self.home/'.claude')}
        self.binary.unlink()
        with self.assertRaisesRegex(RuntimeError,'official Claude runtime'):
            with bridge.claude_context('claude-code',self.request(),env,'a'*64):pass

if __name__=='__main__':unittest.main()
