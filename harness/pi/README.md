# Kindred's Pi harness

OpenRouter, custom-provider, and OpenCode Go/Zen tasks use the actual **Pi coding-agent SDK 0.85.1**, including its agent
loop, provider message conversion, argument validation, tool-result pairing,
sequential tool execution and automatic context compaction. This replaces the
previous Rust OpenRouter conversation loop. OpenAI subscription tasks continue
through the official Codex app-server inside the VM.

Pi runs as a short-lived Node process on the Linux server, started only for an
API-provider task. It exposes Kindred's registered tools through a bounded JSON
stdin/stdout bridge. Every action returns to the same Rust dispatcher used by
Codex, preserving approval decisions, named app accounts, VM isolation, screen
leases, screenshot attachments and human takeover. Pi's built-in host file and
shell tools, resource discovery, plugins and analytics are disabled. Node remains
an application dependency, not a security sandbox.

## Install

From a source checkout, on the Linux host:

```sh
sudo sh deploy/install-pi.sh
sudo -u kindred /opt/kindred/pi/current/node/bin/node /opt/kindred/pi/current/worker.mjs --check
```

The installer fetches Node 24.14.0 from nodejs.org, verifies the archive against
its HTTPS SHA-256 manifest, and installs the exact npm lockfile with lifecycle
scripts disabled. It supports Linux x86_64 and arm64. Versions install into
separate directories; the `current` link changes after the new worker passes its
check. Install the harness before deploying a Rust server version that requires
it. Missing or incompatible harness installations fail visibly without routing
the task through another provider. The Windows and macOS clients need no Node
installation for this feature.

The default server configuration is:

```toml
[pi]
node_binary = "/opt/kindred/pi/current/node/bin/node"
worker_script = "/opt/kindred/pi/current/worker.mjs"
```

## Provider and tool boundaries

- The Rust adapter reads the selected model's current OpenRouter catalog metadata
  and passes its exact ID and capabilities to Pi. Unknown models fail visibly.
- OpenRouter credentials arrive only in the worker's initial stdin frame. They
  are held in memory, never supplied in arguments, inherited environment, session
  files or model instructions. Settings remain in Kindred's existing protected
  credential store.
- The selected model is pinned for generation and compaction. Requests retain
  `provider.zdr` according to the server preference, `data_collection=deny`,
  `require_parameters=true` and `X-OpenRouter-Cache: false`. The provider transport
  rejects redirects, bounds requests and streams, and suppresses upstream error
  bodies. It never selects a different model or provider on failure.
- Calls execute sequentially. Kindred's action budget remains authoritative and
  also bounds Pi's model requests. Automatic retries are disabled so the harness
  cannot silently repeat an uncertain task. Task cancellation drops the worker;
  guest-operation recovery follows the existing scheduler behavior.
- A human subtask holds the Pi tool call open while the Rust scheduler releases
  the screen lease and pauses active run time. Done returns the result to that
  same call and conversation. Cancellation cannot resume it later.
- Pi credentials, runtime settings and session resources remain in memory, with
  the isolated temporary directory removed at completion. Opt-in durable context
  checkpoints allow Kindred's protected store to retain exact SDK messages across
  tasks; legacy callers still bootstrap from bounded chat history. Pi owns
  compaction of both restored and newly generated context.
- Only the two most recent inspection images are included in provider context.
  Explicit screenshot attachments remain available through Kindred's authenticated
  chat endpoint, regardless of model-context pruning.

## OpenCode Go and Zen

See `../../docs/OPENCODE_PROVIDERS.md` for account setup, model coverage, testing and release requirements. Both use the official API-key flow and retain distinct endpoints and credentials. Harness installation 0.44.2 or newer is required.

Custom/local OpenAI-compatible providers also use this harness, with owner-saved endpoints and protected credentials. Neither path enables Pi's built-in computer tools.

## Tests

```sh
cd harness/pi
npm ci --ignore-scripts
npm test
```

The tests exercise Pi itself with synthetic streamed provider responses, including
tool calls, invalid arguments, unavailable host tools, images, cancellation,
human hand-back, action budgets, compaction, privacy policy and sanitized failures.
They do not consume a provider account or prove live OpenRouter inference.

Rust protocol tests launch the real worker. Set `KINDRED_PI_TEST_NODE` to a Node
22.19+ executable if `node` is not on PATH. `KINDRED_PI_TEST_WORKER` optionally
selects a harness copy installed with the appropriate platform dependencies.

Sources: [Pi SDK](https://pi.dev/docs/latest/sdk),
[custom models](https://pi.dev/docs/latest/models),
[custom providers](https://pi.dev/docs/latest/custom-provider).


### Model output budgets (0.44.2)

The former 4,096-token ceiling is removed. OpenCode uses bundled per-model limits; OpenRouter forwards its catalogue's maximum completion tokens; custom catalogues retain max_output_tokens/max_completion_tokens when advertised. If a custom/OpenRouter limit is unknown, the outgoing completion request omits its output-limit parameter and lets the provider apply its default. No guessed 4K maximum is sent.

Known limits are reduced only to leave estimated space for the current conversation, tools, system instructions and a margin of up to 1,024 tokens. This is an estimate, not a tokenizer guarantee. Pi still owns context compaction. Request timeouts now follow the configured server task timeout, and cancellation remains active. Transport/output byte guards remain (64 MiB streamed response; 4 MiB accumulated visible output); they are not token budgets. This does not change CLI subscription harness limits or make truncated tool calls safe to execute.

Release must install harness 0.44.2 alongside the matching server. Existing immutable 0.44.1 workers still contain the old cap. No deployment is implied by a source push.

### Continuity, images and unlimited tool steps (0.44.3)

Install 0.44.3 with Kindred 0.58.0. This immutable harness revision adds persisted compaction summaries, supported image MIME types, and max_steps=0 support. Existing 0.44.2 installations do not contain these changes.

### Early progress and reference loading (0.44.4)

The bridge emits bounded phase-only progress while the provider response is still streaming: waiting, thinking, writing, and preparing an action. Each phase is emitted at most once per model request. Private reasoning, partial tool arguments and incomplete text are not persisted as chat messages; finished assistant messages retain their existing ordering. Activity uses these phases, with human/approval waits taking precedence. Long runs no longer have a separate fixed bridge-frame count limit; cancellation, task timeouts and byte guards still apply.

API models load the core operating instructions and live task context, retrieving the expanded reference chapters through `kindred_guide` as needed, including on large-context models. Bot instructions, memory and capacity-based history budgets are unchanged. Native data views should retrieve a small verified snapshot first and use `visual_panel`, rather than generating a custom app or speculative multi-source scraper. Model selection, reasoning settings and default unlimited tool calls are unchanged.

Release packaging must stage harness 0.44.4 with the matching server. Do not activate it against an older server: older bridges do not recognize progress frames. The upstream Pi SDK remains 0.85.1. This source change does not deploy or activate the harness.

### Expanded tool catalogs (0.44.5)

Removes the stale 64 registered-tool cap so enabling local computer access does
not prevent task startup. Unique tool names, schema validation, transport limits
and dispatcher permissions remain enforced. Regression coverage submits 80 tools
with unlimited calls. Stage 0.44.5 with the matching server for the next release;
the 0.44.4 progress-frame compatibility requirement above still applies.


### Earlier compaction for custom providers

Custom providers reserve up to 32,768 tokens (bounded to one quarter of smaller
context windows) for automatic compaction. With a 262,144-token window, Pi checks
for compaction above 229,376 tokens. Hosted providers retain the existing reserve
of up to 8,192 tokens. Recent-history retention remains up to 12,000 tokens.

When no valid assistant usage is available, Pi estimates messages only. The custom
settings adapter additionally reserves the estimated system prompt and serialized
tool definitions, including Kindred's VM environment instructions. This lowers the
message-only threshold by that fixed input cost. Once valid usage is available,
that extra deduction is removed: reported input already includes system/tools,
and Pi accounts for the response and subsequent tool results. No duplicate usage
or synthetic messages are inserted into the conversation.

This remains approximate, and Pi needs a safe history cut point: an oversized
single recent tool result cannot always be compacted immediately. The change does
not add retries for generic HTTP 400 responses or change the configured output
ceiling. Release packaging must include the updated harness; a source push does
not change running workers.

### Durable context protocol (0.44.6 integration candidate)

Both `--check` and the negotiated `ready` frame advertise `context_protocol: 1`.
A caller opts in with `context_protocol: 1` on `start`, supplies an optional
`context_checkpoint`, and may supply a current `live_context` string. The harness
restores prior SDK messages, then appends live context and the current prompt as
one new user input. The prompt must contain only the new input, not a repeated
chat transcript. Stable instructions and tools remain supplied by Rust each run.
The SDK's appended ephemeral host-cwd suffix is removed from every provider's
request, so the temporary directory does not churn the system prompt.

A checkpoint is `{schema:1, sdk_version:"0.85.1", messages:[...], estimated_tokens:N}`.
Messages retain SDK user/assistant/toolResult blocks, provider signatures, tool
IDs/results, usage and timestamps. After compaction they contain its summary and
only the retained active context. Restoration uses public in-memory SessionManager
entry import; it does not resume an old generation or execute prior tool calls.
Transport diagnostics and arbitrary tool-details metadata are not persisted.

At each successfully completed input boundary, before `turn_complete` or final
`complete`, the harness emits:

- `{type:"checkpoint", input_id:"initial"|offer_id, state:"ready", checkpoint:{...}}`
- Or `{type:"checkpoint", input_id:..., state:"unavailable", reason:"limit"|"secret"|"invalid"}`.

Bounds are 4 MiB encoded checkpoint, 2,048 messages and 1 MiB live context.
Malformed, incompatible, oversized or unpaired restores fail before provider or
tool execution with `context_checkpoint`, `context_limit` or `context_secret`.
Unknown schema/SDK versions must be reset by Rust before sending. Exports that
exceed limits or contain the configured API credential disable reuse without
failing completed work; no message or tool pair is silently truncated. This
known-credential check is defense in depth, not a detector for arbitrary secrets
in owner-supplied conversation text. Credentials/settings/headers are never part
of the checkpoint schema; protected access and storage remain Rust's responsibility.

Rust owns conversation isolation, leasing, generation/CAS, compatibility identity
(provider/model/endpoint/account/instructions/tool schema/policy), and cursor/reset
rules. It must stage frames until successful completion plus a clean process exit.
Failure, interruption, deferred work or unavailable export invalidates reuse for
the next task; authoritative history bootstraps it again without replaying actions.
A completed earlier follow-up frame is not proof that a later input completed.
`estimated_tokens` is approximate SDK budgeting data, never a privacy or trust check.

ZDR, `data_collection=deny`, `X-OpenRouter-Cache:false`, `cacheRetention:none`,
response-storage restrictions and disabled automatic retries remain unchanged.
This protocol does not enable provider response caching or retained prompt caches.
Install immutable harness 0.44.6 before server activation. SDK_VERSION remains
the updater-compatible literal in session.mjs; checkpoint validation receives it
from that single source. A caller must check worker capability before sending a
start frame with the split stable/live envelope, since old workers ignore it.
