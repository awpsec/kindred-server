import importlib.util
import json
from pathlib import Path
import unittest

spec=importlib.util.spec_from_file_location('connectors',Path(__file__).with_name('provider-connectors.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
NAME='mcp__claude_ai_Atlassian__search'
ROWS=[{'name':'claude.ai Atlassian','origin':'claude-account','tools':[NAME]}]

class Stream:
    def __init__(self): self.frames=[]
    def write(self,value): self.frames.append(json.loads(value))
    async def drain(self): pass
class Child:
    def __init__(self): self.stdin=Stream()

class Connectors(unittest.IsolatedAsyncioTestCase):
    async def exercise(self,approve=True,force_approve=True):
        child=Child();events=[];calls=[]
        async def exchange(identifier,name,args,force):
            calls.append((identifier,name,args,force));return {'approved':force_approve if force else approve}
        bridge=m.Bridge(child,ROWS,['remember'],exchange,events.append)
        return bridge,child,events,calls
    def packet(self,event,name=NAME,args=None):
        return {'type':'control_request','request_id':event,'request':{'subtype':'hook_callback','callback_id':event,
            'tool_use_id':'call1','input':{'tool_name':name,'tool_input':args or {'query':'verification'},'tool_response':{'content':[{'type':'text','text':'No results'}]}}}}
    async def test_allow_then_success_has_actual_receipt(self):
        bridge,child,events,calls=await self.exercise()
        await bridge.handle(self.packet('PreToolUse'))
        self.assertEqual(len(calls),1);self.assertEqual(events,[])
        await bridge.handle(self.packet('PostToolUse'))
        self.assertEqual(events[0]['type'],'connector_result');self.assertFalse(events[0]['failed'])
        self.assertIn('No results',events[0]['text'])
    async def test_denial_cannot_be_reported_as_success(self):
        bridge,child,events,calls=await self.exercise(approve=False)
        await bridge.handle(self.packet('PreToolUse'))
        self.assertEqual(child.stdin.frames[-1]['response']['response']['hookSpecificOutput']['permissionDecision'],'deny')
        await bridge.handle(self.packet('PostToolUse'))
        self.assertEqual(len(events),1);self.assertTrue(events[0]['failed'])
    async def test_organization_prompt_is_forced_even_after_allow(self):
        bridge,child,events,calls=await self.exercise(force_approve=False)
        await bridge.handle(self.packet('PreToolUse'))
        await bridge.handle({'request_id':'org','request':{'subtype':'can_use_tool','tool_use_id':'call1','tool_name':NAME,'input':{'query':'verification'}}})
        self.assertTrue(calls[-1][-1]);self.assertEqual(child.stdin.frames[-1]['response']['response']['behavior'],'deny')
        self.assertTrue(events[-1]['failed'])
    async def test_unknown_local_server_and_builtin_are_denied(self):
        for name in ['Bash','Read','mcp__local__search','mcp__claude_ai_Forged__search']:
            bridge,child,events,calls=await self.exercise()
            await bridge.handle(self.packet('PreToolUse',name))
            self.assertEqual(calls,[])
            self.assertEqual(child.stdin.frames[-1]['response']['response']['hookSpecificOutput']['permissionDecision'],'deny')
    async def test_kindred_tools_keep_their_existing_execution_bridge(self):
        bridge,child,events,calls=await self.exercise()
        await bridge.handle(self.packet('PreToolUse','mcp__kindred__remember'))
        self.assertEqual(calls,[]);self.assertEqual(events,[])
        self.assertEqual(child.stdin.frames[-1]['response']['response']['hookSpecificOutput']['permissionDecision'],'allow')
    async def test_failure_and_duplicate_are_not_silently_accepted(self):
        bridge,child,events,calls=await self.exercise()
        await bridge.handle(self.packet('PreToolUse'))
        with self.assertRaises(RuntimeError): await bridge.handle(self.packet('PreToolUse'))
        await bridge.handle(self.packet('PostToolUseFailure'))
        self.assertTrue(events[-1]['failed'])
    async def test_serialized_mcp_error_preserves_failure_and_readable_text(self):
        bridge,child,events,calls=await self.exercise()
        await bridge.handle(self.packet('PreToolUse'))
        packet=self.packet('PostToolUse');packet['request']['input']['tool_response']='{"isError":true,"content":[{"type":"text","text":"Expired connection"}]}'
        await bridge.handle(packet)
        self.assertTrue(events[-1]['failed']);self.assertEqual(json.loads(events[-1]['text'])['isError'],True)
    async def test_edited_input_replaces_original_and_forced_review_uses_saved_edits(self):
        child=Child();events=[];calls=[]
        edited={'query':'edited query','preserved':'unchanged'}
        async def exchange(identifier,name,args,force):
            calls.append((args,force));return {'approved':True,'updated_input':edited}
        bridge=m.Bridge(child,ROWS,[],exchange,events.append)
        await bridge.handle(self.packet('PreToolUse',args={'query':'original','preserved':'unchanged'}))
        self.assertEqual(child.stdin.frames[-1]['response']['response']['hookSpecificOutput']['updatedInput'],edited)
        await bridge.handle({'request_id':'org','request':{'subtype':'can_use_tool','tool_use_id':'call1','tool_name':NAME,'input':edited}})
        self.assertEqual(calls[-1],(edited,True));self.assertEqual(child.stdin.frames[-1]['response']['response']['updatedInput'],edited)
    async def test_changes_feedback_reaches_claude_and_has_no_success_receipt(self):
        child=Child();events=[]
        async def exchange(*args):return {'approved':False,'message':'Make this shorter before sending.'}
        bridge=m.Bridge(child,ROWS,[],exchange,events.append)
        await bridge.handle(self.packet('PreToolUse'))
        self.assertEqual(child.stdin.frames[-1]['response']['response']['hookSpecificOutput']['permissionDecisionReason'],'Make this shorter before sending.')
        self.assertTrue(events[-1]['failed']);self.assertEqual(events[-1]['text'],'Make this shorter before sending.')
        await bridge.handle(self.packet('PostToolUse'));self.assertEqual(len(events),1)
    def test_catalogue_excludes_local_and_disconnected_tools_and_credentials(self):
        base={'name':'claude.ai Atlassian','scope':'claudeai','status':'connected','config':{'type':'claudeai-proxy','id':'atlassian-account-1','url':'SECRET'},'tools':[{'name':'search'}]}
        rows=m.catalogue({'mcpServers':[base,dict(base,scope='local'),dict(base,name='claude.ai Gmail',status='needs-auth')]})
        self.assertEqual(len(rows),2);self.assertEqual(rows[0]['tools'],[NAME]);self.assertEqual(rows[1]['tools'],[])
        self.assertNotIn('SECRET',json.dumps(rows))
    def test_account_identity_is_stable_private_and_separate(self):
        data={'loggedIn':True,'authMethod':'claude.ai','email':'Person@example.com','orgId':'org-a'}
        key=m.account_key(data)
        self.assertEqual(len(key),64);self.assertNotIn('Person',key)
        self.assertEqual(key,m.account_key(dict(data,email='person@example.com')))
        self.assertNotEqual(key,m.account_key(dict(data,orgId='org-b')))
        self.assertNotEqual(key,m.account_key(dict(data,email='another@example.com')))
        for field in ['email','orgId']:
            with self.assertRaises(RuntimeError): m.account_key(dict(data,**{field:''}))
    def test_missing_connection_identity_cannot_inherit_an_old_grant(self):
        server={'name':'claude.ai Gmail','scope':'claudeai','status':'connected','config':{'type':'claudeai-proxy'},'tools':[]}
        with self.assertRaises(RuntimeError):m.catalogue({'mcpServers':[server]})

if __name__=='__main__': unittest.main()
