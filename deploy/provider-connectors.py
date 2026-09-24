"""Claude account connectors through the official SDK control protocol.

The CLI owns authentication and execution. Only verified claudeai servers may
cross this bridge; Kindred owns the approval decision and durable activity.
"""
import asyncio
import hashlib
import json
import re

DENIED = 'This action was declined. Do not retry it through another connector or account.'

def account_key(data):
    if data.get('loggedIn') is not True or data.get('authMethod') != 'claude.ai' or not data.get('email') or not data.get('orgId'):
        raise RuntimeError('Claude account identity unavailable; reconnect Claude.')
    return hashlib.sha256(json.dumps([data['authMethod'], data['email'].lower(), data['orgId']], separators=(',', ':')).encode()).hexdigest()

def catalogue(data):
    rows = []
    for server in data.get('mcpServers', []):
        if server.get('scope') != 'claudeai' or server.get('config', {}).get('type') != 'claudeai-proxy':
            continue
        name = server.get('name', '')
        if not isinstance(name, str) or not name.startswith('claude.ai ') or len(name) > 200:
            continue
        identity = server.get('config', {}).get('id')
        if not isinstance(identity, str) or not identity:
            raise RuntimeError('Claude connector identity unavailable; refresh the connection in Claude.')
        prefix = 'mcp__' + re.sub(r'[^a-zA-Z0-9_-]', '_', name) + '__'
        tools = [prefix + t['name'] for t in server.get('tools', [])
                 if isinstance(t.get('name'), str) and re.fullmatch(r'[a-zA-Z0-9_-]{1,200}', t['name'])]
        rows.append({'name': name, 'display_name': name.removeprefix('claude.ai '),
                     'connector_key': hashlib.sha256(identity.encode()).hexdigest(),
                     'origin': 'claude-account', 'status': server.get('status', 'unavailable'),
                     'tools': tools if server.get('status') == 'connected' else []})
    return rows

async def write(stream, value):
    stream.write((json.dumps(value) + '\n').encode())
    await stream.drain()

async def control(child, identifier, request):
    await write(child.stdin, {'type': 'control_request', 'request_id': identifier, 'request': request})
    for _ in range(1000):
        line = await asyncio.wait_for(child.stdout.readline(), 25)
        if not line: raise RuntimeError('Claude connector discovery closed')
        frame = json.loads(line)
        response = frame.get('response', {})
        if frame.get('type') == 'control_response' and response.get('request_id') == identifier:
            if response.get('subtype') != 'success': raise RuntimeError('Claude connector discovery failed')
            return response.get('response', {})
    raise RuntimeError('Claude connector discovery limit')

async def initialize(child, hooks=True, settle=5):
    events = ('PreToolUse', 'PostToolUse', 'PostToolUseFailure')
    config = {event: [{'matcher': '.*', 'hookCallbackIds': [event], 'timeout': 86400}] for event in events}
    await control(child, 'initialize', {'subtype': 'initialize', 'hooks': config if hooks else None})
    # Account connectors arrive asynchronously after the SDK initialize reply.
    # No user turn or model request is sent during this discovery window.
    await asyncio.sleep(settle)
    for attempt in range(11):
        data = await control(child, 'connectors-' + str(attempt), {'subtype': 'mcp_status'})
        if not any(s.get('status') == 'pending' for s in data.get('mcpServers', [])):
            return catalogue(data)
        await asyncio.sleep(1)
    return catalogue(data)

class Bridge:
    def __init__(self, child, rows, kindred_tools, exchange, emit):
        self.child, self.exchange, self.emit = child, exchange, emit
        self.tools = {tool: row for row in rows for tool in row['tools']}
        self.kindred = {'mcp__kindred__' + n for n in kindred_tools}
        self.calls = {}

    async def handle(self, frame):
        request = frame['request']
        subtype = request.get('subtype')
        if subtype not in ('hook_callback', 'can_use_tool'):
            await write(self.child.stdin, {'type':'control_response','response':{
                'subtype':'error','request_id':frame['request_id'],'error':'Unsupported Claude connector interaction. Complete connector authentication in Claude.'}})
            return
        payload = request.get('input') or {}
        event = request.get('callback_id') if subtype == 'hook_callback' else 'permission'
        name = request.get('tool_name') if event == 'permission' else payload.get('tool_name')
        args = payload if event == 'permission' else payload.get('tool_input', {})
        identifier = request.get('tool_use_id') or payload.get('tool_use_id')
        result = {}
        allowed = False
        denial = DENIED
        if name in self.kindred or name == 'ToolSearch':
            allowed = True  # Kindred MCP itself performs its existing approval.
        elif name in self.tools and isinstance(identifier, str) and len(identifier) <= 512:
            if event == 'PreToolUse':
                if identifier in self.calls: raise RuntimeError('Duplicate Claude connector call')
                self.calls[identifier] = {'name': name, 'args': args, 'finished': False}
                response = await self.exchange(identifier, name, args, False)
                allowed = response.get('approved') is True
                denial = response.get('message') or DENIED
                if allowed:
                    updated = response.get('updated_input', args)
                    if not isinstance(updated, dict): raise RuntimeError('Invalid reviewed connector input')
                    args = updated
                    self.calls[identifier]['args'] = args
                if not allowed: self.finish(identifier, denial, True)
            elif event == 'permission':
                call = self.calls.get(identifier)
                if call and not call['finished'] and call['name'] == name and call['args'] == args:
                    # A callback after our allow hook can carry an organization
                    # or connector requirement. Always present it to a person.
                    response = await self.exchange(identifier, name, args, True)
                    allowed = response.get('approved') is True
                    denial = response.get('message') or DENIED
                    if allowed:
                        updated = response.get('updated_input', args)
                        if not isinstance(updated, dict): raise RuntimeError('Invalid reviewed connector input')
                        args = updated
                        self.calls[identifier]['args'] = args
                    if not allowed: self.finish(identifier, denial, True)
            elif event in ('PostToolUse', 'PostToolUseFailure'):
                call = self.calls.get(identifier)
                if call and not call['finished'] and call['name'] == name:
                    output = payload.get('tool_response') if event == 'PostToolUse' else payload.get('error', 'Connector failed')
                    parsed = output
                    if isinstance(parsed, str):
                        try: parsed = json.loads(parsed)
                        except (ValueError, TypeError): pass
                    failed = event == 'PostToolUseFailure' or isinstance(parsed, dict) and parsed.get('isError') is True
                    text = output if isinstance(output, str) else json.dumps(output, ensure_ascii=True)
                    self.finish(identifier, text[:200000], failed)
                allowed = True
        if event == 'permission':
            result = {'behavior': 'allow', 'updatedInput': args} if allowed else {'behavior': 'deny', 'message': denial}
        elif event == 'PreToolUse':
            result = {'hookSpecificOutput': {'hookEventName': 'PreToolUse',
                      'permissionDecision': 'allow' if allowed else 'deny',
                      'permissionDecisionReason': 'Kindred approval bridge' if allowed else denial}}
            if allowed and name in self.tools: result['hookSpecificOutput']['updatedInput'] = args
        await write(self.child.stdin, {'type': 'control_response', 'response': {
            'subtype': 'success', 'request_id': frame['request_id'], 'response': result}})

    def finish(self, identifier, text, failed):
        self.calls[identifier]['finished'] = True
        self.emit({'type': 'connector_result', 'id': identifier, 'text': text, 'failed': failed})
