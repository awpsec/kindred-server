// One isolated process per run. Only framed JSON goes on stdout; credentials are
// delivered on stdin, never command-line arguments or environment variables.
import { runSession, HarnessError, SDK_VERSION } from './session.mjs';

const MAX_FRAME = 8 * 1024 * 1024;
const pending = new Map();
const controller = new AbortController();
let initialized = false, completed = false, buffer = Buffer.alloc(0);
function send(frame) {
  const encoded = JSON.stringify(frame) + '\n';
  if (Buffer.byteLength(encoded) > MAX_FRAME) throw new HarnessError('frame_limit');
  process.stdout.write(encoded);
}
function stop() {
  controller.abort();
  for (const entry of pending.values()) entry.reject(new HarnessError('aborted'));
  pending.clear();
}
async function start(request) {
  try {
    const result = await runSession(request, (id, name, args, signal) => new Promise((resolve, reject) => {
      if (pending.has(id) || pending.size >= 1) { reject(new HarnessError('protocol')); return; }
      const abort = () => { pending.delete(id); reject(new HarnessError('aborted')); };
      signal?.addEventListener('abort', abort, { once: true });
      pending.set(id, { resolve: result => { signal?.removeEventListener('abort', abort); resolve(result); }, reject });
      send({ type: 'tool_call', id, name, args });
    }), send, controller.signal);
    send({ type: 'complete', ...result });
  } catch (error) {
    send({ type: 'error', code: error instanceof HarnessError ? error.code : 'harness',
      ...(error instanceof HarnessError && error.status ? { status: error.status } : {}) });
  } finally {
    completed = true; stop(); process.stdin.pause();
    process.stdout.write('', () => process.exit(0));
  }
}
function receive(frame) {
  if (!initialized) {
    if (frame.type !== 'start') throw new HarnessError('protocol');
    initialized = true; void start(frame); return;
  }
  if (frame.type === 'abort') { stop(); return; }
  if (frame.type !== 'tool_result' || !pending.has(frame.id)) throw new HarnessError('protocol');
  const entry = pending.get(frame.id); pending.delete(frame.id); entry.resolve(frame.result);
}
if (process.argv.includes('--check')) {
  send({ protocol: 1, sdk_version: SDK_VERSION, providers: ['openrouter', 'openai-compatible', 'opencode', 'opencode-go'] });
} else {
  process.stdin.on('data', chunk => {
    try {
      buffer = Buffer.concat([buffer, chunk]);
      let newline;
      while ((newline = buffer.indexOf(10)) !== -1) {
        if (newline > MAX_FRAME) throw new HarnessError('frame_limit');
        const line = buffer.subarray(0, newline); buffer = buffer.subarray(newline + 1);
        receive(JSON.parse(line.toString('utf8')));
      }
      if (buffer.length > MAX_FRAME) throw new HarnessError('frame_limit');
    } catch {
      stop(); send({ type: 'error', code: 'protocol' }); process.exitCode = 1; process.stdin.destroy();
    }
  });
  process.stdin.on('end', () => { if (!completed) stop(); });
  process.on('SIGTERM', stop);
  process.on('SIGINT', stop);
}
