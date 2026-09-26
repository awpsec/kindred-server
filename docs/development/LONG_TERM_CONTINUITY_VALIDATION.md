# Continuity validation receipt

Development branch: `codex/long-term-continuity`.
Baseline server source: `ddef20b5591e456e755e6c4e64ebd74a483052e1`.
The accompanying commit contains the implementation, tests and this receipt.
No production data, credentials, services, release feeds or deployment were changed.
No paid CI or live model was invoked.

## Exact local commands and results

Working directory `/opt/kindred/kindred-server`, except the indicated Pi subshell.

| Command | Final result |
|---|---|
| `KINDRED_PI_TEST_NODE=/usr/local/bin/node /root/.cargo/bin/cargo test --locked --no-fail-fast` | 455 passed, 0 failed, 57.98 s test execution |
| `KINDRED_PI_TEST_NODE=/usr/local/bin/node /root/.cargo/bin/cargo test --locked continuity -- --nocapture` | 16 passed, 0 failed, 4.81 s |
| `(cd harness/pi && /usr/local/bin/node --test test/*.test.mjs)` | 29 passed, 0 failed, 3.22 s |
| `python3 -m unittest discover -s deploy -p 'test_server_updater.py'` | 7 passed, 0.015 s; includes expected simulated update rejection and rollback |
| `python3 -m unittest discover -s deploy -p 'test_provider_cli.py'` | 12 passed, 3.128 s |
| `python3 tools/testing/continuity/evaluate.py --validate-fixtures` | 8 valid fixtures; live_model_evaluated=false |
| `python3 -m py_compile tools/testing/continuity/evaluate.py` | Passed |
| `/root/.cargo/bin/rustfmt --edition 2024 --check src/continuity.rs src/continuity_store.rs src/continuity_tests.rs` | Passed |
| `git diff --check` | Passed |

The shared UI gate's `verify()` function was run against exact desktop/server
commits; all 31 shared files matched. No shared UI file was changed. Desktop source:
`a0cd5409fbe710e93c624161d1fb84779481cfc3`. Reproduce against the review commit with:

```
python3 scripts/release/verify-shared-ui.py \
  --desktop /opt/kindred/kindred-desktop \
  --server /opt/kindred/kindred-server \
  --desktop-source a0cd5409fbe710e93c624161d1fb84779481cfc3 \
  --server-source <exact-reviewed-server-commit>
```

Existing compiler warnings remain in desktop skill helpers (`dead_code`) and a
local-access test (`unused_must_use`). No new continuity compiler warnings were
reported. Initial iterations exposed regressions and a missing default Node path;
those were corrected and the final commands above passed with the installed Node
binary explicitly selected. The Rust suite includes local mocked provider
transports with the real Pi SDK; “real SDK” does not mean “live model.”

## Bounded measurements

The 10,000-message retained-history fixture emitted:

```json
{"bytes_per_added_message":535,"exact_precision":1.0,"exact_recall_at_8":1.0,"fixture":"mixed-10000","fixture_total_ms":3828,"live_model":false,"queries":10,"query_total_us":154191,"sqlite_bytes":6217728,"sqlite_growth_bytes":5357568}
```

`query_total_us` includes the ten exact queries and subsequent indirect-reference
assembly, not an isolated p95 or production SLA. The exact-query corpus is
synthetic and highly identifiable; it does not demonstrate semantic recall.

Other asserted results:

* 45/45 dormant obligations recovered by paging after unrelated conversation;
  saving them created zero routines. Invented schedule links were rejected.
* Zero private sentinel disclosures through assembled shared context, history
  search, chat read or private memory read in the tested cases.
* One recorded external action request before restart and still one after
  reconstruction. Its success receipt remained available alongside the earlier
  intention. Reconstruction made no external calls. Subsequent live-model
  duplicate-action behavior remains unverified.
* Fresh contexts fit the estimator's 32,768, 64,000 and 128,000-token budgets.
  A 1,024-token context fails explicitly. Runtime diagnostics record estimated
  input tokens, reserved tokens, UTF-8 bytes and assembly/budget duration without
  recording sensitive source content. No live token billing/cost was measured.
* Two concurrent writers with the same expected checkpoint revision produce one
  winner; a forced revision-write failure rolls the entire save back.
* Crash/reopen, absent checkpoints, provider/model changes, source correction,
  supersession, forgetting, backfill interruption, authenticated lifecycle API,
  workspace transfer and saved knowledge recall are covered by deterministic tests.

## What still requires acceptance

The eight optional local-model decision probes are provided but were not run
against a live model. Arbitrary promise detection/recording, faithful summaries,
broad semantic paraphrase recall, multilingual scale, long-lived style continuity,
and actual-model action decisions require the isolated end-to-end acceptance
procedure in `LONG_TERM_CONTINUITY.md`.

Source invalidation is intentionally conservative and may withhold unrelated
legacy memory or summaries until reconstruction. Destination policy currently
blocks all automatic cross-room recall into shared conversations. Legacy raw
memory has no per-fact lineage. Token counts are estimates. There is no learned
embedding component or automatic topic router in this implementation.

Native Windows/macOS verification, release verification, paid CI, publishing and
production rollout were not performed and are not authorized by this development
change. Rollback requires the matching pre-upgrade database and binary; opening an
upgraded database with an older binary is not supported.
