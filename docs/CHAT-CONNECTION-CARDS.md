# Chat connection cards

Bots can call `show_connector({"toolkit":"gmail","account_name":"Household"})` to place a connection request in their private chat with the owner. The tool requires an active run in an active chat and deduplicates a toolkit within that run. The message stores the toolkit, suggested account name and persistent question id. Actual account labels and status come from the signed-in workspace’s live inventory; credentials are never embedded in chat history.

The card shows the service logo, description, account labels, status, and Add account action. Connected chips open account management. Unfinished sign-ins reopen their existing authentication link. Expired or failed connections guide the person to add a replacement while preserving the original account. No monitor is silently reassigned or resumed, and no permission is granted by showing a card.

Composio recommends a fresh auth link for reauthentication: https://docs.composio.dev/kb/guide/platform-connected-accounts. Provider-owned Codex and Claude connections continue using their own settings; this card must not be used to silently substitute a Kindred connection.

Frontend coverage: `tools/frontend/test-chat-connections.cjs` (Chromium and WebKit), plus `test-user-flows.cjs` for approvals and computer handoff. The focused server regression is `connection_card_is_scoped_idempotent_and_contains_no_accounts`. Desktop and server UI must ship together; the new bot tool requires the updated server. Source pushes do not publish a release.

Validation on this workspace: Chromium/WebKit card checks and Chromium handoff/account flows passed. `cargo check --locked -j 1` passed. The focused Rust test build was interrupted after several minutes compiling under host contention, before the test executed; it remains a release verification item. Real OAuth and native desktop acceptance were not performed with user accounts.

## Guided setup (2026-09-22)

`show_connector` now accepts optional `account_name` (e.g. Household). It creates
an in-chat connection request using persistent decision/continuation storage and
ends the current turn. Missing-Gmail `inbox_monitor_setup` uses this same card
instead of an “Open Marketplace / I connected Gmail” question. The card's add
form prefills the account name. Missing Composio credentials offer Connections
setup, while Not now remains available. Existing plain-toolkit cards still work.

Cards store a toolkit, suggested name and question id, never credentials. Live
inventory remains workspace-scoped. A successful OAuth check calls the new
`complete-connection` action, which checks the card's toolkit and verifies the
exact account against Composio before transactionally answering the request.
Repeated callbacks reuse the same continuation. Selecting an existing account
also performs that server check. Reopening chat renders saved completion state.
The resumed bot receives the exact account id and original task. It must verify
API access and save the actual routine before reporting monitoring as active.
Connecting does not authorize financial actions, automatically reassign a
monitor, or make a browser login equivalent to an API connection.

The browser must return to Kindred for its OAuth status polling/check action;
this is not a provider webhook. Computer sign-in continues to use the existing
Take over / Done with subtask handoff. Venmo requests and cancellation have not
been tested live by this change.

Validation for guided setup: 22 Composio backend tests and 11 inbox-monitor tests
passed, including wrong-toolkit/account rejection, one continuation on repeated
completion, named account isolation, read-only restrictions, schedule retention,
quiet checks and deduplication. WebKit checks passed for missing key -> save key
-> refreshed card, Household prefill, authentication callback, account actions,
light/dark and narrow layout. The broader user-flow test passed takeover/return
without cancellation, exactly one completion, approval receipts and multiple
Gmail accounts. Settings polish, accounts window/platform gating and VM update
controls passed; General, Computer and connection-form screenshots were reviewed.
Tests use mocked provider/computer endpoints. The earlier live Google OAuth and
profile probe validate managed Gmail authentication separately. Native MacBook
acceptance and actual Venmo request/cancellation remain unverified.
