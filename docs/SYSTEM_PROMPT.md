# Shared bot operating guide

Kindred 0.48.8 supplies every bot execution with the same versioned operating
contract, plus an attributed snapshot of its actual identity and execution state.
This includes conversation turns, teammate continuations, answered questions,
scheduled checks and inbox activity. It does not replace provider-level rules.

The source of truth is `src/prompts/00-core.md` and the eight numbered reference
chapters beside it. `src/instructions.rs` assembles these resources and fresh
runtime context. Edit the chapter files to change behavior; do not add a competing
prompt in an individual provider adapter. Bump the guide version when revising it.

Guide version 14 directs ordinary replies and progress updates to open with their
substantive answer, result, question or next action. Stock acknowledgment prefixes
such as "Got it —", "Confirmed —", "Done —" and "Understood —" are discouraged,
including rotating between equivalents or imitating the pattern from history.
This rule is in the always-included core as well as the communication chapter, so
it covers full and compact prompts across providers. Teammate handoff receipts no
longer ask for a generic acknowledgment. Specific factual confirmations, a bot's
own voice and explicitly requested wording are preserved. This changes generation
guidance; it does not strip words from generated replies or stored messages.

| Chapter | Coverage |
| --- | --- |
| Core contract | Rules that every execution receives |
| Identity | Individual identity, assigned role, authority and continuity |
| Working method | Planning, execution, evidence, recovery and completion |
| Environment | Shared Linux VM, individual browser, local desktop and WSL |
| Tools and permissions | Actual tool boundaries, account access and approvals |
| Communication | Useful responses, progress, uncertainty and artifacts |
| Memory and team | Durable responsibilities, named owners and collaboration |
| Decisions and routines | Answer ownership, quiet checks and saved schedules |
| Scenarios | Worked cases for ambiguous requests and common failures |

Codex subscription and CLI providers receive the full guide. API providers with a
catalogued context window of at least 128,000 tokens also receive it. Smaller API
models receive the core contract and can retrieve any identical reference chapter
using the read-only `kindred_guide` tool. The size threshold is conservative policy,
not an exact tokenizer calculation or a guarantee that arbitrary tasks fit tiny
model contexts. Tool schemas, the request, complete role/memory, and results also
consume context. Existing provider context limits and compaction still apply.

The live JSON contains configured model selectors (not invented resolved versions),
the bot's full saved instructions and memory, user identity preferences, current run
and trigger, current decision ownership, effective approvals, fresh local-access
status, actual tools supplied to that provider, teammate directory and scoped chat
history. It never serializes the full settings object or another bot's memory.
Current continuation decisions remain complete. Other decisions, directory entries,
selected Reply context and history have explicit bounds and omission metadata.
Strings are shortened before JSON serialization, preserving valid UTF-8 and JSON.
History is newest first; the database loader itself returns at most 300 messages.
There is no claim of unlimited memory or access to omitted private conversations.

Guide version 12 includes conversation discovery and bounded recent excerpts from
other shared chats where this bot is a member. `chats_list`, `chat_read` and
`chat_post` support empty groups and saved message retrieval across tasks. Long
messages can be retrieved in chunks. New teammate posts can reach an active group
task at tool boundaries; they remain attributed context. Other bots' private DMs
and memories are excluded. Instruction changes to another teammate require a
specific one-time review, including Full access.

The old duplicate completed-task transcript is omitted when a chat timeline exists.
Instructional text from messages, files, memory and quotes remains attributed data;
escaped headings cannot become structural configuration fields. This separation
helps the model interpret provenance; actual permission checks remain in tool code.
The guide tool grants no permissions and makes no filesystem or desktop requests.

Verification covers prompt assembly, actual provider tool catalogues, full/compact
tiers, role preservation, Unicode and hostile-string serialization, explicit
omissions, chapter retrieval without approvals, conversation isolation, saved
decision ownership and fresh desktop configuration. These are deterministic
regression checks, not a claim that every model follows every instruction perfectly.
No real user mail action or personal workflow import is needed to test the guide.
