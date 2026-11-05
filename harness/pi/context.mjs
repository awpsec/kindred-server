// Durable conversation data only. Credentials, runtime settings, instructions,
// transport objects and temporary resource paths are never checkpoint fields.
import { estimateTokens, SessionManager } from '@earendil-works/pi-coding-agent';

export const MAX_CHECKPOINT_BYTES = 4 * 1024 * 1024;
export const MAX_CHECKPOINT_MESSAGES = 2048;
export class ContextError extends Error {
  constructor(code) { super(code); this.code = code; }
}
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const number = value => Number.isFinite(value) && value >= 0;
const integer = value => Number.isSafeInteger(value) && value >= 0;
function requireValue(value) { if (!value) throw new ContextError('context_checkpoint'); }
function keys(value, allowed) { requireValue(object(value) && Object.keys(value).every(key => allowed.includes(key))); }
function optional(value, name, predicate) { if (Object.hasOwn(value, name)) requireValue(predicate(value[name])); }
function jsonTree(value, secret, depth = 0) {
  requireValue(depth <= 64);
  if (typeof value === 'string') {
    if (secret && value.includes(secret)) throw new ContextError('context_secret');
  } else if (Array.isArray(value)) value.forEach(item => jsonTree(item, secret, depth + 1));
  else if (object(value)) {
    for (const [key, item] of Object.entries(value)) {
      requireValue(!['__proto__', 'constructor', 'prototype'].includes(key));
      jsonTree(item, secret, depth + 1);
    }
  } else requireValue(value === null || typeof value === 'boolean' || Number.isFinite(value));
}
function usage(value) {
  keys(value, ['input', 'output', 'cacheRead', 'cacheWrite', 'cacheWrite1h', 'reasoning', 'totalTokens', 'cost']);
  for (const field of ['input', 'output', 'cacheRead', 'cacheWrite', 'totalTokens']) requireValue(integer(value[field]));
  for (const field of ['cacheWrite1h', 'reasoning']) optional(value, field, integer);
  keys(value.cost, ['input', 'output', 'cacheRead', 'cacheWrite', 'total']);
  for (const field of ['input', 'output', 'cacheRead', 'cacheWrite', 'total']) requireValue(number(value.cost[field]));
}
function content(value, role) {
  if (role === 'user' && typeof value === 'string') return;
  requireValue(Array.isArray(value));
  for (const part of value) {
    requireValue(object(part));
    if (part.type === 'text') {
      keys(part, ['type', 'text', 'textSignature']); requireValue(typeof part.text === 'string');
      optional(part, 'textSignature', x => typeof x === 'string');
    } else if (part.type === 'image' && role !== 'assistant') {
      keys(part, ['type', 'data', 'mimeType']);
      requireValue(typeof part.data === 'string' && /^[A-Za-z0-9+/=]+$/.test(part.data));
      requireValue(['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(part.mimeType));
    } else if (part.type === 'thinking' && role === 'assistant') {
      keys(part, ['type', 'thinking', 'thinkingSignature', 'redacted']); requireValue(typeof part.thinking === 'string');
      optional(part, 'thinkingSignature', x => typeof x === 'string'); optional(part, 'redacted', x => typeof x === 'boolean');
    } else if (part.type === 'toolCall' && role === 'assistant') {
      keys(part, ['type', 'id', 'name', 'arguments', 'thoughtSignature', 'namespace']);
      requireValue(typeof part.id === 'string' && part.id.length > 0 && part.id.length <= 1024);
      requireValue(typeof part.name === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(part.name) && object(part.arguments));
      optional(part, 'thoughtSignature', x => typeof x === 'string'); optional(part, 'namespace', x => typeof x === 'string');
    } else throw new ContextError('context_checkpoint');
  }
}
const ASSISTANT_FIELDS = ['role', 'content', 'api', 'provider', 'model', 'responseModel', 'responseId', 'providerThinkingLevel', 'usage', 'stopReason', 'rawStopReason', 'endTurn', 'timestamp'];
function validateMessages(messages, request, api) {
  requireValue(Array.isArray(messages) && messages.length > 0);
  if (messages.length > MAX_CHECKPOINT_MESSAGES) throw new ContextError('context_limit');
  const pending = new Map();
  for (const [index, message] of messages.entries()) {
    requireValue(object(message) && integer(message.timestamp) && message.timestamp <= 8640000000000000);
    if (message.role === 'compactionSummary') {
      keys(message, ['role', 'summary', 'tokensBefore', 'timestamp']);
      requireValue(index === 0 && typeof message.summary === 'string' && integer(message.tokensBefore));
      continue;
    }
    content(message.content, message.role);
    if (message.role === 'user') {
      keys(message, ['role', 'content', 'timestamp']); requireValue(pending.size === 0);
    } else if (message.role === 'assistant') {
      keys(message, ASSISTANT_FIELDS); requireValue(pending.size === 0);
      requireValue(message.api === api && message.provider === request.provider && message.model === request.model);
      requireValue(['stop', 'toolUse'].includes(message.stopReason)); usage(message.usage);
      for (const key of ['responseModel', 'responseId', 'providerThinkingLevel', 'rawStopReason']) optional(message, key, x => typeof x === 'string');
      optional(message, 'endTurn', x => typeof x === 'boolean');
      for (const part of message.content.filter(part => part.type === 'toolCall')) {
        requireValue(!pending.has(part.id)); pending.set(part.id, part.name);
      }
    } else if (message.role === 'toolResult') {
      keys(message, ['role', 'content', 'toolCallId', 'toolName', 'isError', 'timestamp', 'details']);
      requireValue(typeof message.toolCallId === 'string' && pending.get(message.toolCallId) === message.toolName && typeof message.isError === 'boolean');
      if (message.details !== undefined) { keys(message.details, ['failed']); optional(message.details, 'failed', x => typeof x === 'boolean'); }
      pending.delete(message.toolCallId);
    } else throw new ContextError('context_checkpoint');
  }
  requireValue(pending.size === 0 && !['user', 'compactionSummary'].includes(messages.at(-1).role));
}
export function validateCheckpoint(checkpoint, request, api, sdkVersion) {
  let encoded;
  try { encoded = JSON.stringify(checkpoint); } catch { throw new ContextError('context_checkpoint'); }
  requireValue(typeof encoded === 'string');
  if (Buffer.byteLength(encoded) > MAX_CHECKPOINT_BYTES) throw new ContextError('context_limit');
  keys(checkpoint, ['schema', 'sdk_version', 'messages', 'estimated_tokens']);
  requireValue(checkpoint.schema === 1 && checkpoint.sdk_version === sdkVersion);
  optional(checkpoint, 'estimated_tokens', integer);
  jsonTree(checkpoint, request.api_key);
  validateMessages(checkpoint.messages, request, api);
  return JSON.parse(encoded);
}
export function readCheckpoint(request, api, sdkVersion) {
  if (request.context_protocol === undefined) {
    requireValue(request.context_checkpoint === undefined && request.live_context === undefined);
    return null;
  }
  requireValue(request.context_protocol === 1);
  optional(request, 'live_context', x => typeof x === 'string' && Buffer.byteLength(x) <= 1024 * 1024);
  return request.context_checkpoint == null ? null : validateCheckpoint(request.context_checkpoint, request, api, sdkVersion);
}
export function restoreSessionManager(cwd, checkpoint) {
  if (!checkpoint) return SessionManager.inMemory(cwd);
  // Public SDK entry import keeps both agent history and compaction history in
  // sync. Rehydration never calls a tool or resumes an unfinished generation.
  const entries = checkpoint.messages.map((message, index) => {
    const base = { id: `context${index}`, parentId: index ? `context${index - 1}` : null, timestamp: new Date(message.timestamp).toISOString() };
    return message.role === 'compactionSummary'
      ? { ...base, type: 'compaction', summary: message.summary, tokensBefore: message.tokensBefore, firstKeptEntryId: base.id }
      : { ...base, type: 'message', message };
  });
  return SessionManager.inMemory(cwd, undefined, entries);
}
export function checkpointFrame(messages, request, api, inputId, sdkVersion) {
  const base = { type: 'checkpoint', input_id: inputId };
  try {
    // SDK diagnostics are transport/debug metadata, not provider conversation.
    // Arbitrary tool details must not become a credential/config storage channel.
    const selected = messages.map(message => {
      if (message.role === 'assistant') return Object.fromEntries(ASSISTANT_FIELDS.filter(key => message[key] !== undefined).map(key => [key, message[key]]));
      if (message.role === 'toolResult') {
        const { details, ...rest } = message;
        return { ...rest, ...(typeof details?.failed === 'boolean' ? { details: { failed: details.failed } } : {}) };
      }
      return message;
    });
    const checkpoint = { schema: 1, sdk_version: sdkVersion, messages: selected,
      estimated_tokens: Math.ceil(selected.reduce((sum, message) => sum + estimateTokens(message), 0)) };
    return { ...base, state: 'ready', checkpoint: validateCheckpoint(JSON.parse(JSON.stringify(checkpoint)), request, api, sdkVersion) };
  } catch (error) {
    return { ...base, state: 'unavailable', reason: error?.code === 'context_secret' ? 'secret' : error?.code === 'context_limit' ? 'limit' : 'invalid' };
  }
}
