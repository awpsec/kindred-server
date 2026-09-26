# Long-term continuity: development implementation and review

Status: unreleased source development. No deployment, release, production data
migration, paid CI or live provider evaluation was performed for this change.

## Reconciliation with existing Kindred

Kindred already owns bot identity, role instructions, private memory, messages,
runs, events, approvals, questions, routines, reminders, managed command receipts,
revisioned continuity notes and transfers. Codex starts ephemeral threads; Claude
CLI disables native persistence; Pi sessions are in memory. These provider choices
are preserved. Each invocation reconstructs the same bot and visible conversation.
There is no new domain-specific project/engagement hierarchy or user session UI.

The implementation extends `continuity.rs` and those existing records. It adds
metadata and source dependencies to the existing notes, not a second independent
knowledge service. SQLite FTS5 ships with Kindred's bundled SQLite. No connector,
external vector database, model subscription or new service account is needed.

Concrete gaps addressed:

* Legacy search could synchronously rebuild its entire index on startup.
* Access to multiple rooms was treated as sufficient to inject their excerpts.
* Notes had revisions but no structured source provenance/invalidation.
* Search ranking and recent notes could bury dormant commitments.
* Context tiers did not account for the combined prompt and tool catalogue.
* Workspace transfer did not retain managed command receipts/waits.

## Implemented behavior

### Persistence, sessions and reconstruction

Bots, IDs, visible chats, credentials, routines, artifacts and execution history
remain in their original tables. `continuity_sessions` records a run's source
high-water mark, provider/model selectors, conversation and source epoch.
`continuity_dependencies` records the allowed source conversations and their
versions in the same transaction. Recording a session starts no provider or work.

There is no autonomous topic routing. Existing fresh-run/provider recovery
boundaries and Pi's within-run compaction remain. A checkpoint is a revisioned
continuity note with `kind=checkpoint`, source references and an attributed
statement kind. Missing checkpoints fall back to retained conversation, pending
records, recent task outcomes and `receipts_read`. Checkpoint revisions reject
concurrent writers; a changed source epoch rejects stale checkpoint publication.

Checkpoints must be refreshed from sources, not solely from a previous summary.
The runtime can enforce source existence/version and transactional writes; it
cannot establish that every sentence of a model-authored summary is correct.

### Context budgets and diagnostics

Context selection uses the provider-supplied context window when available and a
128,000-token planning default when CLI catalogue limits are unavailable. The
estimate is ASCII bytes / 3 plus non-ASCII UTF-8 bytes; it is **not an exact model
tokenizer**. Reserve is min(window / 10, 8192) plus a 5% safety margin. The estimate
includes operating instructions, current user input, tool schemas and selected
context. Current user input and explicit role instructions are never silently cut.
Optional history is removed before authoritative pending counts and core state.

Models below 64,000 tokens receive a compact operating contract and on-demand
reference chapters. Actual tool schemas/descriptions are preserved. Private
memory is paged only when necessary; `memory_read` can recover the rest. Oversized
required input fails before generation with a concrete larger-context/shorter-input
error. Within-run provider/tool-output growth remains governed by the provider
harness, including Pi compaction; this change does not provide universal exact
cross-provider token accounting.

`context_selection` and `context_assembly` run events contain counts, estimated
tokens, context bytes, timing and inclusion flags. They contain no source prose,
queries, prompts, credentials or embeddings. They are accessible through existing
authenticated run activity APIs, not added as ordinary chat messages. Timing
separates budget work from total assembly.

### Retrieval and disclosure

`history_search` supports exact run IDs, `message:123`, keyword matching and
stemming using local SQLite FTS5. Automatic retrieval also expands indirect
references using up to three recent conversational antecedents. Relevant saved
knowledge/work notes are also recalled across allowed conversations with source
references; `continuity_read` can recover the full note in that permitted scope. Retained source
messages remain readable through `chat_read` with attribution and truncation
metadata. Exact reads of a superseded source explicitly include its source state
and successor; automatic selection excludes it.

**This is lexical/contextual retrieval, not a learned embedding model.** There is
no embedding download, account requirement or semantic service dependency. Broad
paraphrase recall without lexical clues/antecedents remains unverified and is not
promised. A local embedding implementation can be added later behind this boundary.

Disclosure is enforced in the context builder and model retrieval tools before
text enters the model. A private owner DM can recall accessible conversations.
Every shared destination is restricted to its own conversation, even if the bot
belongs to another room. Private bot memory, owner identity preferences and other
conversation descriptions are withheld there. The bot's role instructions remain
its global operating instructions. This deliberately conservative first policy
also prevents automatic shared-to-shared recall; explicit destination grants are
not implemented.

### Commitments and execution

`continuity_obligations` stores stable-key, revisioned obligations with source
references, status and optional real execution link. Tools:

* `obligation_save`: record/update a commitment. It creates no timer or execution.
* `obligations_read`: independent of search ranking; total count plus stable paging.
* `pending_read`: page existing pending question/reminder/routine/run/command IDs.
* `receipts_read`: inspect paginated recorded tool requests/results for an owned
  run within the destination disclosure boundary.

Linked routines/reminders/runs/commands must exist and belong to the bot and
appropriate conversation. Routines currently execute in their bot's owner DM;
that existing restriction is retained. A reminder is explicitly notification-only.
Execution status is resolved from the real record on read, not copied as a permanent
memory claim. Completion remains an attributed statement requiring evidence;
run completion alone does not establish successful external execution.

Current pending counts survive context trimming. An obligation whose evidence
changes remains unresolved and queryable, with stale description withheld pending
reconciliation. It is not silently completed or automatically executed. Questions,
routines, reminders and commands retain their existing deterministic scheduling
mechanisms. Models are instructed to record promises; arbitrary natural-language
promise detection has not been proven or made deterministic.

### Correction, consolidation and deletion

`continuity_save` accepts `kind` (work, knowledge, preference, checkpoint),
`statement_kind` (user_statement, observation, inference), and exact message/event
references. Attributed claims require retained sources; user statements require
user-message sources. Existing unannotated notes remain `legacy_unverified`.
Updating a stable topic consolidates it with revision protection. Closed topics
remain attributed historical records.

Source body edits, relevant suppression changes, deletion and receipt changes
invalidate dependent notes/summaries. Model-authored message derivatives also
inherit their run's source dependencies. Legacy private memory without reliable
lineage is quarantined from model context after relevant changes, rather than
being silently erased or treated as current. The owner retains its stored text;
replacement must be reconstructed from retained sources. This is deliberately
coarse invalidation and may withhold unrelated useful context.

Authenticated owner endpoint:

```
PATCH /api/continuity/sources/{message_seq}
{"action":"supersede","expected_revision":0,"expected_text":"exact old text","successor":456}
```

A successor must be a newer retained message in the same conversation. The old
source stays recoverable as explicitly superseded evidence. Use `action=forget`
without a successor to suppress/redact that source, invalidate dependent context,
redact affected note/revision/compaction prose, and quarantine unscoped legacy
memory. Expected text/revision protect concurrent edits. Workspace freeze blocks
these mutations. This is an owner API, not a new ordinary-chat management UI.

This endpoint forgets the selected source and tracked derivatives; independently
authored later messages, external documents and offline backups are separate
source records. Forgetting a source does not cancel a separate authoritative
routine or reminder; those records require their own explicit updates. It does
not claim semantic erasure of all independently repeated
facts or filesystem secure deletion. Existing action IDs/statuses and receipts
remain audit evidence; affected receipt prose is withheld from reconstructed
context when its provenance is stale. No caches or embeddings outside the managed
SQLite store are introduced.

## Upgrade, indexing, backup and transfer

`src/migrations/continuity-2.sql` is the restart-safe, additive continuity schema
version 2. This binary checks for a newer continuity schema before modifying a
workspace. The migration creates tables, views and triggers; it does not scan all
history or rewrite existing identities, memories, credentials or artifact payloads.

New messages are indexed immediately. A background worker processes at most 128
rows per normal batch (API bound 256), transactionally advancing an index cursor,
and yields between batches. The old disposable FTS table is retired outside
startup. While backfill is incomplete, exact lookup and keyword fallback still
search retained evidence. The fallback can be slower for large workspaces. The
old index retirement is a SQLite atomic table drop outside startup, not an
incremental row-by-row deletion.

Disposable state: `continuity_search` and `continuity_index_progress`. To exercise
rebuilding on an **offline disposable test copy only**, drop `continuity_search`
and `continuity_index_progress` after stopping its server; restart this binary.
Startup recreates them and background backfill resumes from retained messages.
Do not drop source, dependency, epoch, obligation or revision tables.

All durable continuity tables and managed command receipts/waits are added to
workspace transfer. Disposable search data/progress is omitted. Older packages
without the added tables are accepted; packages with the new tables require a
compatible destination binary. Existing transfer rules still pause routines and
reminders and exclude credentials, provider sessions, local grants and VM disks.
Source references and execution IDs survive transfer. A transfer does not grant
access to a former device or replay its commands.

Existing updater backup paths copy complete SQLite databases, so all new durable
state is included automatically. Backups remain point-in-time snapshots. Ordinary
workspace transfer is not a replacement for credential/VM backups.

**Rollback:** do not point an older binary at the upgraded database. Older binaries
do not understand the new invalidation/disclosure rules, and may recreate retired
indexes or omit obligations during transfer. Before a real upgrade, stop work and
make consistent SQLite backups plus the existing configuration/credential/VM
backup set. Roll back the binary and its matching pre-upgrade database together.
Writes made after that backup require explicit reconciliation; restoring an old
backup also restores that snapshot's retention state. No down-migration or safe
older-binary compatibility is claimed.

## Validation and measured scope

Local deterministic tests exercise real SQLite migrations, indexes, context
selection, source invalidation, tool boundaries and workspace transfers. Provider
protocol tests use local fixture servers/processes and the actual Pi SDK. They do
not contact live models or execute real external business actions.

Commands (from the server checkout; the node/cargo paths below are this workspace's
installed tools):

```
KINDRED_PI_TEST_NODE=/usr/local/bin/node /root/.cargo/bin/cargo test --locked --no-fail-fast
KINDRED_PI_TEST_NODE=/usr/local/bin/node /root/.cargo/bin/cargo test --locked continuity -- --nocapture
(cd harness/pi && /usr/local/bin/node --test test/*.test.mjs)
python3 -m unittest discover -s deploy -p 'test_server_updater.py'
python3 -m unittest discover -s deploy -p 'test_provider_cli.py'
python3 tools/testing/continuity/evaluate.py --validate-fixtures
git diff --check
```

Final completed broad run: 455 Rust tests passed; 29 Pi harness tests
passed. The focused continuity run passed 16 tests. Server-updater backup/rollback
fixtures passed 7 tests. Provider CLI fixtures passed 12 tests. Exact commands and measured scope are recorded
in `LONG_TERM_CONTINUITY_VALIDATION.md`.

One bounded 10,000-message fixture measured 10/10 exact targets recalled at eight
results and 10/10 returned results relevant for those exact-target queries. Its
SQLite growth was 5,357,568 bytes (~535 bytes/added message); total database size
6,217,728 bytes. The recorded ten-query-plus-indirect-assembly interval was 154,191
microseconds; fixture creation/backfill/evaluation took 3,828 ms. These are local
synthetic measurements, not latency or storage guarantees. FTS supports different
text lengths and vocabularies with different costs.

Privacy tests reject private history/memory reads from shared rooms and check
that private sentinels do not enter assembled context. Commitment tests page all
45 dormant obligations after unrelated conversation, reject stale writers and
invented schedules, and create zero routines. Restart tests remove all provider
session assumptions, change the provider/model, preserve pending intention and
an actual recorded success receipt, and show that reconstruction creates no extra
action request. This proves reconstruction has no execution side effect; it does
**not** prove a live model will never propose or execute a duplicate later.

## Reproducible adversarial acceptance

1. Review the diff, schema, disclosure checks, prompts and source lifecycle API.
   Run the commands above on a development checkout. No release scripts or paid
   CI are necessary.
2. Run the 16 focused continuity tests. Inspect `CONTINUITY_METRICS` and the assertions
   covering missing checkpoints, crash/reopen, provider changes, differing budgets,
   indirect antecedents, concurrent saves, transaction rollback, deletion,
   private-to-shared reads and transfer. Repeat with larger synthetic data if needed.
3. In an isolated local Kindred profile, use a single existing bot across unrelated
   topics. Record a dated preference, an unresolved promise and a completed action
   against a fixture service with a unique receipt. Do not use real clients,
   credentials or production services. Inspect persisted obligations and receipts.
4. Restart the development server and change the bot's configured provider/model.
   Native provider sessions should be absent. Ask an indirect follow-up. Confirm the
   bot retains its identity, retrieves the right sources, distinguishes intentions
   from outcomes, and does not repeat the fixture action. Inspect the fixture's
   action counter, not just the bot's prose.
5. Give a correction, supersede the old source through the authenticated endpoint,
   and repeat after restart. Forget the corrected source, rebuild disposable indexes
   on the offline copy, and repeat. Check that stale checkpoint/summary text stays
   excluded. Test a concurrent note writer against the old revision.
6. Add a shared room containing that bot. Ask for facts found only in its private
   history. Inspect provider input as well as its answer: private content must never
   enter that shared context. Test both automatic and explicit tool retrieval.
7. Make a disposable workspace transfer and a consistent SQLite backup/restore.
   Confirm IDs, notes, obligations, sources and execution receipts survive; schedules
   remain paused after transfer and no command is replayed. Exercise rollback only
   with a matching pre-upgrade snapshot/binary pair.
8. Optional bounded local-model decision probes:
   `python3 tools/testing/continuity/evaluate.py --model-command <local-command> ...`
   The command reads one prompt on stdin and returns a JSON object on stdout. Eight
   synthetic probes score evidence selection, commitments, uncertainty, prohibited
   action proposals, prohibited answer terms and latency. It makes no actual external
   actions. No live-model results are bundled; fixture validation alone is not a
   behavioral pass. Follow with the real Kindred provider/tool-loop checks above.

Remaining acceptance gates: actual model compliance with source attribution,
commitment recording and duplicate prevention; recall for broad paraphrases; long
mixed multilingual histories; real-world latency/storage at substantially larger
scale; and model/style continuity across provider changes. No claim of semantic or
behavioral reliability is made solely from the offline tests.
