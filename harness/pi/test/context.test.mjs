import test from 'node:test';
import assert from 'node:assert/strict';
import { runSession } from '../session.mjs';
import { MAX_CHECKPOINT_BYTES, MAX_CHECKPOINT_MESSAGES } from '../context.mjs';

const tool = { name: 'remember', description: 'Remember a fact.', inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false } };
const request = (overrides = {}) => ({ protocol: 1, context_protocol: 1, provider: 'openrouter', model: 'test/model',
  model_info: { id: 'test/model', reasoning: true, vision: true, context_window: 128000, max_tokens: 4096 },
  api_key: 'private-fixture-key', instructions: 'Standing instructions.', prompt: 'Remember the work.', tools: [tool], max_steps: 8, ...overrides });
function sse(delta, reason = 'stop') {
  if (delta.tool_calls) delta.tool_calls = delta.tool_calls.map((call, index) => ({ index, ...call }));
  const chunk = (delta, finish_reason) => ({ id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'test/model', choices: [{ index: 0, delta, finish_reason }] });
  return new Response(`data: ${JSON.stringify(chunk(delta, null))}\n\ndata: ${JSON.stringify(chunk({}, reason))}\n\ndata: [DONE]\n\n`, { headers: { 'Content-Type': 'text/event-stream' } });
}
const answer = text => sse({ role: 'assistant', content: text });
const action = (id = 'call1') => sse({ role: 'assistant', tool_calls: [{ id, type: 'function', function: { name: 'remember', arguments: '{"text":"A fact"}' } }] }, 'tool_calls');
async function simple(overrides = {}, text = 'Done.') {
  const events = [], bodies = [];
  await runSession(request(overrides), async () => { throw Error('Unexpected replay'); }, event => events.push(event), undefined,
    { fetch: async req => { bodies.push(await req.json()); return answer(text); } });
  return { events, bodies, checkpoint: events.find(event => event.type === 'checkpoint')?.checkpoint };
}

test('actual SDK restores exact prior provider prefix across processes without replaying tools', async () => {
  const events = [], bodies = []; let actions = 0;
  await runSession(request(), async () => { actions++; return { text: 'Persisted result.', failed: false }; }, event => events.push(event), undefined,
    { fetch: async req => { bodies.push(await req.json()); return bodies.length === 1 ? action() : answer('Saved.'); } });
  const frame = events.find(e => e.type === 'checkpoint');
  assert.equal(frame.state, 'ready'); assert.equal(frame.input_id, 'initial'); assert(frame.checkpoint.estimated_tokens > 0);
  const second = await simple({ context_checkpoint: frame.checkpoint, live_context: 'Current work: reviewing.', prompt: 'What happened?' }, 'The fact was saved.');
  const firstPrefix = bodies[1].messages;
  assert.deepEqual(second.bodies[0].messages.slice(0, firstPrefix.length), firstPrefix);
  assert.deepEqual(second.bodies[0].messages.at(-1).content, [{ type: 'text', text: 'Current work: reviewing.\n\nWhat happened?' }]);
  assert.equal(second.bodies[0].messages.filter(m => m.role === 'tool').length, 1);
  assert.equal(actions, 1);
  assert.equal(second.checkpoint.messages.filter(m => m.role === 'toolResult').length, 1);
  assert.equal(second.checkpoint.messages.filter(m => m.role === 'user').length, 2);
});

test('hosted and custom instructions remain byte-stable across temporary session directories', async () => {
  for (const provider of ['openrouter', 'custom-11111111-1111-4111-8111-111111111111']) {
    const overrides = provider === 'openrouter' ? {} : { provider, endpoint: 'https://custom.example/v1/chat/completions' };
    const a = await simple(overrides), b = await simple({ ...overrides, context_checkpoint: a.checkpoint });
    assert.equal(a.bodies[0].messages[0].content, b.bodies[0].messages[0].content);
    assert(!JSON.stringify(b.bodies[0].messages[0]).includes('kindred-pi-'));
  }
});

test('restored provider text/thinking signatures and tool argument/result pairs remain intact', async () => {
  const { checkpoint } = await simple();
  checkpoint.messages[1].content.unshift({ type: 'thinking', thinking: 'Opaque fixture context.', thinkingSignature: 'signed-fixture', redacted: false });
  checkpoint.messages[1].content.at(-1).textSignature = 'text-fixture';
  const next = await simple({ context_checkpoint: checkpoint });
  assert.deepEqual(next.checkpoint.messages.slice(0, checkpoint.messages.length), checkpoint.messages);
});

test('malformed, mismatched, incomplete and oversized restores fail before provider or tool calls', async () => {
  const { checkpoint } = await simple();
  const candidates = [
    { ...checkpoint, schema: 2 }, { ...checkpoint, sdk_version: 'old' }, { ...checkpoint, api_key: 'unexpected' },
    { ...checkpoint, estimated_tokens: -1 }, { ...checkpoint, messages: [{ role: 'user', content: 'Incomplete.', timestamp: 1 }] },
    { ...checkpoint, messages: [...checkpoint.messages, { role: 'toolResult', toolCallId: 'missing', toolName: 'remember', content: [], isError: false, timestamp: 1 }] },
    { ...checkpoint, messages: checkpoint.messages.map(m => m.role === 'assistant' ? { ...m, provider: 'different' } : m) },
    { ...checkpoint, messages: checkpoint.messages.map(m => m.role === 'assistant' ? { ...m, stopReason: 'deferred' } : m) },
    { ...checkpoint, messages: checkpoint.messages.map(m => m.role === 'assistant' ? { ...m, content: [{ type: 'toolCall', id: 'missing', name: 'remember', arguments: {} }] } : m) },
    { ...checkpoint, messages: Array(MAX_CHECKPOINT_MESSAGES + 1).fill(checkpoint.messages[1]) },
    { ...checkpoint, messages: [{ role: 'user', content: 'x'.repeat(MAX_CHECKPOINT_BYTES), timestamp: 1 }] },
  ];
  for (const context_checkpoint of candidates) {
    let providers = 0, tools = 0;
    await assert.rejects(runSession(request({ context_checkpoint }), async () => { tools++; }, () => {}, undefined,
      { fetch: async () => { providers++; return answer('Unexpected.'); } }), e => ['context_checkpoint', 'context_limit'].includes(e.code));
    assert.equal(providers, 0); assert.equal(tools, 0);
  }
});

test('credential echo disables checkpoint persistence without failing completed work', async () => {
  const result = await simple({ prompt: 'Do not persist private-fixture-key.' }, 'Completed work.');
  assert.equal(result.checkpoint, undefined);
  assert.deepEqual(result.events.find(e => e.type === 'checkpoint'), { type: 'checkpoint', input_id: 'initial', state: 'unavailable', reason: 'secret' });
  const clean = await simple({ instructions: 'Standing private configuration.', session_id: 'transport-only-session' });
  assert(!JSON.stringify(clean.checkpoint).includes('private-fixture-key'));
  assert(!JSON.stringify(clean.checkpoint).includes('Standing private configuration'));
  assert(!JSON.stringify(clean.checkpoint).includes('transport-only-session'));
  assert(!JSON.stringify(clean.checkpoint).includes('authorization'));
});

test('oversized successful work emits unavailable checkpoint instead of failure or truncating history', async () => {
  const result = await simple({ prompt: 'x'.repeat(MAX_CHECKPOINT_BYTES), model_info: { id: 'test/model', context_window: 4000000, max_tokens: 4096 } });
  assert.equal(result.events.find(e => e.type === 'checkpoint').reason, 'limit');
  assert(result.events.some(e => e.type === 'assistant' && e.text === 'Done.'));
});

test('legacy caller receives no checkpoint frames unless protocol was negotiated', async () => {
  const result = await simple({ context_protocol: undefined });
  assert(!result.events.some(e => e.type === 'checkpoint'));
});

test('provider failure and interruption export no successful checkpoint or replay', async () => {
  const original = (await simple()).checkpoint;
  for (const mode of ['http', 'truncated', 'abort']) {
    const events = [], controller = new AbortController(); let calls = 0;
    await assert.rejects(runSession(request({ context_checkpoint: original }), async () => { throw Error('Unexpected tool'); }, event => events.push(event), controller.signal,
      { fetch: async () => {
        calls++;
        if (mode === 'http') return new Response('Private upstream failure', { status: 503 });
        if (mode === 'abort') controller.abort();
        return mode === 'truncated' ? sse({ role: 'assistant', content: 'Partial.' }, 'length') : answer('Interrupted.');
      } }));
    assert.equal(calls, 1); assert(!events.some(e => e.type === 'checkpoint'));
  }
});

test('follow-up checkpoints precede turn completion and bind exact acknowledged input IDs', async () => {
  const events = []; let turns = 0;
  await runSession(request({ followup_protocol: 1 }), async () => {}, event => events.push(event), undefined,
    { fetch: async () => answer('Completed turn.'), nextTurn: async id => {
      assert.equal(events.at(-1).type, 'checkpoint'); assert.equal(events.at(-1).input_id, id);
      return turns++ === 0 ? { offer_id: 'next-input', prompt: 'Follow up.' } : null;
    } });
  assert.deepEqual(events.filter(e => e.type === 'checkpoint').map(e => e.input_id), ['initial', 'next-input']);
  assert.equal(events.filter(e => e.type === 'checkpoint').at(-1).checkpoint.messages.filter(m => m.role === 'user').length, 2);
});

test('actual SDK compaction restores its summary and retained context without replaying actions', async () => {
  const events = []; let normal = 0, summaries = 0, actions = 0;
  const overrides = { model_info: { id: 'test/model', reasoning: true, vision: true, context_window: 16000, max_tokens: 4096 } };
  await runSession(request(overrides), async () => { actions++; return { text: 'Observation from completed work. '.repeat(1700), failed: false }; }, event => events.push(event), undefined,
    { fetch: async req => {
      const body = await req.json();
      if (!body.tools?.length) { summaries++; return answer('Verified prior work. Continue from saved observations.'); }
      return ++normal <= 2 ? action('long' + normal) : answer('Long work complete.');
    } });
  assert(summaries > 0); assert.equal(actions, 2);
  const frame = events.find(e => e.type === 'checkpoint'); assert.equal(frame.state, 'ready');
  assert.equal(frame.checkpoint.messages[0].role, 'compactionSummary');
  const restored = await simple({ ...overrides, context_checkpoint: frame.checkpoint, prompt: 'Recap the saved work.' });
  assert(JSON.stringify(restored.bodies).includes('Verified prior work.'));
  assert(!JSON.stringify(restored.bodies).includes('kindred-pi-'));
});

test('failed tool results retain their original pairing and failure state after restore', async () => {
  const events = []; let calls = 0;
  await runSession(request(), async () => ({ text: 'Action was declined.', failed: true }), e => events.push(e), undefined,
    { fetch: async () => ++calls === 1 ? action() : answer('No action was taken.') });
  const checkpoint = events.find(e => e.type === 'checkpoint').checkpoint;
  const result = checkpoint.messages.find(m => m.role === 'toolResult');
  assert.equal(result.toolCallId, 'call1'); assert.equal(result.isError, true); assert.equal(result.details.failed, true);
  const restored = await simple({ context_checkpoint: checkpoint });
  assert.deepEqual(restored.checkpoint.messages.find(m => m.role === 'toolResult'), result);
});

test('interruption with an action in flight never exports an incomplete tool pair', async () => {
  let entered; const started = new Promise(resolve => { entered = resolve; });
  const controller = new AbortController(), events = []; let calls = 0, actions = 0;
  const work = runSession(request(), async (_id, _name, _args, signal) => {
    actions++; entered(); await new Promise((_, reject) => signal.addEventListener('abort', () => reject(Error('interrupted')), { once: true }));
  }, e => events.push(e), controller.signal, { fetch: async () => { calls++; return action(); } });
  await started; controller.abort(); await assert.rejects(work, e => e.code === 'aborted');
  assert.equal(calls, 1); assert.equal(actions, 1); assert(!events.some(e => e.type === 'checkpoint'));
});
