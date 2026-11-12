// Offline acceptance gate for a managed SDK update. No real model, credentials,
// workspace, or external actions are used. Import-only checks miss API drift.
import test from 'node:test';
import assert from 'node:assert/strict';
import { runSession } from './session.mjs';

const tool = { name: 'remember', description: 'Fixture action.', inputSchema: {
  type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false,
} };
const request = () => ({ protocol: 1, provider: 'custom-11111111-1111-4111-8111-111111111111', model: 'fixture',
  model_info: { id: 'fixture', context_window: 128000, max_tokens: 4096, reasoning: false, no_auth: true },
  endpoint: 'http://127.0.0.1:1/v1/chat/completions', api_key: 'unused-fixture',
  instructions: 'Use only the supplied fixture tools.', prompt: 'Save a fact and report.',
  tools: [tool], max_steps: 4, reasoning_effort: '',
});
const call = id => ({ index: 0, id, type: 'function', function: { name: 'remember', arguments: '{"text":"fixture"}' } });
function response(delta, finish = 'stop') {
  const chunk = (delta, reason) => ({ id: 'fixture', object: 'chat.completion.chunk', created: 1,
    model: 'fixture', choices: [{ index: 0, delta, finish_reason: reason }] });
  return new Response(`data: ${JSON.stringify(chunk(delta, null))}\n\ndata: ${JSON.stringify(chunk({}, finish))}\n\ndata: [DONE]\n\n`,
    { headers: { 'Content-Type': 'text/event-stream' } });
}

test('SDK completes a paired action and answer once', async () => {
  let requests = 0, actions = 0;
  const result = await runSession(request(), async () => { actions++; return { text: 'Saved.' }; }, () => {}, undefined, {
    fetch: async () => ++requests === 1
      ? response({ role: 'assistant', tool_calls: [call('saved')] }, 'tool_calls')
      : response({ role: 'assistant', content: 'Saved.' }),
  });
  assert.equal(requests, 2); assert.equal(actions, 1); assert.equal(result.output, 'Saved.');
});

test('SDK stops at truncation without executing partial tools or continuing work', async () => {
  let requests = 0, actions = 0;
  await assert.rejects(runSession(request(), async () => { actions++; return { text: 'Saved.' }; }, () => {}, undefined, {
    fetch: async () => ++requests === 1
      ? response({ role: 'assistant', tool_calls: [call('saved')] }, 'tool_calls')
      : response({ role: 'assistant', content: 'Partial', tool_calls: [call('truncated')] }, 'length'),
  }), error => error.code === 'custom_truncated');
  assert.equal(requests, 2); assert.equal(actions, 1);
});

test('SDK performs only one tools-disabled summary for an empty result', async () => {
  let requests = 0, actions = 0;
  const result = await runSession(request(), async () => { actions++; return { text: 'Unexpected.' }; }, () => {}, undefined, {
    fetch: async req => {
      requests++;
      if (requests === 1) return response({ role: 'assistant', content: '' });
      assert.equal((await req.json()).tools?.length || 0, 0);
      return response({ role: 'assistant', content: 'Nothing changed.' });
    },
  });
  assert.equal(requests, 2); assert.equal(actions, 0); assert.equal(result.output, 'Nothing changed.');
});

test('SDK preserves checkpoint context and tool pairing across process boundaries', async () => {
  const events = [], bodies = [];
  let actions = 0;
  await runSession({ ...request(), context_protocol: 1 }, async () => {
    actions++; return { text: 'Saved fixture fact.', failed: false };
  }, e => events.push(e), undefined, { fetch: async req => {
    bodies.push(await req.json());
    return bodies.length === 1
      ? response({ role: 'assistant', tool_calls: [call('saved')] }, 'tool_calls')
      : response({ role: 'assistant', content: 'Saved.' });
  } });
  const frame = events.find(e => e.type === 'checkpoint');
  assert.equal(frame?.state, 'ready', 'SDK must retain a valid durable checkpoint');
  const restored = [];
  await runSession({ ...request(), context_protocol: 1, context_checkpoint: frame.checkpoint, prompt: 'Recall the fact.' },
    async () => { throw Error('An old action must not replay'); }, () => {}, undefined, { fetch: async req => {
      const body = await req.json(); restored.push(body);
      return response({ role: 'assistant', content: 'The fixture fact was saved.' });
    } });
  assert.equal(actions, 1); assert.equal(restored.length, 1);
  assert.deepEqual(restored[0].messages.slice(0, bodies[1].messages.length), bodies[1].messages);
  assert.equal(restored[0].messages.filter(m => m.role === 'tool').length, 1);
});
