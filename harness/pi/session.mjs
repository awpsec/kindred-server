import opencodeModels from './opencode-models.json' with { type: 'json' };
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  estimateTokens, createAgentSession, DefaultResourceLoader, ModelRuntime, SessionManager, SettingsManager,
} from '@earendil-works/pi-coding-agent';
import { InMemoryCredentialStore, InMemoryModelsStore } from '@earendil-works/pi-ai';

export const SDK_VERSION = '0.85.1';
const MAX_REQUEST = 24 * 1024 * 1024;
const MAX_RESPONSE = 64 * 1024 * 1024;
const MAX_OUTPUT = 4 * 1024 * 1024;

export class HarnessError extends Error {
  constructor(code, status) { super(code); this.code = code; this.status = status; }
}

// Custom endpoints can spend a long time streaming reasoning. Count only actual
// nonempty SDK deltas, never timers or connection keepalives. No token text leaves
// this process; the small activity receipt is throttled to once per five seconds.
export function progressReporter(emit, custom = false, now = () => performance.now()) {
  const phases = new Set();
  let lastActivity = -Infinity;
  return (state, delta = '') => {
    const streamed = custom && typeof delta === 'string' && delta.length > 0;
    const tick = now();
    if (phases.has(state) && (!streamed || tick - lastActivity < 5000)) return;
    phases.add(state);
    if (streamed) lastActivity = tick;
    emit({ type: 'progress', state, ...(streamed ? { streamed: true } : {}) });
  };
}

// Hosted endpoints retain their provider default when no output limit is advertised.
// Custom endpoints receive this bounded fallback: some local servers otherwise
// generate until the entire context window is full, including hidden reasoning.
function outputLimit(info) {
  if (info.max_tokens != null && (!Number.isSafeInteger(info.max_tokens) || info.max_tokens < 1 || info.max_tokens > info.context_window)) throw new HarnessError('model');
  return info.max_tokens ?? Math.min(32768, Math.floor(info.context_window / 2));
}
// Pi appends its host cwd even with a custom system prompt. Kindred's tools
// execute in the bot VM, so that ephemeral host path is not a usable workspace.
export function customToolContext(context, harnessDirectory) {
  const suffix = `\nCurrent working directory: ${harnessDirectory.replace(/\\/g, '/')}\n`;
  const prompt = context.systemPrompt || '';
  return { ...context, systemPrompt: (prompt.endsWith(suffix) ? prompt.slice(0, -suffix.length) : prompt)
    + '\n\nKindred tool environment: guest_exec and computer tools run in the bot VM, not in this model harness process. Use /workspace for VM working files. Chat uploads are not staged there until read_attachment is called with the exact attachment ID from the conversation; use the path it returns. Do not search the host temporary directory or scan the filesystem to locate chat uploads. Local-desktop tools, when available, target their explicitly selected device.\n' };
}

// Discovery should not inject every ordinary skill's instructions into a local
// model turn. Keep identifiers, descriptions, parameters and guarded-edit hashes;
// skill_load still returns the selected skill's complete instructions unchanged.
export function customSkillListing(text) {
  try {
    const skills = JSON.parse(text);
    if (!Array.isArray(skills) || !skills.every(skill => skill && typeof skill === 'object' && !Array.isArray(skill))) return text;
    return JSON.stringify(skills.map(({body, ...metadata}) => metadata));
  } catch { return text; }
}

export function responseBudget(model, context) {
  const input = context.messages.reduce((sum, message) => sum + estimateTokens(message), 0)
    + Math.ceil(((context.systemPrompt || '').length + JSON.stringify(context.tools || []).length) / 3);
  return Math.max(1, Math.min(model.maxTokens, model.contextWindow - input - Math.min(1024, Math.floor(model.contextWindow / 16))));
}
function providerDefault(payload, info) {
  if (info.max_tokens == null) { delete payload.max_tokens; delete payload.max_completion_tokens; }
  return payload;
}

// Provider adapters supply metadata and transport policy, not another agent loop.
// Add future custom/local adapters here, with explicit endpoint and privacy rules.
const adapters = {
  openrouter(request) {
    const endpoint = request.endpoint || 'https://openrouter.ai/api/v1/chat/completions';
    const url = new URL(endpoint);
    const production = endpoint === 'https://openrouter.ai/api/v1/chat/completions';
    const fixture = request.fixture === true && url.protocol === 'http:' && url.hostname === '127.0.0.1';
    if ((!production && !fixture) || url.username || url.password || url.search || url.hash)
      throw new HarnessError('endpoint');
    if (!endpoint.endsWith('/chat/completions')) throw new HarnessError('endpoint');
    const info = request.model_info;
    if (!info || info.id !== request.model || !Number.isInteger(info.context_window) || info.context_window < 1024)
      throw new HarnessError('model');
    return {
      endpoint,
      model: {
        id: info.id, name: info.id, api: 'openai-completions',
        provider: 'openrouter', baseUrl: endpoint.slice(0, -'/chat/completions'.length),
        reasoning: info.reasoning === true,
        input: info.vision === true ? ['text', 'image'] : ['text'],
        contextWindow: info.context_window, maxTokens: outputLimit(info),
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        compat: {
          thinkingFormat: 'openrouter', supportsStore: false,
          supportsDeveloperRole: false, maxTokensField: 'max_tokens',
          openRouterRouting: { zdr: request.require_zdr !== false, data_collection: 'deny', require_parameters: true },
        },
      },
      headers: { 'X-OpenRouter-Cache': 'false' },
      prepare(payload) {
        // Reassert policy after SDK request conversion, including compaction requests.
        payload.model = request.model;
        payload.provider = { zdr: request.require_zdr !== false, data_collection: 'deny', require_parameters: true };
        delete payload.models;
        delete payload.route;
        delete payload.prompt_cache_key;
        delete payload.prompt_cache_retention;
        if (request.reasoning_effort) payload.reasoning = { effort: request.reasoning_effort };
        else delete payload.reasoning;
        // Many OpenRouter models do not advertise this request parameter. Pi's
        // execution queue is sequential independently of model-side batching.
        delete payload.parallel_tool_calls;
        return providerDefault(payload, info);
      },
    };
  },
};

export function providerAdapter(request) {
  if (['opencode', 'opencode-go'].includes(request.provider)) {
    const source = opencodeModels[request.provider].find(m => m.id === request.model);
    if (!source || !/^[A-Za-z0-9:_-]{1,200}$/.test(request.session_id || '')) throw new HarnessError('model');
    const base = request.provider === 'opencode-go' ? 'https://opencode.ai/zen/go' : 'https://opencode.ai/zen';
    const suffix = { 'anthropic-messages': 'messages', 'openai-responses': 'responses', 'openai-completions': 'chat/completions' }[source.api];
    const endpoint = `${base}/v1/${suffix}`;
    if (!suffix || request.endpoint !== endpoint) throw new HarnessError('endpoint');
    const model = { ...source };
    return { endpoint, model, headers: { 'User-Agent': 'Kindred/1.0', 'x-opencode-session': request.session_id },
      prepare(payload) {
        payload.model = request.model;
        delete payload.provider; delete payload.models; delete payload.route;
        delete payload.prompt_cache_key; delete payload.prompt_cache_retention;
        delete payload.parallel_tool_calls;
        return payload;
      },
    };
  }
  if (/^custom-[a-f0-9-]{36}$/.test(request.provider)) {
    const url = new URL(request.endpoint);
    if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.search || url.hash || !url.pathname.endsWith('/chat/completions')) throw new HarnessError('endpoint');
    const info = request.model_info;
    if (!info || info.id !== request.model || !Number.isInteger(info.context_window) || info.context_window < 1024) throw new HarnessError('model');
    return { endpoint: request.endpoint, headers: {}, model: {
      id: info.id, name: info.id, provider: request.provider, api: 'openai-completions',
      baseUrl: request.endpoint.slice(0, -'/chat/completions'.length), reasoning: info.reasoning === true,
      input: info.vision ? ['text', 'image'] : ['text'], contextWindow: info.context_window,
      maxTokens: outputLimit(info), cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      compat: { supportsStore: false, supportsDeveloperRole: false, maxTokensField: 'max_tokens' },
    }, prepare(payload) {
      payload.model = request.model;
      delete payload.provider; delete payload.models; delete payload.route;
      delete payload.prompt_cache_key; delete payload.prompt_cache_retention;
      delete payload.parallel_tool_calls;
      if (request.reasoning_effort) payload.reasoning_effort = request.reasoning_effort;
      // Keep the context-aware response budget even when the catalogue does not
      // advertise an output maximum. Never leave local generation unbounded.
      return payload;
    } };
  }
  const factory = Object.hasOwn(adapters, request.provider) && adapters[request.provider];
  if (!factory) throw new HarnessError('provider');
  return factory(request);
}

export function pruneScreenshots(messages) {
  let remaining = 2;
  return [...messages].reverse().map(message => {
    if (!Array.isArray(message.content)) return message;
    const content = [...message.content].reverse().map(part => {
      if (part.type !== 'image' || remaining-- > 0) return part;
      return { type: 'text', text: 'Earlier screenshot omitted; take a fresh screenshot if needed.' };
    }).reverse();
    return { ...message, content };
  }).reverse();
}

// Use Pi's injectable fetch hook. Credentials cannot follow redirects, and raw
// provider error bodies never enter the bridge, database, or visible task error.
export function guardedFetch(endpoint, failure, fetchImpl = globalThis.fetch, onUsage = () => {}, noAuth = false) {
  return async (input, init = {}) => {
    const request = new Request(input, { ...init, redirect: 'error' });
    const betaUrl = endpoint.endsWith('/messages') ? endpoint + '?beta=true' : endpoint;
    if (![endpoint, betaUrl].includes(request.url) || request.method !== 'POST') {
      failure.value = new HarnessError('endpoint'); throw failure.value;
    }
    if (noAuth) request.headers.delete("authorization");
    const body = await request.clone().arrayBuffer();
    if (body.byteLength > MAX_REQUEST) {
      failure.value = new HarnessError('request_limit'); throw failure.value;
    }
    let response;
    try { response = await fetchImpl(request); }
    catch { failure.value = new HarnessError(request.signal.aborted ? 'aborted' : 'transport'); throw failure.value; }
    if (!response.ok) {
      await response.body?.cancel();
      failure.value = new HarnessError('provider_http', response.status); throw failure.value;
    }
    let bytes = 0, partial = '';
    const decoder = new TextDecoder();
    const limited = response.body?.pipeThrough(new TransformStream({
      transform(chunk, controller) {
        bytes += chunk.byteLength;
        if (bytes > MAX_RESPONSE) {
          failure.value = new HarnessError('response_limit'); controller.error(failure.value); return;
        }
        partial += decoder.decode(chunk, { stream: true });
        let newline;
        while ((newline = partial.indexOf('\n')) >= 0) {
          const line = partial.slice(0, newline).trim(); partial = partial.slice(newline + 1);
          if (line.startsWith('data:') && line !== 'data: [DONE]') {
            try { const frame = JSON.parse(line.slice(5)); if (frame.usage) onUsage(frame.usage); } catch { /* SDK validates provider framing. */ }
          }
        }
        controller.enqueue(chunk);
      },
    }));
    return new Response(limited, { status: response.status, headers: response.headers });
  };
}

export async function runSession(request, callTool, emit, signal, options = {}) {
  if (!request || request.protocol !== 1 || !Number.isInteger(request.max_steps) || request.max_steps < 0 || request.max_steps > 100)
    throw new HarnessError('protocol');
  if (typeof request.api_key !== 'string' || !request.api_key) throw new HarnessError('auth');
  if (!['', 'low', 'medium', 'high'].includes(request.reasoning_effort || '')) throw new HarnessError('reasoning');
  if (!Array.isArray(request.tools) || !request.tools.length) throw new HarnessError('tools');
  const names = request.tools.map(tool => tool.name);
  if (new Set(names).size !== names.length || names.some(name => !/^[a-z][a-z0-9_]{0,63}$/.test(name))) throw new HarnessError('tools');
  const adapter = providerAdapter(request);
  if (request.reasoning_effort && !adapter.model.reasoning) throw new HarnessError('reasoning');
  const temporary = await mkdtemp(join(tmpdir(), 'kindred-pi-'));
  let session, currentInputId='initial';
  const timeoutMs = request.request_timeout_ms ?? 1_800_000;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 30000 || timeoutMs > 7_200_000) throw new HarnessError('budget');
  const failure = { value: null };
  let modelCalls = 0, toolCalls = 0, outputBytes = 0;
  const output = [];
  const custom = request.provider.startsWith('custom-');
  let recoveringResult = false, emptyEnding = false, finalAnswer = false;
  let progress = progressReporter(emit, custom);
  try {
    const modelRuntime = await ModelRuntime.create({
      credentials: new InMemoryCredentialStore(), modelsStore: new InMemoryModelsStore(),
      modelsPath: null, allowModelNetwork: false, refreshOnCreate: false,
    });
    modelRuntime.registerProvider(request.provider, {
      api: adapter.model.api, baseUrl: adapter.model.baseUrl,
      headers: adapter.headers, models: [adapter.model],
    });
    await modelRuntime.setRuntimeApiKey(request.provider, request.api_key);
    const settingsManager = SettingsManager.inMemory({
      enableAnalytics: false, enableInstallTelemetry: false, enableSkillCommands: false,
      packages: [], extensions: [], skills: [], prompts: [], themes: [], defaultTools: [],
      retry: { enabled: false, provider: { maxRetries: 0, timeoutMs } },
      compaction: { enabled: true, reserveTokens: Math.min(8192, Math.floor(adapter.model.contextWindow / 4)),
        keepRecentTokens: Math.min(12_000, Math.floor(adapter.model.contextWindow / 3)) },
      images: { autoResize: false, blockImages: false },
    });
    const loader = new DefaultResourceLoader({
      cwd: temporary, agentDir: temporary, settingsManager,
      noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true, noContextFiles: true,
      systemPromptOverride: () => request.instructions,
    });
    await loader.reload();
    const streamSimple = modelRuntime.streamSimple.bind(modelRuntime);
    modelRuntime.streamSimple = (model, context, streamOptions = {}) => {
      if (model.provider !== request.provider || model.id !== request.model) {
        failure.value = new HarnessError('model'); throw failure.value;
      }
      if (++modelCalls > request.max_steps + 1 && request.max_steps > 0) {
        failure.value = new HarnessError('budget'); throw failure.value;
      }
      if (custom) context = customToolContext(context, temporary);
      const requestId = modelCalls;
      progress = progressReporter(emit, custom);
      progress('waiting');
      emit({type:"usage",request_id:requestId,input_tokens:0,output_tokens:0,cached_tokens:0,cost:null,cost_source:"unknown",tokens_reported:false});
      const beforePayload = streamOptions.onPayload;
      return streamSimple(model, context, {
        ...streamOptions, apiKey: request.api_key, cacheRetention: 'none', transport: 'sse',
        maxRetries: 0, timeoutMs, maxTokens: responseBudget(adapter.model, context),
        headers: { ...streamOptions.headers, ...adapter.headers },
        fetch: guardedFetch(adapter.endpoint, failure, options.fetch, usage => {
          const input = usage.prompt_tokens, output = usage.completion_tokens;
          if (![input, output].every(n => Number.isSafeInteger(n) && n >= 0 && n <= 1e9)) return;
          const cached = usage.prompt_tokens_details?.cached_tokens || 0;
          if (!Number.isSafeInteger(cached) || cached < 0 || cached > input) return;
          let cost = null, source = 'unknown';
          if (typeof usage.cost === 'number' && Number.isFinite(usage.cost) && usage.cost >= 0) { cost = usage.cost; source = 'reported'; }
          else if ([request.model_info.input_cost, request.model_info.output_cost].every(n => typeof n === 'number' && Number.isFinite(n) && n >= 0)) {
            cost = (input * request.model_info.input_cost + output * request.model_info.output_cost) / 1e6; source = 'estimated';
          }
          emit({ type: 'usage', request_id: requestId, input_tokens: input, output_tokens: output, cached_tokens: cached, cost, cost_source: source, tokens_reported:true });
        }, request.provider.startsWith("custom-") && request.model_info.no_auth === true),
        onPayload: async payload => {
          const prepared = adapter.prepare((await beforePayload?.(payload, model)) || payload);
          if (Buffer.byteLength(JSON.stringify(prepared)) > MAX_REQUEST) {
            failure.value = new HarnessError('request_limit'); throw failure.value;
          }
          return prepared;
        },
      });
    };
    const customTools = request.tools.map(tool => ({
      name: tool.name, label: tool.name, description: tool.description + (custom && tool.name === 'skills_list' ? ' Returns metadata only; use skill_load(name) to read the selected skill instructions before using or editing them.' : ''), parameters: tool.inputSchema,
      executionMode: 'sequential',
      execute: async (id, args, toolSignal) => {
        if (recoveringResult) throw new HarnessError('empty_result');
        if (++toolCalls > request.max_steps && request.max_steps > 0) { failure.value = new HarnessError('budget'); throw failure.value; }
        if (signal?.aborted || toolSignal?.aborted) throw new HarnessError('aborted');
        const result = await callTool(id, tool.name, args, toolSignal);
        if(result.kindred_followup){
          const next=result.kindred_followup;
          if(request.followup_protocol!==1||currentInputId!=='initial'||typeof next.offer_id!=='string'||typeof next.prompt!=='string'||!next.prompt.trim()) throw new HarnessError('protocol');
          await session.steer(next.prompt);currentInputId=next.offer_id;
        }
        const text = custom && tool.name === 'skills_list' && result.failed !== true ? customSkillListing(result.text || '') : result.text || '';
        const content = [{ type: 'text', text }];
        if (result.image) {
          const match = /^data:(image\/(?:png|jpeg|gif|webp));base64,([A-Za-z0-9+/=]+)$/.exec(result.image);
          if (!match || match[2].length > 7 * 1024 * 1024) throw new HarnessError('image');
          content.push({ type: 'image', mimeType: match[1], data: match[2] });
        }
        return { content, details: { failed: result.failed === true } };
      },
    }));
    ({ session } = await createAgentSession({
      cwd: temporary, agentDir: temporary, modelRuntime, model: adapter.model,
      thinkingLevel: request.reasoning_effort || 'off',
      tools: names, noTools: 'builtin', customTools, resourceLoader: loader,
      sessionManager: SessionManager.inMemory(temporary), settingsManager,
    }));
    if (session.getActiveToolNames().sort().join(',') !== [...names].sort().join(',')) throw new HarnessError('tools');
    session.agent.toolExecution = 'sequential';
    const transformContext = session.agent.transformContext;
    session.agent.transformContext = async (messages, toolSignal) => pruneScreenshots(await transformContext?.(messages, toolSignal) || messages);
    const afterToolCall = session.agent.afterToolCall;
    session.agent.afterToolCall = async (context, toolSignal) => {
      const original = await afterToolCall?.(context, toolSignal);
      return { ...original, isError: context.isError || original?.isError || context.result.details?.failed === true };
    };
    const shouldStop = session.agent.shouldStopAfterTurn;
    session.agent.shouldStopAfterTurn = async (context, toolSignal) => recoveringResult || Boolean(failure.value) || Boolean(await shouldStop?.(context, toolSignal));
    const abort = () => { session.agent.abort(); session.abortCompaction(); };
    signal?.addEventListener('abort', abort, { once: true });
    session.subscribe(event => {
      // Report phases, never private reasoning or partially formed tool arguments.
      // Hosted adapters retain phase-only reporting; custom adapters also report real activity.
      if (event.type === 'message_update') {
        const kind = event.assistantMessageEvent?.type;
        if (kind === 'thinking_delta') progress('thinking', event.assistantMessageEvent.delta);
        if (kind === 'text_delta') progress('writing', event.assistantMessageEvent.delta);
        if (kind === 'toolcall_start' || kind === 'toolcall_delta') progress('preparing', event.assistantMessageEvent.delta);
      }
      if (event.type === 'message_end' && event.message.role === 'assistant') {
        if (['opencode', 'opencode-go'].includes(request.provider) && adapter.model.api !== 'openai-completions' && event.message.usage) {
          const usage = event.message.usage;
          if (usage.input + usage.output + usage.cacheRead > 0 && [usage.input, usage.output, usage.cacheRead].every(n => Number.isSafeInteger(n) && n >= 0))
            emit({ type: 'usage', request_id: modelCalls, input_tokens: usage.input + usage.cacheRead + (usage.cacheWrite || 0), output_tokens: usage.output, cached_tokens: usage.cacheRead, cost: null, cost_source: 'unknown', tokens_reported: true });
        }

        if (['error', 'aborted', 'length'].includes(event.message.stopReason)) {
          failure.value ||= new HarnessError(event.message.stopReason === 'length' ? (custom ? 'custom_truncated' : 'truncated') : 'provider_response');
          return;
        }
        const text = event.message.content.filter(part => part.type === 'text').map(part => part.text).join('');
        const hasTools = event.message.content.some(part => part.type === 'toolCall');
        emptyEnding = !text.trim() && !hasTools;
        finalAnswer = !!text.trim() && !hasTools;
        if (text) {
          outputBytes += Buffer.byteLength(text);
          if (outputBytes > MAX_OUTPUT) { failure.value = new HarnessError('output_limit'); abort(); return; }
          output.push(text); emit({ type: 'assistant', text,
            phase: event.message.content.some(part => part.type === 'toolCall') ? 'commentary' : 'final_answer' });
        }
      }
      if (event.type === 'compaction_start') emit({ type: 'context', state: 'compacting' });
      if (event.type === 'compaction_end') emit({ type: 'context', state: event.errorMessage || event.aborted ? 'failed' : 'compacted', ...(!event.errorMessage && !event.aborted && typeof event.result?.summary === 'string' ? { summary: event.result.summary.slice(0, 24000) } : {}) });
    });
    emit({ type: 'ready', protocol: 1, sdk_version: SDK_VERSION, provider: request.provider, model: request.model, native_followup:request.followup_protocol===1 });
    if (signal?.aborted) throw new HarnessError('aborted');
    try {
      let prompt=request.prompt;
      while (true) {
        emptyEnding = false;
        await session.prompt(prompt, { expandPromptTemplates: false });
        if (custom && emptyEnding && !failure.value && !signal?.aborted) {
          // One same-context, tools-disabled summary. Do not replay any action.
          recoveringResult = true;
          session.setActiveToolsByName([]);
          try {
            await session.prompt('Your last response ended without a user-visible answer. Give a concise final response based only on the existing conversation and tool results. State what was completed and what remains unresolved. Do not perform or repeat any actions, and do not claim unverified success.', { expandPromptTemplates: false });
            if (!finalAnswer) failure.value ||= new HarnessError('empty_result');
          } finally {
            recoveringResult = false;
            session.setActiveToolsByName(names);
          }
        }
        if(signal?.aborted) throw new HarnessError('aborted');
        if (failure.value) throw failure.value;
        if (!options.nextTurn) break;
        const next=await options.nextTurn(currentInputId);
        if (!next) break;
        if (typeof next.offer_id!=='string'||typeof next.prompt!=='string'||!next.prompt.trim()) throw new HarnessError('protocol');
        currentInputId=next.offer_id;prompt=next.prompt;
      }
    }
    catch { throw signal?.aborted ? new HarnessError('aborted') : failure.value || new HarnessError('harness'); }
    finally { signal?.removeEventListener('abort', abort); }
    if (signal?.aborted) throw new HarnessError('aborted');
    if (failure.value) throw failure.value;
    return { output: output.join('\n'), model_calls: modelCalls, tool_calls: toolCalls };
  } finally {
    session?.dispose();
    await rm(temporary, { recursive: true, force: true });
  }
}
