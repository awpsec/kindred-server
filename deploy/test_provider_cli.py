"""Protocol integration fixtures: no subscription, network or model charge."""
import json
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name('provider-cli.py').resolve()
spec=importlib.util.spec_from_file_location('bridge', HELPER)
bridge=importlib.util.module_from_spec(spec);spec.loader.exec_module(bridge)
LAUNCH = r'''
import asyncio, importlib.util, json, os, sys
spec=importlib.util.spec_from_file_location('bridge',sys.argv[1]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.BIN[sys.argv[3]]=sys.argv[2]
m.account=lambda *args: {'connected':True,'account_key':'a'*64,'data':[{'model':v,'supportedReasoningEfforts':[{'reasoningEffort':e} for e in m.EFFORTS]} for v in ('sonnet','claude-sonnet-5')]}
original=m.subprocess.run
def fixture(*args,**kwargs):
    if 'kimi-config' in args[0]: return subprocess_result()
    return original(*args,**kwargs)
class subprocess_result: returncode=0
m.subprocess.run=fixture
load=m.connector_module
def connectors():
    module=load();original=module.initialize
    async def initialize(child,*args,**kwargs): return await original(child,settle=0)
    module.initialize=initialize
    return module
m.connector_module=connectors
asyncio.run(m.worker(sys.argv[3]))
'''
FAKE = r'''#!/usr/bin/python3
import json,sys,subprocess
def emit(v): print(json.dumps(v),flush=True)
def read(): return json.loads(sys.stdin.readline())
if '--wire' in sys.argv:
    i=read(); assert i['method']=='initialize'
    names=[t['name'] for t in i['params']['external_tools']]
    emit({'jsonrpc':'2.0','id':'init','result':{'protocol_version':'1.10','external_tools':{'accepted':names,'rejected':[]}}})
    p=read(); assert p['method']=='prompt'
    emit({'method':'event','params':{'type':'TextPart','payload':{'type':'text','text':'Work'}}})
    emit({'method':'event','params':{'type':'TextPart','payload':{'type':'text','text':'ing. '}}})
    emit({'method':'request','id':'call-1','params':{'type':'ToolCallRequest','payload':{'id':'call-1','name':'remember','arguments':'{"text":"hello"}'}}})
    result=read(); assert result['result']['return_value']['is_error'] is True
    emit({'method':'event','params':{'type':'StatusUpdate','payload':{'token_usage':{'input_other':10,'input_cache_read':2,'output':4}}}})
    emit({'method':'event','params':{'type':'TextPart','payload':{'type':'text','text':'Declined.'}}})
    emit({'jsonrpc':'2.0','id':'prompt','result':{'status':'finished'}})
else:
    if '--model' not in sys.argv:
        packet=read();assert packet['request']['subtype']=='initialize'
        emit({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{'models':[{'value':'sonnet','resolvedModel':'claude-sonnet-5','displayName':'Sonnet','supportsEffort':True,'supportedEffortLevels':['low','max']}]}}})
        assert not sys.stdin.readline(), 'Catalogue must not send a user turn'
        sys.exit()
    model=sys.argv[sys.argv.index('--model')+1]
    if model=='claude-sonnet-5': assert sys.argv[sys.argv.index('--effort')+1]=='max'
    else: assert '--effort' not in sys.argv
    assert sys.argv[sys.argv.index('--tools')+1]=='ToolSearch'
    assert '--restricted' in sys.argv and '--strict-mcp-config' not in sys.argv
    assert sys.argv[sys.argv.index('--permission-prompt-tool')+1]=='stdio'
    packet=read();assert packet['request']['subtype']=='initialize'
    assert set(packet['request']['hooks'])=={'PreToolUse','PostToolUse','PostToolUseFailure'}
    emit({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{}}})
    packet=read();assert packet['request']['subtype']=='mcp_status'
    emit({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{'mcpServers':[]}}})
    packet=read();assert packet['type']=='user' and packet['message']['content'].startswith('Remember hello.\n\nKindred saved connector configuration')
    assert '"preferred_source": "kindred"' in packet['message']['content']
    assert '--system-prompt' not in sys.argv
    assert open(sys.argv[sys.argv.index('--system-prompt-file')+1]).read().startswith('Use only supplied tools.')
    config=json.load(open(sys.argv[sys.argv.index('--mcp-config')+1]))['mcpServers']
    assert list(config)==['kindred']
    c=config['kindred'];child=subprocess.Popen([c['command'],*c['args']],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    def rpc(v): child.stdin.write(json.dumps(v)+'\n');child.stdin.flush();return json.loads(child.stdout.readline())
    assert rpc({'id':1,'method':'initialize','params':{'protocolVersion':'2025-03-26'}})['result']['capabilities']=={'tools':{}}
    names=[t['name'] for t in rpc({'id':2,'method':'tools/list'})['result']['tools']]
    assert 'remember' in names
    assert set('mcp__kindred__'+n for n in names)==set(sys.argv[sys.argv.index('--allowedTools')+1].split(','))
    emit({'type':'system','subtype':'init','mcp_servers':[{'name':'kindred','status':'connected'}],'tools':['mcp__kindred__'+n for n in names]})
    result=rpc({'id':3,'method':'tools/call','params':{'name':'remember','arguments':{'text':'hello'}}})
    assert result['result']['isError'] is True and result['result']['content'][0]['text']=='Declined'
    emit({'type':'assistant','message':{'id':'msg-1','usage':{'input_tokens':10,'output_tokens':4},'content':[{'type':'text','text':'Declined.'}]}})
    emit({'type':'result','subtype':'success','is_error':False})
    child.terminate();child.wait()
'''

class Bridge(unittest.TestCase):
    def test_account_inheritance_rejects_local_mcp_configuration_before_startup(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'.claude.json';env={'CLAUDE_CONFIG_DIR':d}
            bridge.verify_claude_connector_home(env)
            for data in [{'mcpServers':{'local':{'command':'unexpected'}}}, {'projects':{'/tmp':{'mcpServers':{'local':{'command':'unexpected'}}}}}]:
                path.write_text(json.dumps(data))
                with self.assertRaises(bridge.ToolBridgeError): bridge.verify_claude_connector_home(env)
            path.write_text(json.dumps({'projects':{'/tmp':{'mcpServers':{}}}}))
            bridge.verify_claude_connector_home(env)
    def test_claude_worker_allows_cleanup_after_its_result(self):
        self.exercise('claude-code',cleanup=True)
    def test_disconnected_claude_cannot_produce_a_fake_reply_or_execute_tools(self):
        self.exercise('claude-code',bad_bridge=True)
    def test_claude_rejects_missing_failed_or_incomplete_tool_bridge(self):
        spec=importlib.util.spec_from_file_location('bridge_test',HELPER);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        good={'mcp_servers':[{'name':'kindred','status':'connected'}],'tools':['mcp__kindred__remember']}
        module.validate_claude_tools(good,{'remember':{}})
        for packet in [{},dict(good,tools=[]),dict(good,mcp_servers=[{'name':'kindred','status':'failed'}])]:
            with self.assertRaises(module.ToolBridgeError):module.validate_claude_tools(packet,{'remember':{}})

    def exercise(self,provider,model=None,effort='',error_packet=None,bad_bridge=False,cleanup=False,tool_count=1,max_steps=4):
        with tempfile.TemporaryDirectory() as d:
            binary=Path(d)/'fixture';fixture=FAKE.replace("'name':'kindred','status':'connected'","'name':'kindred','status':'failed'") if bad_bridge else FAKE;fixture=fixture.replace('    child.terminate();child.wait()',"    child.terminate();child.wait()\n    import time;time.sleep(.15)\n    from pathlib import Path\n    Path(__file__).with_name('worker-cleaned').write_text('done')") if cleanup else fixture;binary.write_text(fixture if error_packet is None else FAKE.replace("    emit({'type':'result','subtype':'success','is_error':False})", "    emit("+repr(error_packet)+")\n    emit({'type':'result','subtype':'error_during_execution','is_error':True})"));binary.chmod(0o755)
            child=subprocess.Popen([sys.executable,'-c',LAUNCH,str(HELPER),str(binary),provider],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            start={'protocol':1,'provider':provider,'model':model or ('sonnet' if provider=='claude-code' else 'kimi'),'reasoning_effort':effort,'instructions':'Use only supplied tools.','prompt':'Remember hello.','max_steps':max_steps,
                   'tools':[{'name':'remember','description':'Remember text','inputSchema':{'type':'object','properties':{'text':{'type':'string'}},'required':['text']}}]}
            start['tools'] += [dict(start['tools'][0],name='extra_'+str(i)) for i in range(tool_count-1)]
            child.stdin.write(json.dumps(start)+'\n');child.stdin.flush()
            events=[]
            for _ in range(20):
                line=child.stdout.readline()
                if not line: break
                frame=json.loads(line);events.append(frame)
                if frame['type']=='connector_catalogue':
                    self.assertEqual(frame['account_key'],'a'*64)
                    child.stdin.write(json.dumps({'type':'tool_result','id':'connector-context','result':{'preferred_source':'kindred'}})+'\n');child.stdin.flush()
                if frame['type']=='tool_call':
                    self.assertEqual(frame['args'],{'text':'hello'})
                    child.stdin.write(json.dumps({'type':'tool_result','id':frame['id'],'result':{'text':'Declined','failed':True}})+'\n');child.stdin.flush()
                if frame['type']=='complete': break
            child.stdin.close();child.wait(timeout=10)
            errors=child.stderr.read();child.stdout.close();child.stderr.close()
            if cleanup:self.assertEqual((Path(d)/'worker-cleaned').read_text(),'done')
            if bad_bridge:
                self.assertNotEqual(child.returncode,0)
                self.assertFalse(any(e['type'] in ('ready','assistant','tool_call','complete') for e in events))
                self.assertIn('ToolBridgeError',errors);return
            if error_packet is not None:
                self.assertNotEqual(child.returncode,0)
                replies=[e for e in events if e['type']=='assistant']
                self.assertEqual(replies[0]['text'],'Declined.');self.assertFalse(replies[0]['is_error'])
                self.assertEqual(replies[-1]['text'],'Diagnostic only');self.assertTrue(replies[-1]['is_error'])
                self.assertFalse(any(e['type']=='complete' for e in events));return
            self.assertEqual(child.returncode,0,errors)
            self.assertEqual(sum(e['type']=='tool_call' for e in events),1)
            self.assertEqual(events[-1]['type'],'complete',events)
            self.assertEqual(events[-1]['output'],'Declined.' if provider=='claude-code' else 'Working.\nDeclined.')
            self.assertEqual(sum(e['type']=='assistant' for e in events),1 if provider=='claude-code' else 2)
            receipt=next(e for e in events if e['type']=='usage');self.assertEqual(receipt['cost_source'],'subscription')
            self.assertEqual(receipt['output_tokens'],4)
            self.assertEqual(next(e for e in events if e['type']=='ready').get('reasoning_effort',''),effort)
    def test_claude_official_mcp_transport_preserves_denial(self): self.exercise('claude-code')
    def test_claude_pinned_model_and_max_effort_reach_cli(self): self.exercise('claude-code','claude-sonnet-5','max')
    def test_large_tool_catalogue_with_unlimited_calls(self):
        for provider in ['claude-code','kimi-code']:
            with self.subTest(provider=provider): self.exercise(provider,tool_count=80,max_steps=0)

    def test_kimi_wire_protocol_preserves_text_and_denial(self): self.exercise('kimi-code')

    def test_claude_diagnostics_keep_explicit_error_provenance(self):
        for marker in [{'error':'authentication_failed'},{'is_error':True}]:
            with self.subTest(marker=marker):
                self.exercise('claude-code',error_packet={'type':'assistant',**marker,'message':{'content':[{'type':'text','text':'Diagnostic only'}]}})

    def test_catalogue_uses_control_request_without_user_turn(self):
        with tempfile.TemporaryDirectory() as d:
            binary=Path(d)/'fixture';binary.write_text(FAKE);binary.chmod(0o755)
            original=bridge.BIN['claude-code'];bridge.BIN['claude-code']=str(binary)
            try: rows=bridge.claude_models({'HOME':d,'PATH':'/usr/bin:/bin'})
            finally: bridge.BIN['claude-code']=original
            self.assertEqual([r['model'] for r in rows],['sonnet','claude-sonnet-5'])
            self.assertEqual([e['reasoningEffort'] for e in rows[0]['supportedReasoningEfforts']],['low','max'])

    def test_catalogue_preserves_context_and_rejects_unknown_choices(self):
        rows=bridge.normalize_claude_models([
            {'value':'opus[1m]','resolvedModel':'claude-opus-5[1m]','displayName':'Opus','supportsEffort':True,'supportedEffortLevels':['low','high','invented']},
            {'value':'haiku','resolvedModel':'claude-haiku-4-5-20251001','displayName':'Haiku'},
            {'value':'claude-fable-5-1[1m]','resolvedModel':'claude-fable-5-1','displayName':'Fable','description':'Requires usage credits'},
        ])
        self.assertEqual(len(rows),len(set(r['model'] for r in rows)))
        visible=[r for r in rows if not r['advanced']]
        self.assertEqual([r['model'] for r in visible],['opus[1m]','haiku','claude-fable-5-1[1m]'])
        self.assertFalse(any('pinned' in r['displayName'] or 'current' in r['displayName'] for r in visible))
        self.assertTrue(next(r for r in rows if r['model']=='claude-opus-5[1m]')['advanced'])
        self.assertEqual(next(r for r in rows if r['model']=='claude-fable-5-1[1m]')['selectionKind'],'fixed')
        self.assertNotIn('claude-fable-5-1',[r['model'] for r in rows])
        self.assertIn('Requires usage credits',next(r for r in rows if r['model']=='claude-fable-5-1[1m]')['description'])
        bridge.claude_selection(rows,'opus','high')
        bridge.claude_selection(rows,'haiku','')
        for model,effort in [('haiku','high'),('opus','max'),('unknown',''),('opus','ultracode')]:
            with self.assertRaises(bridge.SelectionError): bridge.claude_selection(rows,model,effort)

    def test_catalogue_allows_inflight_auth_cleanup_before_returning(self):
        with tempfile.TemporaryDirectory() as d:
            binary=Path(d)/'fixture'
            binary.write_text('''#!/usr/bin/python3
import json,os,sys,time
from pathlib import Path
root=Path(os.environ['HOME']);lock=root/'refresh.lock';lock.mkdir()
packet=json.loads(sys.stdin.readline())
print(json.dumps({'type':'control_response','response':{'request_id':packet['request_id'],'subtype':'success','response':{'models':[{'value':'haiku','displayName':'Haiku'}]}}}),flush=True)
assert sys.stdin.read() == '', 'No paid user turn may be sent'
time.sleep(.15)
lock.rmdir()
(root/'finished').write_text('graceful')
''')
            binary.chmod(0o755)
            original=bridge.BIN['claude-code'];bridge.BIN['claude-code']=str(binary)
            try: self.assertEqual(bridge.claude_models({'HOME':d,'PATH':'/usr/bin:/bin'})[0]['model'],'haiku')
            finally: bridge.BIN['claude-code']=original
            self.assertEqual((Path(d)/'finished').read_text(),'graceful')
            self.assertFalse((Path(d)/'refresh.lock').exists())

if __name__=='__main__': unittest.main()
