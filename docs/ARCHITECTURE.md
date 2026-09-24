# Architecture and operational boundaries


## Account gateway and managed computers (0.43.0)

`profiles.rs` authenticates account sessions before dispatching to a profile-specific
`App`. Each app has its own SQLite database, credential store, scheduler and VNC state.
Internal per-app routing tokens are random and never accepted at the public gateway.
Only explicitly imported legacy devices can use the old personal token, and only
for their original profile. Public assets and update files use a separate static
route. VNC tickets resolve to their originating profile, retain their one-use proof
and origin checks, and are invalidated when the account is disabled.

`deploy/vm-manager.py` creates one persistent QEMU guest per server-issued profile
UUID. Distinct SSH client/host keys, pinned known-host files, disks and cloud-init
seeds prevent guest crossover. The service has no Docker socket or mounted host
filesystem inside guests. Guest bots have passwordless sudo within their own VM.
The guest firewall defaults to blocking private-network egress, but guest root can
change it; mandatory network isolation requires enforcement outside the guest.
Computer creation is lazy and serialized per profile;
port allocation, image caching and running-computer admission have shared locks.

Desktop profile metadata contains only saved origins, IDs and names plus protected
sessions. Native IPC returns sanitized directory entries and notification counts.
Cross-server switches use that server's saved session, rotate it with the server,
and restart the webview with an immutable server/profile local-access scope. The
bundled local profile window owns standalone setup and startup registration.

## Provider accounts and local desktop access (0.40.0)

`provider_accounts.rs` stores custom endpoint/model metadata separately from private
credentials. Provider IDs are stable UUIDs; bot validation accepts only registered ID
shapes and the four built-ins. Model selections are explicit and there is no fallback.
Custom adapters reuse Pi's normal session, tool validation and compaction. Each adapter
pins its destination; credentials cannot follow redirects. Keyless mode explicitly
removes Authorization. OpenRouter retains its ZDR and no-data-collection routing.

Since 0.42.0, `provider_catalog.rs` discovers custom models from the configured base
URL plus `/models`. Server startup refreshes connected custom providers once; a client
warms each connected custom provider once per session, reusing checks younger than one
minute. Saved provider changes trigger background discovery, cached GET model reads
avoid repeated network fetches, and explicit Refresh models bypasses freshness reuse.
Per-provider gates coalesce concurrent refreshes; at most four catalogue downloads run
at once. Downloads have a 20-second timeout, an 8 MiB limit and a 4,096-entry bound;
redirects are rejected and credentials are sent only to that saved endpoint.

Catalogue models and sanitized refresh status persist with provider configuration.
Failures retain the last successful models, while a valid empty catalogue clears them.
Configuration revisions prevent in-flight responses from overwriting newer URL/auth
choices. Changing the base URL clears the former key unless a new key is supplied.
Client-supplied model arrays are ignored on save; pre-existing manually configured
models remain usable until discovery succeeds. Provider names can change without
losing card order, cached models or the usage history keyed by stable provider ID.

Discovery accepts OpenAI-style `data` arrays, `models` arrays and plain arrays. It uses
reported context, reasoning/vision metadata and OpenRouter-style per-token pricing
where present. Missing context uses a 32,768-token working limit; missing prices stay
unknown rather than producing invented estimates. Context and capability defaults are
not claims about the provider's limits. Explicit non-chat/tool-incompatible entries
are excluded. A paginated response is rejected instead of silently caching a partial
list; this integration expects the customary complete OpenAI-compatible `/models`
catalogue. No provider selection or model task is silently substituted on failure.

`provider_usage` records a run-local model request ID, bot, provider, model, token
counts and optional USD receipt. Cumulative receipts replace the same request rather
than adding it twice. Each API attempt starts as unknown; valid stream usage updates
it. Compaction goes through the same accounting. Reported and estimated costs remain
separate; subscription usage is not presented as an API bill. Custom pricing uses
configured per-million rates and OpenRouter estimates use its current model catalog.
Historical tasks are outside this ledger. Provider billing remains authoritative.

Since 0.41.0, usage receipts include an appearance-only bot identity snapshot and
are independent of operational bot/run foreign keys. Migration preserves existing
metrics and dates, and initializes snapshots from the current identity. A deletion
trigger preserves the final name and avatar; history resolves current profiles for
existing bots and snapshots for deleted ones. Archived bots and disconnected provider
accounts remain visible when they have receipts. Missing prior receipts are never
reconstructed. The current app has bot archival; deletion retention also covers later
database maintenance or a future deletion workflow.

The client warms provider status and API usage at startup, refreshes in the background
once per minute while visible, and shares in-flight reads. Hovering Usage reads the
cached catalog. API previews show the top five bots by known cost; the full dialog
sorts and filters all recorded bot rows locally. Cost sorting sums reported charges
and estimates, excludes unpriced requests, and keeps entirely unpriced bots last.
Manual Refresh usage fetches only that provider's usage and preserves current filters.

`cli_providers.rs` talks over the existing guest SSH connection to the root-owned
`provider-cli.py`. Each subscription uses the unmodified pinned vendor binary and
its own home under `/home/bot/.kindred-providers`. Claude login uses the bounded
`provider-login.py` relay: the CLI supplies its official browser URL, the user's
browser opens it, and any one-use return code is sent to the CLI's standard input.
Only verified public connection status comes back. Codes and terminal output are
not saved; the CLI retains its own credentials. Starts are deduplicated per profile,
attempt IDs bind code submission and cancellation, and pending login expires after
ten minutes. Kimi still opens its official terminal on the selected idle bot screen.
Claude runs with only the built-in ToolSearch discovery tool, the explicit Kindred
MCP server and account connectors fetched by the official CLI. Restricted settings,
no session persistence, no Chrome integration and no slash commands remain in use.
`provider-connectors.py` uses the official SDK control protocol to verify connector
origin, gate every call with PreToolUse, preserve explicit permission callbacks,
and record PostToolUse or PostToolUseFailure receipts. Local MCP configuration in
the dedicated sign-in home is rejected. Account credentials stay in the CLI.
Kimi Wire 1.10 uses an empty agent tool list and empty MCP configuration;
only accepted Kindred external tools execute. Both return every operation through
Kindred's tool dispatcher, approvals, cancellation and human-subtask handling.
EOF kills the guest provider process group. Kimi exposes its own usage projection;
Claude's CLI account-wide quota is unavailable and the UI links to its usage page.

Local access is opt-in at three independent layers: workspace `general.local_access`,
`BotProfile.local_access` plus a specific `local_device_id`, and the desktop's native
mode. Browser and older native clients cannot grant native permissions. The hosted
main window may only open the bundled permission window and read sanitized status.
Tauri capability restrictions and origin checks reserve mode changes and per-operation
consent for that bundled local window. Full access grants the current OS account's
capabilities; it is not a sandbox. Native commands retain ordinary Kindred external
approval rules in addition to native permission rules.

The desktop makes authenticated outbound polls. An independent persistent random
device credential prevents another linked client from claiming an existing device's
work. A 20-second heartbeat indicates availability. The database queue validates the
global/bot binding at claim, serializes claims per desktop, binds receipts to a nonce,
and never requeues a claimed operation. Cancellation, permission changes, expiration
or server restart invalidate unfinished requests. Disconnect stops active commands;
work never falls back to cloud paths. Local requests expire after five minutes and
commands have a 60-second limit. A pending native approval does not pause the model
run's overall configured deadline.

The native worker holds an installation-specific process lock. It persists a journal
before effects and a result before acknowledgment. A restart with an unfinished
journal returns an unknown-outcome failure rather than executing it again. Receipt
retries cannot repeat effects. Commands run without a visible console and their
Windows Job Object kills descendants on cancellation, disconnect, timeout and parent
exit. Permission changes also cancel active native work. File operations already in
progress may finish before cancellation is observed; inspect receipts before retrying.

The workspace is `<Kindred installation>/workspace`, outside version folders; native
configuration and the operation journal are also outside that workspace. Filesystem
tools list/read/write/create directories, with UTF-8 file and transfer limits. They
reject traversal, UNC/device/ADS paths, symlink/reparse prefixes and hard-linked files.
Outside paths require a native single-use approval in Ask mode. Workspace-only mode
rejects every command, since cwd does not confine execution. Paths are re-resolved
following consent. These checks prevent model-directed link escapes; they do not
claim isolation against an independent local process racing filesystem changes.
Local content and results enter the normal task history and selected model context.

The computer header's screen menu derives candidates from the current chat's
unarchived bot members, or the selected bot for a DM. Selection validates against
those candidates and reconnects when the selected member disappears. Workspace
Computer settings can preview any bot; opening an outside participant's computer
enters its DM. This is conversation navigation, not a new backend permission boundary.

Character faces have per-silhouette anchors and motion limits. Smooth cubic eye
contours follow the local surface; triangle/drop faces have narrower spacing and
less travel. The blink origin moves with the face's neutral eye line. Face clipping
handles deliberate turn occlusion; ordinary gaze stays inside the body. Avatar
saves repaint cached conversation content alongside the sidebar and details so
welcome avatars update without navigation or another history request.

Each guest X display runs an unprivileged native desktop shell: feh sets a root
wallpaper and tint2 provides Browser, Files and Terminal launchers. A per-display
flock prevents duplicate docks. Browser launch derives its private profile from
the validated DISPLAY; file-manager profiles are also named per screen, while
/workspace remains shared. Wallpaper and dock are present in bot screenshots,
thumbnails and VNC. Rounded screen corners are a client presentation detail and
do not alter the framebuffer or input coordinates. New profiles show the desktop;
existing browser profiles restore their prior session before the readiness gate opens.
The dock uses tint2 background ID 1 for a translucent dark surface and sits flush with the bottom edge, reserving only its 56 px height,
returning 26 px compared with the previous floating 64 px dock and 18 px gap.
It uses tint2's bottom layer; maximized
applications use the remaining work area and fullscreen windows cover the dock.
For an existing deployment, restart only the dock shell after applying its
configuration, preserving X and browser processes. Legacy normal browser windows that
were fitted to the previous work area can be expanded to the new 1280x744 work
area. Preserve independently positioned/sized windows. New windows already use
the smaller default launch size; maximizing uses the full available work area.
See the [tint2 panel configuration](https://gitlab.com/o9000/tint2/-/blob/master/doc/tint2.md#panel).

Public assistant events are written atomically to chat history using their source
event ID. Completion promotes the last matching reply to a result, preserving earlier
replies and avoiding a second final-answer bubble. Recovery backfills missing public
replies from existing events without rerunning work. An indexed history-order key
places recovered messages before their result when timestamps share a second; message
IDs, reactions and page cursors remain stable. Reasoning events are not chat messages.

The global `show_activity` setting controls raw tool logs and defaults to false.
Approval cards and human subtask controls are independent of that preference.
All provider harnesses receive shared instructions that browser use on the bot's own
screen is available without a connector and as a fallback for connector failures.
Authentication uses the existing human takeover flow, with the same authorization
and external-action approval requirements as connected apps.

Teammate handoffs identify the sender, recipient and subject separately. The shared
prompt includes the current bot's ID and an active teammate directory, and requires
actual delivery for requests to tell or ask another bot. A recipient must save an
ongoing user-assigned responsibility through its own `remember` tool before claiming
it retained that role. The sender saving a fact about the recipient is not sufficient.
Memory remains bot-owned and travels across chats; unrelated chat transcripts are
not copied into a new conversation. Exact pairs reuse active `team-*` chats, while
archived chats, user-created groups and groups with other members stay separate.

Chat work indicators are scoped to the run's own conversation. A handoff reference
cannot pull a source DM's working avatar or public progress into its destination.
Only work observed in the current view can linger on completion: a 280 ms reply
reveal overlaps a 1,100 ms working presence, followed by a 650 ms reverse-dot exit
and a short space collapse. Timing survives ordinary polling; history navigation,
reload, reduced motion and cancelled work do not replay it. If the run feed finishes
before its result page arrives, the presence remains until the reply is available.

```text
Windows Tauri client / browser
        | authenticated HTTP over localhost SSH tunnel or HTTPS
Linux host: Rust Axum service + SQLite + interval scheduler
        |-- OpenRouter: isolated Pi SDK Node worker on the host
        |                 tools return to Rust for policy and VM execution
        |-- pinned SSH as the guest bot account (guest-only sudo)
One libvirt VM: shared bot account / workspace / one Xvfb + Chromium profile per bot
        |-- OpenAI: official Codex app-server (ChatGPT subscription)
```

The host launches only fixed guest bridge commands through SSH. Model-generated
shell commands travel as bounded JSON on stdin and run inside the VM. The `KINDRED_GUEST`
environment check prevents accidental use of the guest CLI on an ordinary host;
it is not a security sandbox. The VM provides the host isolation boundary; its bot account can administer the guest.

The service can use a root-owned, UUID-bound helper for just `domstate`, `start`, and
`shutdown` on its own VM. Without that helper it calls local virsh using the configured
URI. Prefer the scoped helper over granting the service membership in the libvirt
group, which would expose other VMs.

## Tasks and approvals

SQLite holds bots, chats, chat messages, skills, routines, runs, events, approvals, human subtasks, and computer takeover
state. A process lock rejects a second server on the same database. A task claims
its own screen during execution and approval waits. An explicit human subtask
releases that lease until the user hands control back. The scheduler considers
queued bots in oldest-message order and runs up to `max_parallel_runs` (default four).
An atomic claim prevents concurrent turns for the same bot. Default behavior
requires approval of guest actions. Each bot inherits the live global approval mode
or uses an explicit ask/auto/full override. Legacy auto-approve migrates to auto, never full.

Cancellation does not replay work. Restart marks in-flight runs interrupted and
expires pending approvals; queued work survives. Cancellation, timeout, and interrupted
startup impose a persisted 65-second quiet period (except cancellation while the
screen is released for a human; per screen for cancellation/timeout,
all screens after interrupted startup) because a guest command
can run up to its own 60-second timeout plus kill grace. Deliberately detached guest
processes can outlive that period: cancellation is not a guaranteed rollback or process
tree containment mechanism. A VM shutdown remains available for an operator.

Human takeover persists across restart and prevents that screen's bot from being
scheduled. During an explicit human subtask the UI offers Take over and Done with
subtask; otherwise it requires stopping that bot's active task before takeover. Other
bots can continue. VM shutdown/start requires all screen leases to be available. Routines use elapsed intervals of at least
60 seconds, coalesce missed ticks, and skip a bot with outstanding work. They are not
wall-clock cron jobs. Internal teammate messages enqueue work without a manual approval.
A recipient joins the current chat (maximum six members), and its final result
queues a continuation for the requester. Messages share a 24-turn root budget,
a maximum nesting depth of 12, and at most three outgoing requests per turn.
Duplicate completion cannot queue duplicate replies. Failures are returned visibly;
cancelled tasks do not wake the requester. Stop cancels the whole root conversation
round, including queued teammates. Runs default to 24 tool steps and a 30-minute timeout.

Existing bot conversations migrate into persistent direct chats without changing
run or event IDs. New messages and their targeted runs are inserted transactionally;
invalid membership or full queues do not leave half-submitted messages. Membership
edits and archiving wait for active/queued work. Restart preserves queued work and
records interrupted results without automatically replaying an uncertain action.

The chat HTTP endpoint uses timestamp/sequence keyset pagination: `before` and
`after` identify messages in the same chat, `inclusive=true` refreshes a loaded
range, and `limit` defaults to 50 with a hard maximum of 150. A composite SQLite
index supports chronological paging, including migrated messages and timestamp ties.
No stored messages are deleted when the client unloads a page. The UI keeps a
150-message window per chat and a six-chat LRU cache, preserving a visible-message
anchor while prepending or trimming pages. Only visible and active run details are
loaded, in batches of eight; inactive detail entries are pruned above 200. Model
context retrieval keeps its existing separate history limit.

## Provider implementation

Each bot persists an optional model ID and reasoning effort. The Codex adapter reads
all pages of `model/list`, resolves an explicit or account-default model, validates the
thinking level against that model, passes the model to `thread/start` and effort to
`turn/start`, and records the selection. Unsupported combinations fail visibly without
falling back. The additive SQLite migration preserves prior bots, memory and history.
The frontend caches catalogs per provider for the page session with explicit refresh;
the runtime reads the current catalog again before a run.

Protocol reference: [Codex App Server](https://learn.chatgpt.com/docs/app-server).
OpenRouter's public [model catalog](https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties)
is projected to tool-capable model metadata. Reasoning Low/Medium/High is sent explicitly
when chosen. OpenRouter execution still requires an API key and compatible ZDR routing.


The Codex adapter uses the official CLI's app-server JSON-RPC protocol and experimental
dynamic function tools. It forces ChatGPT authentication, removes the OpenAI API key
from that subprocess, uses a dedicated Codex home, and declines built-in shell/file
approval requests. Its own guest tools pass through Kindred's action-approval flow.
Built-in shell tools and hosted web search are disabled. The CLI is an external
dependency and can change its experimental protocol; the tested release is 0.153.4.

The OpenRouter adapter uses Pi's coding-agent SDK through a per-run Node worker,
with only the configured model at the fixed HTTPS endpoint. Codex subscription
execution remains on its existing app-server adapter. The Rust server resolves
current OpenRouter model capabilities before handing the selected model to Pi.
With the default `require_zdr = true`, requests include `provider.zdr = true`,
`provider.data_collection = "deny"`, `provider.require_parameters = true`, and
`X-OpenRouter-Cache: false`. HTTP redirects are disabled. No alternate model/provider
adapter is attempted on error. Eligible endpoint selection inside OpenRouter follows
its routing policy. Setting `require_zdr = false` explicitly removes the ZDR requirement.

## Data and credentials

The server token belongs in a root-readable environment file. An OpenRouter or Composio key
entered in Settings is written atomically to a mode-600 file beside the database;
a configured environment key is also supported. Provider keys are never returned
by the settings API. General identity preferences are included in bot instructions.
Codex subscription credentials live in the guest's
`/home/bot/.local/share/kindred/codex`, separate from the operator's own login.
The Windows launcher passes the bearer token through the desktop child environment;
the UI stores it in sessionStorage. Processes running as that same local OS user can
inspect their environment/browser storage. The public UI assets contain no credential.

Transcripts, tool commands/results, memories, and skills are stored unencrypted in
SQLite. Inspection screenshots are sent to the selected model but not saved into
task events. Screenshots explicitly shared in chat are stored as PNG attachments.
Browser profiles and files persist inside the guest. Backups and copied VM
disks can contain credentials and personal data. Kindred has no telemetry.

Every model turn receives that bot's memory and a bounded reconstruction of its last
six completed task prompts/results from the current chat (up to 4 KB prompt and
8 KB output each), plus up to 40 recent shared messages and member identities.
Other chats are excluded. Shared skills are available through tools. The current
memory limit is 16 KB. This remains bounded reconstruction, not a full historical
context engine.

OpenAI subscription privacy is governed by the account/plan, not OpenRouter settings.
OpenRouter ZDR is a routing constraint, not end-to-end deletion. Browser destinations
can collect data independently. All teammates can access the same guest sessions;
the default libvirt NAT network also allows guest outbound access, including reachable
private networks. There is no outbound network allowlist in this preview.

The API uses constant-time bearer comparison, validates Origin when provided, and
sets CSP and no-store headers. Keep it on loopback behind SSH or an authenticated
HTTPS endpoint. It is a personal service, not a public multi-tenant deployment.

## Live computer

noVNC is bundled locally. An authenticated request issues a single-use ticket with
a 30-second lifetime. The WebSocket handshake checks the configured origin and
consumes the ticket; it does not place the persistent bearer token in a URL.
At most sixteen live connections and sixteen pending tickets are allowed. Tickets
are bound to the selected screen and control mode.

Each screen has two guest-loopback x11vnc listeners. Screen 1 uses 5900 for input
and 5901 for server-enforced viewing. Later screens use consecutive pairs
`5900 + 2*(screen-1)` and the following port. The host connects to them through
pinned SSH forwarding. A modified watch client still cannot control the desktop.
Control connections require persisted takeover and hold the same exclusive lease
used by model tasks. Return control closes interactive connections, waits for the
lease, then resumes that bot. Closing a window alone leaves human takeover in
place. Paste text uses the guest typing bridge without adding text to task history.

The installed Tailscale Serve listener exposes HTTPS port 9446 only on the tailnet,
forwarded to host loopback 7340. The browser derives its WebSocket origin from its
current address; the localhost SSH tunnel and remote HTTPS UI use the same code.
The raw VNC ports have no separate password because they are guest-loopback-only
and accessed through pinned SSH. Guest users can access them; this remains a single
shared-account VM, not a boundary between mutually untrusted local users.

The Apps on your computer section contains HTTPS browser bookmarks. Marketplace
search queries Composio's toolkit catalog and manages API connections. A bookmark
never proves authentication. Custom auth-config listings are explicitly projected
to ID, name and auth scheme; credentials are never returned.

Read-only resource sampling runs a fixed Python script inside the guest. CPU is a
250 ms /proc/stat delta; memory uses MemAvailable; disk covers /workspace's mounted
filesystem. The UI refreshes visible metrics every five seconds. Samples are not
host-machine metrics or a historical monitoring database. The compact VNC panel
can expand only when explicitly requested.

Composio requests go from the Rust host directly to its fixed HTTPS v3.1 API. The
project key is private server configuration; OAuth credentials remain with Composio.
Only locally linked accounts for a persistent Kindred user id can execute tools.
Catalog results carry concrete versions and input schemas. The server checks toolkit,
version, argument names/types, account identity and current connection status.
A strict read-tool allowlist governs read-only access; unknown tools and mutations
require one-use approval in Ask and Approve for me modes; Full access skips prompts
while preserving connector access limits and provider-granted scopes. The account
binding and cancellation state are checked again after approval. Writes are never
automatically retried. Account responses are explicitly projected; raw OAuth state
is not returned to the model, UI or logs. Tool data is untrusted content and may enter
local transcripts and the selected model context. Composio and Google have their own
data handling and retention policies, independent of OpenRouter ZDR.

Tool requests, execution starts, results and run completion are distinct durable
events. The activity endpoint reads the latest relevant events. Character reactions
reflect those events and run status; the avatar picker is an explicitly labeled preview.

## Access from several devices

Authenticated clients can issue one-use device links, limited to eight pending
links and a ten-minute lifetime. The random code is in the URL fragment, cleared
by the page before confirmation, and exchanged only by a same-origin POST. Links
expire on server restart. A linked device receives the same personal account's
full-access bearer token. There are no per-device roles or individual revocation.
Without Remember, the client uses sessionStorage. Opting to keep a device connected
also stores its token in localStorage; Disconnect this app clears both stores.

## Activity characters

Server activity includes the most recent task state and timestamps. Bots stay online
during active turns and for 30 minutes after their latest persisted activity. The
attention endpoint reads all persisted runs, events and bot messages, including
bots outside the recent-runs feed. After 30 minutes, the green presence dot disappears
and the bot holds an open-eyed pose looking slightly up and right, without breathing,
blinking or changing expression. Reloading the app does not restart the idle window.
The client cycles build tools every nine seconds and uses the
same bot activity for sidebar, header and chat characters. Cached working
characters and synchronized motion timelines prevent refreshes from restarting loops.
Completed replies return directly to the normal idle pose; task completion does not
trigger a celebration. Working chat rows use a 48-pixel character without an inline
Stop control. Their status text shimmers on hover or keyboard focus and is visually
hidden otherwise.
Per-bot phases keep teammates from blinking in unison. Decorative tool lines
and facial features reveal only after the silhouette transition. Reduced motion and
in-app preferences suppress loops and finish morphs immediately.

Sidebar rows retain the most recent message excerpt during work, approval waits
and idle. Activity labels remain in the working conversation, while the top bar
contains the selected name and avatar without a status subtitle.

New bot appearances and new working-message avatars have a 1.15-second dot-to-body
entrance. Its timestamp belongs to the cached avatar and is not restarted by polling,
search, or selection changes. The silhouette and face settle before a tool morph begins.
Generic thinking uses a shared RAF renderer: gentle squash and lean, followed by an
occasional 0.72-second turn within an 18-second cycle. Face projection and clipping
simulate the face passing behind a body that retains its shape and volume. A short
gradient stroke follows the turn; no stars or dream decorations are added. The
server activity timestamp keeps different sizes of the same bot in phase. The loop
stops for disconnected or hidden avatars and respects reduced-motion changes while
running. These are original SVG transformations; reference GIF frames are not bundled.

Unread blue dots use persisted, monotonic per-chat read cursors. Public bot messages
and questions can create unread activity; the user's own messages and private
reasoning do not. Reads acknowledge the captured message cursor only after the
latest history is rendered in a focused, visible conversation at the bottom.
Hidden tabs, settings dialogs, expanded computers and older history do not clear
the dot. A reply arriving during acknowledgment stays unread, including when its
cursor is newer than the rendered history. Receipts survive restart and synchronize
across the personal workspace's clients. Blue dots are independent of OS notification
preferences. Before this feature there were no read receipts, so existing bot history
appears unread until viewed.

The bot-name panel edits name, label, description and notifications through a narrow
transactional identity update that preserves model, instructions, memory and avatar.
Drafts survive polling and avatar customization; those writes serialize within the
client. Connections, Skills and the deeper settings gear remain available. The monitor
opens the compact computer panel. Clicking its screen or the expand control focuses
the computer; noVNC refits after the panel animation so transformed intermediate
bounds cannot leave the screen at thumbnail size.

## Direct messages, collaboration and pins

A direct message always queues its selected bot, even when it contains another
bot's mention. Group mentions retain explicit recipient routing. The composer
never creates a group as a side effect of mentioning a teammate in a DM.

A real `send_to_bot` call from a DM atomically creates one shared chat per
collaboration round, adds the recipient, writes a named bot-to-bot request and
adds a link to the original DM. The source run and user message stay in the DM.
Only the authored request is shared, not private DM history. The recipient's
result queues the requester in the shared chat. That continuation also receives
its own original task, preserving the requester's purpose without exposing the
private prompt to the recipient. Existing turn budgets, cancellation and
idempotent completion still apply. Role guidance distinguishes a teammate's name
from their assigned responsibilities and saves concrete lasting facts to memory.

Provider progress remains in assistant events and is visible during execution.
Completed output uses the final assistant message instead of concatenating every
progress update and acknowledgement into a single answer. The Pi bridge still
checks the complete emitted transcript against its completion frame.

Bots retain the profile pin flag; chats gain a persistent pin flag. Dedicated,
authenticated pin endpoints change only that flag, so pinning during work cannot
overwrite a bot's freshly saved memory or edit chat membership. Chat readbacks
include the latest non-system conversational message for sidebar previews. Bot
previews use their own DMs. Hover and keyboard focus reveal a bounded preview
beside the pinned tile; narrow screens keep it within the viewport. Group composers
use the same 27-pixel provider logo as direct messages.

## Separate screens and client behavior

A persistent SQLite mapping assigns each bot a unique slot from 1 through 32. Slot 1
retains the original desktop and browser profile. Additional screens start lazily
through a lingering unprivileged user systemd manager and `kindred-screen@.service`.
Each runs Xvfb, Openbox, Chromium and two loopback VNC listeners. Profiles live under
`/home/bot/.local/share/kindred/browser-N`; the first retains `browser`. Files in
`/workspace`, API app connections and the Codex auth store remain shared. Bots must
coordinate shared-file changes; these screens are not a security boundary.

The host adds the screen number outside model-controlled tool arguments. The guest
validates the slot, sets DISPLAY for every input/screenshot/shell operation, and opens
URLs in that screen's persistent browser profile. The VNC bridge sends a heartbeat
every 20 seconds. The client obtains a fresh single-use ticket when reconnecting,
uses bounded backoff, and cancels stale attempts when the selected screen changes.
Returning control broadcasts a screen-specific close to interactive viewers.

`react_to_message` stores a bot-authored emoji against a message in the current chat.
It cannot target another chat. Repeated reactions replace that bot's prior emoji.
Reaction-only completion is allowed; the UI skips empty result bubbles. Direct chats
use the bot identity in the header; group chats label a new sender group. Internal
completion events and zero-tool turns do not create activity summaries.

Usage is read from the guest's authenticated Codex app-server. The UI prefers
`rateLimitsByLimitId`, computes remaining as `100 - usedPercent`, and uses each
window's reported duration/reset time. Missing windows are not treated as unused.
Opening the menu never consumes reset credits. Composio failures retain HTTP status,
a sanitized provider error code and request id, without echoing credential fields
or arbitrary provider error bodies. See the official [project-key permission guide](https://docs.composio.dev/kb/guide/platform-project-api-key-permissions).

## Desktop release channel

Windows installs keep a stable launcher and atomic current.json pointer under the
user's LocalAppData Programs/Kindred directory. Each release occupies a distinct
versions/N.N.N directory. The native local updater window calls narrowly scoped
Rust commands that reject the remote main window; it cannot supply paths or shell
arguments. The remote workspace only requests this native window through an
intercepted kindred-update URL. Window creation uses a separate thread to avoid the
[documented Windows WebView2 deadlock](https://docs.rs/tauri/latest/tauri/webview/struct.WebviewWindowBuilder.html).

The updater verifies an RSA/SHA-256 signature against the bundled public key,
checks channel/platform/version, downloads from the configured HTTPS origin, and
checks the signed size and SHA-256 digest. Redirects, oversized downloads, unexpected
archive paths, duplicate names and missing required files are rejected. It stages
files before changing the version pointer. Only the exact requesting installed app
process is closed; failed startup restores the prior pointer and launches it again.
The local updater reports progress through a bounded, atomically written status
file. Closing its popout cancels work before the restart phase. Provider secrets
are not needed to download a release. The private signing key is not on the server.

Composer drafts are saved locally for the requested restart in a token-hash namespace,
then consumed by the next app instance. Draft contents remain local to the client.
The desktop update does not deploy the Linux server or stop its bot runs.

### Approval modes and action scope (0.8)

Every gated action resolves the stored bot override and global default again, so an
already-running bot sees preference changes. Ask gates computer mutations; auto permits
VM tools only when action_scope is explicitly routine_vm. Missing, external, or unknown
scope asks. Routines and external connector writes ask in auto. Full skips these prompts.
Connector read-only limits and account/session validation remain enforced in all modes.

Computer/shell scope is classified by the model from the action's effects; it is not
a network sandbox or a deterministic semantic proof. Instructions explicitly classify
sends, publishing, remote deletion, account changes, scripts with network writes, and
uncertain effects as external. A misclassified arbitrary shell/browser action is a
limitation of auto mode; use ask for individual review. Connector writes are gated
independently of model-declared scope. Never use action_scope to bypass a denied action.

Preference writes are serialized per editor, with latest-change status and explicit
retry after a failed save. Theme previews are immediate; the Saved status confirms
server persistence. Avatar edits save on selection. SVG morphs restore exact Bezier
paths after morphs. Eyes look around by default, following local face curvature
and foreshortening. Generic thinking, investigation and reading retain the normal
working body motion.

### Release notification and idle rendering (0.9)

The browser checks the public stable feed on launch and every minute while visible.
The bounded manifest is verified against the installer's pinned RSA public key before
a numeric version comparison with the native injected desktop version. Equal, older,
invalid and unavailable feeds hide the notification. Actual installation remains in
the native signed updater. Browser-only clients compare UI/server versions instead.

The contenteditable composer grows naturally until about twelve 22px lines, then
scrolls, and cannot be compressed by chat layout.
Version 0.11.1 removes the idle star twirl, dream graphics and yawn overlay.
The connection-page mascot blinks and looks around without credentials or backend activity.
OpenAI provider marks are separate unmodified official assets; see third-party notices.

### Computer workspace and teaching (0.10)

Opening a computer explicitly chooses a view-only VNC ticket. A pre-existing takeover
flag does not silently make the new viewer interactive. Take control explicitly
requests the existing per-screen lease. Status epochs prevent an older parallel
refresh from overwriting the post-takeover status. Watch tickets remain input-blocked
on the server. Expansion stays within the Kindred content window.

The teaching flow acquires control, records click/drag coordinates, scrolling and
navigation keys in browser memory, and omits typed text. Add step pauses recording
and collects a description plus a local canvas thumbnail for review. Resume is
explicit after disconnect. The selected screen cannot change during a lesson.
Saving writes reviewed instructions and a success check through the existing shared
Skills API, then returns control. Raw events and thumbnails are not persisted or
sent to a model. This is an annotated demonstration workflow, not automatic visual
skill inference or a coordinate-replay engine. Closing the view pauses the lesson;
discard and reload protection prevent accidental loss.

Reboot uses the same all-screen idle/takeover checks and lock acquisition as the
existing VM actions. The privileged helper and sudo rule allow only the pinned VM.
Marketplace detail reads validate the toolkit ID and project an explicit metadata
allowlist; provider credentials never appear in the response. No connector approval
or OAuth scope policy changed.

### Interaction recovery and connector inspection (0.11)

Action controls expose pending state and ignore repeated clicks until completion.
Requests time out after 60 seconds; write timeouts tell the user to check whether
the action completed before retrying. Settings and marketplace failures provide
explicit recovery. Query generations discard obsolete marketplace responses.
Avatar updates are serialized per bot and refresh epochs protect newer saved state.
Skill editing counts UTF-8 bytes against the server's 16 KB limit before submission.
Teaching Resume explicitly reacquires control after reopening a paused viewer.

The connector dialog retains sign-in, pending, verified and disconnected states in
place. Connection changes refresh the surrounding marketplace. Tool inspection uses
the authenticated `/api/marketplace/{id}/tools` route and existing account access
filter; expired accounts fail before provider dispatch. Returned tool descriptions
show read-only or approval-required policy. Inspection does not execute tools or
change OAuth scopes. Real account-level checks require a completed connection.

Settings, menus, loading placeholders and action feedback share short transitions;
system and in-app reduced-motion preferences suppress them. Long chats expose a
Latest messages control without moving readers away from older messages.

### Character corrections (0.11.1)

Sleep retains the selected SVG path and applies a slight whole-body compression
and lean around its baseline, including the face. Pose interpolation reverses
cleanly if the activity changes. The zzz and subtle breathing remain; there is no
yawn overlay or dream artwork. Reduced motion preserves the static sleep pose.
Body action loops are disabled during morphing and start from their initial frame
only after the exact tool silhouette is restored. Blink phases remain independent.
The Connections chain path stays inside its SVG view box with stroke clearance.
At 0.11.1, both avatar pickers exposed sixteen colors, including explicit Black / white;
the adaptive color renders white in dark mode and black in light mode.


### Named connector accounts and human subtasks (0.12.0)

Connector settings retain the legacy map entry for an existing account. Additional
entries have independent opaque storage keys and provider account IDs. A missing
account name in older data deserializes as `default`. Account names are local,
case-insensitively unique within one toolkit, and limited to 80 characters. Each
toolkit allows ten accounts. Account management, tool discovery and execution bind
to the selected provider `account_id`. Omission is accepted only for a single
account, for older clients; ambiguity fails before dispatch. Provider user, toolkit,
auth-config, active status, concrete tool version and permission checks remain in
place. Approval details include the selected name and ID. Changing the connection
while an approval is pending invalidates the prepared dispatch.

Hosted sign-in links are stored privately for initiated accounts so Authenticate
reopens an unfinished sign-in without making another grant. These URLs are excluded
from the tool-visible account catalog and removed after activation or disconnect.
The direct REST link endpoint supports additional accounts; SDK `allow_multiple`
is a client-side check, not a field sent to that endpoint. Account credentials stay
with Composio. No external accounts are authorized by installing an update.

`request_user_action` inserts a durable `user_tasks` record and changes the existing
run to `awaiting_user`. The original provider future stays suspended inside that
tool call. The scheduler releases its screen lease and suspends the run time budget.
The human explicitly takes over that screen. Done closes control sessions, acquires
the screen lease, binds the completion to the exact bot/run/subtask, and marks it
ready while clearing takeover. The scheduler reacquires the lease before marking
the subtask resumed and letting the tool result return to the original provider.
The result tells the bot to inspect a fresh screenshot and verify the state. A
skipped subtask reports that outcome without implying success. Repeated Done requests
are idempotent and do not release a later control session. Cancellation expires the
subtask; restart interrupts the run and expires pending requests without replay.

Queued work for the same bot stays blocked while awaiting the user. Other bot
screens remain independent within the configured concurrency limit. The existing
run retains its capacity reservation while paused. The human flow does not record
input or capture screenshots for the model while the user is working. The separate,
explicit teaching feature keeps its prior privacy and review behavior.

The UI uses compact conversation cards, a single rounded composer, grouped account
rows, a fourteen-color palette with two neutrals, and a static connecting mascot.
Historical permission decisions are projected alongside each run's events. Existing
approval policy semantics are unchanged; this release does not add natural-language
auto-review rules.

### Screenshot delivery and desktop integration (0.13.0)

The original screenshot tool returned an image to the provider but stored only a
`has_image` flag in events. That could not deliver images to chat. A screenshot
requested with `share_in_chat=true` now stores a PNG attachment and metadata event
atomically. Images are limited to 5 MB and twenty per run. Ordinary inspection
screenshots remain provider context only. Both provider tool specifications explain
the distinction. Run details and completed chat messages carry attachment metadata;
the authenticated `/api/attachments/{id}` endpoint serves image bytes with no-store
headers. The frontend creates temporary blob URLs for preview, enlargement and
download. The small circular SVG download link appears on image hover or keyboard
focus; devices without hover retain a visible touch target. Neither bearer tokens
nor image payloads enter link URLs or event text.

Each bot has a `notifications` preference, defaulting to true for older profiles.
The authenticated notification feed uses durable event sequence numbers, suppresses
muted/archived bots and resolved approval/human requests, and skips historical events
when a client first connects. Batches retain their cursor so no completions are
dropped at a page boundary. The native client polls on its own background thread,
independently of webview visibility; browser clients use permission-gated web
notifications. Closing every client leaves server work running but stops device
notifications until a client is opened again. Text is limited to the bot's name and
a generic completion/input notice. Task contents are not put on the lock screen.

The native window has custom decorations with Windows/Linux controls or macOS
traffic lights. Runtime capabilities grant only four bounded desktop commands to
the configured origin in the `main` window. Every handler rechecks that origin and
window label. No host files, shell execution or general Tauri APIs are granted.
Updater commands have a separate capability limited to the bundled `updater`
window. On Windows, notification identity is registered under the current user's
`Software/Classes/AppUserModelId/dev.kindred.personal`; no administrator access is
required. OS notification failures are returned and shown in notification settings.

Version 0.13.0 did not use Pi: its missing chat image was an attachment transport
gap. Version 0.14.0 separately replaces the non-Codex loop as described below.

### Pi provider harness (0.14.0)

The pinned Pi coding-agent SDK owns OpenRouter's agent loop, provider conversion,
tool argument validation and proactive context compaction. Rust launches one
worker per active OpenRouter run, sends the selected model, tool schemas,
instructions and API key through bounded stdin, and accepts only versioned frames.
Built-in host tools and resource/plugin discovery are disabled. Tools call back
into `runtime::call_tool`, so both providers use the same approvals, account
bindings, screen leases, guest execution and screenshot attachment transport.

The SDK executes calls sequentially; both sides enforce the action budget and
Rust rejects duplicate tool-call IDs. Pi model requests, including compaction,
retain the selected model and OpenRouter privacy settings. The injectable fetch
transport rejects redirects and bounds payloads and response streams. Upstream
error bodies never become task errors. Automatic retry is disabled and failures
never switch providers. The outer Rust scheduler owns cancellation and active
run time; a suspended human step releases its screen lease while retaining the
original Pi call and conversation. Dropping a provider future kills its worker.

Pi credentials, settings, session state and model catalogs are in memory. Its
isolated resource directory contains no discovered user files and is removed on
normal completion. It receives no inherited server credentials. The Rust database
remains the durable conversation store; Pi compaction does not expand prior-chat
history or alter other chats. Older screenshots are omitted from model context
without dropping their tool-result IDs or deleting shared image attachments.

The adapter registry separates provider metadata and endpoint/privacy policy from
the SDK session. Future custom or local endpoints can use this boundary; neither
arbitrary URLs nor additional providers are currently exposed. See the
[harness guide](../harness/pi/README.md) for installation, extension requirements
and exact dependency versions.

## Teammate drafts, attachments and chat position (0.17)

The `draft_bot` tool fills a `bot_drafts` row and a `bot_draft` chat message without
creating a bot. The card exposes Create and Details; the latter prefills the normal
creation form, including instructions. Authenticated creation validates the edited
fields and atomically inserts the bot and its DM, then records the created ID. Repeated
requests return that same bot. Model/provider settings carry over; private memory and
per-bot approval overrides do not. An archived chat cannot create a pending proposal.

Authenticated uploads accept up to 8 MB per file. Up to five IDs bind to a new user
message inside the same transaction as its queued runs, with duplicate and cross-chat
IDs rejected. Unsent uploads expire after 24 hours on the next upload; sent files stay
in SQLite. Downloads use authenticated fetches and attachment disposition. The
`read_attachment` tool requires membership through the current run's chat, transfers
bytes on SSH stdin to a fixed VM Python program, and returns the saved path plus text
or a PNG/JPEG when supported. Directory handles and O_NOFOLLOW prevent symlink-based
write redirection. No filenames or file contents are interpolated into shell code.

Chat refreshes snapshot a visible message anchor when reading history and follow the
bottom when reading current messages. ResizeObserver restores that intent after
images or composer layout change. Scroll positioning is immediate; CSS smooth scroll
and browser anchoring cannot compete with the refresh. User gestures take precedence.

Bot settings preserve instructions and memory atomically. Separate text editors use
field-only writes with a comparison against the opened value, so a stale editor cannot
overwrite a concurrent model memory update. Model memory writes also update only memory.
Global notification frequency filters the shared server feed, covering browser and
native clients. Muted events still advance the cursor and cannot become a backlog.

Group navigation uses bounded participant stacks containing the user's initials
and member avatars; extra members have a count badge. The header uses a compact
version of the same stack. Group sender blocks reserve an avatar gutter and tint
names to maintain contrast in either theme. Consecutive messages retain alignment.
Right-click, Shift+F10 and the header action button expose rename/pin/settings/archive.
Bot rows and pinned bot tiles also handle right-click and Shift+F10. Their menu
exposes Edit bot, Pin/Unpin, Instructions, Memory and Archive bot; row overflow
buttons and the header offer the same controls without a context click. Archive
fetches current preferences and preserves instructions/memory; active tasks and
routines still block it. Archival is reversible through General settings and does
not delete history. Menus remain within the viewport and restore keyboard focus
even if polling replaced the originating sidebar control.
Rename retrieves current chat metadata before updating its name, preserving members
and pin state; message history and bot profiles are not part of this write.

## Conversation decisions and scheduled checks

`ask_question` stores an immutable question/context/options card, keyed by bot,
conversation and stable topic. Custom text is always available. Both Codex dynamic
tools and its native input request, and the Pi/OpenRouter tool loop, end at the
persisted question boundary before returning an empty answer to the model. This
releases the screen and concurrency slot instead of retaining a provider connection
while the user is away. The existing `request_user_action` remains a separate
same-turn handoff for interactive sign-in and sensitive entry on the computer.

The authenticated answer endpoint validates the selected option or custom text,
current membership and archived state. One transaction saves the answer and queues
one continuation for the question's original bot/chat, with the original request,
question context and actual answer. An identical retry returns that continuation;
a conflicting later response is rejected. The answer is not proof that a requested
action succeeded and does not change the existing action-approval policy. Questions
and answer receipts survive restart; interrupted external actions are not replayed.
The current run ID, continuation ownership and current continuation status are
explicit in the model context and decision lookups. The assigned continuation must
perform the chosen work; a later routine check must not repeat that work merely
because it sees an answered card. The task trigger also explicitly distinguishes a
routine check from a conversation or continuation, so quiet completion is available
only in the appropriate context.

Recent decisions appear in bot instructions, and `decisions_list` can fetch older
topics exactly. Reusing a pending topic returns the existing card; an answered topic
returns its decision and continuation reference without creating another action.
The model must use a stable key for an unchanged issue. A materially different issue
needs its own topic. This is not semantic email deduplication or an inbox connector.

Routines retain their interval mode and optionally store a weekly schedule: IANA
time zone, ISO weekdays, first/last local time and repeat minutes. Scheduling uses
[chrono-tz](https://docs.rs/chrono-tz/0.10.4/chrono_tz/) and
[Chrono's local-time mapping](https://docs.rs/chrono/0.4.45/chrono/offset/enum.LocalResult.html).
Nonexistent spring times are skipped; a repeated fall time executes at most once.
The scheduler skips off-window missed checks and coalesces eligible missed ticks,
then advances to the next wall-clock slot without drifting the requested minute.
Paused routines do not schedule. The Run now endpoint retains routine provenance.
The check's prompt stays in task history without adding a repeated user message
to the conversation or replacing its sidebar preview.
`finish_quietly` is restricted to routine runs and suppresses a successful completion
notification; failures still notify. Fresh questions notify once and suppress the
redundant completion notification. Global and per-bot notification preferences apply.

Browser navigation launches Chromium without waiting for its long-lived process to
exit or killing it when the request returns. An immediate launch error is surfaced;
otherwise the result says navigation was requested. A fresh screenshot is required
to establish which page loaded.

## Message replies and user reactions

Authenticated message sends accept an optional original message sequence. The
server resolves it in the current chat and stores its relationship to the new
user message and queued runs in the same transaction as uploads and routing.
Selected source text is available in model instructions as quoted context,
separately from the user's new request. Source excerpts are bounded to 8,000
characters and mark truncation; the full original remains in chat history.
In a group, an unmentioned reply addresses the original bot author; explicit
mentions override that choice. DMs keep their existing routing. A removed author
requires an explicit current recipient. Invalid sources and failed attachment
binding roll back the whole send.

User reactions have their own message-keyed table, so they cannot impersonate a
bot or collide with existing bot reactions. PUT sets/replaces one allowlisted
emoji; null removes it idempotently. Archived chats and cross-chat message IDs
are rejected. Reactions do not queue bot work. History responses decorate each
message with user/bot reactions and its quoted source, including sources outside
the loaded history page.

The UI groups matching emoji into badges, marks the user's selection, and keeps
reply drafts scoped to their chat. A failed send retains the text and quote;
cancelling a quote keeps the text. The existing update/resume flow carries both.
Menus support keyboard access and reposition against the current message after
refresh. Narrow and touch layouts expose actions below the bubble. A sent quote
can load and highlight its original message through bounded history pagination.
