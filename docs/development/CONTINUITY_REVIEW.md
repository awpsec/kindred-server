# Continuity implementation review

Reviewed the implementation introduced by ae0f5ae against the Kindred-only
continuity requirements. This is a development review, not release acceptance.
No production database or provider session was changed.

## Assessment

The database-first foundation is appropriate. Revision-checked checkpoints,
separate obligations and execution receipts, conservative audience filtering,
resumable indexing, bounded context assembly, and transfer support are useful.
The implementation does not yet establish dependable months/years of semantic
recall or model behavior. Do not describe it as fully accepted long-term memory.

## Defects reproduced and fixed in this review

1. Exact-message reads bypassed the invalidation used by history search and paged
   reads. A bot could retrieve a generated response repeating a forgotten source
   by its message ID. Exact reads now reject invalidated generated prose, and
   checkpoint source validation rejects it too. Obligation readback and event-source
   validation now apply the same validity checks, so saved descriptions cannot
   reintroduce invalidated derived evidence. Original retained human statements
   remain inspectable with their explicit supersession metadata.
2. Receipt reads required the entire conversation's epoch to remain zero. After
   any correction, even receipts from newly reconstructed sessions were withheld
   forever. The recent-task journal had the same problem. Both now compare the
   source session's epoch with the current source epoch and check dependencies.
3. Generated messages from legacy runs had no dependency rows, so the current-
   messages view treated them as permanently valid even after source forgetting.
   The view now also checks the session's conversation epoch (zero for legacy
   runs). Human-authored statements are not mistaken for derived model prose.

The first two defects failed regression tests before correction. Dedicated tests
cover exact reads, checkpoint source references, fresh receipts and task history,
legacy generated messages, and preservation of human-authored evidence.

## Remaining acceptance concerns

- **Overly broad invalidation:** editing one source quarantines a participant bot's
  entire legacy memory. A DM session records dependencies on every permitted chat,
  not only evidence actually consumed. An unrelated correction can therefore
  invalidate unrelated notes and generated history. This is conservative for
  privacy, but disruptive for continuity. Move toward actual evidence dependencies
  and scoped facts, with explicit reconstruction for legacy memory. Do not simply
  remove the guards; that would reintroduce stale/deleted information.
- **No semantic retrieval implementation:** retrieval currently uses FTS stemming,
  keywords, recent-conversation query expansion and substring matches on notes.
  There is no learned embedding model. It can miss indirect references with no
  useful lexical overlap. A bundled local semantic layer remains unimplemented;
  multilingual and paraphrase behavior needs measured acceptance.
- **Commitment capture is still model-dependent:** once saved, obligations are
  durable and pageable independently of ranked search, but arbitrary promises are
  not guaranteed to be recorded. Saving an obligation correctly does not create a
  schedule. Test real Kindred turns making and fulfilling commitments.
- **Behavioral validation remains outstanding:** the optional evaluator scores
  eight hand-authored context packets; it does not execute Kindred retrieval or
  its real provider/tool loop. Fixture validation is not evidence of reliable
  summaries, recall, source attribution, or duplicate-action prevention.
- **Upgrade coverage is bounded:** an isolated synthetic database made by the
  published 0.83.0 binary upgrades successfully. This does not replace a backed-up
  representative workspace acceptance run, large-history migration measurements,
  or a complete rollback rehearsal.

## Validation

The review adds `review_*` regression tests and an explicit ignored test named
`published_database_upgrades_in_place_without_losing_continuity`.
For the latter, KINDRED_TEST_LEGACY_CONTINUITY_DB points to a disposable synthetic
0.83.0 database containing legacy-bot, legacy-run, its DM/source message, a stable
memory, checkpoint, and paused legacy-routine. It opens a copy twice, verifies
those records, completes index backfill, and checks SQLite integrity. Never point
fixture preparation at a real workspace.

Run the focused suite with `cargo test --locked continuity`. Run the explicit
upgrade fixture using `cargo test --locked published_database_upgrades_in_place
-- --ignored` with that environment variable set. Full server tests also exercise
provider adapters using local mock transports; they are not live-model trials.

An in-place upgrade is supported by the additive migration, but downgrade is not:
rollback requires the matching old binary and its pre-upgrade database backup.
No native CI, package publication or rollout is authorized by this review.

Review validation results: 459 full server tests passed (one fixture-dependent
upgrade test intentionally ignored in the default suite); 20 focused continuity
tests passed. The explicit published-0.83.0 database upgrade test passed separately.
These results use local mock provider transports, not live-model acceptance.
