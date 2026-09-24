# Kindred user guide

Detailed setup and feature reference. For downloads and a quick introduction, see the [README](../README.md).

## Start a server

Install Docker Engine with Compose (Linux), or Docker Desktop (Windows/macOS), then:

```sh
git clone https://github.com/awpsec/kindred-server.git
cd kindred-server
docker compose up -d --build
```

Open `http://127.0.0.1:9444`. Create the first account to become the server
administrator. Add a provider and create a bot. Its profile's computer is created
on the first message, with **2 CPUs, 6 GB RAM and a 30 GB persistent disk**. Other
bots in the same profile share that computer; each profile gets a separate VM,
provider credentials, database, browser sessions and workspace.

The first image build and first computer setup download software and can take
several minutes. No computer is created just by registering or opening the app.
The defaults, limits and port are in `compose.yaml`; `.env.example` documents
overrides. A server needs room for 6 GB per running computer plus host overhead.
On Linux with `/dev/kvm`, enable hardware acceleration:

```sh
KINDRED_KVM_GID=$(stat -c %g /dev/kvm) docker compose -f compose.yaml -f compose.kvm.yaml up -d --build
```

Without KVM, QEMU uses software emulation. This also supports standalone on
Windows/macOS through Docker, but first setup and CPU-intensive tasks are slower.
See [deployment and backups](HOSTING.md) for HTTPS, team access and recovery.

## Desktop and accounts

On Windows, double-click **Kindred-VERSION-Setup.exe** from the release. The same
installer upgrades existing installations and preserves profiles and settings.
On other platforms, install the matching desktop package. Launch Kindred and enter your server's HTTPS address,
or choose **Standalone** to run the bundled server through Docker on this machine.
Standalone restores its server after launch when needed. Profile computers start
when needed for a task or provider sign-in. **Open Kindred when I sign in** is an
optional setting in the profile window.

Use **⇄** beside your name to switch accounts. **Add account** offers sign-in or
account creation, with an optional server choice. Each new account gets its own
bots, conversations, providers and computer. The app restores your last account
and window placement. **Manage accounts** keeps saved sign-ins across servers.
Existing accounts with multiple workspaces retain them in **Account settings**.

The first administrator can close registration, issue one-use invitations and
disable accounts. Disabling an account revokes sessions, stops queued and active
work and closes its computer viewers. See [account boundaries](PROFILES.md).

## What works

- Compact expandable AI account cards for Codex, Claude Code, Kimi Code, OpenRouter
  and custom OpenAI-compatible providers. Subscription sign-in stays in the official
  provider CLI. Add custom providers by name, base URL and optional API key; Kindred
  discovers their model catalogue automatically. Each saved provider gets its own
  named card, with another **Add custom provider** card below. **Refresh models**
  reloads the catalogue on demand. Startup refreshes run in the background, and
  failed refreshes retain the last successful catalogue. Keyless private endpoints
  use **No API key required**.
- Hover **You → Usage → provider** for that account's usage; provider status loads
  in the background. OpenRouter and custom providers preview five bots with avatars.
  Click the provider or **View all** for searchable, sortable history including
  archived and deleted bots. Recorded tokens, reported charges, estimates and unpriced
  requests stay distinct. Disconnected providers with history remain available.
  Claude's account quota links to its
  usage page because its CLI does not expose an account-wide quota query here.
- Optional local access in the updated Windows app: a persistent `workspace` inside
  the Kindred installation, separate from the version directories. Enable **Settings →
  Computer → Allow local access**, choose **Desktop permissions**, then enable **Local access**
  in the bot's gear settings and select that desktop. All three permissions default off.
  Choose workspace files only, ask before outside files or any command, or full access
  as your OS account. Keep that desktop app open for local operations. Browser clients
  do not expose local files. Local file content and command results go to the bot's AI provider.

- Hover a message to react, reply, or open its Copy menu. Reactions persist and
  your own reaction can be changed or removed. Replies use an inset quote in the
  composer, retain the original message in history, and send that context to the
  bot. In groups, replies address the author unless you mention another teammate.
  The composer softly highlights on hover and focus, opens upward for a reply,
  and eases back when cancelled. Reduced-motion preferences are respected.
  Conversations span the available pane, with bot messages on the left, your
  messages on the right, and a composer aligned to both edges.

- Bots can ask a question with selectable choices and a custom response. The
  selected answer stays in chat and queues one continuation with the same bot.
  Decisions survive restarts and recurring checks reuse them instead of re-asking.
- Bots can create shared checklists from your objectives and action items, track
  a current item, and save contextual one-time reminders. Tick items in chat,
  edit or reorder a list, or choose **Work on this**. **Lists & reminders** keeps
  them accessible later. Simple reminders arrive without waiting for a busy bot.
  See [lists and reminders](LISTS_AND_REMINDERS.md) for delivery behavior.
- Routines support selected weekdays and a time window in a named time zone,
  including hourly checks from 08:33 through 19:33. Quiet checks can finish without
  a reply or completion notification; meaningful changes can request a decision.

- Rounded computer previews and a dark neutral desktop wallpaper with soft colour, with a native
  translucent Browser / Files / Terminal dock on every bot screen. The dock reserves its own
  desktop space and does not sit over application content.
- Named bots with original animated SVG characters, eight shapes, colors,
  labels, descriptions, pinning, role instructions, and editable persistent memory.
  The first shape is a true circle; the second is a softly asymmetric pebble.
  Eye placement, gaze and blinking adapt to each shape; avatar changes update
  the welcome screen while you customize.
  Eyes also ease into round wide looks, narrow squints and uneven questioning
  expressions, with pauses between them and a calm pose for reduced motion.
- Bots can draft a new teammate with a name, avatar, role, description and instructions.
  Review its chat card, choose **Details** to edit it, or **Create** to add it.
- The composer **+** menu offers **Attach files** (up to five files, 8 MB each) and
  **Teach a task**. Bots can read attached files in their shared computer.
- Bot Instructions and Memory open in large editors. Notifications have a simple
  per-bot toggle and a global **All / Input needed / None** preference.
- Monochrome dark/light themes, identity preferences, bot details, and responsive chat.
  Skills and Connections live in Settings; each bot’s routines live in its computer workspace.
- Persistent group chats with up to six bots, inline avatar mentions, chat renaming,
  archiving, and synchronized conversation history across clients.
- Public progress replies remain as separate chat messages after a task finishes.
  **Settings > General > Show activity in chats** controls tool logs for every chat
  and defaults off. Approval cards and sign-in requests remain visible.
- Replies ease into the conversation while the working bot briefly lingers, then
  shrinks into a dot. Reduced motion skips this sequence; old history never replays it.
- Role handoffs tell the recipient to save its own assigned responsibilities before
  replying. Each bot has an explicit identity and teammate directory. Repeated
  handoffs between a pair reuse their active collaboration chat.
- Bots can use websites on their own computer when a connector is missing or failing.
  They can open a sign-in page and hand control to you, then resume the same task.
- History loads in 50-message pages as you scroll, with up to 150 messages displayed.
  Offscreen pages unload and can be fetched again. The six most recently visited chats
  keep their loaded window and reading position when you switch between them.
- Teammates can invite one another, request work, receive automatic replies, and
  continue over multiple turns. Requests and tool activity are visible in the chat.
- Durable tasks, task history, live activity, cancellation, and compact permission cards
  with the exact action details and a lasting approval receipt.
- A bot can ask you to complete a computer subtask. Take over, then press **Done with
  subtask** to resume the same task. The bot time limit pauses while it waits for you.
- Small activity characters in chat and the sidebar, with smoother morphs and loops.
  Sidebar previews keep the latest message visible, and the top bar shows the name
  and avatar. New bots grow from a colored dot; working bots gently wobble and
  occasionally turn, with their face moving around the body and a short colored trail.
  Idle bots blink and look around, with eyes that curve and narrow as they turn.
  They stay awake; generic thinking uses the normal working motion, with less frequent
  turns. Long builds rotate through hammer, saw and drill. Motion preferences are respected.
- Eight original body shapes and fourteen colors in two complete rows. Grey and
  adaptive Black / white are the two neutral choices. Eyes are black in dark mode
  and white in light mode. The picker focuses on shape and color, with outlines
  that follow the selected silhouette. Legacy bean/ghost profiles render as hexagon/cloud.
- Per-bot model and thinking-level dropdowns. Codex choices come from the signed-in
  account's current catalog, with only that model's supported thinking levels. Existing
  model choices are preserved by migration; an unavailable choice never silently falls back.
- A searchable Composio marketplace with app onboarding, hosted OAuth/key flows,
  custom auth configurations, and app actions governed by bot approval settings. Name each account
  before sign-in; connect up to ten accounts per app, rename them and select each
  explicitly for tools. Existing connections retain their identity as **default**. Google apps
  retain explicit read-only scope options. Connection checks, recoverable sign-in
  flows and a permission-filtered tool browser help verify available capabilities.
- One persistent Linux VM with a separate desktop and browser profile per bot,
  screenshots, live VNC control, and shared shell/file work in `/workspace`.
- Up to four concurrent bot runs by default (configurable from one to eight), with
  a separate lock and human takeover for each screen. Browser sign-ins are per profile;
  connected API apps, files, skills and the Codex subscription are shared.
- A compact computer side panel keeps chat visible beside the live desktop. CPU,
  memory, disk and uptime are sampled inside the guest and shown there and in Settings.
- Automatic desktop reconnection, a static bot with shimmering Connecting text, and a screen picker.
- A full computer workspace opens in watch mode. Take control explicitly, or teach
  a task through annotated steps that you review and save as a shared skill.
- Clear loading and retry states, duplicate-click prevention, smoother menus and
  settings transitions, and a Latest messages shortcut for long conversations.
- Compact chats, real bot message reactions, optional activity details, and a Skills list/editor.
- Shared text skills, interval routines, and bounded asynchronous teammate handoffs.
- **Codex (Subscription) first:** sign in through the official Codex CLI app-server
  inside the VM. No OpenAI API key is required. Your account must support Codex.
- **OpenRouter through Pi:** the Pi coding-agent SDK manages the tool loop, validates
  arguments and compacts long task context. Choose an explicit model. ZDR routing is required by default;
  data collection and response caching are disabled in the request. Requests fail
  when a compatible endpoint is unavailable; there is no cross-provider fallback.
- Screenshot attachments display in chat and can be enlarged or downloaded from
  the image's bottom-right corner. Ask a
  bot to open a website and share a screenshot; the image persists after reload.
- Per-bot completion and input-needed notifications show the bot's name and an
  actual message preview. Windows uses a text-only notification. Desktop polling continues
  while minimized; browser clients can enable notifications for the current device.
  Keep Kindred open to receive them. OS/browser notification preferences still apply.
- Custom desktop title bars: Windows/Linux window buttons and macOS traffic lights.
- Browser access and a thin Tauri desktop client. The Windows client was built and
  opened successfully. The Linux server and guest were tested on Debian 13.

## Connect your provider

In **Settings → Connections**, expand Codex, Claude Code or Kimi Code, choose
**Sign in**, and complete the provider's interactive login. Each managed computer
installs the official CLIs automatically. Kindred does not extract subscription
tokens for direct API calls. Claude Code runs through the official `claude -p`
path; read the [provider policy notes](PROFILES.md#claude-code-and-subscriptions)
before using a company plan.

OpenRouter and custom OpenAI-compatible endpoints use the Pi SDK on the server.
Save each key through its provider card. New profiles never inherit server-wide
provider environment credentials. Model lists come from the configured provider.
No alternate model or provider is silently substituted.

## Model selection and client updates

Open a bot's details, choose **Bot settings**, then select **AI provider**, **Model**,
and **Thinking level**. Changes save automatically. Account default follows the current Codex default;
Model default follows the selected model's advertised default thinking level. Each Codex
run records its resolved model and thinking level in Activity. Changes affect subsequent
runs. The OpenRouter dropdown lists tool-capable models from its public catalog; actual
access still depends on your account, balance and required privacy routing.

Click **Update Kindred**, or **You → Update Kindred**, to check the signed stable
channel. A native popout shows checking, downloading and verification progress.
Once ready, Kindred restarts automatically into the downloaded version. An unsent
composer draft and the selected chat survive the restart. An offline or rejected
update leaves the installed version in place; a failed application launch restores
the prior version. Updates to this desktop client do not stop server-side bot runs.
Browser clients retain a separate interface reload indicator.

See [desktop installation and release publishing](DESKTOP_UPDATES.md) for the
release process and recovery paths. The feed must be published when shipping future
versions; rebuilding an executable alone does not update the channel.

## Apps through Composio

Save a project API key in **Settings → Connections**, then open **Marketplace**.
Search for an app, choose **Add**, and complete **Continue in your browser**.
Return to Kindred and **Check connection**. Every bot can discover and use the
connected app's tools; actions outside the reviewed read-only policy need approval unless Full access is selected.
See [marketplace setup](COMPOSIO.md). Live Composio consent still requires the
project key and user sign-in on the installed instance.

## Chats and collaboration

Use **+ → New bot** or **+ → New chat**. Select up to six teammates for a chat;
its first member coordinates unmentioned messages. In a group, type `@Piper` to
insert an avatar badge and address that bot; multiple mentions wake those recipients.
In a direct message, mentions are references for the selected bot. Asking Izabella
to consult Piper goes to Izabella first. Her `send_to_bot` call opens a separate
shared chat with a link from the original DM. Piper's answer wakes Izabella there
to continue, including saving lasting role facts to her own memory. The original
DM and unrelated chat history are not copied into the group.

Hover a sidebar row to **Pin** a bot or chat. Pinned conversations appear above the
regular list; hover or focus a tile for the latest message and sender. The same
pin control unpins it. Pins are saved on the server and sync across clients.

A user message has a shared budget of 24 turns, a nesting limit of 12, and at most
three outgoing requests per turn. **Stop** cancels that message's active and queued
collaborators. Completed external actions are not undone. Each bot still shares
the single VM and takes its turn at the computer. Click a group heading to rename,
change members, or archive it; archived chats can be restored in General settings.

## Build from source

Ordinary Rust builds use the checked-in browser bundle and need no Node build step.
To refresh it, run `npm ci --ignore-scripts && npm run build` in `tools/frontend`.
The noVNC source and license notices are included under `third-party/`.
OpenRouter execution and its protocol tests require Node 22.19+ and the Pi
dependencies: run `npm ci --ignore-scripts` in `harness/pi` first. The Rust protocol
tests launch the real SDK worker against local fixtures; see its README for runtime
path overrides. Production Codex-only execution does not launch Pi or require Node
on the server host (the guest Codex CLI has its own installation).
Install stable Rust, a C compiler, and platform build essentials for bundled SQLite:

```sh
cargo test --locked
cargo build --release --locked
./target/release/kindred init
./target/release/kindred check
export KINDRED_TOKEN="$(./target/release/kindred token)"
./target/release/kindred serve
```

This starts the UI at `http://127.0.0.1:7340`. Configure a guest before running tasks.
Full provisioning and service instructions are in [docs/SETUP.md](SETUP.md).
Before packaging coordination or connector changes, run the source-only
[coordination preflight](COORDINATION_PREFLIGHT.md). It checks backend and
browser workflows without building installation packages.

The optional desktop app uses the operating system webview:

```sh
cd desktop
cargo build --release --locked
```

On Windows, use the Rust MSVC toolchain with Visual Studio C++ build tools and
WebView2 Runtime. Linux desktop builds additionally need the Tauri WebKitGTK 4.1
development prerequisites; See the release workflow and verification report for platform build results.
The supplied Windows build used the GNU target and includes its WebView2 loader.
Set `KINDRED_SERVER_URL` or pass the server URL as the first argument. A remote
address must use HTTPS; localhost HTTP supports local servers and tunnels.

## Resource model and boundaries

The small Rust process excludes guest VMs, browsers, provider CLIs and native
webview processes. Default computers use 2 CPUs, 6 GB RAM and a 30 GB thin disk.
The server limits running computers and checks available memory before starting
another. An idle computer can be shut down from its computer controls; its disk
is preserved. Closing the desktop does not stop hosted work.

Bots within one profile share a guest account and filesystem. Their screens and
browser profiles provide workflow separation. Separate profiles use separate QEMU
guests with distinct SSH keys and pinned guest host keys. Bots have passwordless
sudo inside their profile's VM, with no host filesystem mount or Docker socket.
The default guest firewall blocks private/host network egress, but guest root can
change that policy; mandatory network restrictions must be enforced outside the VM.
A server administrator can access server data and guest disks. Protect the host and
backups accordingly.

Provider policies still apply. A subscription does not itself imply ZDR. OpenRouter
ZDR addresses provider retention, not local transcripts, websites or backups.
The native local-access feature is opt-in and scoped separately for each profile.
OS vendor signing/notarization requires release-owner signing identities; the
project's signed Windows update feed is a separate mechanism.

See [architecture and privacy](ARCHITECTURE.md), the
[verification report](VERIFICATION.md), and [third-party notices](../THIRD_PARTY_NOTICES.md).

## Provider references

- [Official Codex app-server](https://learn.chatgpt.com/docs/app-server)
- [Codex authentication](https://learn.chatgpt.com/docs/auth)
- [OpenRouter ZDR](https://openrouter.ai/docs/guides/features/zdr)
- [OpenRouter response caching](https://openrouter.ai/docs/guides/features/response-caching)
- [Grok Bot public overview](https://docs.x.ai/grok-bot/overview)

## Preferences and approval defaults

Settings and bot preferences save as they change. Theme switches take effect immediately;
inline status distinguishes Saved from a failed write, with a retry control. The small
download icon beside the bottom-left name plate opens desktop updates.

General settings offers Ask for approval, Approve for me, and Full access. Bots inherit
the global default or use an explicit per-bot override. Approve for me permits routine
VM work and asks before external changes. Full access removes action prompts; read-only
connector access still applies. Bot animation is always enabled, with the system and
workspace reduced-motion preferences honored. Chalk/white bots become dark in light mode.

Version 0.9 refines shared spacing and narrow Details layouts, replaces the composer
provider label with its icon, and lets the editor grow to about twelve lines. The
update affordance is hidden until a newer signed desktop release is available.
The earlier stars, yawn overlay and dream artwork were removed. In current releases,
bots keep their normal idle expressions and green presence dot for 30 minutes after
activity, then hold an open-eyed rest pose looking slightly up and right.
Reduced-motion preferences are respected. Blue unread dots persist until the latest
conversation is viewed. The bot name opens editable profile fields; the monitor
opens the compact computer panel, and clicking its screen expands it.

Version 0.10 makes the computer a workspace: Open expands a live watch-only screen,
with separate Take control and Teach a task actions. Teaching records a demonstration
for review; the user adds step descriptions and saves them as a shared skill. Typed
text is omitted, and reference screenshots remain in the current review only.
Routines live with their selected bot's computer and use a compact plus control.
The sidebar has larger characters and a vivid palette, with hover customization in
Details. Computer settings put resource meters beside the preview and include Reboot.
App cards open complete connection descriptions with marketplace navigation.
Pane, dialog, expansion and hover transitions respect reduced-motion preferences.

## Additional provider setup

For an existing Linux x86_64 bot VM, install Python 3.12 or newer, then run
`sudo sh deploy/install-providers.sh` from the source checkout **inside that VM**.
It installs unmodified Claude Code 2.1.263, Kimi Code 1.50.0 and the root-owned
Kindred CLI bridge. On the Kindred server, `sudo sh deploy/install-pi.sh` installs
harness 0.40.0 with the pinned Pi SDK 0.85.1 and Node 24.14.0. Older harness releases
remain intact. Existing server configuration uses the `current` symlink. For an
upgrade, use `sudo sh deploy/install-pi.sh /opt/kindred/pi --stage-only`, wait for
idle tasks, stop the service, and activate the new server and harness together.

In **Connections**, expand Claude Code or Kimi Code and choose **Sign in**. Open
the bot computer, take control, and complete the official login in its terminal or
browser. Return to **Check connection**, then select that provider and model in a
bot's settings. Kindred does not import subscription tokens or redirect them through
an API adapter. A paid subscription and provider eligibility may be required.

For a custom provider, enter its base URL (for example `https://provider.example/v1`),
explicit model IDs and context sizes. It must support OpenAI-compatible streaming
chat completions and tool calls. Optional prices are USD per million input/output
tokens. Private HTTP endpoints are reached from the server, not from the desktop.
Prices are estimates unless the response reports its actual cost. Missing token or
price receipts remain unknown; historical tasks are not retroactively billed.

Kindred hosts private collaborative artifacts on the existing server, including standalone installs. Open **Artifacts** to browse documents, slides, sheets and interactive HTML/React apps, or use a saved `/artifacts/<id>` link. Users and bots edit the same revisioned source and shared data. No separate hosting service is needed; 14 days without an edit takes an artifact offline while preserving its content. See [the artifact workspace](COLLABORATIVE_ARTIFACTS_DESIGN.md).

See [rich chat, artifacts and file delivery](CHAT_ARTIFACTS.md) for supported previews, download cards, document preparation and human handoff limits.

## Release policy

Releases are manual and require explicit owner approval. Every desktop release must include Windows setup/ZIP, an Apple Silicon DMG, and Linux DEB/AppImage. Ordinary pushes start no GitHub Actions builds. See [releasing](RELEASING.md).
