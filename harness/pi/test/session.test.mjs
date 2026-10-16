import test from 'node:test';
import assert from 'node:assert/strict';
import { runSession, providerAdapter, customToolContext, customSkillListing, responseBudget, compactionSettings, progressReminder, pruneScreenshots, guardedFetch, HarnessError } from '../session.mjs';

const remember = { name: 'remember', description: 'Save a short fact.', inputSchema: {
  type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false,
} };
function request(overrides = {}) {
  return { protocol: 1, provider: 'openrouter', model: 'test/model',
    model_info: { id: 'test/model', reasoning: true, vision: true, context_window: 128000, max_tokens: 4096 },
    api_key: 'fixture-key', instructions: 'You are a Kindred bot. Use the supplied tools.',
    prompt: 'Remember concise reports.', tools: [remember], max_steps: 4, reasoning_effort: 'high', require_zdr: true,
    ...overrides };
}
function sse(message, finish = 'stop') {
  const delta = { ...message };
  if (delta.tool_calls) delta.tool_calls = delta.tool_calls.map((call, index) => ({ index, ...call }));
  const chunk = (delta, reason) => ({ id: 'fixture-completion', object: 'chat.completion.chunk', created: 1, model: 'test/model',
    choices: [{ index: 0, delta, finish_reason: reason }] });
  return new Response('data: ' + JSON.stringify(chunk(delta, null)) + '\n\n' +
    'data: ' + JSON.stringify(chunk({}, finish)) + '\n\ndata: [DONE]\n\n', {
    headers: { 'Content-Type': 'text/event-stream' },
  });
}
function toolCall(name, args, id = 'call_1') {
  return { id, type: 'function', function: { name, arguments: typeof args === 'string' ? args : JSON.stringify(args) } };
}

test('actual Pi SDK executes tools and preserves call pairing, selected model and privacy', async () => {
  const bodies = [], events = [], tools = [];
  const result = await runSession(request(), async (id, name, args) => {
    tools.push({ id, name, args }); return { text: 'Memory saved.', failed: false };
  }, event => events.push(event), undefined, { fetch: async req => {
    assert.equal(req.url, 'https://openrouter.ai/api/v1/chat/completions');
    assert.equal(req.redirect, 'error');
    assert.equal(req.headers.get('authorization'), 'Bearer fixture-key');
    assert.equal(req.headers.get('x-openrouter-cache'), 'false');
    const body = await req.json(); bodies.push(body);
    assert.equal(body.model, 'test/model'); assert.equal(body.models, undefined);
    assert.deepEqual(body.provider, { zdr: true, data_collection: 'deny', require_parameters: true });
    assert.deepEqual(body.reasoning, { effort: 'high' });
    assert.equal(body.parallel_tool_calls, undefined);
    assert.deepEqual(body.tools.map(tool => tool.function.name), ['remember']);
    return bodies.length === 1 ? sse({ role: 'assistant', tool_calls: [toolCall('remember', { text: 'Concise reports' })] }, 'tool_calls')
      : sse({ role: 'assistant', content: 'Memory saved.' });
  } });
  assert.equal(result.output, 'Memory saved.'); assert.equal(result.tool_calls, 1); assert.equal(bodies.length, 2);
  assert.deepEqual(tools, [{ id: 'call_1', name: 'remember', args: { text: 'Concise reports' } }]);
  const messages = bodies[1].messages;
  assert.equal(messages.find(m => m.role === 'tool').tool_call_id, 'call_1');
  assert.equal(messages.find(m => m.role === 'assistant').tool_calls[0].id, 'call_1');
  assert.equal(events.filter(e => e.type === 'ready').length, 1);
  assert.equal(events.filter(e => e.type === 'assistant').length, 1);
});

test('schema failures and unavailable native tools never reach the Kindred action bridge', async () => {
  let count = 0, calls = 0;
  const result = await runSession(request(), async () => { calls++; throw Error('unexpected bridge'); }, () => {}, undefined, {
    fetch: async req => {
      const body = await req.json(); count++;
      if (count === 1) return sse({ role: 'assistant', tool_calls: [toolCall('read', { path: '/etc/passwd' }, 'native'), toolCall('remember', {}, 'invalid')] }, 'tool_calls');
      assert.equal(body.messages.filter(m => m.role === 'tool').length, 2);
      return sse({ role: 'assistant', content: 'Those actions could not run.' });
    },
  });
  assert.equal(calls, 0); assert.equal(count, 2); assert.equal(result.tool_calls, 0);
});

test('provider errors are sanitized, do not retry, and do not choose another model', async () => {
  let requests = 0;
  await assert.rejects(runSession(request(), async () => {}, () => {}, undefined, { fetch: async () => {
    requests++; return new Response('private upstream error fixture-key', { status: 404 });
  } }), error => error instanceof HarnessError && error.code === 'provider_http' && error.status === 404 && !error.message.includes('private'));
  assert.equal(requests, 1);
});

test('provider IDs are explicit and OpenRouter credentials cannot be sent to another endpoint', () => {
  assert.throws(() => providerAdapter(request({ provider: 'custom' })), /provider/);
  assert.throws(() => providerAdapter(request({ endpoint: 'https://other.example/chat/completions' })), /endpoint/);
  assert.throws(() => providerAdapter(request({ endpoint: 'http://localhost:1234/chat/completions' })), /endpoint/);
  assert.equal(providerAdapter(request({ require_zdr: false })).model.compat.openRouterRouting.zdr, false);
});

test('old images are pruned without discarding their tool result or ID', () => {
  const messages = [1, 2, 3].map(i => ({ role: 'toolResult', toolCallId: 'call_' + i,
    content: [{ type: 'text', text: 'Screenshot ' + i }, { type: 'image', data: 'image' + i, mimeType: 'image/png' }] }));
  const pruned = pruneScreenshots(messages);
  assert.deepEqual(pruned.map(m => m.toolCallId), ['call_1', 'call_2', 'call_3']);
  assert.equal(pruned.flatMap(m => m.content).filter(c => c.type === 'image').length, 2);
  assert.equal(messages[0].content[1].type, 'image');
});

test('a human tool stays pending until its result returns, then Pi continues the same conversation', async () => {
  let release, entered;
  const pending = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  let calls = 0, requests = 0;
  const human = { ...remember, name: 'request_user_action' };
  const work = runSession(request({ tools: [human] }), async (id, name) => {
    calls++; assert.equal(id, 'human_1'); assert.equal(name, 'request_user_action'); entered();
    await pending; return { text: 'Done with subtask. Inspect a fresh screenshot.', failed: false };
  }, () => {}, undefined, { fetch: async req => {
    const body = await req.json(); requests++;
    if (requests === 1) return sse({ role: 'assistant', tool_calls: [toolCall('request_user_action', { text: 'Sign in' }, 'human_1')] }, 'tool_calls');
    assert.equal(body.messages.find(m => m.role === 'tool').tool_call_id, 'human_1');
    assert.match(JSON.stringify(body.messages), /Done with subtask/);
    return sse({ role: 'assistant', content: 'Continued the original task.' });
  } });
  await started;
  assert.equal(requests, 1); assert.equal(calls, 1);
  release();
  assert.equal((await work).output, 'Continued the original task.');
  assert.equal(requests, 2); assert.equal(calls, 1);
});

test('cancellation aborts a waiting tool without another model request or action', async () => {
  const controller = new AbortController(); let entered, requests = 0;
  const started = new Promise(resolve => { entered = resolve; });
  const work = runSession(request(), async (_id, _name, _args, signal) => {
    entered();
    await new Promise((_, reject) => signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true }));
  }, () => {}, controller.signal, { fetch: async () => {
    requests++; return sse({ role: 'assistant', tool_calls: [toolCall('remember', { text: 'Wait' })] }, 'tool_calls');
  } });
  await started; controller.abort();
  await assert.rejects(work, error => error.code === 'aborted');
  assert.equal(requests, 1);
});

test('tool errors and images travel back through Pi with their originating call IDs', async () => {
  let requests = 0;
  await runSession(request(), async () => ({ text: 'Action declined by user.', failed: true,
    image: 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=' }),
  () => {}, undefined, { fetch: async req => {
    const body = await req.json(); requests++;
    if (requests === 1) return sse({ role: 'assistant', tool_calls: [toolCall('remember', { text: 'Fixture' })] }, 'tool_calls');
    const result = body.messages.find(m => m.role === 'tool');
    assert.equal(result.tool_call_id, 'call_1'); assert.match(JSON.stringify(result), /declined by user/);
    assert.match(JSON.stringify(body.messages), /data:image\/png;base64,iVBOR/);
    return sse({ role: 'assistant', content: 'The action was declined.' });
  } });
  assert.equal(requests, 2);
});

test('multiple tool calls run sequentially and stop at the shared action budget', async () => {
  let actions = 0, inFlight = 0, maximum = 0;
  await assert.rejects(runSession(request({ max_steps: 1 }), async () => {
    actions++; maximum = Math.max(maximum, ++inFlight); await Promise.resolve(); inFlight--;
    return { text: 'Saved', failed: false };
  }, () => {}, undefined, { fetch: async () => sse({ role: 'assistant', tool_calls: [
    toolCall('remember', { text: 'One' }, 'one'), toolCall('remember', { text: 'Two' }, 'two'),
  ] }, 'tool_calls') }), error => error.code === 'budget');
  assert.equal(actions, 1); assert.equal(maximum, 1);
});

test('incomplete provider output is reported as failure, not a successful partial answer', async () => {
  const events = [];
  await assert.rejects(runSession(request(), async () => {}, e => events.push(e), undefined,
    { fetch: async () => sse({ role: 'assistant', content: 'Partial response' }, 'length') }), error => error.code === 'truncated');
  assert.equal(events.filter(e => e.type === 'assistant').length, 0);
});

test('HTTP redirects and oversized streams fail before credentials or raw errors can be forwarded', async () => {
  const failure = { value: null }; let requests = 0;
  const redirect = guardedFetch('https://openrouter.ai/api/v1/chat/completions', failure, async req => {
    requests++; assert.equal(req.redirect, 'error');
    return new Response(null, { status: 307, headers: { location: 'https://elsewhere.invalid/' } });
  });
  await assert.rejects(redirect('https://openrouter.ai/api/v1/chat/completions', { method: 'POST', body: '{}' }), e => e.code === 'provider_http' && e.status === 307);
  assert.equal(requests, 1);
  const oversized = guardedFetch('https://openrouter.ai/api/v1/chat/completions', failure, async () => new Response(new Uint8Array(64 * 1024 * 1024 + 1)));
  const response = await oversized('https://openrouter.ai/api/v1/chat/completions', { method: 'POST', body: '{}' });
  await assert.rejects(response.arrayBuffer(), e => e.code === 'response_limit');
});

test('Pi compacts long tool context through the same model and privacy policy', async () => {
  const events = []; let normal = 0, summaries = 0;
  const result = await runSession(request({ max_steps: 8, model_info: {
    id: 'test/model', reasoning: true, vision: true, context_window: 16000, max_tokens: 4096,
  } }), async () => ({ text: 'Observation from the completed tool. '.repeat(1700), failed: false }),
  event => events.push(event), undefined, { fetch: async req => {
    const body = await req.json();
    assert.equal(body.model, 'test/model');
    assert.deepEqual(body.provider, { zdr: true, data_collection: 'deny', require_parameters: true });
    assert.equal(req.headers.get('x-openrouter-cache'), 'false');
    if (!body.tools?.length) { summaries++; return sse({ role: 'assistant', content: 'Completed observations were recorded. Continue the original task.' }); }
    normal++;
    return normal <= 2 ? sse({ role: 'assistant', tool_calls: [toolCall('remember', { text: 'Observe' }, 'long_' + normal)] }, 'tool_calls')
      : sse({ role: 'assistant', content: 'Long task completed.' });
  } });
  assert.equal(result.output, 'Long task completed.');
  assert(summaries > 0, 'expected a real SDK compaction request');
  assert(events.some(e => e.type === 'context' && e.state === 'compacted' && e.summary.includes('Completed observations were recorded.')));
});


test('custom model uses only its chosen endpoint and records reported and missing usage independently', async()=>{
  const events=[],payloads=[];let turns=0;
  const custom=request({provider:'custom-11111111-1111-4111-8111-111111111111',endpoint:'https://custom.example/v1/chat/completions'});
  const result=await runSession(custom,async()=>({text:'saved'}),e=>events.push(e),undefined,{fetch:async req=>{
    assert.equal(req.url,custom.endpoint);assert.equal(req.headers.get('authorization'),'Bearer fixture-key');
    const body=await req.json();payloads.push(body);assert.equal(body.provider,undefined);assert.equal(body.reasoning,undefined);assert.equal(body.reasoning_effort,'high');
    const response= turns++===0?sse({role:'assistant',tool_calls:[toolCall('remember',{text:'hello'})]},'tool_calls'):sse({role:'assistant',content:'Done'});
    if(turns===1)return response;
    let text=await response.text();text=text.replace('data: [DONE]','data: '+JSON.stringify({choices:[],usage:{prompt_tokens:120,completion_tokens:30,cost:0.004,prompt_tokens_details:{cached_tokens:20}}})+'\n\ndata: [DONE]');
    return new Response(text,{headers:{'content-type':'text/event-stream'}});
  }});
  assert.equal(result.output,'Done');assert.equal(payloads.length,2);
  const receipts=events.filter(e=>e.type==='usage');assert.equal(receipts.length,3);assert.equal(receipts[0].tokens_reported,false);
  assert.deepEqual(receipts.at(-1),{type:'usage',request_id:2,input_tokens:120,output_tokens:30,cached_tokens:20,cost:0.004,cost_source:'reported',tokens_reported:true});
});

test('stream usage parsing tolerates arbitrary byte splits and cannot leak raw provider fields',async()=>{
  const values=[],failure={value:null};const source='data: '+JSON.stringify({usage:{prompt_tokens:1,completion_tokens:2}})+'\n\ndata: [DONE]\n\n';
  const result=await guardedFetch('https://example.com/chat/completions',failure,async()=>new Response(new ReadableStream({start(c){for(const b of new TextEncoder().encode(source))c.enqueue(Uint8Array.of(b));c.close();}})),u=>values.push(u))('https://example.com/chat/completions',{method:'POST',body:'{}'});
  assert.equal(await result.text(),source);assert.equal(values.length,1);assert.deepEqual(values[0],{prompt_tokens:1,completion_tokens:2});
});


test('explicit keyless custom endpoint receives no Authorization header',async()=>{
  const value=request({provider:'custom-11111111-1111-4111-8111-111111111111',endpoint:'http://127.0.0.1:9999/v1/chat/completions'});value.model_info.no_auth=true;
  const result=await runSession(value,async()=>({text:'ok'}),()=>{},undefined,{fetch:async req=>{assert.equal(req.headers.get('authorization'),null);return sse({role:'assistant',content:'Keyless endpoint verified'});}});
  assert.equal(result.output,'Keyless endpoint verified');
});

test('OpenCode pins Go/Zen endpoints and preserves conversation headers across requests', async()=>{
  const {default:catalog}=await import('../opencode-models.json',{with:{type:'json'}});
  for(const provider of ['opencode','opencode-go']){
    for(const api of ['openai-completions','anthropic-messages','openai-responses']){
      const model=catalog[provider].find(m=>m.api===api);
      const base=provider==='opencode-go'?'https://opencode.ai/zen/go/v1':'https://opencode.ai/zen/v1';
      const endpoint=base+'/'+{'openai-completions':'chat/completions','anthropic-messages':'messages','openai-responses':'responses'}[api];
      const input=request({provider,model:model.id,session_id:'bot-123:chat-456',reasoning_effort:'',endpoint,model_info:{id:model.id,context_window:model.contextWindow,reasoning:model.reasoning,max_tokens:4096}});
      let calls=0;
      const operation=runSession(input,async()=>({text:'Saved.'}),()=>{},undefined,{fetch:async req=>{
        calls++;assert.equal(new URL(req.url).pathname,new URL(endpoint).pathname);
        assert.equal(req.headers.get('x-opencode-session'),'bot-123:chat-456');assert.equal(req.headers.get('user-agent'),'Kindred/1.0');
        const payload=await req.json();assert.equal(payload.model,model.id);assert.equal(payload.provider,undefined);
        if(api!=='openai-completions')return new Response('fixture-error',{status:403});
        return calls===1?sse({role:'assistant',tool_calls:[toolCall('remember',{text:'remember'},'opencode-tool')]},'tool_calls'):sse({role:'assistant',content:'Done.'});
      }});
      if(api==='openai-completions'){assert.equal((await operation).output,'Done.');assert.equal(calls,2);}
      else {await assert.rejects(operation,e=>e.code==='provider_http'&&e.status===403);assert.equal(calls,1);}
      assert.throws(()=>providerAdapter({...input,endpoint:endpoint.replace('/zen/go/','/zen/').replace('/zen/v1','/elsewhere/v1')}),/endpoint/);
      assert.throws(()=>providerAdapter({...input,session_id:''}),/model/);
    }
  }
});

test('OpenCode compaction retains the selected account, model and conversation',async()=>{
  const {default:catalog}=await import('../opencode-models.json',{with:{type:'json'}});
  const model=catalog['opencode-go'].filter(m=>m.api==='openai-completions').sort((a,b)=>a.contextWindow-b.contextWindow)[0];
  let summaries=0,normal=0;const events=[];
  const result=await runSession(request({provider:'opencode-go',model:model.id,session_id:'bot-123:chat-456',reasoning_effort:'',endpoint:'https://opencode.ai/zen/go/v1/chat/completions',max_steps:8,model_info:{id:model.id,context_window:model.contextWindow,reasoning:model.reasoning,max_tokens:4096}}),
    async()=>({text:'Completed observation and verified tool result. '.repeat(Math.ceil(model.contextWindow/3))}),e=>events.push(e),undefined,{fetch:async req=>{
      assert.equal(req.url,'https://opencode.ai/zen/go/v1/chat/completions');assert.equal(req.headers.get('x-opencode-session'),'bot-123:chat-456');assert.equal(req.headers.get('user-agent'),'Kindred/1.0');
      const body=await req.json();assert.equal(body.model,model.id);
      if(!body.tools?.length){summaries++;return sse({role:'assistant',content:'The observation is saved. Finish the task.'});}
      return ++normal===1?sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Observe'},'observe')]},'tool_calls'):sse({role:'assistant',content:'Finished.'});
    }});
  assert.equal(result.output,'Finished.');assert(summaries>0);assert(events.some(e=>e.state==='compacted'));
});

test('OpenCode Anthropic and Responses models return normal Kindred answers',async()=>{
  const {default:catalog}=await import('../opencode-models.json',{with:{type:'json'}});
  for(const api of ['anthropic-messages','openai-responses']){
    const model=catalog.opencode.find(m=>m.api===api),events=[];
    const input=request({provider:'opencode',model:model.id,session_id:'bot-123:chat-456',reasoning_effort:'',endpoint:'https://opencode.ai/zen/v1/'+(api==='anthropic-messages'?'messages':'responses'),model_info:{id:model.id,context_window:model.contextWindow,reasoning:model.reasoning,max_tokens:4096}});
    const message={id:'msg_1',type:'message',role:'assistant',status:'completed',content:[{type:'output_text',text:'Done.',annotations:[]}]};
    const frames=api==='anthropic-messages'?[
      {type:'message_start',message:{id:'msg_1',type:'message',role:'assistant',model:model.id,content:[],stop_reason:null,usage:{input_tokens:10,output_tokens:0}}},
      {type:'content_block_start',index:0,content_block:{type:'text',text:''}},
      {type:'content_block_delta',index:0,delta:{type:'text_delta',text:'Done.'}},
      {type:'content_block_stop',index:0},
      {type:'message_delta',delta:{stop_reason:'end_turn'},usage:{output_tokens:2}},
      {type:'message_stop'},
    ]:[
      {type:'response.created',response:{id:'resp_1',status:'in_progress',output:[]}},
      {type:'response.output_item.added',output_index:0,item:{...message,status:'in_progress',content:[]}},
      {type:'response.content_part.added',output_index:0,content_index:0,part:{type:'output_text',text:'',annotations:[]}},
      {type:'response.output_text.delta',output_index:0,content_index:0,delta:'Done.'},
      {type:'response.output_text.done',output_index:0,content_index:0,text:'Done.'},
      {type:'response.output_item.done',output_index:0,item:message},
      {type:'response.completed',response:{id:'resp_1',status:'completed',output:[message],usage:{input_tokens:10,output_tokens:2,total_tokens:12,input_tokens_details:{cached_tokens:0}}}},
    ];
    const result=await runSession(input,async()=>({text:'unused'}),e=>events.push(e),undefined,{fetch:async()=>new Response(frames.map(f=>`event: ${f.type}\ndata: ${JSON.stringify(f)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}})});
    assert.equal(result.output,'Done.');assert(events.some(e=>e.type==='usage'&&e.tokens_reported&&e.output_tokens===2));
  }
});


test('API output budgets bound custom generation and preserve hosted defaults', async () => {
  for (const provider of ['openrouter','custom-11111111-1111-4111-8111-111111111111','opencode','opencode-go']) {
    for (const limit of (provider.startsWith('opencode')?[131072]:[131072,2048,null])) {
      const model=provider.startsWith('opencode')?'glm-5.3-flash':'test/model';
      const endpoint=provider==='openrouter'?'https://openrouter.ai/api/v1/chat/completions':provider.startsWith('custom-')?'http://localhost:8000/v1/chat/completions':`https://opencode.ai/zen/${provider==='opencode-go'?'go/':''}v1/chat/completions`;
      let calls=0;
      const result=await runSession(request({provider,model,endpoint,session_id:'bot:chat',reasoning_effort:'',model_info:{id:model,context_window:1000000,reasoning:true,max_tokens:limit}}),async()=>({text:'ok'}),()=>{},undefined,{fetch:async req=>{
        calls++;const body=await req.json();assert.equal(body.max_tokens,limit??(provider.startsWith('custom-')?32768:undefined));assert.equal(body.max_completion_tokens,undefined);
        return sse({role:'assistant',content:'Complete response.'});
      }});
      assert.equal(calls,1);assert.equal(result.output,'Complete response.');
    }
  }
});
test('model output budgets preserve context room and reject malformed limits',()=>{
  const adapter=providerAdapter(request({model_info:{id:'test/model',context_window:32768,max_tokens:32768}}));
  const context={systemPrompt:'Instructions',tools:[],messages:[{role:'user',content:'A'.repeat(40000),timestamp:0}]};
  const budget=responseBudget(adapter.model,context);assert(budget>4096&&budget<23000);
  assert(responseBudget({contextWindow:1024,maxTokens:512},{messages:[],tools:[]})===512);
  for(const max_tokens of [0,-1,1.5,'131072',Infinity,200000])assert.throws(()=>providerAdapter(request({model_info:{id:'test/model',context_window:128000,max_tokens}})),e=>e.code==='model');
});

test('large valid answers are not cut off by the old visible-output byte cap',async()=>{
  const answer='Detailed result. '.repeat(40000);
  const result=await runSession(request({reasoning_effort:'',model_info:{id:'test/model',context_window:1000000,max_tokens:200000}}),async()=>({text:'ok'}),()=>{},undefined,{fetch:async()=>sse({role:'assistant',content:answer})});
  assert.equal(result.output.trim(),answer.trim());
});

for (const mime of ['image/png','image/jpeg','image/webp','image/gif']) {
 test('attachment MIME reaches the model: '+mime, async()=>{
  let requests=0;
  const image='data:'+mime+';base64,AQID';
  const result=await runSession(request({provider:'custom-11111111-1111-4111-8111-111111111111',endpoint:'http://127.0.0.1:8000/v1/chat/completions'}),async()=>({text:'Uploaded image',image}),()=>{},undefined,{fetch:async req=>{
   const body=await req.json();
   if(++requests===1)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Read attachment'})]},'tool_calls');
   assert(JSON.stringify(body.messages).includes(image));
   return sse({role:'assistant',content:'Image received'});
  }});
  assert.equal(requests,2);assert.equal(result.output,'Image received');
 });
}

test('unlimited tasks continue beyond the former 24-call default',async()=>{
 let requests=0,tools=0;
 const result=await runSession(request({max_steps:0}),async()=>{tools++;return {text:'Done'};},()=>{},undefined,{fetch:async()=>{
  requests++;
  return requests<=30?sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Step '+requests},'step_'+requests)]},'tool_calls'):sse({role:'assistant',content:'Finished all 30 steps'});
 }});
 assert.equal(tools,30);assert.equal(result.output,'Finished all 30 steps');
});

test('streaming progress precedes the completed answer and is bounded without exposing reasoning', async () => {
  const events = [];
  let requests = 0;
  await runSession(request(), async () => ({text: 'Saved', failed: false}), e => events.push(e), undefined, {
    fetch: async () => {
      requests++;
      if (requests === 1) return sse({role: 'assistant', reasoning: 'PRIVATE_REASONING_SENTINEL', tool_calls: [toolCall('remember', {text: 'A fact'})]}, 'tool_calls');
      return sse({role: 'assistant', content: 'Done.'});
    },
  });
  const phases = events.filter(e => e.type === 'progress');
  assert.equal(phases.filter(e => e.state === 'waiting').length, 2);
  assert.ok(phases.some(e => e.state === 'thinking'));
  assert.ok(phases.some(e => e.state === 'preparing'));
  assert.ok(phases.some(e => e.state === 'writing'));
  assert.ok(phases.length <= 8);
  assert.ok(events.findIndex(e => e.type === 'progress' && e.state === 'writing') < events.findIndex(e => e.type === 'assistant'));
  assert.ok(!JSON.stringify(events).includes('PRIVATE_REASONING_SENTINEL'));
  assert.equal(events.filter(e => e.type === 'assistant').length, 1);
});

test('writing progress is delivered while the provider response is still open', async () => {
  let controller, sawProgress = false;
  const events = [];
  const encode = delta => new TextEncoder().encode('data: ' + JSON.stringify({id:'live', object:'chat.completion.chunk', model:'test/model', choices:[{index:0, delta, finish_reason:null}]}) + '\n\n');
  await runSession(request(), async () => { throw Error('unexpected tool'); }, e => {
    events.push(e);
    if (e.type === 'progress' && e.state === 'writing') {
      sawProgress = true;
      assert.equal(events.filter(e => e.type === 'assistant').length, 0);
      controller.enqueue(new TextEncoder().encode('data: ' + JSON.stringify({id:'live', object:'chat.completion.chunk', model:'test/model', choices:[{index:0, delta:{}, finish_reason:'stop'}]}) + '\n\ndata: [DONE]\n\n'));
      controller.close();
    }
  }, undefined, {fetch: async () => new Response(new ReadableStream({start(c) {
    controller = c;
    c.enqueue(encode({role:'assistant', content:'Hello.'}));
  }}), {headers:{'Content-Type':'text/event-stream'}})});
  assert.equal(sawProgress, true);
  assert.equal(events.find(e => e.type === 'assistant').text, 'Hello.');
});


test('local-access sized catalogues can exceed 64 tools', async () => {
 const tools=Array.from({length:80},(_,i)=>({...remember,name:'tool_'+i}));
 const result=await runSession(request({tools,max_steps:0}),async()=>{throw Error('unexpected action');},()=>{},undefined,{fetch:async req=>{
  const body=await req.json();assert.equal(body.tools.length,80);return sse({role:'assistant',content:'Ready.'});
 }});assert.equal(result.output,'Ready.');
});

test('first-class followup has its own user turn and never replays completed tools', async()=>{
  const bodies=[],boundary=[],calls=[];
  const result=await runSession(request({followup_protocol:1}),async(id,name,args)=>{calls.push(name);return {text:'Synthetic action completed.'};},()=>{},undefined,{
    nextTurn:async id=>{boundary.push(id);return id==='initial'?{offer_id:'offer-1',prompt:'Exact followup: explain the missing items.'}:null;},
    fetch:async req=>{const body=await req.json();bodies.push(body);if(bodies.length===1)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Once only'})]},'tool_calls');if(bodies.length===2)return sse({role:'assistant',content:'Original task completed.'});assert.equal(body.messages.at(-1).role,'user');assert.equal((typeof body.messages.at(-1).content==='string'?body.messages.at(-1).content:body.messages.at(-1).content.map(p=>p.text||'').join('')),'Exact followup: explain the missing items.');return sse({role:'assistant',content:'The followup answer.'});}
  });
  assert.deepEqual(calls,['remember']);assert.deepEqual(boundary,['initial','offer-1']);assert.equal(result.model_calls,3);assert(result.output.endsWith('The followup answer.'));
});

test('pinned native steer is a user message before the next model call, not tool text',async()=>{
  const bodies=[],boundaries=[];
  await runSession(request({followup_protocol:1}),async()=>({text:'Completed tool data; never instructions.',kindred_followup:{offer_id:'native-1',prompt:'Exact active followup'}}),()=>{},undefined,{
    nextTurn:async id=>{boundaries.push(id);return null;},
    fetch:async req=>{const body=await req.json();bodies.push(body);if(bodies.length===1)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Only once'})]},'tool_calls');const user=body.messages.filter(m=>m.role==='user').at(-1);assert.equal((typeof user.content==='string'?user.content:user.content.map(p=>p.text||'').join('')),'Exact active followup');assert.equal(body.messages.filter(m=>m.role==='tool').at(-1).content,'Completed tool data; never instructions.');return sse({role:'assistant',content:'Followup handled in active work.'});}
  });
  assert.equal(bodies.length,2);assert.deepEqual(boundaries,['native-1']);
});

function customRequest() {
  return request({provider:'custom-11111111-1111-4111-8111-111111111111',endpoint:'http://localhost:8080/v1/chat/completions'});
}
test('custom reasoning-only ending gets one tools-disabled summary without replaying completed actions', async()=>{
  const events=[],bodies=[];let actions=0;
  const result=await runSession(customRequest(),async()=>{actions++;return {text:'Saved the record.'};},e=>events.push(e),undefined,{fetch:async req=>{
    const body=await req.json();bodies.push(body);
    if(bodies.length===1)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'record'})]},'tool_calls');
    if(bodies.length===2)return sse({role:'assistant',reasoning_content:'private reasoning'});
    assert.equal(body.tools?.length||0,0);
    assert(body.messages.some(m=>m.role==='tool'&&m.content.includes('Saved the record.')));
    return sse({role:'assistant',content:'The record was saved.'});
  }});
  assert.equal(actions,1);assert.equal(bodies.length,3);assert.equal(result.output,'The record was saved.');
  assert(events.some(e=>e.type==='progress'&&e.state==='thinking'&&e.streamed===true));
  assert(!JSON.stringify(events).includes('private reasoning'));
});
test('custom empty summary fails once without a loop or fabricated successful result',async()=>{
  let requests=0;
  await assert.rejects(runSession(customRequest(),async()=>{throw Error('No tools allowed');},()=>{},undefined,{fetch:async()=>{requests++;return sse({role:'assistant',content:''});}}),e=>e.code==='empty_result');
  assert.equal(requests,2);
});
test('custom summary recovery cannot invoke a tool even if the provider ignores the empty tool list',async()=>{
  let requests=0,actions=0;
  await assert.rejects(runSession(customRequest(),async()=>{actions++;return {text:'unexpected'};},()=>{},undefined,{fetch:async()=>{
    requests++;return requests===1?sse({role:'assistant',content:''}):sse({role:'assistant',tool_calls:[toolCall('remember',{text:'must not execute'})]},'tool_calls');
  }}),e=>e.code==='empty_result');
  assert.equal(requests,2);assert.equal(actions,0);
});
test('hosted empty endings retain the existing single request behavior',async()=>{
  let requests=0;const events=[];
  const result=await runSession(request(),async()=>{},e=>events.push(e),undefined,{fetch:async()=>{requests++;return sse({role:'assistant',reasoning_content:'private'});}});
  assert.equal(requests,1);assert.equal(result.output,'');assert(!events.some(e=>e.streamed));
});
test('real custom deltas refresh activity while keepalives and hosted phase labels do not',async()=>{
  const {progressReporter}=await import('../session.mjs');let now=0;const events=[];
  const report=progressReporter(e=>events.push(e),true,()=>now);
  report('waiting');report('thinking','first token');
  now=4999;report('thinking','next token');assert.equal(events.length,2);
  now=5000;report('thinking','next token');assert.equal(events.length,3);
  now=40000;report('thinking');report('thinking','');assert.equal(events.length,3);
  report('thinking','new token');assert.equal(events.length,4);
  assert(events.slice(1).every(e=>e.streamed===true));assert(!JSON.stringify(events).includes('token'));
  const hosted=[];const other=progressReporter(e=>hosted.push(e),false,()=>now);
  other('thinking','first');now+=6000;other('thinking','second');assert.deepEqual(hosted,[{type:'progress',state:'thinking'}]);
});

test('custom summary recovery restores tools for a subsequent queued user turn',async()=>{
  let requests=0,actions=0,boundaries=0;
  const result=await runSession({...customRequest(),followup_protocol:1},async()=>{actions++;return {text:'Saved only for the followup.'};},()=>{},undefined,{
    nextTurn:async()=>++boundaries===1?{offer_id:'next-user-turn',prompt:'Now save a new record.'}:null,
    fetch:async req=>{
      const body=await req.json();requests++;
      if(requests===1)return sse({role:'assistant',reasoning_content:'private'});
      if(requests===2){assert.equal(body.tools?.length||0,0);return sse({role:'assistant',content:'First answer.'});}
      assert.equal(body.tools.length,1);
      if(requests===3)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'new record'})]},'tool_calls');
      return sse({role:'assistant',content:'New record saved.'});
    }
  });
  assert.equal(actions,1);assert.equal(requests,4);assert(result.output.endsWith('New record saved.'));
});


test('262k custom model without an advertised output limit gets a bounded wire budget', async()=>{
  const value=customRequest();
  value.model_info={id:value.model,context_window:262144,reasoning:false};
  value.reasoning_effort='';
  let calls=0;
  const result=await runSession(value,async()=>({text:'unused'}),()=>{},undefined,{fetch:async req=>{
    calls++;const body=await req.json();
    assert.equal(body.max_tokens,32768);
    assert.equal(body.max_completion_tokens,undefined);
    return sse({role:'assistant',content:'Finished.'});
  }});
  assert.equal(calls,1);assert.equal(result.output,'Finished.');
});

test('custom wire budget shrinks near context capacity, including without a configured maximum',async()=>{
  for(const maximum of [undefined,30000]){
    const value=customRequest();value.reasoning_effort='';
    value.model_info={id:value.model,context_window:32768,max_tokens:maximum};
    value.instructions='I'.repeat(60000);
    await runSession(value,async()=>({text:'unused'}),()=>{},undefined,{fetch:async req=>{
      const body=await req.json();assert(body.max_tokens>0&&body.max_tokens<14000);
      return sse({role:'assistant',content:'Finished.'});
    }});
  }
});

test('custom truncation preserves completed actions, rejects truncated tool calls and never retries them',async()=>{
  let calls=0,actions=0;const events=[];
  await assert.rejects(runSession(customRequest(),async()=>{actions++;return {text:'Saved once.'};},e=>events.push(e),undefined,{fetch:async()=>{
    calls++;
    if(calls===1)return sse({role:'assistant',tool_calls:[toolCall('remember',{text:'Completed action'})]},'tool_calls');
    return sse({role:'assistant',content:'Incomplete answer',tool_calls:[toolCall('remember',{text:'Must not execute'},'partial')]},'length');
  }}),e=>e.code==='custom_truncated');
  assert.equal(calls,2);assert.equal(actions,1);
  assert.equal(events.filter(e=>e.type==='assistant').length,0);
});


test('custom environment correction removes only the SDK cwd suffix and preserves the caller context',()=>{
 const context={systemPrompt:'Keep this exact instruction.\nA quoted /tmp/kindred-pi-example is data.\nCurrent working directory: /tmp/kindred-pi-owned\n',messages:[],tools:[]};
 const fixed=customToolContext(context,'/tmp/kindred-pi-owned');
 assert(fixed.systemPrompt.startsWith('Keep this exact instruction.\nA quoted /tmp/kindred-pi-example is data.'));
 assert(!fixed.systemPrompt.includes('Current working directory:'));
 assert(fixed.systemPrompt.includes('read_attachment'));assert(fixed.systemPrompt.includes('/workspace'));
 assert(context.systemPrompt.endsWith('/tmp/kindred-pi-owned\n'));
 assert.strictEqual(fixed.messages,context.messages);
});

test('actual custom requests and summary recovery get the VM environment, hosted requests stay unchanged',async()=>{
 for(const custom of [true,false]){
  let calls=0;const value=custom?customRequest():request();
  const result=await runSession(value,async()=>({text:'unused'}),()=>{},undefined,{fetch:async req=>{
   calls++;const body=await req.json();const system=body.messages.find(m=>m.role==='system').content;
   assert(system.startsWith(value.instructions));
   assert.equal(system.includes('Current working directory: /tmp/kindred-pi-'),!custom);
   assert.equal(system.includes('Kindred tool environment:'),custom);
   if(custom&&calls===1)return sse({role:'assistant',reasoning_content:'private reasoning'});
   return sse({role:'assistant',content:'Finished.'});
  }});
  assert.equal(result.output,'Finished.');assert.equal(calls,custom?2:1);
 }
});


test('custom skill discovery preserves maintenance metadata and malformed receipts',()=>{
 const skill={name:'sample',description:'Useful workflow',body:'Instructions that must be loaded explicitly.',parameters:[{name:'target'}],command:'sample',current_hash:'a'.repeat(64),import:null};
 assert.deepEqual(JSON.parse(customSkillListing(JSON.stringify([skill]))),[{name:skill.name,description:skill.description,parameters:skill.parameters,command:skill.command,current_hash:skill.current_hash,import:null}]);
 for(const raw of ['failed: unavailable','{"error":"unavailable"}','[null]'])assert.equal(customSkillListing(raw),raw);
});

test('actual custom skill listing is compact but skill_load and hosted listings retain complete bodies',async()=>{
 const skills=[{name:'sample',description:'Synthetic skill',command:'sample',current_hash:'a'.repeat(64),parameters:[],import:null,body:'Complete selected workflow.'}];
 const tools=[{name:'skills_list',description:'List available skills.',inputSchema:{type:'object',properties:{}}},{name:'skill_load',description:'Read a selected skill.',inputSchema:{type:'object',properties:{name:{type:'string'}},required:['name']}}];
 for(const custom of [true,false]){
  let calls=0;
  await runSession({...custom?customRequest():request(),tools},async(id,name)=>({text:JSON.stringify(name==='skills_list'?skills:skills[0])}),()=>{},undefined,{fetch:async req=>{
   const body=await req.json();calls++;
   assert.equal(body.tools.find(t=>t.function.name==='skills_list').function.description.includes('metadata only'),custom);
   if(calls===1)return sse({role:'assistant',tool_calls:[toolCall('skills_list',{})]},'tool_calls');
   const results=body.messages.filter(m=>m.role==='tool');
   assert.equal(JSON.parse(results[0].content)[0].body,custom?undefined:skills[0].body);
   assert.equal(JSON.parse(results[0].content)[0].current_hash,'a'.repeat(64));
   if(calls===2)return sse({role:'assistant',tool_calls:[toolCall('skill_load',{name:'sample'},'selected-skill')]},'tool_calls');
   assert.equal(JSON.parse(results.at(-1).content).body,skills[0].body);
   return sse({role:'assistant',content:'Read the selected skill.'});
  }});
  assert.equal(calls,3);
 }
});


test('skill catalogue projection refuses unknown error and incomplete shapes without rewriting any bytes',()=>{
 const valid={name:'owned',description:'description',command:'owned',current_hash:'a'.repeat(64),parameters:[],import:null,body:'PRIVATE_BODY'};
 const receipts=[
  '[{"body":"OWNED_ERROR_BODY","error":"unavailable"}]', '[{"body":"OWNED_NOT_A_SKILL"}]',
  ...Object.keys(valid).map(key=>{const row={...valid};delete row[key];return JSON.stringify([row]);}),
  ...[{name:3},{description:null},{command:[]},{current_hash:'not-a-hash'},{parameters:{}},{import:false},{body:{error:'PRIVATE_BODY'}},{error:'unavailable'},{failed:true},{isError:true}].map(change=>JSON.stringify([{...valid,...change}])),
  JSON.stringify([valid,{...valid,body:'OTHER_PRIVATE_BODY'}]),
  JSON.stringify([{name:'same',body:'A'},{name:'same',body:'B'}]),
  JSON.stringify([valid,{body:'error',error:'unavailable'}]),JSON.stringify([valid,null]),JSON.stringify([valid,[]]),
  JSON.stringify({skills:[valid]}),'null','false','42','"body"','[]  ',
 ];
 // Empty catalogues need no projection; preserving their serialization is safest.
 for(const raw of receipts)assert.equal(customSkillListing(raw),raw,raw);
});

test('skill catalogue success keeps arbitrary nested metadata and imported rows without mutation',()=>{
 const ordinary={name:'owned',description:'description',command:'owned',current_hash:'a'.repeat(64),parameters:[{name:'target',schema:{enum:['one','two']}}],import:null,body:'PRIVATE_BODY',future:{body:'nested metadata'}};
 const imported={...ordinary,name:'imported',import:{source:'local',hash:'b'.repeat(64),files:2,origin:{device_id:'synthetic'},future:{body:'import metadata'}}};delete imported.body;
 const rows=Object.freeze([Object.freeze(ordinary),Object.freeze(imported)]),raw=JSON.stringify(rows);
 const expected=JSON.parse(raw);delete expected[0].body;
 assert.deepEqual(JSON.parse(customSkillListing(raw)),expected);
 assert.equal(JSON.stringify(rows),raw);assert.equal(ordinary.body,'PRIVATE_BODY');
});

test('actual skills_list error and failed receipts remain exact in custom and hosted provider packets',async()=>{
 const tools=[{name:'skills_list',description:'List available skills.',inputSchema:{type:'object',properties:{}}}];
 const valid={name:'owned',description:'description',command:'owned',current_hash:'a'.repeat(64),parameters:[],import:null,body:'PRIVATE_BODY'};
 for(const custom of [true,false])for(const receipt of [
  {text:' [ {"body":"OWNED_ERROR_BODY","error":"unavailable"} ] '},
  {text:JSON.stringify([valid]),failed:true},
 ]){
  let calls=0;
  await runSession({...custom?customRequest():request(),tools},async()=>receipt,()=>{},undefined,{fetch:async req=>{
   const body=await req.json();calls++;
   if(calls===1)return sse({role:'assistant',tool_calls:[toolCall('skills_list',{})]},'tool_calls');
   assert.equal(body.messages.find(m=>m.role==='tool').content,receipt.text);
   return sse({role:'assistant',content:'Receipt preserved.'});
  }});assert.equal(calls,2);
 }
});

test('actual SDK successful catalogue projection keeps full source result and selected load; recovery and queued turn do not leak other bodies',async()=>{
 const rows=[{name:'one',body:'PRIVATE_BODY_ONE',current_hash:'a'.repeat(64),description:'one',command:'one',parameters:[{name:'x',default:'v'}],import:null},{name:'two',body:'PRIVATE_BODY_TWO',current_hash:'b'.repeat(64),description:'two',command:'two',parameters:[],import:null}];
 const listing=Object.freeze({text:JSON.stringify(rows),structuredContent:{catalogue:'unmodified'}});const original=JSON.stringify(listing);
 const selected=Object.freeze({text:JSON.stringify(rows[0])});let calls=0,actions=0,boundaries=0;
 const tools=['skills_list','skill_load'].map(name=>({name,description:name,inputSchema:{type:'object',properties:{}}}));
 await runSession({...customRequest(),tools,followup_protocol:1},async(id,name)=>{actions++;return name==='skills_list'?listing:selected},()=>{},undefined,{
 nextTurn:async()=>++boundaries===1?{offer_id:'owned-next',prompt:'Next owned question'}:null,
 fetch:async req=>{const body=await req.json();calls++;
 if(calls===1)return sse({role:'assistant',tool_calls:[toolCall('skills_list',{})]},'tool_calls');
 const wire=JSON.stringify(body.messages);assert(!wire.includes('PRIVATE_BODY_TWO'));
 const results=body.messages.filter(m=>m.role==='tool');assert.equal(JSON.parse(results[0].content)[0].body,undefined);assert.equal(JSON.parse(results[0].content)[0].current_hash,'a'.repeat(64));
 if(calls===2)return sse({role:'assistant',tool_calls:[toolCall('skill_load',{name:'one'},'owned-load')]},'tool_calls');
 assert.deepEqual(JSON.parse(results[1].content),rows[0]);
 if(calls===3)return sse({role:'assistant',reasoning_content:'owned thinking'});
 if(calls===4){assert.equal(body.tools?.length||0,0);return sse({role:'assistant',content:'First complete'});}
 return sse({role:'assistant',content:'Second complete'});
 }});assert.equal(calls,5);assert.equal(actions,2);assert.equal(JSON.stringify(listing),original);
});


test('silent custom runs request milestones only at tool boundaries and respect cadence', () => {
  let now = 0;
  const reminder = progressReminder(() => now);
  assert.equal(reminder.afterTool(), '');
  now = 89999; assert.equal(reminder.afterTool(), '');
  now = 90000; assert.match(reminder.afterTool(), /brief milestone/);
  assert.equal(reminder.afterTool(), '');
  reminder.written(); now = 180000;
  assert.equal(reminder.afterTool(), '', 'one action alone is not long work');
  assert.match(reminder.afterTool(), /Honor explicit quiet-work instructions/);
});


// Rust str::trim uses Unicode White_Space. NEL is blank; BOM and zero-width
// space are retained text. Do not rewrite any model content to reset cadence.
const backendWhitespace = '\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000';
async function reminderPackets({ texts = ['', ''], hosted = false, quiet = false, reasoning = '' } = {}) {
  const custom = 'custom-11111111-1111-4111-8111-111111111111';
  let clock = 1700000000000, calls = 0, actions = 0;
  const packets = [], events = [], original = Date.now;
  Date.now = () => clock;
  try {
    const result = await runSession(request({
      ...(hosted ? {} : { provider: custom, endpoint: 'http://localhost:8080/v1/chat/completions' }),
      max_steps: texts.length + 1,
      instructions: quiet ? 'Work quietly. Do not send progress updates.' : 'Use the supplied tools.',
    }), async () => {
      actions++; clock += 45000;
      return { text: 'OWNED_RESULT_' + actions };
    }, event => events.push(event), undefined, { fetch: async req => {
      packets.push(await req.json()); calls++;
      if (calls <= texts.length) return sse({ role: 'assistant', content: texts[calls - 1],
        ...(reasoning ? { reasoning_content: reasoning } : {}),
        tool_calls: [toolCall('remember', { text: 'Tool arguments are not written milestones' }, 'owned-' + calls)],
      }, 'tool_calls');
      return sse({ role: 'assistant', content: 'Finished owned work.' });
    } });
    assert.equal(calls, texts.length + 1, 'no extra model request');
    assert.equal(actions, texts.length, 'each tool executes once');
    assert.equal(result.model_calls, calls); assert.equal(result.tool_calls, actions);
    for (const text of texts.filter(Boolean))
      assert(events.some(event => event.type === 'assistant' && event.text === text), 'raw assistant text is retained');
    assert(!events.some(event => event.type === 'assistant' && event.text.includes('PRIVATE_REASONING')));
    return packets.at(-1).messages.filter(message => message.role === 'tool').map(message => message.content);
  } finally { Date.now = original; }
}
for (const [label, text] of [['empty', ''], ['ASCII whitespace', ' \t\n\r'], ['Unicode White_Space', backendWhitespace]]) {
  test('actual SDK reminder ignores ' + label + ' as a written update', async () => {
    const results = await reminderPackets({ texts: [text, text] });
    assert(!results[0].includes('Kindred progress reminder:'));
    assert.match(results[1], /Kindred progress reminder:/);
  });
}
for (const [label, text] of [['ordinary milestone', 'Owned milestone.'], ['Unicode milestone', '完了しました。'], ['BOM retained by backend', '\ufeff'], ['zero width space retained by backend', '\u200b']]) {
  test('actual SDK reminder resets for ' + label, async () => {
    const results = await reminderPackets({ texts: ['', text] });
    assert(!results.some(result => result.includes('Kindred progress reminder:')));
  });
}
test('actual SDK reminder ignores private reasoning and tool argument text', async () => {
  const results = await reminderPackets({ reasoning: 'PRIVATE_REASONING is not a written milestone' });
  assert.match(results[1], /Kindred progress reminder:/);
});
test('actual SDK reminder retains hosted and quiet-work behavior', async () => {
  const hosted = await reminderPackets({ hosted: true, texts: [' \n', backendWhitespace] });
  assert(!hosted.some(result => result.includes('Kindred progress reminder:')));
  const quiet = await reminderPackets({ quiet: true, texts: [' \n', backendWhitespace] });
  assert.match(quiet[1], /If progress updates are appropriate/);
  assert.match(quiet[1], /Honor explicit quiet-work instructions/);
});
test('actual SDK reminder preserves ninety-second cadence across blank tool turns', async () => {
  const results = await reminderPackets({ texts: ['', ' \n', backendWhitespace, ''] });
  assert.deepEqual(results.map(result => result.includes('Kindred progress reminder:')), [false, true, false, true]);
});


test('custom compaction reserves 32K while hosted providers keep their existing threshold', () => {
  const model = { provider: 'custom-local', contextWindow: 262144 };
  assert.equal(model.contextWindow - compactionSettings(model).reserveTokens, 229376);
  assert.equal(compactionSettings(model).keepRecentTokens, 12000);
  assert.equal(compactionSettings({ ...model, provider: 'openrouter' }).reserveTokens, 8192);
  assert.equal(compactionSettings({ ...model, contextWindow: 16000 }).reserveTokens, 4000);
});

test('custom fallback includes fixed instructions and tools without double counting reported usage', () => {
  const model = { provider: 'custom-local', contextWindow: 262144 };
  const context = { messages: [], systemPrompt: 'x'.repeat(30000), tools: [remember] };
  const fixed = Math.ceil((context.systemPrompt.length + JSON.stringify(context.tools).length) / 3);
  assert.equal(compactionSettings(model, context).reserveTokens, 32768 + fixed);
  context.messages.push({ role: 'assistant', stopReason: 'toolUse', content: [],
    usage: { input: 100000, output: 2000, cacheRead: 1000, cacheWrite: 0, totalTokens: 103000 } });
  assert.equal(compactionSettings(model, context).reserveTokens, 32768);
  context.messages.push({ role: 'toolResult', content: [{ type: 'text', text: 'New tool output' }] });
  assert.equal(compactionSettings(model, context).reserveTokens, 32768);
  context.messages[0].usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
  assert.equal(compactionSettings(model, context).reserveTokens, 32768 + fixed);
  assert.equal(compactionSettings({ ...model, provider: 'openrouter' }, context).reserveTokens, 8192);
});

test('custom SDK compacts a tool loop before 32K headroom is consumed, including fixed input without usage', async () => {
  const events = []; let normal = 0, summaries = 0, actions = 0;
  const custom = request({ provider: 'custom-11111111-1111-4111-8111-111111111111', endpoint: 'https://custom.example/v1/chat/completions',
    instructions: 'Standing instructions. '.repeat(900), max_steps: 8,
    model_info: { id: 'test/model', reasoning: true, vision: false, context_window: 262144, max_tokens: 4096 } });
  const result = await runSession(custom, async () => {
    actions++; return { text: 'Evidence '.repeat(actions <= 2 ? 49000 : 3000) };
  }, event => events.push(event), undefined, { fetch: async req => {
    const body = await req.json();
    if (!body.tools?.length) { summaries++; return sse({ role: 'assistant', content: 'Evidence collected. Finish the task.' }); }
    normal++;
    if (normal <= 3) assert.equal(summaries, 0, 'below the early threshold');
    if (normal === 4) assert(summaries > 0, 'must compact before the next model request');
    return normal <= 3 ? sse({ role: 'assistant', tool_calls: [toolCall('remember', { text: 'Observe' }, 'early_' + normal)] }, 'tool_calls')
      : sse({ role: 'assistant', content: 'Done.' });
  } });
  assert.equal(result.output, 'Done.');
  assert.equal(actions, 3, 'compaction must not repeat completed actions');
  assert(events.some(e => e.type === 'context' && e.state === 'compacted'));
});
