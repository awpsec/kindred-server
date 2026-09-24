# Provider connectors and computer handoff validation

Unreleased work based on source commit
`a6b8fa2d3b30c6e07da890afb68e7e6eaf9c284e`, version 0.48.34.
The release policy remains manual. No release, stable-feed change, deployment,
native package build, or GitHub workflow was performed for this lane.

## Implemented behavior

- A saved **Prefer provider** choice follows a bot between Claude and Codex.
  Legacy Claude choices normalize without granting new permissions. Kindred
  connections remain available when the provider changes or is unavailable.
- Codex inventory and execution use the dedicated subscription account and
  current canonical metadata. Unresolved bindings stay unavailable. Every
  imported Codex call receives its own review card and retains a receipt.
- A returned human handoff requires a new successful screenshot before
  computer input, guest commands, or local commands. A skipped handoff is not
  treated as successful sign-in. Resume is bound to the exact bot and run.
- Password-manager guidance directs human unlock and secret entry in the
  browser, followed by closing exposed vault views before returning control.
  Native Codex secret questions cannot become ordinary chat questions.

## Tested scenarios

| Scenario | Evidence and result |
| --- | --- |
| Change Claude bot to Codex and then API provider | Policy test preserves preference, chooses the matching inventory, transfers no grants, retains Kindred accounts |
| Codex in Full access mode | Synthetic RPC call waits for a separate review; standing and email grants are rejected |
| User edits an email draft | Actual artifact update path dispatches the edited body once |
| Denied call | No direct MCP call is sent; the saved card remains denied |
| Account, schema, or installed state changes during review | Fresh inventory blocks dispatch |
| Cancel or change bot provider during review | No direct MCP call is sent |
| Provider error, disconnect, or extra challenge | Failure remains visible; one attempt at most; no automatic challenge approval |
| Wrong app, server, tool, or account | Exact binding rejects it |
| Ambiguous tool name or repeated page cursor | Import cannot turn it into an executable binding or unbounded loop |
| Uninstalled store entry | Excluded from imported connections |
| Native Codex secret question mixed with ordinary questions | No chat question is created from that request |
| Native app or MCP tools remain exposed despite configuration | Codex stops before the model turn; no unreviewed native call is made |
| Human Done or Skipped without a later image | Runtime blocks seven computer/command action types before VM access |
| Empty/failed image, repeated handoff, other run's screenshot | Observation gate remains scoped to a successful later image in this run |
| Cross-bot/run resume, cancellation, restart | Stale human tasks cannot resume another task |
| Takeover, delayed computer connection, repeated Done clicks | Real app UI fixture returns control once without cancelling the original task |
| Connector settings and cards | Edge and WebKit show accurate source labels, no Codex bulk grant, no duplicate Claude rows, editable review cards, and mobile layout |
| Multiple Kindred accounts | Browser fixture exercises account selection, rename, reconnect, single connection submission, and scoped disconnect |

The Rust suite exercises actual database, policy, artifact, runtime, and RPC
adapter paths. Provider traffic comes from local synthetic peers. Browser
tests run the checked-in app modules against fixture APIs and a simulated
computer session; they do not prove real provider or website behavior.

Commands used: `cargo fmt --check`, `cargo test --locked`, and
`cargo build --release --locked` in the existing isolated Linux builder;
`test-codex-connector-preferences.cjs`, `test-connector-preferences.cjs`,
`test-connector-artifacts.cjs`, and `test-user-flows.cjs` in Edge and bundled
WebKit. Screenshots were visually inspected. Test artifacts stay outside Git.

Final result: 244 Rust tests passed (34.65 seconds); formatting passed; the
optimized backend build passed (42.35 seconds) with the same three pre-existing
dead-code warnings. All four browser suites passed in both engines. The
changed Rust and prompt files were checked by SHA-256 against the passing
builder snapshot before the local commit.

## Remaining acceptance work and features

1. Live Codex app acceptance: verify a connected app's actual inventory shape,
   then one authorized read and one separately reviewed write with a readback.
   The protocol fixtures do not establish that every hosted app has the exact
   name binding required by this gateway.
2. Password-manager acceptance: test a selected manager and site with the
   person unlocking it, returning to a masked destination page, and resuming
   the original bot task. No real credentials or unlock were exercised here.
3. A dedicated sensitive-entry mode would need an explicit capture/storage
   policy, visible entry/exit state, and tested handling of unmasked secrets.
   Current human takeover is not a password vault or credential-redaction
   guarantee.
4. Stable linked-account identity from the Codex runtime is required before
   persistent per-service grants can be offered. A subscription fingerprint
   alone cannot authorize a particular linked mailbox or cloud account.
