# Computer handoff workflow

This describes the current Kindred workflow for a human-only sign-in or
password-manager step. This lane used synthetic accounts and pages; it did not test real sign-in or
password-manager unlock.

The bot first opens and observes the relevant page. `request_user_action` is
requires a successful screenshot after this run's latest computer action.
A request without an earlier computer action is permitted, so the operating
guide also directs the bot to open and inspect the relevant page first. Its instructions tell the person to sign in or unlock the manager in
the open browser, enter passwords and one-time codes directly, and press
**Done with subtask** when control is returned. Before returning, the person
closes any vault or revealed-password view and returns to the destination app. The task record contains the
human instruction and only a `done` or `skipped` outcome.

In the UI, **Take over** enables the screen takeover and opens a controlled
session. While takeover is active, the bot's screen lease is released and the
bot remains suspended. The **Done with subtask** control is visible only while
the selected screen reports takeover. Completion closes interactive controls,
waits for the screen lock, records the outcome, and returns the screen to bot
control. Repeated matching completion requests are idempotent.

After the handback, a fresh successful screenshot is required before the bot
can rely on the resulting page. A screenshot with no image or a failed
screenshot does not satisfy this requirement. A skipped handoff is incomplete
and does not prove that login, verification, or account access succeeded.

Cancellation expires the pending human task and prevents a late completion
from resuming the run. Restart expiry follows the same fail-closed rule. A
resume is bound to the task's run and bot identity, so a stale or cross-bot
task cannot resume another run.

The computer action gate is enforced in runtime dispatch for computer input,
`guest_exec`, and `local_exec` while a resumed handoff still lacks its fresh
observation. `handoff_needs_observation` is run-scoped and compares the latest
`user_action_done` with the latest successful screenshot; screenshots from
another run do not satisfy it.

Kindred does not currently expose a dedicated password-manager integration,
unlock API, secret-entry field, credential redaction layer, or end-to-end
password-manager acceptance test. The supported boundary is human entry in
the visible browser. Passwords, one-time codes, recovery codes, tokens, and
payment details must never be placed in task titles, instructions, chat,
handoff messages, events, screenshots shared in chat, or source fixtures.
Adding heuristic secret scanning is not a substitute for an explicit
sensitive-entry mode and storage policy.

Relevant implementation and tests:

- `src/user_tasks.rs`: task lifecycle, lease release, resume binding, and
  observation predicates.
- `src/runtime.rs`: human-task tool and post-handoff action dispatch.
- `src/screen_control.rs`: takeover state and control identity.
- `src/web.rs`: authenticated completion route and screen-lock serialization.
- `tools/frontend/test-user-flows.cjs`: actual `app.js` fixture coverage for
  takeover, controlled session, duplicate Done clicks, and returned control.
- `src/user_tasks.rs` test module: synthetic cancellation, restart expiry,
  screenshot failure, repeated handoff, and cross-run/bot resume coverage.
