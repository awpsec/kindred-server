# Live call work

GPT-Live handles speech through client delegation. Requests enter the originating
bot's ordinary task system, retaining its configured provider, instructions,
memory, tools and approval policy. Live receives a bounded recent chat excerpt;
the task receives normal bot context plus the new spoken input. Voice statements
are context, not evidence of completed work or user authorization.

The delegation ledger records the call, upstream event ID, source run and covered
user fragments in the same transaction as task creation. Duplicate events reuse
the task. Corrections can join the running task through the existing steering
protocol. Ending or recovering a call hands off only previously undispatched
input. It preserves one quiet call receipt and does not cancel running work.

Only public chat replies are relayed to Live. Provider errors, private reasoning
and raw tool output are excluded. Full results and links remain in chat; Live's
copy is bounded and split into Unicode-safe updates under the append limit.
Pending questions, user actions and approvals direct the caller to their chat
cards. This feature does not add a voice approval mechanism.

## Verification

Run `cargo test --bin kindred -j1 voice:: -- --test-threads=1`, question context
tests and the conversation-update tests. The tests cover duplicate delegation,
corrections, pending questions, public result delivery, trailing transcript input,
recovery, provider independence and the existing local WebSocket relay fixture.

The server test harness compiled and all 15 `voice::` tests passed during release
preparation on 2026-10-30. Fixtures now use the production bot-creation path,
including its direct conversation. Coverage includes the local WebSocket relay,
correction steering, public results, pending approvals and restart deduplication.
Chromium/WebKit decision-card and voice adapter tests also passed. These checks
use local provider fixtures; they do not validate real provider audio or a device.

An existing task-question browser fixture expects four top-level progress rows
but observes zero on both the preceding source and this candidate. Reconcile
that baseline expectation with the current transcript layout before declaring
the broader suite green; it is not a passing regression check.

Before release, follow the desktop repository's `mobile/ios/VOICE_CALL_QA.md`.
Verify a real lookup during a call, spoken correction, retained links, required
approval, ending during work, restart recovery, and the non-OpenAI bot-provider
case. Native audio, ActivityKit, Dynamic Island controls and all Duo postures
require the Mac/device pass. No real provider call or native build was validated
by the Linux preparation task.
