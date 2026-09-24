# Provider connector preference contract

Kindred exposes one bot-level preference for duplicate connector services:

- `provider`: prefer the bot provider's account connections. Claude bots use
  Claude account connections; Codex bots use Codex account connections.
- `kindred`: prefer the connected Kindred account.
- `ask`: ask on each duplicate choice.

The legacy `claude` value is accepted by the API and normalized to `provider`.
The UI uses “Prefer provider” so the choice remains valid when a bot changes
between Claude and Codex.

`GET /bots/:id/connectors` returns `provider_source` (`claude`, `codex`, or
null), `provider_name`, `provider_origin`, `provider_account_key`,
`provider_all_allowed`, and `provider_connections`. Each connection retains
its actual source and status, including unavailable or authentication-required
states. Kindred inventories remain in `apps[].accounts[]`.

`PUT /bots/:id/connectors` accepts `{ "source": "provider" | "kindred" |
"ask" }`. This preference controls routing only. It never grants connector
permissions. Permission switches and pending approval cards remain separate;
a denied source choice never silently falls back to another source.

Provider refresh uses `POST /provider-cli/claude-code/connectors` for Claude and
`POST /codex/connectors` for Codex. No live account calls are needed for UI
tests; fixtures should include connected, needs-auth, unavailable, duplicate,
Kindred-only, and pending approval states.

## Codex import and execution

The import uses the dedicated Kindred Codex subscription account in the bot VM,
not the operator's Codex desktop session. A Codex bot's `connectors_list`
refreshes that inventory; settings expose the same refresh operation.
OpenRouter or custom API models do not automatically inherit a Codex
subscription identity merely because their model name contains GPT.

The pinned Codex CLI 0.153.4 generated schema was checked against the installed
binary. The relevant protocol is documented in the official
[Codex app-server reference](https://learn.chatgpt.com/docs/app-server).
`app/installed` is the authoritative installed-app snapshot and gates effective
availability. Targeted `app/read` calls provide canonical names and tool summaries
in batches of at most 100 IDs; `mcpServerStatus/list` supplies live tool schemas.
Refresh does not call `app/list`, whose public-directory fetch can return a
Cloudflare browser challenge independently of the installed connector runtime.
Store entries that are not installed are not imported.

If installed-app discovery fails, the previous callable catalogue is cleared and
a concise error is returned. If metadata verification fails, installed entries
remain visible by their runtime names but have no callable tools, with an explicit
warning. Missing or ambiguous metadata never creates execution permission.
RPC and Settings error handling suppress upstream HTML/challenge tokens and bound
long messages; refresh controls remain usable after a failure. The account panel
and bot settings both handle errors from older backends.

Execution currently requires an exact, unique canonical tool-name join to a
connected `codex_apps` server. Ambiguous names, unavailable apps, missing
schemas, or another server remain unavailable. Display names and undocumented
metadata never supply an execution identity. Importing this metadata is not
an OAuth connection flow or permission grant.

`codex_connector` takes `app_id`, `server`, `tool_name`, `account_key`, and
schema-shaped `arguments`. Kindred creates a saved review card and executes
the reviewed inputs only after rechecking provider, account, availability,
and tool schema. It uses a separate ephemeral RPC thread with no model turn.
The reasoner disables native app tools and configured MCP servers while
retaining Kindred's dynamic tools. An unsolicited provider approval or input
challenge is rejected; it does not receive an automatic answer.
Before starting a model turn, Kindred checks the effective installed-app and
MCP snapshots. A callable native app or exposed non-hosted MCP server stops
the turn instead of relying on configuration flags alone.

The subscription email is fingerprinted for account-change detection. This is
not proof of the linked service account. Until the runtime exposes a stable
linked-account identity, every Codex call requires review, including reads and
Full access mode. No bulk or email-specific standing grant can be saved.
Failures and uncertain results are retained; writes are never automatically
retried. An external account change during a call cannot be made atomic by
this client, and a provider receipt does not prove eventual delivery.

Verification in this lane used synthetic RPC peers and browser fixtures.
No live connector action, OAuth flow, provider model turn, or real account
content was used. A live acceptance check is still required for a particular
app's returned metadata and its actual read/write behavior.

## Dedicated ChatGPT Finance and Health experiences

Connecting accounts inside ChatGPT is not by itself a Kindred connection.
Kindred can use only the installed, authenticated apps and exact callable tools
actually exposed by its Codex account bridge. The operator's separate desktop
session, ChatGPT dashboards and dedicated experiences are not imported.

OpenAI's [Finance guide](https://learn.chatgpt.com/use-cases/track-bills-subscriptions-and-spending)
currently describes the dedicated Finance experience as web-only, unavailable
in its desktop app (checked 2026-09-18). Its Plaid account connection must not be
advertised as available to Kindred without an exposed, supported tool and live
verification. The same requirement applies to dedicated Health connections;
this work establishes no supported path to their private data.

OpenAI documents a [shared plugin directory across supported ChatGPT/Codex surfaces](https://learn.chatgpt.com/docs/enterprise/apps-and-connectors),
with availability, service authorization and runtime permissions as separate
requirements. That is support for eligible plugins, not evidence that every
ChatGPT feature or consent grant transfers. A separate direct data connection
would need its own supported integration and authorization; do not extract
ChatGPT cookies or undocumented Finance/Health tokens to simulate one.
