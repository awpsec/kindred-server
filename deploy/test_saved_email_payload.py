"""Check backend-produced saved email fixtures through the actual Claude bridge.

Run after email_body_saved_review_has_exact_native_payload_once with
KINDRED_EMAIL_PAYLOAD_DIR set to a private disposable output directory.
This does not call a provider or send email.
"""
import asyncio
import importlib.util
import json
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location('connectors', Path(__file__).with_name('provider-connectors.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Sink:
    def __init__(self): self.frames = []
    def write(self, value): self.frames.append(json.loads(value))
    async def drain(self): pass

async def verify(directory):
    for suffix in ['send_email', 'send_message']:
        name = 'mcp__claude_ai_Gmail__' + suffix
        saved = json.loads((directory / (name + '.json')).read_text())
        sink = Sink()
        child = type('Child', (), {'stdin': sink})()
        calls = []
        async def exchange(identifier, tool, args, forced):
            calls.append((args, forced))
            return {'approved': True, 'updated_input': saved['approved_input']}
        bridge = m.Bridge(child, [{'tools': [name]}], [], exchange, lambda event: None)
        await bridge.handle({'request_id': 'synthetic-pretool', 'request': {
            'subtype': 'hook_callback', 'callback_id': 'PreToolUse', 'input': {
                'tool_name': name, 'tool_use_id': 'synthetic-call', 'tool_input': saved['original']}}})
        outgoing = sink.frames[-1]['response']['response']['hookSpecificOutput']['updatedInput']
        assert outgoing == saved['approved_input']
        assert outgoing['body'] == 'Keep this.\nSignature retained'
        assert outgoing['htmlBody'] == '<p>Keep this.<br>Signature retained</p>'
        assert 'DELETE SYNTHETIC SPAN' not in json.dumps(outgoing)
        assert saved['html_only_fields']['body']['format'] == 'html'
        for key in ['to', 'cc', 'bcc', 'subject', 'replyThreadId', 'attachments']:
            assert outgoing[key] == saved['original'][key]
        await bridge.handle({'request_id': 'synthetic-forced', 'request': {
            'subtype': 'can_use_tool', 'tool_use_id': 'synthetic-call', 'tool_name': name,
            'input': saved['original']}})
        assert sink.frames[-1]['response']['response']['behavior'] == 'deny', 'Stale permission payload must refuse'
        assert len(calls) == 1, 'Do not review an unbound stale payload'
        await bridge.handle({'request_id': 'synthetic-current', 'request': {
            'subtype': 'can_use_tool', 'tool_use_id': 'synthetic-call', 'tool_name': name,
            'input': outgoing}})
        assert calls[-1] == (outgoing, True), 'Forced review must use the saved input'
        assert sink.frames[-1]['response']['response']['updatedInput'] == outgoing
        print(name + ': exact saved body/HTML serialized; unrelated fields and forced review preserved')

if __name__ == '__main__':
    asyncio.run(verify(Path(sys.argv[1])))
