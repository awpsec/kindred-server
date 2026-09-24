# Version 0.48.9 - preserve sign-in through updates

- 158 backend tests pass, including account ownership, persistent server sessions,
  authenticated username projection and bounded, expiring update-session parsing.
- Fourteen Edge/WebKit suite combinations cover saved and temporary sessions,
  account/profile choices, adding another account, settings and existing flows.
- The previous Windows build reproduces the lost-persistence bug with empty browser
  storage. Real native fixtures exercise DPAPI storage, repeated launches, explicit
  fresh-account navigation, temporary sessions and transient server failures.
- Account chooser layouts are checked in desktop dark/light and mobile views.
- The installed Windows package and signed update are verified separately. Test
  accounts and installation directories are isolated from the user's live app.
- See DESKTOP_UPDATES.md for session retention and account-choice behavior.

# Version 0.48.8 - shared bot operating guide

- Every provider uses a single versioned core and reference guide, with attributed
  live identity, role, memory, tool catalogue, access status, decisions and history.
- 157 backend tests pass, including full/compact prompt selection, preservation of
  role and memory, valid Unicode/JSON bounds, explicit omissions, exact selected
  Reply source, private-chat isolation, decision continuation ownership and fresh
  local access. All reference chapters are callable without action permissions.
- The Codex bridge fixture checks the actual instructions and guide tool sent to
  thread/start. API and CLI adapters call the same builder with their exact tools.
- Ten existing Edge/WebKit suite combinations pass. This change has no new UI
  controls and changes no saved bot role, memory or permission settings.
- These are deterministic runtime/transport checks. No claims are made about a
  live model's perfect compliance; no real personal workflows or mail are executed.
- See SYSTEM_PROMPT.md for source layout, context budgets and verification limits.

# Verification snapshot - 2026-09-08, version 0.46.0

## Version 0.46.0 imported workflow library

- 134 backend tests pass, including import preview/hash checks, name conflicts,
  explicit replacement, path/file validation, native discovery, binary supporting
  assets, source edits/deletion after queuing, and command receipt badges.
- A bot-tool integration test exercises preview and import through device-bound
  desktop requests and persisted receipts. No workflow is executed on import.
- A real filesystem fixture exercises the guest package writer: exact files,
  idempotent reload, rejection of changed materialized files and linked inputs.
- Edge and WebKit cover folder selection, full skill packages, review-before-import,
  shared catalogue refresh, keyboard command suggestions, persisted badges,
  ordinary slash text without badges, and mobile/light layouts. The UI tests use
  fixture API responses; backend behavior is exercised separately above.
- The existing command UI suite passes in both engines. No real Nessus scan,
  provider inference or import of the user's personal workflows was performed.

## Version 0.45.1 Claude sign-in in the user's browser

- 128 backend tests and 8 Python tests passed, including the VM manager, provider
  transports and the new subprocess login relay.
- Claude's official CLI supplies an authorization link through a bounded headless
  process. Kindred opens it on the user's computer and forwards a one-use return
  code to the CLI's standard input. The CLI owns token exchange and its credential
  store. Account/session tokens and terminal output are not returned by this relay.
- Four subprocess login tests cover concurrent starts, one persistent flow,
  fragmented terminal hyperlinks, verified completion, stale attempts, cancellation,
  expiry, logout, malformed input and sanitized provider failures. Two existing
  subscription transport tests continue to cover allowed tools and denied actions.
- Edge and WebKit exercise the local browser popup, return-code submission and
  clearing, connection verification, cancellation and rejection of unexpected URLs.
- The real installed Claude Code 2.1.263 was checked in an isolated temporary home:
  its official authorization URL was captured, repeated starts reused the same
  login, and cancellation stopped the flow. No real consent or inference was
  performed; this does not establish the user's account entitlement.

## Version 0.45.0 profiles and desktop settings

- 128 backend tests passed, including explicit legacy claim, empty profile creation
  during sign-in, duplicate-request recovery, profile renaming, and workspace
  transfer across two account registries. Transfers preserve history and attachments,
  reject active work and nonempty destinations, validate schema and references, and
  exclude credential stores and desktop grants. Cancellation and repeated imports
  are covered; source scheduling stops and destination routines start paused.
- Edge and WebKit cover the General startup switch, desktop-permission spacing,
  administration layout, server choice before creation, in-app server menus,
  two-step forgetting, null setup status, transfer errors and cleared password inputs.
- Actual Windows native IPC checks cover profile switching, protected saved sessions,
  isolated local access, custom dialog headers and the forget/keep confirmation.
- An actual Windows client moved a fixture workspace between two actual local Rust
  servers, restarted into the destination profile, retained the paused source and
  preserved the destination's existing workspace. Saved sessions remained protected
  and local access stayed off. This check also caught and fixed a Windows dialog
  creation deadlock; opening the transfer dialog now runs asynchronously.
- Testing against actual server headers exposed a CSP restriction that blocked the
  desktop bridge. The policy now permits only the native IPC destinations in addition
  to the selected server; native command origin and capability checks still apply.

## Version 0.44.1 functionality and cohesion

- 125 backend tests passed, including scheduled command snapshots, failure before
  VM/provider work when an alias disappears, independent due routines, preserved
  multiline inputs and apostrophes, and exactly one continuation after an empty
  teammate result. The real Pi SDK fixture retains routine delivery and skill tests.
- Two Linux VM-manager tests verify cache reuse, new media after a software change,
  preservation of old attached media, and rejection of missing or changing bundles.
  These checks do not boot a new guest or update software inside an existing disk.
- Browser regression checks cover commands, approvals, routine schedules, connected
  accounts, attachments, drafts, message actions, collaboration, profile switching
  and paged chat history. New checks cover stale catalog responses, edits during
  command validation, input composition, multiline messages and single-time labels.
- A deleted routine command fails through normal task history and chat delivery;
  its failure does not halt the scheduler. Quiet routine output remains suppressed,
  while empty teammate results still wake the requester without an empty bubble.

## Version 0.44.0 routine delivery and commands

- 121 backend tests passed, including the actual Pi SDK driven by a local keyless
  model fixture: two routine progress turns produce one final report; quiet checks
  produce no chat reply; a bot saves a parameterized skill and a subsequent slash
  invocation runs its saved workflow. These are harness tests, not live MLB or
  Gmail/Calendar execution.
- Command validation rejects missing/extra arguments, unfinished quotes, duplicate
  aliases and reserved names before a message/run is queued. Quoted Windows paths,
  stable legacy aliases, saved metadata, per-profile separation and immutable queued
  workflow receipts are covered. Connected-account status and action permissions
  determine whether Gmail/Calendar commands are offered.
- Edge and WebKit checks passed for slash filtering, click/keyboard insertion,
  non-submitted gray parameter hints, validation, library creation/search/use,
  bot-created command refresh, paste and desktop/mobile dark/light layouts.
- Restart tests preserve raw event/run/message history while suppressing old quiet
  routine progress, quiet results and duplicated question completion text. Genuine
  failures remain visible; whitespace-only differences do not duplicate a final reply.
- The existing dedicated VM passed passwordless `sudo -n id` through its normal
  bot SSH account. Guest sudo is also part of both provisioning paths. The guest
  has no host filesystem mount or Docker socket. Its default firewall is editable
  by guest root; it is not a mandatory network boundary against that administrator.

## Version 0.43.0 profiles and hosting

- 114 backend tests passed, including transactional first-admin creation, cross-account
  data and credential isolation, token rotation, cross-device profile/read-state sync,
  persisted sessions, legacy claim preservation, invitations, disabled-account run
  cancellation, password rotation, preferred-profile ownership and rejection of old
  desktop local-access polling in managed profiles.
- Profile UI checks passed in Edge and WebKit for registration, switching, reload,
  notification counts, dark/light and mobile layouts. Windows GNU checks and a release
  build passed. Actual Windows WebView2/native IPC verified encrypted saved sessions,
  restoration, cross-server switching without credential crossover, native profile
  badges and default-off local access in another profile.
- An isolated Docker Compose installation created two accounts through the normal
  registration endpoint. First messages provisioned distinct KVM/QEMU computers with
  2 CPUs, 6 GB RAM and 30 GB disks. Both completed the real Pi SDK guest command and
  screenshot tool loop against a local keyless model fixture. Guest users had no sudo,
  host/private network access was blocked, and another account could not fetch a
  screenshot attachment. No subscription or paid model was used.
- Testing found a QEMU boot loop when the minimal device set omitted VGA; adding the
  VGA device fixed both guests. One initial package download exceeded the earlier
  570-second setup allowance, failed visibly and retained its disk; it completed
  setup later and a normal subsequent message succeeded. The setup allowance is now
  1,770 seconds, with a bounded outer timeout. The original failed/cancelled runs
  remain in the fixture history.
- Shutdown/restart checks preserved unique per-guest files and completed new normal
  messages on the same computers. An immediate-message shutdown race was found and
  fixed by holding the computer operation lock until QEMU has fully stopped.
- Hosted CLI integration invokes the unmodified official Claude CLI; it does not
  extract its OAuth tokens for direct API calls. Current upstream hosted-CLI and
  subscription policy references are documented in PROFILES.md. A real Claude
  company-plan sign-in or entitlement has not been exercised by these tests.
- GitHub Actions built Windows NSIS, macOS Apple Silicon/Intel DMG, Linux DEB and
  AppImage packages. Final downloads are published only after the final source
  revision passes the same workflows. Packages are not OS-signed or notarized.
- The existing service migrated to 0.43.0 with its original VM UUID, provider
  credentials and all 14 usage receipts preserved. SQLite integrity and foreign-key
  checks passed. The migration backup is
  `/opt/kindred-test/backups/20260908T034918Z-v50`. Its legacy profile is ready for
  the owner to claim using the existing connection. No owner account was invented.
- Windows updater signatures are project signatures, not OS Authenticode or Apple
  notarization.

## Version 0.42.0 automatic custom-provider catalogues

- Custom setup now asks for name, base URL and optional API key. Saved providers
  become named cards with one fresh Add custom provider card below. Renaming preserves
  card order, ID, catalogue and usage history. Each card shows refresh time, discovered
  model count/list, a Refresh models button and any sanitized discovery error.
- Catalogues load in the background on provider creation/configuration changes,
  server startup and once per client startup. Recent startup checks and concurrent
  requests are shared; normal cached model reads do not refetch. The bot picker's
  Refresh models control explicitly reloads a custom catalogue. Failed refreshes keep
  the last good models; a valid empty response clears the list. No model is substituted.
- **108 backend tests passed**, including six catalogue tests for parsing limits,
  capability/pricing metadata, unknown prices, cached reads, startup reuse, concurrent
  refreshes, authenticated/keyless requests, redirects, malformed/error responses,
  multiple named providers and URL/key changes during an in-flight request. A newly
  discovered model completed the real Pi SDK tool loop and recorded usage with no
  manually configured model entry. Test providers and credentials were local fixtures.
- The new provider-catalogue suite passed in **Edge and WebKit**, covering startup,
  manual refresh, failure retention, unsaved URL handling, multiple cards, rename/order,
  blank Add-card renewal and discovered models in the bot picker. Existing provider/
  local-settings and full usage-history suites also passed in both engines. Reviewed
  desktop dark/light and mobile screenshots; no page errors.
- Live server **0.42.0** and deployed app/style/icon bytes match source. The simplified
  form was checked live in both themes. QA did not add a real provider, send a model
  request, or change bots, settings, existing provider accounts, read markers or manual
  control. A tested external provider's unauthenticated catalogue returned HTTP 403;
  authenticated discovery was not verified.
- Linux/Windows builds passed. Idle deployment preserved existing usage receipts and
  backed up at `/opt/kindred-test/backups/20260908T015220Z-v49`. The signed Windows
  archive is **3,911,442 bytes**, SHA-256
  `be1c8a5c9534c5e6fb993af2aec78a5203852faed0fc6c8e298519c1274b357a`.
  The installed updater verified its signature, download, archive and version 0.42.0.
  Linux executable SHA-256:
  `e2446512ad443e46b2c02552d551acb5a149aeb9460a8c0e4c0120d90bb79ed9`.

## Version 0.41.1 computer collapse direction

- The expanded computer's collapse button now uses two right-facing chevrons.
  Edge visual and interaction checks confirmed the panel collapses into the right
  sidebar, stays open and retains the same desktop session. Its accessible label
  still switches between Collapse computer and Expand computer; no page errors.
- Linux and Windows builds passed; the live 0.41.1 app and icon assets match source.
  Idle deployment preserved usage totals and backed up at
  `/opt/kindred-test/backups/20260908T012425Z-v48`.
- The signed desktop archive is **3,910,533 bytes**, SHA-256
  `51e73b62003caec14d1fc40ae411ae08eaa6c9d69166b946cba7d1bd09cab74e`.
  The installed updater verified signature, download, archive and version 0.41.1.
  Linux executable SHA-256:
  `1f3420977cd97cc6cafbde2c08c5619e22eef4c91fea689c3fe02fd6e5e0c734`.

## Version 0.41.0 provider usage history

- Clicking OpenRouter or a custom API provider opens the full usage dialog. Hover
  previews show five bots with their avatars, ordered by known cost. The dialog
  includes every bot with recorded usage, search, active/archived/deleted filters,
  and ascending/descending cost, total/input/output/cached tokens, requests, last-use
  and name sorting. Reported and estimated costs remain distinct; entirely unpriced
  bots sort last and missing tokens remain explicitly unknown.
- Provider status and API usage warm in the background. Repeated Usage hovers use
  cached status; a manual usage refresh requests only that provider's receipts.
  Tests cover delayed initial checks, background refresh, a disconnect overtaking
  a stale catalog read, and subscription status completing while a provider button
  has keyboard focus. The focused item and open preview remain intact.
- The transactional migration retained all live receipt metrics and dates. Historical
  names and appearance-only avatar snapshots survive operational bot/run removal;
  archived bots and disconnected providers retain their recorded usage. No receipt
  history before tracking was enabled is fabricated. Database integrity and foreign
  key checks passed after migration, with the existing live receipt preserved.
- **102 backend tests passed**, including five provider-account tests. The dedicated
  usage-history and existing provider/local-settings suites passed in **Edge and
  WebKit**, with zero page errors. Coverage includes every sort in both directions,
  avatars, empty and disconnected custom providers, filters during refresh, Tab and
  Escape behavior, dark/light layouts, 390x844 mobile, 844x390 landscape and 390x500
  short windows. Reviewed the final screenshots in both themes and on mobile.
- Linux and Windows release builds passed. Live UI assets match source. Live checks
  verified startup prefetch, three repeated menu openings without extra provider
  status checks, and the full popup's recorded-row count. Bots, settings, usage,
  read markers and the existing manual-control session remained unchanged by QA.
- Server **0.41.0** is healthy. The idle deployment backed up database and executable
  at `/opt/kindred-test/backups/20260908T011755Z-v47`, with database restoration included
  in the startup-failure rollback path. Pi harness 0.40.0, CLI binaries, guest bridge
  and dock configuration did not change in this release.
- The signed Windows archive is **3,910,448 bytes**, SHA-256
  `3dafe7b2bbbede067ac4b7b0b3362163bccdd5c3bd1cd877c138c7388e6a6e9e`.
  The installed updater verified its signature, download, archive and version 0.41.0
  without forcing an update of the running client. Linux executable SHA-256:
  `caa27b8f94eac0cdf5741d2125ac94cd3241174cb1810f86e078a1d20042d694`.


## Version 0.40.0 providers, usage and opt-in local access

- Transparent tint2 background ID 0 applied to all eight existing bot displays.
  Native dock/window checks retained the 56 px reservation and 1280x744 work area;
  browser window IDs and geometries were identical before and after. The manual
  session on display 5 remained untouched. Native evidence is in the deployment
  backup `/var/backups/kindred-dock-20260907T233812Z/applied.json` on the bot VM.
- Connections now shows one-line expandable provider cards, checkmarks for connected
  accounts and muted disconnected accounts. Codex and OpenRouter were read back live;
  official Claude Code 2.1.263 and Kimi Code 1.50.0 are installed and await owner sign-in.
  Custom OpenAI-compatible providers support explicit models, context sizes, optional
  API keys and optional input/output prices. Real Pi SDK fixtures cover custom tools,
  keyless requests, exact destinations, reported costs and unavailable usage.
- Usage cascades through connected providers. API providers show each bot's recorded
  token counts, reported costs, estimates and unpriced requests. No historical cost
  was invented. Claude's account-wide quota is not exposed by this integration; its
  menu links to the provider usage page and shows recorded Kindred tokens. Kimi uses
  its official CLI's account projection. No authenticated Claude/Kimi model run is
  claimed before owner sign-in; protocol fixtures verify tool transport and denial.
- Local access defaults off in global settings, each bot and the native desktop.
  Real Windows IPC/OS checks prove workspace file access, outside rejection, native
  Deny and Allow once, full access, command cancellation and disconnect handling.
  Hosted content cannot grant native permissions. A persisted interrupted operation
  returns an unknown-outcome receipt without replay. Tests used an isolated temporary
  installation and WebView2 profile, leaving the installed user's permissions alone.
- **100 backend tests passed**, including provider receipt deduplication, a complete
  keyless custom-provider tool loop, device credential mismatch, bot binding, live
  disable, offline handling and single-use local receipts. **15 real Pi SDK tests**
  passed, including compaction, stream byte boundaries and missing usage. **Two CLI
  protocol tests** passed; Kimi token fragments become coherent messages. The installed
  Claude binary also confirmed support for private system-prompt files, avoiding
  command-line context and per-argument size limits. **Three Windows native unit tests**
  cover paths, hard links, file operations, commands and cancellation.
- Edge and WebKit passed provider/local settings, attention/profile, conversation
  motion, desktop glass, settings-screen and experience suites. Checks cover dark/light
  UI, compact account cards, API usage labels, mobile cascades, provider saves, bot
  binding, existing chat/profile flows, screen sizing and scrolling. Live assets match
  source; reviewed both Connections themes and Usage. No page errors, read-marker
  changes or manual-takeover changes occurred during live QA.
- Linux and Windows release builds passed. The idle deployment backed up the database
  and executable, activated server/harness 0.40.0 together, and retained prior runtime
  releases. The bot-VM provider helper matches source SHA-256
  `13af4f65fd35afc44190f7c96a0c91524205b790e49f74bd20c2726de39dc9b9`.
- The signed stable Windows archive is **3,906,531 bytes**, SHA-256
  `f07bf3b4f5ff2b7636bb1d3685b2b9c4e09589a21f9c0671fb944f79c6a00818`.
  The installed updater verified signature, download, archive and version 0.40.0.
  The user's running client was not forcibly updated. Linux executable SHA-256:
  `74b3df983818f5f41a79883e3e7a9e26a5e3d7ef5df0776076ed34701895a495`.

## Version 0.39.1 presence, unread activity and profile navigation

- Bots retain their normal idle expressions and green presence dot for 30 minutes
  after persisted activity. Active turns remain online. Dormant bots hold open eyes
  slightly up and right, with no blinking, breathing or changing expression. All
  eight original silhouettes and the existing questioning expression remain intact.
- Blue unread dots cover direct and group conversations, including pins. Monotonic
  database receipts survive restart and cannot clear replies arriving after the
  captured cursor. Unfocused, covered and unrendered conversations stay unread.
  There were no historical receipts; existing bot messages initially appear unread.
- The bot-name panel now edits Name, Label, Description and Notifications. Its
  drafts survive background refresh and queued avatar saves. Connections, Skills
  and the deeper settings gear remain. The monitor opens the compact screen panel;
  clicking the screen or expand control focuses it.
- All **94 backend tests passed**, including new receipt persistence, stale cursor,
  authentication, old activity and identity-preservation tests. The first full run
  lacked the configured Pi test runtime; rerunning with the installed pinned Node
  and worker paths passed the entire suite.
- Edge and WebKit passed `test-attention-profile.cjs`, `test-conversation-motion.cjs`,
  `test-desktop-glass.cjs`, `test-settings-screen.cjs` and `test-experience.cjs`.
  Checks include the exact 1,799/1,800-second boundary, rest gaze on all shapes,
  read acknowledgment races, modal visibility, pinned-dot geometry, delayed identity
  saves concurrent with avatar edits, mobile forms, preserved deep settings and
  compact/focused screen navigation. The scroll test now waits for its final layout
  before asserting the bottom position.
- Live noVNC QA exposed a resize calculation using intermediate animated bounds
  in 0.39.0. Version 0.39.1 refits the viewer after expansion finishes. Both browser
  fixtures now reproduce transformed measurements and assert the final fitted size.
  Live readback measured a 977.6x611 canvas inside the 1031x611 focused area, preserving
  the 1280x800 framebuffer's aspect ratio. Reviewed compact/focused screenshots and
  both profile themes. Served UI assets match source and no page errors occurred.
- Linux and Windows release builds passed. Deployment backed up the idle database
  and host executable, validated configuration and Pi 0.85.1, and left the service
  active at 0.39.1. Guest binaries and desktop configuration were unchanged.
  Live QA preserved all read cursors and Izabella's existing manual control.
- Signed Windows 0.39.1 supersedes the initially published 0.39.0. The installed
  updater verified signature, download, archive and version with `-VerifyOnly`;
  the user's running client was not switched or restarted.
- Windows archive SHA-256:
  `accd0a000259a5251e4c1df9f97fc6297da466cddc964b2e8b0dccfdb1d66427`.
  Linux executable SHA-256:
  `50ca4fa009c0962b12d9de0a62b932ddafcb1ea332f6e91b5d24e2ae1323d5de`.

## Version 0.38.0 bottom-attached native dock

- Removed the 18 px gap below the native dock and reduced panel height from
  64 to 56 px, with 6 px vertical padding and the same 40 px launcher icons.
  It sits flush with the bottom edge. The desktop work area increases from
  1280x718 to 1280x744, reclaiming 26 px for application windows.
- Verified actual tint2 17.0.1, Openbox, Xvfb and XTerm on temporary display 32:
  exactly one dock at y=744, height=56; maximized work area 1280x744; normal
  raised and fullscreen windows remain above the dock. The temporary X session
  was stopped afterward. Reviewed the native maximized screenshot.
- Applied the configuration to all eight running screens with configuration
  and geometry backups. Restarted only dock shells, preserving X and browser
  processes and all browser window IDs. Expanded six normal full-width browsers
  fitted to the old work area; independently sized windows were preserved.
  No explicit window resize was sent to manually controlled display 5.
- The first live check held for manual control. The application procedure was
  narrowed to preserve that session and skip direct resizing there. A subsequent
  geometry-equality check caught the window manager's automatic expansion of
  its already-maximized browser from 718 to 744 px. Readback confirmed the same
  window and maximize state. The procedure resumed only on remaining docks;
  Izabella's manual control remained active throughout final readbacks.
- All eight docks now measure 178x56 at (551,744), with work area 1280x744.
  Reviewed Piper's live screenshot with the browser filling the reclaimed height.
  Initial backup: `/var/backups/kindred-dock-20260907T223127Z`; final application
  receipt: `/var/backups/kindred-dock-20260907T223254Z/applied.json`.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active at 0.38.0 and served UI assets match source.
  Signed Windows release published; installed updater verified signature, archive,
  download and version using `-VerifyOnly`. No client switch/restart. Guest binary
  unchanged; native desktop configuration updated.
- Windows archive SHA-256:
  `9b0ed871cf03072f4bda574118303a4f5788fad20d20e7821860b755bebf036d`.
  Linux executable SHA-256:
  `7cf7a4db16a2b95746dc661e86be8daeb010d543cdf38ecfa30820a48191288c`.

## Version 0.37.0 group message sequence framing

- Each bot sequence keeps its name above the first message and one avatar to
  the left of the final bubble, aligned with that bubble's bottom edge. As
  another same-sender message arrives, the avatar moves to the new final bubble.
  Individual message IDs/actions remain intact. Bubble anchoring keeps wrapped
  mobile actions from pulling the avatar below the message. Visible handoff
  links now explicitly break a sequence, as do other senders and timestamps.
- Group presentation and dedicated sequence suites passed in Edge and WebKit.
  Coverage includes name/last-avatar placement, sender/user/time/system/handoff
  boundaries, arrival of another reply, multiline/mobile layouts, both themes,
  unchanged direct chats, group portraits, rename and keyboard/mobile menus.
- Live 0.37.0 assets match source. All six bot sequences in general have exactly
  one avatar at the final bubble, positioned left and bottom-aligned, with the
  name above the first bubble. This includes Piper's two-message sequence from
  the reported screenshot. Reviewed a cropped live dark-theme capture; no page
  errors. No chat messages or settings were changed during live verification.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active. Signed Windows 0.37.0 is published and the
  installed updater verified signature, archive and version with `-VerifyOnly`.
  Client was not switched or restarted; guest unchanged.
- Windows archive SHA-256:
  `44eb976ef36c154536479997d82e083347d809d41beb1bdd6e873476d358565a`.
  Linux executable SHA-256:
  `1121173e9b4ad5530ee02141d521106bf4f1097bbe0125cfe135db98b5e6b7f4`.

## Version 0.36.0 clearer group portraits

- Replaced overlapping participant strips and heavy silhouette cutouts with a
  compact grid. Three participants form a triangle; larger groups place the
  extra-member count in its own cell. Initials remain visible and full member
  names remain in the accessible label. Row portraits use the same 44 px width
  as direct-chat avatars, aligning names and previews. Pinned and header
  portraits use the same arrangement at their respective sizes.
- Group presentation tests passed in Edge and WebKit: two/six-bot portraits,
  no overlapping faces or counts, containment, row/direct-chat name alignment,
  header, sender gutters, both themes, preserved rename state, keyboard and
  mobile menus. The Edge collaboration/pins suite also passed, including
  persistence across clients and current-message previews.
- Reviewed both themes against current group data. Deployed 0.36.0 serves the
  exact source assets. All three live group rows have 44 px portraits with no
  overlapping cells, visible CZ initials and names aligned at x=71, matching
  direct chats. The general group's +1 count has its own cell. No page errors.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active. Signed Windows 0.36.0 is published and the
  installed updater verified signature, archive and version with `-VerifyOnly`.
  Client was not switched or restarted; guest unchanged.
- Windows archive SHA-256:
  `bcb83024954c37d6826d4409d664a68716fc2bbe3e2d08f6e3df7cfc41a46863`.
  Linux executable SHA-256:
  `f84fa48fa106f8d938dd576e5e9d0403a5a48436e01cf8111e869ce8fcbe329a`.

## Version 0.35.0 fuller pill-shaped eyes

- Default resting eyes are about 40% wider and 22% taller. Their contour now
  keeps parallel sides and fully rounded ends, with reduced resting tilt.
  Gaze still uses position, tilt and foreshortening. Wide and questioning eyes
  scale with the fuller baseline; squints remain distinct horizontal pills.
- Character-face suites passed in Edge and WebKit, each sampling 576,000
  contour/safety-margin checks across eight silhouettes and four eye styles,
  idle and working poses through a complete expression cycle. No clipping or
  blink-center drift was found. Circular body geometry, synchronized expressions
  and stable reduced motion also passed. The Edge conversation-motion suite
  passed, including full turns, shape preservation and tool morph ordering.
- Reviewed both theme expression sheets and before/after comparisons at actual
  32, 44 and 84 px sizes. Live 0.35.0 assets match source; twenty rendered sidebar
  eyes use the new dimensions, with no page errors. Reviewed the live light UI.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active. Signed Windows 0.35.0 is published and the
  installed updater verified signature, archive and version using `-VerifyOnly`.
  Client was not switched or restarted; guest unchanged.
- Windows archive SHA-256:
  `13d9ee70dfcb8156f55c7a0ea0f54203e36b0a4ccb0d774fcda7e914cb6722d8`.
  Linux executable SHA-256:
  `4a5fe6c49bb0a29d77183a0a28d9edbe9c74c27bd2f522958780973b5c13e923`.

## Version 0.34.0 frosted computer margins

- The live viewer now uses a blurred, theme-tinted copy of the desktop behind
  the actual frame. noVNC's background is transparent. The decorative copy has
  a maximum 240 px edge, samples twice per second, skips hidden tabs and stops
  on close/disconnect. It cannot receive pointer input. Teaching snapshots
  explicitly select the original desktop canvas.
- The desktop-glass integration suite passed in Edge and WebKit: both themes,
  desktop/mobile widths, updated colors, unchanged source resolution/aspect,
  clear desktop and pointer targeting, original-frame lesson capture, reduced
  motion, reconnect and close cleanup. RFB is a local fixture in this suite.
  Existing screen-menu and Settings screen-selector suites also passed in Edge.
- Live read-only noVNC validation passed against deployed 0.34.0 in both themes:
  original 1280x800 frame, 240x150 decorative copy, 52.3 px frosted side margins,
  transparent noVNC background, no filter on the desktop and no page errors.
  Reviewed both screenshots; served UI assets exactly match source.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active. Signed Windows 0.34.0 is published. The
  installed updater verified its download, signature and version with
  `-VerifyOnly`, without switching or restarting the client. Guest unchanged.
- Windows archive SHA-256:
  `88c1fadb9538f4db1d99ed07bfa019c2a0d3e7593248256803fbf880b630ff5a`.
  Linux executable SHA-256:
  `68958f56718334b228c87b53340949e1f13c41b6a9a376e08dfe11815f78d12c`.

## Version 0.33.0 Settings switch alignment

- The generic `.settings-section > label` rule overrode `.switch-row`'s flex
  layout, leaving switches inline at the text baseline. Excluded switch rows
  from that generic rule so their existing centered, spaced row layout applies.
- Measured Reduce motion and Show activity in chats in Edge and WebKit, at 1320
  and 390 px widths in both themes. Each label and switch had identical vertical
  centers, with the switch aligned to the row's right edge. Reviewed desktop
  and mobile screenshots. Existing experience suites passed in both engines,
  including notification settings persistence, editors, uploads and drafts.
- Linux and Windows release builds passed. Host deployment followed idle and
  backup checks; service is active at 0.33.0 and served assets match source.
  The installed updater verified the signed download and version using
  `-VerifyOnly`, without switching or restarting the client. Guest unchanged.
- Windows archive SHA-256:
  `2055bd6f08851ef7411bbf5184ff95372771062662d6130233bfd569f9d9d78a`.
  Linux executable SHA-256:
  `2469606568a3fe5de34f0fdea87a79e13c01d5db8c49ee9464f08cc1e78d3fb6`.

## Version 0.32.0 native dock layering

- The guest dock was configured with `panel_layer = top`. Live inspection also
  found legacy normal Chromium windows occupying 1279x799 pixels despite the
  window manager reserving the bottom 82 px for the dock. The source config now
  selects the bottom layer while retaining that reserved work area.
- Verified on unassigned temporary display 32 using actual Xvfb, Openbox, tint2
  and XTerm: one dock, normal raised windows above it, maximized content contained
  in the 1280x718 work area, and fullscreen content covering the dock. Reviewed
  maximized and fullscreen screenshots; stopped the temporary X session afterward.
  Used xwininfo absolute coordinates because xdotool's reported Y included the
  window decoration offset twice in this environment.
- Applied the config to all eight active bot screens after checking for active
  tasks and user takeover. Backed up configuration and window geometry, restarted
  only dock shells, and fitted legacy full-desktop browser windows to the work
  area. Existing positioned/sized windows were retained. Readback confirmed one
  bottom-layer dock per screen, no overlap with browser content, and the same
  eight Chromium window IDs. Piper's live screenshot shows the dock on wallpaper
  beneath the browser, with its original tabs still open. Browser and X sessions
  were not restarted. A configuration-only reload had updated the dock state
  property; the final application used fresh dock processes and geometric checks.
- Windows and Linux release builds passed. Host service is active at 0.32.0 and
  served assets match source. Pi worker and guest executable remain unchanged;
  the guest change is desktop configuration plus the live dock/window adjustment.
  The installed updater verified signature, archive and version with `-VerifyOnly`
  without switching or restarting the client.
- Windows archive SHA-256:
  `96df923e72f5eb3f2a93445a18784fe98a073d56eb822e828ef00d912ebcd61e`.
  Linux executable SHA-256:
  `0f85448d5be87e516ba25b37946a96335d5fe8eee0e931a3d1ac1f1e29ed185d`.

## Version 0.31.0 chat pane alignment

- Removed the message-group and composer width caps that centered conversations
  inside a narrow column on wide displays. Direct-message bubbles now align to
  the same left/right edges as the full-width composer. Existing bubble line
  lengths and group-avatar gutters are preserved.
- Rendered and measured DM/group layouts in Edge and WebKit at 2560, 1440 and
  390 px viewport widths, including desktop DMs with Details open and closed.
  Desktop outer margins are 16 px; narrow-screen margins are 12 px. At 2560 px
  with Details open, the pane is 1885 px wide and the composer is 1853 px wide;
  bot/user bubble edges exactly match its left/right edges. Reviewed desktop
  and mobile screenshots. API data for these previews is synthetic.
- Existing message-action and composer-motion suites passed in both engines,
  including replies, cancellations, attachments, draft preservation and reduced
  motion. No new automated test was added for this small CSS change.
- Windows/Linux release builds passed. The host was idle, backed up and deployed;
  service is active. Live version and embedded JS/CSS match the 0.31.0 source.
  Installed updater `-VerifyOnly` verified the signed archive and version without
  switching or restarting the installed client. Guest and Pi worker are unchanged.
- Windows archive SHA-256:
  `2cfbb7c515b91def53ad28ad469267a91b175f9c617fc0e83907176b4324a9d0`.
  Linux executable SHA-256:
  `c926a6ccc374df7e052c56621229398becdeaf3427696cabd95056daee492503`.

## Version 0.30.0 circle and pebble silhouettes

- Reordered the shared picker to circle first, pebble second. The round profile
  uses exact circular SVG arcs; the pebble has a wider, softly asymmetric cubic
  contour. Shape identifiers and existing profile selections are preserved.
- Circle breathing uses uniform scaling, and working turns move the face around
  a circular body rather than compressing its width. Other shape and tool motion
  retain their existing behavior. The body and face clip share the same contour.
- Character-face and conversation-motion suites passed in Edge and WebKit.
  Each engine passed 576,000 face-containment samples across shapes, expressions
  and activities, plus circle-first picker order, circularity throughout idle
  breathing and working turns, and the pebble aspect ratio. Maximum sampled
  circle radius spread was under 0.011 px. Visually reviewed both contours and
  eye expressions at small and large sizes, with dark/light screenshot coverage.
- Windows and Linux release builds passed. Host deployment followed an idle
  check and database/executable backup; service is active. Live 0.30.0 and its
  embedded JS/CSS match the verified source. Guest and Pi worker are unchanged.
  The installed updater verified the signed download, archive and version using
  `-VerifyOnly`, without switching or restarting the installed client.
- Signed Windows archive SHA-256:
  `73d09d19c54e258d4e893d9c0e64052bd9e35cb309a3dbddcce6e5b06ea5ca31`.
  Linux executable SHA-256:
  `e8e7df9179d63ec04efff3c97f38dd89e5e9967313ed2b8c38c630128208921f`.

## Version 0.29.0 composer motion

- Added a soft hover/focus border highlight and a 320 ms eased reply expansion
  and cancellation. The composer grows upward with its bottom edge anchored;
  the writing area and controls move from their current visual positions.
  The quote fades in and its inert outgoing preview fades out on cancellation.
- Rapid cancellation/reopening retargets from the displayed height. Completed
  animations and outgoing previews are removed. Chat changes restore directly;
  unchanged polling does not restart motion. Text/file changes and resizing
  release the measured height. Both app and OS reduced-motion settings skip it.
- Composer-motion, message-action, chat-history and chat-transition suites
  passed in Edge and WebKit. Frame samples verified intermediate heights,
  monotonic opening/closing, stable bottom edge and continuous writing-area
  position. Also checked rapid reversal, multiline drafts, attachment replies,
  hover/focus colors, cleanup, polling and reduced motion. Dark/light desktop and
  mobile screenshots were visually reviewed. Browser APIs/uploads use fixtures.
- Linux and Windows release builds passed. Deployed the host after confirming
  no active tasks and backing up its database and executable. Service is active;
  live version and embedded JS/CSS match the verified 0.29.0 source. The existing
  guest and Pi worker remain unchanged; no backend behavior changed in this release.
- Signed Windows 0.29.0 archive SHA-256:
  `10cdf50bc171ade1ff596bfad099c73c3751460edab0f306b95f712d9502c089`.
  Linux executable SHA-256:
  `1f833f7adaac1dfa6c973d16e5ff0e2a1798bf2ab437458fdb909d837a50ebf4`.
- Installed updater `-VerifyOnly` verified the signature, downloaded archive and
  embedded version without switching or restarting the user's installed client.

## Version 0.28.0 message actions and quoted replies

- Replaced the conversation's detached copy button with a compact hover toolbar:
  React, Reply and More. More opens a Copy menu; React opens sixteen choices.
  User reactions can be replaced or removed and coexist with existing bot
  reactions. Matching emoji show a count and the user's selection is marked.
- Reply opens a rounded composer with an inset source excerpt, cancel control,
  separate writing area and bottom actions. Cancelling keeps typed text; failed
  sends keep both text and quote. Draft quotes follow their own chat and are
  included in the existing updater draft-resume data. Sent quotes persist and
  can load/highlight originals outside the current history page.
- Reply sources are resolved in the same chat on the server and attached
  atomically to the posted message and queued runs. Group replies route to the
  original bot author unless explicit mentions choose another current recipient;
  ordinary messages and DMs retain their routing. The model receives the exact
  selected source as bounded quoted context, separately from the new request.
  User reactions do not queue work or impersonate a bot.
- All 91 Rust tests passed, including new persistence/restart, older source
  context, author/mention routing, auth, archive, invalid source and upload rollback
  checks. Existing approval, provider, scheduling and human-handoff coverage passed.
- The new message-action suite passed in Edge and WebKit for hover placement,
  Copy (with a clipboard fixture), emoji add/remove/reload, keyboard control,
  chooser focus through a message refresh, per-chat drafts, failed sends,
  cancellation, quoted sends and old-message jumps. Group-presentation,
  chat-history and decision/schedule suites also passed in both engines.
  Visually reviewed desktop dark/light and narrow layouts. Touch/narrow layouts
  expose actions below the bubble rather than relying on hover.
- Actual Codex acceptance used the isolated verification bot. It posted two
  synthetic parcel references. Replying through the UI to the older MAPLE-4729
  message returned MAPLE-4729, despite the more recent BIRCH-8156 message.
  Reaction add/remove and reload persisted correctly; a later thumbs-up remained
  attached to its source. The sent quote survived reload and jumped to the correct
  original. Live assets matched source and there were no browser errors. Reviewed
  the live Copy menu and composer. Restored the test bot's instructions and archived
  its chat/bot after completion; no real account or external action was involved.
- Linux and Windows release builds passed. Deployed the host after backing up its
  idle database; guest behavior and the pinned Pi worker were unchanged. Published
  signed 0.28.0. The installed helper verified signature, download, archive and
  version without switching versions or restarting the user's client. Final diff
  and archive integrity checks passed.
  Linux SHA-256: `1c3bddaf1954fd93179177dfea061bda49c8f8b40a1baf0d0d908baeeb78cbe7`.
  Windows archive SHA-256: `b1e37c3cb7e25d7ce70898ec24297ef58c3db55782e630c57721c29b7257b65d`.

## Version 0.27.0 expressive eyes

- The default pill eyes now transition into larger almost-circular wide eyes,
  thin horizontal squints and an asymmetric questioning expression. A 23-second
  sequence holds each expression briefly and rests between them; the existing
  gaze and blink cycles continue independently. These are decorative expressions,
  not claims about the model's internal state. Happy/sleepy profile styles remain
  distinct, and reduced motion fixes the face in a calm central pose.
- Rounded cubic contours preserve continuity between eye shapes and retain gaze
  curvature. Bot identity supplies the animation phase so sidebar, header and
  conversation instances agree across refreshes. No body or color assets changed.
- The expanded character-face suite passed in Edge and WebKit: 576,000 sampled
  boundary checks per engine across eight silhouettes, four eye styles and the
  full expression cycle in idle/front-facing working motion. Verified wide-eye
  proportions, thin squints, asymmetric eye heights, same-bot synchronization and
  static reduced motion. Reviewed 32/84 px expression sheets in dark and light.
  Existing conversation-motion and experience suites passed in both engines.
- Live source bytes matched the served JavaScript/CSS. Watched the actual Izabella
  sidebar avatar over a full cycle: eye width reached 11.62 viewBox units and
  squint height reached 3.30, with no browser errors. Visually reviewed the live
  page. This check used a separate browser session and did not change bot profiles.
- Linux and Windows release builds passed. Deployed after backing up the idle
  database; no backend behavior or guest update was needed. Published signed
  0.27.0, and the installed Windows helper verified signature, download, archive
  and version without switching versions or restarting the user's client.
  Diff and archive integrity checks passed.
  Linux SHA-256: `7d0a6c94900173cc4d67bed2b57c662d74bba8255a8925b2a1e837eb948073e0`.
  Windows archive SHA-256: `d764de4175a8fdb61fc69d8a932b78b4fc909bb4848ed9561a5464bda6a36587`.

## Version 0.26.0 recurring checks and conversation decisions

- Bots can present a factual summary with two to six selectable options and a
  custom response. Answer receipts persist across reloads and server restarts.
  The question ends the provider turn and releases its screen/concurrency slot;
  one authenticated answer queues one continuation in the original bot/chat.
  Same-answer retries return that continuation instead of duplicating work.
- Saved topic decisions are available in context and through exact lookup.
  Continuation ownership and task trigger are explicit: recording a choice does
  not mean its action has run, and later checks must not repeat that action.
  Quiet routine completion suppresses a completion notification. Routine prompts
  remain in task history without adding repeated user bubbles to chat. New
  questions notify once; real failures and the existing notification preferences
  remain effective. External actions retain the normal approval policy.
- Weekly schedules support IANA time zones, selected weekdays, first/last local
  times and repeat minutes. Tests cover all twelve hourly slots from 08:33 through
  19:33 on weekdays, weekends, busy/delayed ticks, retained minute alignment and
  spring/fall clock changes. Invalid schedules and cross-owner updates are rejected.
- Full `cargo test --locked -- --test-threads=1` passed: 88 tests, including
  persistent/concurrent answers, selected/custom responses, membership/archive
  boundaries, duplicate topics, notification behavior and existing approval,
  cancellation and human-subtask regressions. Codex native/dynamic question and
  Pi continuation tests use controlled provider fixtures; Pi uses pinned SDK 0.85.1.
- `test-decisions-schedules.cjs` passed in Edge and WebKit for three choices,
  custom text, durable receipts, links, focus/caret preservation during refresh,
  weekly editor round trips and mobile layout. Existing user-flow, chat-history
  and group-presentation suites also passed in both engines. Reviewed desktop
  light/dark and mobile question/schedule screenshots.
- Actual Codex synthetic-email checks used an isolated temporary bot and disabled
  routine invoked through Run now. Real UI selections verified let-lapse inaction,
  a self-service link, custom inaction and opening the supplied example.com page.
  The action emitted successful navigation and screenshot tool results; inspected
  the loaded page. Reload retained the selected receipt. A later same-topic check
  called `finish_quietly`, with no assistant output, repeat question or external
  action. The temporary routine was removed and its bot/chat archived.
- Live testing exposed and fixed cold-browser launch waiting on Chromium's
  persistent process, ambiguous ownership of a recorded answer, and missing
  scheduled-trigger context. A browser-launch test proves the process stays alive
  and immediate failures surface. Navigation still requires a screenshot to prove
  the destination loaded. These checks did not access a real inbox or billing
  account, enter a card, or configure the user's actual recurring inbox monitor.
  Topic deduplication requires stable keys; it is not semantic email deduplication.
- Linux/Windows release builds passed. Host and guest binaries were aligned after
  an idle check and database/binary backup; the configured Pi worker was retained.
  Published signed 0.26.0. The installed Windows helper verified its signature,
  download, archive and version without switching versions or restarting the app.
  Final diff and archive integrity checks passed.
  Linux SHA-256: `0fefff2a4f8668d1d5d4005b6b090ed7c89067234260619959cb8523aa25ab69`.
  Windows archive SHA-256: `e7b8cb1cff121788ecaf0523510d05fbcded20f8f0869af807389ae4187498fe`.

## Version 0.25.0 group presentation and chat actions

- Group tiles now contain the user's initials above overlapping bot silhouettes,
  with theme-aware separator strokes and a count for additional participants.
  Sidebar rows and the group header use compact composites rather than one bot.
  Initials preserve a single uppercase name such as CZ; the user approved and
  saved CZ as their display name while retaining other general preferences.
- Bot messages use a dedicated left avatar gutter with a colored name above the
  bubble. Consecutive messages keep the gutter without repeating the author.
  Sender colors meet 4.5:1 contrast against the dark/light conversation background.
- Right-click or Shift+F10 on a group exposes Rename, Pin/Unpin, Chat settings and
  Archive chat. A visible header action button supplies the same menu on mobile.
  Rename uses a small focused dialog and current chat metadata to retain members,
  pin state and history. Menus support keyboard navigation and outside dismissal.
- The new group-presentation suite passed in Edge and WebKit for two/six-bot
  bounds, initials, headers, gutters, both themes, rename persistence, keyboard
  focus, pin/unpin and mobile menu placement. Existing collaboration/pins,
  chat-transitions and chat-history suites passed in both engines. Reviewed
  desktop light/dark and narrow-layout screenshots.
- Live server assets matched source. The actual pinned general chat showed CZ,
  its member composite, sender gutters and the right-click Rename dialog with the
  current name. Closed it without saving; name, members and pin remained unchanged.
  No browser errors. The live group has three members, while the pair/six-member
  cases above use fixtures. No backend or guest behavior changed.
- Linux and Windows release builds passed. Deployment backed up the idle database
  and retained the configured Pi worker. Published signed 0.25.0; the installed
  Windows helper verified signature, download, archive and version without
  switching versions or restarting the client. Final diff and archive checks passed.
  Linux SHA-256: `602febb302765a3391a49b8f088e86ace7d39707d3ed24a4f493e1d968f02c56`.
  Windows archive SHA-256: `ac61ee8b5b4783c2f166806725cae3e8690167fdceccf6cbce8a9dd7bc8011f2`.

## Version 0.24.0 conversation screen menu

- Replaced the native computer-header select, whose popup displayed low-contrast
  text in the Windows client, with an explicitly themed HTML menu. Its current
  selection has a checkmark; arrow/Home/End keys, Enter, Escape, Tab and outside
  clicks are supported. It fits the narrow layout and retains focus appropriately.
- The computer header lists only unarchived bots in the active chat, with one
  choice in a DM. Selection rejects nonmembers, and a removed/archived selected
  member falls back to a remaining participant before reconnecting. Menu refreshes
  preserve an open menu unless membership, names, selection or teaching state changes.
- Global Computer settings retains its workspace-wide preview selector. Opening
  an outside bot's preview enters that bot's DM so the computer's chat context and
  menu agree. Native preview options also have explicit theme colors.
- New screen-menu tests passed in Edge/WebKit for dark/light contrast, exact pair
  membership, single-bot DMs, keyboard selection/dismissal, mobile fit and member
  removal reconnects. Existing settings-screen and user-flow suites passed in both
  engines, including stale-preview protection and human takeover/resume behavior.
- Live app/CSS bytes matched the source. The actual Izabella/Piper chat displayed
  exactly Izabella's screen and Piper's screen with a readable dark menu; the
  computer remained view-only and there were no page errors. Visually reviewed it.
- Linux/Windows builds and final diff checks passed. No backend or guest changes.
  Published the signed 0.24.0 archive; the installed helper verified its signature,
  download and contents without switching versions or restarting the client.
  Linux SHA-256: `30bb2d3d859c05e02efc995bee0fd47bdc53604594b59c1887cded1c85dcf531`.
  Windows archive SHA-256: `824cfc129c5aef23391bdfaa14c1368e2d6e50e78ecc2facce1fddfe84914ea4`.

## Version 0.23.0 application logo

- Replaced the gray bot on a dark rounded tile with an original apricot K-shaped
  character, subtle shading and curved eyes. The mark has a transparent background.
  Reviewed it at 16, 24, 32, 64, 128 and 256 px on both dark and light backgrounds.
- The previous SVG source still contained the earlier letter K while the PNG/ICO
  contained a different gray bot. desktop/icons/icon.svg is now the canonical
  source; scripts/generate-icons.cjs renders the 512 px PNG, seven Windows icon
  sizes, and an identical SVG favicon. Tauri explicitly references those assets.
- Verified RGBA transparency and clear corners for every exported size. Parsed
  the compiled Windows PE resources and confirmed all seven exact PNG payloads
  from the ICO are embedded in the executable. ICO SHA-256:
  `31df7014fd7a6f631a61d248bc25fec9c687090cbf714ffe17f4a30d370825cc`.
- The live app loads a versioned favicon URL, served as image/svg+xml with bytes
  identical to the canonical source, and opened without page errors. Linux and
  Windows release builds and diff checks passed. No runtime logic or guest desktop
  changes were required; broad functional suites were not repeated for this asset update.
- Published the signed 0.23.0 update. The installed helper verified the signature,
  download and archive without switching versions or restarting the user's app.
  Linux SHA-256: `1e1c328433b78eafc2a7210fa352ab78739c24a8766419dc5f5854bdf38fd230`.
  Windows archive SHA-256: `d5e7a158b1d4b4bc1850b7cd603e88f48d5cbeab24c5bb299d5d832c3ffceeea`.

## Version 0.22.0 facial geometry and live avatar edits

- Replaced sampled eye polygons with smooth cubic contours and adjusted taper,
  tilt and surface curvature. Rounded the capsule, triangle and cloud contours.
  Each of the eight silhouettes now has its own eye position, spacing, gaze travel,
  vertical movement and turn distance. Triangle/drop faces sit lower and closer
  together. Blinking uses the shape's own eye line as its origin.
- Avatar saves now render the current conversation from cached history as well as
  refreshing its sidebar/header/details, so an empty DM's welcome avatar changes
  shape and color while the customization popover remains open.
- The new test-character-faces suite passed in Edge and WebKit: changed triangle
  shape and three colors in the welcome view without navigation; checked 422,400
  sampled contour/margin points per engine across eight shapes, four eye styles,
  idle and front-facing working poses; and checked blink-center stability. Full
  turns intentionally occlude the face at the silhouette edge and are covered
  separately by the existing conversation-motion suite.
- Edge experience, conversation-motion, chat-transition and desktop-polish suites
  passed, covering reduced motion, arrival/departure order, keyboard focus and
  avatar preference persistence. Reviewed shape contact sheets at 44/84 px and
  the welcome/picker screenshot. No backend logic or guest desktop scripts changed.
- Live 0.22.0 app/character assets matched the source. Vivienne's actual welcome
  view showed the triangle's new eye layout and 50/59 blink origin without page
  errors; this read-only check did not alter any user's avatar or conversation.
- Linux/Windows release builds and diff checks passed. The signed Windows update
  was published and verified by the installed updater without restarting the client.
  Linux SHA-256: `77023de24827dc87f33ae6ab662e843c97d1c1593e5730e0eafddc7ee5a2b8e8`.
  Windows archive SHA-256: `4d15fe89def98228637e94e6c21b2b5472186c1c299f024dbeb610525190d624`.

## Version 0.21.0 computer desktop appearance

- Added 12 px rounding to the actual noVNC canvas and computer thumbnail. A live
  Edge session on Izabella's screen rendered the wallpaper and dock at the unchanged
  1280 by 800 resolution, with the computed corner radius and no page errors.
- Installed the native tint2 dock, original SVG wallpaper/icons, feh, PCManFM and
  xterm in the guest. Applied the shell to all seven existing running screens without
  restarting their X servers or browsers. Other desktops receive it on startup.
- Used the unassigned screen 32 for actual mouse-click checks of Browser, Files and
  Terminal. All three produced visible windows on that screen; Chromium used the
  browser-32 profile. A repeated shell start left exactly one dock. Openbox reserved
  the bottom 82 px for the dock. The temporary screen was stopped after verification.
- Exercised the real ensure-screen/systemd path for a fresh profile and a restart
  with a saved browser profile. Both became ready; the latter restored Chromium.
  New computers start at the desktop, while the saved-session readiness gate still
  prevents navigation from racing browser restoration.
- Shell syntax, diff checks, Linux/Windows release builds, and existing Edge/WebKit
  user-flow suites passed. No backend logic changed. Published the signed Windows
  0.21.0 archive; the installed updater verified its signature, download and contents.
- Linux SHA-256: `ff9002b50708e02b68b704a7cdaebac1ad41f9581279de0948d2b02d8688dfc4`.
  Windows archive SHA-256: `88d667ca396274ccaac4c6a3bbcb1a782ca41177eccabf58f6a04616da2c6cdc`.

## Version 0.20.0 role retention and chat completion motion

- Traced the live failure to an empty recipient memory: Piper saved Izabella's role
  only in Piper's memory. The receiving handoff now explicitly requires its own
  memory save. A real-model canary also exposed sender/subject confusion; the final
  shared prompt and tool contract include an explicit bot identity, teammate
  directory, named-subject preservation and actual-delivery requirements.
- The final canary used the same configured Codex model as Izabella. The recipient
  saved its own ongoing responsibilities, recalled urgent/billing/follow-up support
  email duties in a separate DM without consulting the coordinator, and a second
  handoff reused the same group. Temporary check bots and chats were archived.
- Repeated the user's original Piper request through the normal message endpoint.
  Izabella received it in the existing collaboration chat, called remember
  successfully, and saved her inbox/email responsibilities herself. A subsequent
  independent DM asking What is your role? included organizing and managing email.
  No manual database or memory repair was used, and no additional pair chat was created.
- Fixed group work rendering so a handoff reference cannot display a source DM's
  stale worker or its progress messages. Dedicated pair chats are reused; archived,
  user-created and larger groups remain separate.
- Replies reveal over 280 ms while the working bot lingers for 1,100 ms, then
  shrinks toward a dot over 650 ms and its empty space collapses. Polling preserves
  the sequence. Reduced motion and reopening completed history skip it. A late
  message page keeps the working presence until the result arrives.
- All 77 backend tests passed. Edge and WebKit transition and public-reply checks
  passed, covering linger, reverse-dot exit, delayed result pages, DM/group scope,
  no history replay and reduced motion. Edge chat-history, collaboration/pinning,
  experience and user-flow suites also passed. Transition screenshots were reviewed.
- Live browser acceptance verified Izabella's new DM answer and the completed group
  with no phantom worker or visible raw activity. Served app.js, style.css and
  characters.js match source bytes. The service is active with zero automatic restarts.
  Linux and Windows release builds passed; deployment passed idle-task, backup,
  configuration and pinned Pi runtime checks. Linux SHA-256:
  `08eabf5b5cbdf475d9d468f3be6130d93a3a60c69c263ae333d48fb3259abbea`.
- Signed Windows 0.20.0 is 3,801,167 bytes, SHA-256
  `3a8692797008698b08ab10dcaeaf1cb583035bc940a59e0a2c046f39b94ef712`.
  The installed updater verified signature, download, archive and version with
  VerifyOnly, without installing or restarting the user's app.

## Version 0.19.0 persistent replies, browser fallback and activity preference

- Public assistant events now enter chat history atomically. Completion keeps each
  earlier reply and promotes the final matching message without duplicating it.
  Migration restored all 15 missing public replies in the live database, including
  two in Piper's chat. Read-only post-deployment inspection found zero remaining
  missing replies. Recovery did not enqueue or replay tasks.
- General settings now contain Show activity in chats, default off. Its persisted
  value controls every chat while leaving approval and human sign-in cards visible.
  The shared model prompt explains browser access without a connector, user sign-in
  on the bot's own screen, and fallback from unavailable or malfunctioning connectors.
- All 75 backend tests passed, including live-message persistence, final-result
  deduplication, repeatable historical recovery with equal timestamps, bounded and
  chat-scoped pagination, and settings default/persistence/validation. Existing
  Codex and Pi human takeover/resume and approval tests passed.
- Edge and WebKit public-reply and chat-history suites passed. Edge experience,
  user-flow and settings-screen suites passed. These cover completion and reload,
  activity toggling, visible sign-in controls, bounded history, anchoring, cached
  conversations, human takeover and account flows. Linux and Windows builds passed.
- Live read-only browser acceptance confirmed recovered messages, default-hidden
  activity and the General switch. Served app.js/style.css/characters.js match source
  bytes. The General settings screenshot was visually reviewed after its entry
  animation. The service is active with zero automatic restarts.
- Deployment passed the idle-task gate, database/binary backup, configuration check
  and pinned Pi runtime check. Linux SHA-256:
  `4f3d52985bac3e383f8192f56bb8782339106fa7fcac2ab245340fb4a64d1e81`.
- Signed Windows 0.19.0 is 3,799,535 bytes, SHA-256
  `2324ae675f9af761e51636882a4296c7c9be92bd62482b77407b7131af276fbe`.
  The installed updater verified its signature, download, archive and version using
  VerifyOnly, without installing or restarting the user's app.

## Version 0.18.2 settings header containment

- Removed conflicting settings padding and sticky-header backdrop rules. The header
  is now a fixed flex row flush with its panel; only settings-content scrolls, clipping
  content beneath the header. Switching settings tabs resets the content scroll.
- Targeted Edge and WebKit browser checks passed at 1320px, 768px and 390px widths
  at the top, middle and bottom of Connections. Header/panel tops and header/content
  boundaries matched within one pixel, with no outer-panel scrolling. Desktop and
  mobile screenshots were reviewed. Edge settings-screen and user-flow suites passed.
  This layout-only patch did not rerun backend tests. Linux and Windows builds passed.
- Read-only live acceptance confirmed the same aligned header and clipped content
  after scrolling Connections. Served app.js/style.css match source bytes; the service
  is active with zero automatic restarts.
- Deployment passed its idle-task check, backup, configuration and Pi runtime checks.
  Linux SHA-256:
  `c889a81273f4a34ebb69ed73e4f2f64ddbfddc18d67c00ab679fdf8e5b9fc409`.
- Published signed Windows 0.18.2: 3,799,445 bytes, SHA-256
  `2f9a083758779aaf41f5af127bd9163632c7dfed4e14004df7d66c2f797a26c4`.
  The installed updater verified signature, download, archive and version in VerifyOnly
  mode without installing or restarting the user's app.

## Version 0.18.1 Settings screen identity and connector access wording

- Settings > Computer shows a small bot-name selector beside Live computer. It lists
  active bots and updates the preview without changing the conversation or taking
  control. Opening the preview uses that selected screen. Teaching still prevents
  switching screens. Preview requests are grouped by bot, and late responses cannot
  overwrite another bot's preview.
- Connector access now says Read and write and explains that mutations follow the
  bot's approval setting. Existing Full access skips prompts for write-enabled accounts;
  read-only restrictions and provider scopes still apply. No account grants or backend
  permission policies were changed. Documentation now distinguishes these two controls.
- Edge and WebKit passed the new settings-screen suite, including delayed response
  races, distinct underlying/detail previews, view-only opening and mobile bounds.
  Both passed user-flow checks with the updated account-access labels. Desktop and
  mobile screenshots were reviewed. This frontend patch did not rerun backend tests.
- Read-only live acceptance switched the Settings preview from Piper to Izabella,
  verified the image changed, and kept the Piper conversation selected. Served app.js
  and style.css match source bytes. Linux and Windows release builds passed.
- Deployment passed its idle-task check, backup, configuration and Pi runtime checks.
  The service is active with zero automatic restarts. Linux SHA-256:
  `d13a1092916cc433844758f8101960e2f0a4f234c74f124269701687a04d2ce2`.
- Published signed Windows 0.18.1: 3,799,158 bytes, SHA-256
  `07d671adc622a8e51acafdf70c784c6321693a1f6f8e4c2932cc00f70e58ee3f`.
  The installed updater verified signature, download, archive and version in VerifyOnly
  mode without installing or restarting the user's app.

## Version 0.18.0 paged conversation history and pin icon

- Conversation history loads 50 messages initially and pages in either direction.
  The rendered window is capped at 150 stored messages; offscreen pages are unloaded.
  Six recently visited chats retain their loaded window and scroll anchor. Selecting
  the current chat does not clear it; switching to a cached chat paints immediately
  before background refresh. Unchanged refreshes leave the rendered view intact.
- The API validates limits and chat-bound cursors, uses chronological keyset paging,
  and preserves timestamp/sequence ordering for migrated history. Visible old results
  load their run details even when absent from the recent-runs feed. Model context
  limits are unchanged. The pin glyph is now a simple upright pushpin.
- All 72 Rust tests passed, including complete traversal beyond the old 300-message
  cutoff, tied/out-of-order timestamps, forward and inclusive-range queries, concurrent
  insertion stability, authorization, page limits and invalid cursors.
- Edge and WebKit passed the new 1,000-message history suite, including offloading,
  anchor preservation, cached switching under network delay, arrivals while reading,
  page retry, historical activity, LRU eviction and 390px mobile layouts. Both also
  passed the existing experience suite. Edge passed collaboration/pins and desktop
  polish. Desktop pin and mobile history screenshots were visually reviewed.
- Read-only live acceptance verified two-message before pages, an inclusive window,
  and Piper/Izabella cache restoration against the deployed service. Piper has 32
  messages, so full scroll-triggered paging was exercised in the synthetic long-history
  suite. Served app.js, characters.js and style.css match source bytes.
- Linux and Windows release builds passed. The server passed an idle-task check,
  database backup, configuration and Pi runtime checks; it is active with zero automatic
  restarts. Linux SHA-256:
  `8f4837c182527dc423f81e4a18481422244de42fe89307d1adc1add3b4fa233d`.
- Published signed Windows 0.18.0: 3,798,847 bytes, SHA-256
  `9b1c42b1c6e70c00cbd7c20da2f5e73f79506c5056e3937a7ceff1f14ebd8c43`.
  The installed updater verified signature, download, archive and version in VerifyOnly
  mode, without installing or restarting the user's app.

## Version 0.17.1 combined avatar menu

- The Details avatar popover shows all eight shapes in two rows above its fourteen
  colors. The extra Shape & color button is removed. Choices save immediately;
  the same popover remains open and retains keyboard focus across profile refreshes.
  Selected shape/color states and shape previews reflect the saved profile.
- Edge and WebKit desktop-polish checks passed, including keyboard selection,
  reload persistence, touch selection and viewport bounds at 390px. Desktop and
  mobile screenshots were visually reviewed. The Edge experience suite also passed.
  This patch changes frontend presentation only; the 71 backend tests recorded
  below were not rerun. Linux and Windows 0.17.1 release builds passed.
- The deployment passed its idle-task check, database backup, configuration and Pi
  runtime checks. The service is active with zero automatic restarts. Served app.js,
  style.css and characters.js match the source bytes. Linux SHA-256:
  `5809c0cff8cea7d001e2cbc2179eca844c2994f3b4d129c1d25e81227157bfa1`.
- Published signed Windows 0.17.1: 3,797,374 bytes, SHA-256
  `e4eb9adbbe0ec0acfc0817f5bcade4c65adcf3d1fcb861d18d20461c5fae9685`.
  The installed updater's VerifyOnly mode verified the signature, download, archive
  and version without installing or restarting the user's app.


## Version 0.17.0 idle gaze, quieter chat and teammate proposals

- Idle characters blink and look around, with curved eye paths and perspective
  narrowing. The pebble silhouette is fuller and asymmetric. Timeout-based rest,
  slouch, closed-eye and zzz animations are removed. Generic thinking, investigation
  and reading use the normal working motion; full turns now occur once per 18-second
  cycle. Working labels omit the bot name.
- Global notifications offer All, Input needed and None. Each bot retains one toggle;
  device helper text and test-notification buttons are removed. Server-side filtering
  covers both native and browser clients, advancing cursors through muted events.
- Chat follows the latest content during incoming refreshes and delayed image loads,
  while visible-message anchors preserve the reader's position in older history.
  Refreshes no longer start competing smooth-scroll animations. User scrolling or
  typing during a pending send is preserved. Instructions and Memory open in large
  editors; field-only compare-and-save writes protect concurrent model memory updates.
- Bots can fill a teammate proposal with avatar, name, role, description and instructions.
  Its persistent chat card offers Create and editable Details. Creation is authenticated,
  atomic and repeat-safe, with a new DM available immediately. No bot exists before
  the user's click. The composer menu now opens Attach files and Teach a task.
- All 71 Rust tests passed with the pinned Pi runtime and serial execution. New tests
  cover draft-only tool behavior, edited creation, concurrent retries, stale text edits,
  global notification frequency, authenticated uploads/downloads, body size, filename
  validation, atomic attachment binding and cross-chat access rejection. Existing
  provider, approval, takeover, handoff, migration and restart tests also passed.
- Edge and WebKit passed the new experience suite, conversation-motion suite and
  existing user-flow suite. Edge also passed desktop-polish and collaboration/pin
  regression checks. The experience suite includes delayed screenshots, reader
  anchors, uploads, editable proposal creation, compact text editors, notifications
  and opening teaching from the composer. Desktop screenshots were visually reviewed.
- Real Codex run `05185835-f5a9-4e92-bb27-3e8ad5e9b04e` used an uploaded synthetic
  CSV through read_attachment, computed the exact $300.00 total, and filled the
  Invoice Steward proposal. Details preserved its generated fields; editing the name
  and clicking Create added one Invoicing Review bot. Repeating the same creation
  request returned its existing ID. The model initially supplied color names instead
  of hex; named palette colors are now normalized and covered by a regression test.
- Actual attachment `0443f366-cefc-43f5-a354-403068450ad2` was imported into the VM
  and read successfully. Draft `c9f20670-ee61-446a-9f00-3dce77e78479` and its exact
  99-byte download survived the final server restart. Both test bots were archived;
  readback hashes verified the original Piper and Izabella records were unchanged.
- Linux and Windows release builds passed. The server is active with zero automatic
  restarts after an idle-task check and database backup. Deployed app.js, characters.js
  and style.css match the source bytes. Final Linux SHA-256:
  `0f7b2911984ce82d9dde900cb5c9fc672e5d7fe4bad2d6ac3a76971ea56e127a`.
- Published signed Windows 0.17.0: 3,797,408 bytes, SHA-256
  `4e4245200e006255e6405e6653b46e2d768c3ee4faec1e372c464c488b2f9eaa`.
  The installed updater's VerifyOnly mode verified the signature, download, archive
  and version without installing or restarting the user's app.

## Version 0.16.0 direct-message handoffs and pinned conversations

- Fixed DM mention routing: mentioning another bot no longer creates a group or
  sends the user's request to that bot. The selected bot receives the request.
  Its real handoff creates a linked group, keeps the source DM unchanged, and
  returns the teammate's result to the requester in the shared chat.
- Completed Codex and Pi answers use the final assistant message; progress events
  remain available during execution and in stored events. A live two-message
  acknowledgement/answer sequence produced one final answer, without concatenation.
- Bots and chats can be pinned from their sidebar hover controls and unpinned from
  their tiles. Hover/focus previews show the latest conversation message and sender.
  Pin writes are isolated from memory/membership updates. Group composers retain
  the normal 27-pixel provider logo instead of substituting a small @ symbol.
- All 67 Rust tests passed with the pinned Pi test runtime and serial execution,
  including DM routing, atomic rollback, private-history separation, automatic
  continuation, pin persistence, authenticated pin routes, and both provider loops.
  A parallel run hit a transient lock on the new database-reopen test; serial
  execution passed. Edge and WebKit passed the new collaboration/pin suite and the
  existing user-flow suite. Desktop and mobile previews were visually reviewed.
- Real Codex acceptance used two temporary bots. The first attempt exposed a
  semantic error: a role was interpreted as a name. Revised guidance distinguishes
  names from responsibilities and requests concrete facts from memory. A fresh
  pair then passed the same ambiguous 'ask Piper for your role' request through the
  real composer, bot discovery, handoff, reply, continuation and remember tool.
  The requester saved 'Role responsibilities: inbox triage and flagging renewal
  emails.' The group contained the requester/recipient/requester sequence, with no
  user message moved into it. Live pin preview and provider-logo checks also passed.
- Passing live run IDs: seed `89d755fb-44c2-462e-bb33-c766889effdf`, requester
  `cd4867d5-f162-4dc3-8118-5f5e511edaba`, recipient
  `76ade207-5547-4e6c-8d03-b9bf57d2e00b`, continuation
  `32d3202d-1bbf-4c9d-8b0b-a360e0c653a4`. All test bots and their groups were archived
  afterward. Readback hashes confirmed the original Piper and Izabella bot records
  were unchanged by both checks. No VM or external account actions were requested.
- Linux and Windows release builds passed. Deployment used an idle-task check and
  database backup. Final Linux SHA-256:
  `4edfbe71a5643f73be5e6527b5b74e43cd449231fc6e38e3ff43bf1b38e6e246`.
- Published signed Windows 0.16.0: 3,795,106 bytes, SHA-256
  `d0944d9f00a357457e68527d186a8735651e2e1cefbdff493348b2ff94b32935`.
  The installed updater verified the signature, download, archive and version
  without installing or restarting the user's app.

## Version 0.15.3 calmer completion and working presentation

- Completed replies now return directly to idle in the server activity API and
  client, including older responses carrying the former success shape. Sleeping
  starts at 3,600 seconds of inactivity. Active tasks and approval/human waits
  remain awake.
- Both conversation renderers share a 48-pixel working character (previously 20).
  The inline Stop control is removed. Status text is hidden until hover or keyboard
  focus and uses the existing shimmer, respecting reduced-motion preferences.
- Edge and WebKit conversation-motion checks passed: immediate neutral completion,
  the exact one-hour boundary, active-state exclusions, status visibility/shimmer,
  keyboard access, narrow layout, character size, preserved arrival/turn/tool motion
  and reduced motion. Screenshots were reviewed.
- All 64 Rust tests passed using the pinned Pi test runtime. An initial invocation
  of the older build script lacked the Pi test environment and failed five harness
  tests; rerunning with the configured runtime resolved all five.
- Linux and Windows release builds passed. Live JavaScript, CSS and character code
  match source; nine completed bots reported the neutral idle shape. Browser
  readback passed with no JavaScript errors. The service is active with zero
  automatic restarts, after an idle-task check and database backup. Linux SHA-256:
  `1272b8fb465317b1e26d2fa3d060eb1cb5cd8e96bced2782ca508c4efb77c75b`.
- Published signed Windows 0.15.3: 3,794,004 bytes, SHA-256
  `bb058494f1e3653bf8516a589381a39803ca029096460a3aea896d5763533e25`.
  The installed updater verified the signature, download, archive and version
  without installing or restarting the user's app.

## Version 0.15.2 screenshot download affordance

- Replaced the text download control with a 32-pixel circular SVG icon. It is
  hidden until the image is hovered or a control in the image receives keyboard
  focus. Devices without hover keep the compact icon visible for touch access.
  An accessible label, tooltip and explicit tab stop preserve its purpose.
- The desktop-polish suite passed in Edge and WebKit, including hidden/revealed
  states, circular dimensions, SVG-only content, keyboard navigation, touch
  download, exact downloaded image bytes, enlargement and reload. Live 0.15.2
  readback also passed hover, download and keyboard checks with no JavaScript
  errors; deployed JavaScript/CSS match source.
- Linux and Windows release builds passed. The server reports 0.15.2 with zero
  automatic restarts after an idle-task check and database backup. Provider and
  VM components are unchanged. Linux SHA-256:
  `d4c7c4e9f0ad0af81106b2db7275925ca93b0891af8f737ba357be108c58db54`.
- The signed Windows ZIP is 3,794,045 bytes, SHA-256
  `1f6215cd4ded83fee674dc2d59c22b598b8a7ecd9a93eff994f12c35fc0f57ef`.
  The installed 0.15.1 updater verified the signature, download, archive and
  version 0.15.2 without installing it or restarting the user's app.

## Version 0.15.1 conversation previews and character motion

- Sidebar previews retain the most recent message during rest, queued work,
  execution, approval waits, human subtasks and failures. The top-bar status
  subtitle is removed, and the remaining name and avatar are vertically centered.
- The supplied 26.6-second animation reference was inspected frame by frame.
  Original SVG code implements a dot-to-body arrival, gentle working deformation,
  face projection around the back of the body, and a brief colored turn trail.
  Existing chosen shapes, sleep poses and tool gestures remain. Reference frames
  are not included in the source or desktop package.
- `test-conversation-motion.cjs` passed in **Edge and WebKit**. It checks all six
  preview states, header alignment/removal, creation through the normal bot form,
  arrival playing once across sidebar refreshes, ten deterministic turn frames,
  preserved silhouettes, synchronized small/large instances, tool motion starting
  after morph completion, and reduced motion while the loop is running.
- The desktop-polish and user-flow suites also passed in **both engines**, covering
  screenshot display/download, avatar selection and hover, title-bar controls,
  notifications, named accounts, approvals and human hand-back. macOS/Linux chrome
  checks use browser fixtures; no new native-platform execution is claimed here.
  Both Linux and Windows release builds passed. Provider code is unchanged from
  the 64-test Rust and 12-test Pi snapshot below.
- Live authenticated readback on 0.15.1 confirmed that the HTML, JavaScript and CSS
  match source, Piper's preview matches the latest message, and the header has no
  status subtitle. Desktop and mobile views were visually reviewed, with no
  JavaScript errors or API writes from the review. A rendered animation preview
  was captured from the implemented character renderer.
- The final alignment check corrected a 2.5-pixel inline baseline offset in the
  avatar wrapper. Because 0.15.0 was already published, that correction received
  version 0.15.1; the earlier immutable archive remains available.
- The server reports 0.15.1 with zero automatic restarts. Deployment followed an
  idle-task check and database backup. Linux SHA-256:
  `bf1e0c9b15e215a2a4e9a5c6a17deb71d6fb2b1ebd48a8818f669d4f7a59793e`.
  This release changes UI assets and version metadata; VM and Pi components
  were unchanged.
- The signed Windows ZIP is 3,794,119 bytes, SHA-256
  `a5de0101480cad2697c0dd15dceb2d1137ae4f817ffd0c02f9449fd6fa4a5471`.
  The public feed and archive agree. The installed 0.14.0 updater verified the
  signature, download, archive and version 0.15.1 without installing it or
  restarting the user's app.

## Version 0.14.0 Pi integration and image controls

- Rust: **64 passed, 0 failed**. The OpenRouter integration tests now launch the
  real Pi worker; they cover approval denial, cancellation while awaiting a human,
  same-call continuation, memory persistence and sanitized provider failures.
  Both Linux and Windows release builds passed.
- Pi SDK: **12 passed, 0 failed**, using the installed 0.85.1 SDK and streamed
  provider fixtures. Coverage includes tool/result pairing, schema validation,
  unavailable host tools, sequential execution, budgets, images, human hand-back,
  cancellation, real automatic compaction, privacy parameters, redirect rejection,
  response limits and incomplete output. Fixtures do not spend provider credits.
- Live OpenRouter inference used `anthropic/claude-haiku-4.5` through Pi 0.85.1.
  Run `0e86512e-079f-453e-b48a-b6c99667da1d` paused for manual approvals, created
  and read `/workspace/kindred-pi-0.14-check.txt`, and persisted memory. Its first
  navigation timed out: a cold screen let navigation race Chromium startup, and
  the short-lived navigation process became the profile owner before being killed.
  The screen template now waits for Chromium's first visible window in
  `ExecStartPost`. Only the idle test screen was restarted; existing screens were
  left running. A subsequent cold start passed that readiness check.
- Live browser run `10c5947f-5eab-46e1-b160-e618442eaa06` then completed with no
  failed tools. It opened Aldabra, identified the heading "Pentests and audits,
  run efficiently." from the actual image, shared a 1280 by 800 screenshot and
  updated memory. Its downloaded attachment is 519,053 bytes, SHA-256
  `5de7a0ea2e8fde5b578f12402fd67ddf607dee66c0bb0447bb97612d5f4f586e`.
  The dedicated acceptance bot preserved all existing bots' provider/model/thinking
  settings, including Piper's Codex selection.
- Edge and WebKit desktop fixtures passed the image download overlay bounds at
  desktop/mobile widths, centered profile hover, animation completion and reduced
  motion checks. The live authenticated chat was reviewed at 1320 and 390 pixels;
  its downloaded screenshot matched attachment size, the control stayed inside
  the image, and there were no JavaScript errors or unexpected API writes.
  macOS/Linux title bars were checked in browser fixtures, not native applications.
- The server runs 0.14.0 with zero service restarts since deployment. Deployed UI
  files match source. Pi's pinned Node 24.14.0 archive passed its published SHA-256
  check, and the worker import/version check passed as the service account.
  Linux SHA-256:
  `e0f96aaa8c551a3dab0047dda8b4b8ea05edfdd79eb88c31963706276b02f386`.
  An idle-task check and database backup preceded deployment. Guest changes were
  limited to the screen readiness helper and user-service template.
- The signed Windows ZIP is 3,792,129 bytes, SHA-256
  `07a03a37c04c73ce5dce27ba4056c60d127269cb5e4ea6a67a581206a318af99`.
  The public feed and archive agree. The installed 0.13.0 updater verified the
  signature, download, archive and version 0.14.0 without installing it or
  restarting the user's app. Custom/local Pi adapters have an extension point;
  they are not enabled or live-tested in this release.

## Version 0.13.0 desktop polish and screenshot delivery

- Rust: **63 passed, 0 failed**. New coverage includes authenticated PNG
  persistence and retrieval, malformed images, missing attachments, retained chat
  metadata, notification pagination, mute preferences and stale input requests.
  Linux and Windows release builds passed.
- Edge and WebKit passed the desktop polish fixture: eight revised silhouettes,
  background-colored eyes, black dark background, shape-following hover outlines,
  simplified customization, notification preferences, browser permission gestures,
  all three platform title bars, and screenshot display, enlargement and exact
  download bytes. Existing account/human-subtask, teaching and avatar motion
  regressions also passed. macOS and Linux title bars were checked in browser
  fixtures; native execution was verified on Windows only.
- An isolated real Windows desktop build passed maximize, restore, minimize and
  close checks. While minimized, its native notification worker delivered a
  synthetic completion toast, independently confirmed in Windows Notification
  Center under the Kindred application ID. The remote main window could invoke
  its window controls and notification commands, but could not invoke the local
  updater command. Notifications require the desktop process to remain open;
  browser notifications require the page to remain open and permission granted.
- Live Piper run `d8f716ad-c889-44a1-ad94-f512d8c9fa88` completed the request to
  open `https://aldabra.sh` and send a screenshot in chat. The model attached four
  captures; the authenticated live chat displayed all four as 1280 by 800 images
  with no JavaScript errors. One persisted PNG was 549,089 bytes, SHA-256
  `54d1fde22a4b1fcad15704434f52b8a2ee9fd11741d26e6daf93ca8bd588a6ec`.
  The page and the actual chat display were visually reviewed. Previously,
  screenshots reached the model but were not persisted as user-visible images.
- The running server reports 0.13.0 with zero service restarts. Deployed UI assets
  match source. Linux SHA-256:
  `0c404d093de5c8f1f8e46452bc1b4b0fc49532bc90a5acb3f926333da48d0ee1`.
  Database backup and an idle-task check preceded deployment; VM guest components
  were unchanged.
- The signed Windows ZIP is 3,791,314 bytes, SHA-256
  `c79cf8d516ad06fbcf7256ead8ad1791e3360f2d18d77ff4be116ce09ed3fe5a`.
  The public feed and local archive agree. The installed 0.11.1 updater verified
  the signature, download, archive and version 0.13.0 without installing it or
  restarting the user's app. Earlier immutable releases remain available.

## Version 0.12.1 mobile label correction

- Kept the Marketplace back label on one line at narrow widths. The Edge
  account, approval and human-subtask fixture passed again, including the 390-pixel
  dialog; the corrected mobile screenshot was visually reviewed. Runtime behavior
  is unchanged from the 60-test and Edge/WebKit acceptance snapshot below.
- Linux and Windows release builds passed. The running service reports 0.12.1
  with zero restarts; deployed JavaScript, character assets and CSS match source.
  Linux SHA-256:
  `8307e32b6846461ffe27b1480bcd2342c90c9c996783adfb20ee3a1bfade8339`.
- The signed Windows ZIP is 2,236,845 bytes, SHA-256
  `fbe9be670478c0ecacee57b9c14c55486d301233220a180916dcc3330aaf125e`.
  The public feed and local archive agree. The installed updater verified the
  signature, download, archive and version 0.12.1 without installing it or
  restarting the user's app. Database backup and idle-task checks preceded the
  server deployment; existing published version archives were preserved.

## Version 0.12.0 named accounts and human hand-back

- Rust: **60 passed, 0 failed**. Named-account tests cover preservation of legacy
  identities, unique labels before provider calls, independent activation/removal,
  private sign-in link projection, explicit provider account dispatch, ambiguity
  rejection and cross-toolkit rejection. Prior permission and account-binding
  regression tests continue to pass.
- Human-subtask tests cover releasing and reacquiring the actual screen mutex,
  suspending the run deadline, blocking a second run for the same bot, authenticated
  completion, wrong-bot rejection, repeated Done clicks, later manual control,
  cancellation, restart expiry and visibility after history limits. Codex RPC and OpenRouter protocol fixtures
  verify continuation of the original tool call and conversation without a new turn.
- Edge and WebKit passed the complete new UI flow: human takeover without cancelling,
  same-run Done, persistent approval receipts, three named Gmail accounts, renaming,
  per-account tool discovery, naming before a popup, duplicate-submit suppression,
  isolated account removal, and app dialogs at 1320, 800 and 390 CSS pixels.
- Both engines passed the existing teaching, screen selection, routine, resource,
  full-description and settings-layout regression flow. Avatar checks retained all
  eight sleep silhouettes, interruption behavior, morph-before-hammer timing and
  reduced motion. Both pickers have fourteen colors with only Grey and adaptive
  Black / white as neutral choices. Connecting uses one static SVG and shimmering text.
- Live read-only check: the deployed Composio project returned five Gmail-related
  catalog entries. No account is connected. OAuth completion, multiple real accounts
  and external account-level tool calls remain unverified; provider and sign-in
  fixtures do not establish that those external accounts are authorized.
- Final Linux and Windows release builds passed. Both engines passed the new UI
  acceptance flow against deployed assets; served JavaScript and CSS bytes matched
  the source. The portable fixture is included as
  `tools/frontend/test-user-flows.cjs`, with setup in `tools/frontend/TESTING.md`.
- Deployment backed up the database after confirming no queued or active tasks.
  The service is running with zero restarts. Linux SHA-256:
  `b402eb4734db7369c01cdbd787d8bb3f4826f0132fd56c1f0f0912078bbadc47`.
  Signed Windows ZIP is 2,236,734 bytes, SHA-256
  `a12ac64081d71c51c234636e436a19727fc90c864b9e508688065546cb4a5190`.
  Local, remote and signed payload hashes agree. The installed 0.11.1 updater
  verified signature, download, archive and version 0.12.0 without installing it or
  restarting the user's app. The VM guest helper and sign-ins were not changed.

## Version 0.11.1 character corrections

- Removed idle star spins, sleeping dream graphics and the old yawn overlay. Sleep
  keeps the selected outline, with a mild slouch, breathing and the retained zzz.
- Edge and WebKit checks covered all eight shapes: the exact chosen path remains
  intact in sleep, interrupted transitions return to the correct resting shape,
  and reduced motion shows a static slouch without running animations.
- Sampled the complete hammer transition in both browser engines. Every frame while
  morphing had no body animation or body transform. Hammering began only after the
  exact hammer path was restored, at least 500 ms after the transition started.
- Connections icon geometry, including stroke clearance, fits its SVG view box.
  Icon and label fit at 1320, 1000, 800 and 390 pixels, and the button opens Connections.
- Both color pickers expose all sixteen options. Black / white saves as an adaptive
  white profile and renders white in dark mode and black in light mode. Avatar edits
  refresh the header immediately along with Details and the sidebar. Saves used UI
  fixtures; the user's profile was not changed by these checks.
- Reviewed dark/light Details, the complete shape comparison and hammer screenshots.
  The existing Edge computer/teaching/routines/settings/marketplace regression flow
  passed. Backend logic is unchanged from the 52-test 0.11.0 snapshot below.
- Final Linux and Windows release builds passed. The visual checks also passed
  against deployed assets. The installed 0.11.0 updater verified the signature,
  download, archive and package version without installing or restarting the app.
- Server deployment followed an idle-run check and database backup. Linux SHA-256:
  `dbf2098691c5f66d584d573c99d0fd55d9f4ac1e96e6ea45f1d620da5f603ee6`.
  Signed Windows ZIP is 2,234,193 bytes, SHA-256
  `c2b63404d31ae629e11e54c90fdd381e755384459f927af500cf8297e69625d9`.
  Local, remote and signed payload hashes agree.

## Version 0.11.0 interaction and tool acceptance

- Rust: **52 passed, 0 failed**. The new authenticated tool-browser route test
  verifies read-only filtering, query forwarding and rejection of expired accounts
  before provider dispatch. Final Linux and Windows release builds passed.
- Edge and WebKit exercised teaching pause/reopen/resume with explicit control,
  step review/save, screen-bound routines, avatar editing, dialog transitions,
  connector loading failures/retry, API-probe failure/recovery, custom auth config
  selection, pending OAuth to active connection, and disconnect/reconnect state.
  Repeated Connect clicks produced one request. Provider writes used fixtures.
- Full app descriptions and pinned connection actions fit 1320, 800 and 390 CSS
  pixels. The real Composio catalog now responds successfully. Live marketplace
  layout passed at those widths. No account is connected, so actual OAuth completion
  and account-level connector tool calls remain unverified; fixtures do not prove
  those external services work for a signed-in account.
- The Skills editor blocked a 20 KB multibyte input without submitting, accepted a
  valid smaller skill, and showed the UTF-8 count. Long-chat Latest messages behavior
  passed. Avatar writes serialize and stale refreshes cannot replace newer edits.
- A live task using the existing Codex account and the archived acceptance bot
  completed app/connector/bot discovery, skill save/read, memory update, guest file
  write/read and computer screenshot/click/type/key/scroll checks. A separate live
  task opened `https://example.com` and verified its page. An initial unsupported
  data-URL navigation was rejected correctly; HTTPS validation was preserved.
- A temporary 60-second routine scheduled a real task and completed with the expected
  response. The routine and test skill were removed; the acceptance bot's original
  settings, memory and archived state were restored. Piper was not modified.
  Archived test conversations and `/workspace/kindred-acceptance-v11/result.txt`
  remain as test evidence. Cleanup confirmed no active test runs.
- After deployment, real HTTPS/noVNC viewing passed without taking control or sending
  manual-action writes. Takeover remained unchanged. Expanded layouts fit 1320,
  1000, 800 and 390 pixels; actual resource previews, light mode and OS reduced
  motion passed. The disposable browser had local-network permission granted.
- Live server reports 0.11.0, deployed after an idle-run check and database backup.
  Linux SHA-256 matches the deployed binary:
  `ab38221afa2c3dceb74afad8b1fd2cabdcf0f4d9bffab016cff83d7ae34d3edd`.
- Signed 0.11.0 ZIP is 2,235,005 bytes, SHA-256
  `9843dcd179c4b886042a3f2300048cda40d226df497276068ac1f9d1f52f7878`.
  Local, remote and signed payload hashes agree. The installed 0.10.0 updater's
  VerifyOnly diagnostic validated its signature, download, archive and version.
  A NoUi diagnostic initially honored an existing cancellation marker; the regular
  VerifyOnly path passed without removing that marker. Native update startup resets
  the marker. No installed application was restarted or upgraded by this check.
- Served UI offers the update to desktop 0.10.0 and hides it for 0.11.0. The public
  connection-page character's idle-to-sleep transition also passed. Existing account
  data, connector approval rules and the native updater implementation are preserved.

The older snapshots below describe their release-time state and limitations.

## Version 0.10.0 computer workspace acceptance

- Rust: **51 passed, 0 failed**, including exact toolkit detail projection without
  provider credentials, invalid toolkit rejection and reboot denial during takeover.
  Linux and Windows release builds passed. `git diff --check` passed.
- Edge and WebKit: 52px sidebar avatars, vivid legacy palette rendering, hover
  customization, Open overlay, expanded watching without takeover, per-screen
  routines, pause action, teaching input omission, step review, skill save/return
  control, collapse/close, resource placement, reboot affordance and full connection
  descriptions with marketplace navigation passed. WebKit also checked new routines
  bind to the selected screen and a dialog reopened during closing remains open.
  UI write paths used fixtures and did not alter user bots, skills or routines.
- Teaching records clicks, drag endpoints, scrolling and navigation keys, with typed
  text omitted. Users describe each step and review the resulting instructions.
  Reference screenshots and raw events remain in browser memory and are discarded
  when saving. This release does not infer a skill automatically from a video.
- Real deployed noVNC viewing passed over the configured HTTPS origin with local
  network permission granted in the disposable test browser. Opening requested only
  view-only tickets, sent no takeover/manual-action writes, and left takeover state
  unchanged. Expanded layouts fit 1320, 1000, 800 and 390 CSS pixels. Actual resource
  previews, light mode and reduced-motion pane behavior were checked.
- Corrected a stale parallel status response that could overwrite the state after
  takeover, an old duplicate pane animation, routine spacing and the preview loading
  placeholder. Pane close/reopen cancels stale transitions; dialogs preserve a quick
  reopen. Routines refresh with workspace data rather than waiting for resource polls.
- Reboot uses the existing shared-VM idle, takeover and screen-lock gates. The live
  privileged helper and sudo rule were updated for the same pinned VM. A stubbed
  virsh test accepted reboot for its UUID and rejected an unsupported action and a
  mismatched UUID. No live VM reboot was performed for this release.
- Composio descriptions and navigation were exercised with provider fixtures; the
  existing project-key/provider-access limitation was not changed or bypassed.
- Live server reports 0.10.0. Deployment required an idle-run check and database
  backup. Linux SHA-256 matches the deployed binary:
  `2da87829ba4edaadae721ff5e6b8060344907e15aa1d71b9f409d978daae7a8a`.
- Signed 0.10.0 ZIP is 2,232,943 bytes, SHA-256
  `ed28d332959f402622b0a905e698a75ac07b53e311ef0a769c9d3efc8f4bca00`.
  Local, remote and signed payload hashes agree. The installed 0.8.0 updater's
  VerifyOnly path validated its signature, download, archive and package version.
  Served UI offers the update to desktop 0.8.0 and hides it for desktop 0.10.0.
  The installed app remains 0.8.0 until the user presses Update. No app restart
  was forced; the native update engine and persistent account data were preserved.


## Version 0.9.0 layout and character acceptance

- Rust: **51 passed, 0 failed**. Linux and Windows release builds passed.
- Edge and WebKit checks passed: newer signed release notification, equal-version
  suppression, tampered-signature rejection, right-anchored expansion to the left, aligned
  provider icon, ten-line natural growth and approximately twelve-line scroll cap,
  original sleeping silhouette, typing peek, model-button spacing, GPU dream and
  the animated public connection page. UI writes used fixtures, not user settings.
- Details bounds checked at 1320, 1000, 800 and 390 CSS pixels. All four settings
  sections checked at 1320, 800 and 390 pixels with no horizontal overflow. Rendered
  dark/light, composer, Details, settings and character screenshots were reviewed.
- Reviewed and corrected low-contrast dream artwork, a fast reverse twirl, and an
  initial broken-preview image. Loading/failure placeholders now cover unavailable
  computer images. Idle flourishes are suppressed inside the 3D loading turntable.
- Live server reports 0.9.0. Deployment required an idle-run check and database backup.
  Live, unmocked UI showed the update for desktop 0.8.0 and hid it for desktop 0.9.0.
  Public assets, including the official provider logos and release-check module, loaded.
- Signed 0.9.0 ZIP SHA-256 is
  `1cbe02390a1d614c9c28d383e0b2e5be690390118064591bcd398af650628c2f`.
  Local, remote and signed-manifest hashes agree. The actual installed 0.8.0 helper's
  VerifyOnly path accepted the signature, downloaded archive and package version.
  The installed app remains 0.8.0 until the user presses Update; no restart was forced.
- The native updater mechanism and backend approval policy are unchanged. Notification
  checks independently verify the same pinned RSA key with Web Crypto, enforce a
  bounded feed and compare numeric semantic versions. Failed/unavailable checks do
  not display a misleading update control.

## Version 0.8.0 preferences, artwork, and approvals

- Rust: **51 passed, 0 failed**. New checks cover inherited defaults, explicit bot
  overrides, live policy changes with a stale in-memory bot, required action scopes,
  missing/external scopes, and external connector gating under auto mode.
- Linux and Windows release builds passed. The server was deployed only after an
  idle-run check and SQLite backup. Live status reports 0.8.0; existing bots, their
  history, accounts, and credentials were preserved. Legacy broad VM approvals
  migrated to auto; no bot was migrated to full access.
- Edge and WebKit UI checks passed with isolated write fixtures: immediate themes,
  serialized rapid changes, inline failed-save state and retry, inherited/per-bot
  policy controls, character selection autosave, grouped Build preview, curved rest
  eyelids, adaptive white/black coloring, 3D turntable, and mobile sidebar placement.
  Read requests used the existing workspace; fixture writes did not change real
  user preferences or bot permission settings. Rendered screenshots were reviewed.
- Signed stable channel 0.8.0 is published. The installed 0.7.6 updater downloaded
  and verified its signature, archive contents and package version using VerifyOnly.
  Remote ZIP SHA-256 matches the signed manifest and local archive:
  `6a815004c0f8e276107307eada568767a986c31d321a86755b371cbfd7495d44`.
- The installed desktop remains 0.7.6 for the user to exercise Update Kindred.
  This release's helper/native update mechanism is unchanged from the actual
  0.7.5 -> 0.7.6 restart acceptance below; an automatic 0.8.0 restart was not forced.
- Routine VM versus external scope is model-classified for arbitrary computer and
  shell actions, not a network sandbox. Connector mutations are independently
  classified and retain their connection permission limits. See ARCHITECTURE.md.

## Version 0.7.6 desktop update acceptance

- Backend tests: **49 passed, 0 failed**. The new release-route test verifies public
  manifest access, no-store responses, rejected filenames/traversal, and continued
  authentication for workspace APIs. Linux and Windows release builds passed.
- Installed under the current user's LocalAppData Programs/Kindred directory and
  launched through the actual Start menu shortcut. The shortcut points to a stable
  launcher; a small current.json pointer selects an immutable version directory.
- The actual native Update Kindred button opened a rendered local update popout.
  Its up-to-date state displayed the installed version, and Close dismissed it.
- **A real signed update from 0.7.5 to 0.7.6 completed through the app button.**
  update-status.json reported complete at 2026-09-06T22:21:48Z. The restarted process
  ran from versions/0.7.6/Kindred.exe. The unsent test draft was visibly preserved,
  then cleared without being submitted. The prior installed version remains available.
- The production updater's verification path accepted the signed feed and exact
  downloaded archive. An unrelated public key caused signature rejection, and an
  unreachable HTTPS feed produced an error state. Both failure fixtures retained
  their original current.json version. Startup rollback is implemented but was not
  exercised by intentionally crashing the live app.
- Windows integration rehearsals caught and corrected PowerShell module lookup and
  WebView2 window-creation issues. The final native popout is created on a separate
  thread; updater commands accept only that local window. The remote workspace page
  can request the popout but cannot choose executable paths or updater arguments.
- No test chat message was sent. No provider credential, server token, or private
  signing key is included in release packages. Server-side tasks were idle at the
  separately guarded server deployments.

## Version 0.6 acceptance

- Rust suite: **48 passed, 0 failed**. Added checks cover per-screen leases, persistent
  takeover/recovery, per-bot atomic claims, real message-bound reactions, safe
  Composio denial diagnostics, and nonoverlapping VNC port blocks.
- Two live Codex Luna/Low runs completed on separate screens: A used `:2`
  (`deb73231-c26a-4021-8e0b-fedb709bc254`) and B used `:3`
  (`af902047-dcbb-493a-a257-1c701e5b27c2`). Their approved guest commands overlapped
  for eight seconds. Each verified a different local browser page and added a real
  thumbs-up reaction. The first rehearsal was stopped before approving a command
  that differed from the requested shell text; its cancellation hold was respected.
- Browser verification checked actual VNC frame pixels for both screens: the page
  backgrounds read `[217,233,250,255]` and `[215,239,228,255]`. A connected socket
  alone was not accepted as proof of a rendered desktop. Test bots were restored
  to their prior names, settings and archived state afterward.
- Live subscription usage was read from the guest account: ChatGPT Pro, the Codex
  weekly window and Spark's 5-hour/weekly windows. The main Codex 5-hour window was
  absent in this account response; the UI labels missing windows Not reported.
  No rate-limit reset was consumed.
- UI checks covered the usage hover flyout, outside-click dismissal, sticky settings
  close, inline credential errors, a list-first Skills editor, desktop/mobile layouts,
  the Marketplace SVG/empty state, direct-chat author grouping, and removal of
  zero-action summaries. Usage percentage fixtures and Skills edit fixtures were
  tested separately from live account/desktop checks. No JavaScript errors occurred.
- The real Settings computer screenshot and live metrics loaded. Browser network
  interruption recovered without a page reload. A forced restart of the second
  desktop service obtained a fresh VNC ticket and rendered frames automatically;
  the following 70-second idle hold had zero unexpected disconnects. The VM now has
  2 vCPUs and a 4 GiB RAM ceiling; three open desktops used about 1.4 GiB in the observed sample.
  The host service reported zero automatic restarts and about 15 MiB peak memory
  during this rehearsal. Heavy-workload capacity has not been established.
- No Composio key had been persisted after the user's rejected entry. The endpoint
  and authentication header match the official v3.1 API. Whitespace handling and
  failure diagnostics are fixed, but a successful live catalog/consent check still
  requires the user to enter an accepted project key in Connections. A subsequent
  user retry returned HTTP 401/code 801. A deliberately invalid diagnostic key
  reproduced code 801 with slug `APIKey_InvalidAPIKey`; Kindred now gives a specific
  full-key recovery message without exposing provider-returned key fragments.
  The user's supplied key itself was not read or retained after denial.



## Earlier version 0.5 acceptance (retained)

- Rust suite: **41 passed, 0 failed**. New coverage verifies additive thinking-level
  migration, reopen persistence, model-specific supported effort and no fallback.
  The OpenRouter mock verifies the chosen effort reaches each tool-loop request.
- Three live Codex runs completed with exact model/effort selections recorded:
  Luna / Low (`9117f425-5864-421d-93f4-c9684e1f0d05`),
  Spark / High (`a9834cc7-0479-45f5-bfc3-895e4b6343ac`), and
  Astra / Medium (`c9732004-c572-4f9f-9362-d7d1cda7601c`).
  All returned the requested `KINDRED_MODEL_CHECK` text without tool calls.
- Browser-created and edited bot settings persisted the chosen model and thinking level.
  Switching providers and back retained the unsaved Codex selection. The live OpenRouter
  catalog returned 363 tool-capable models. OpenRouter inference remains mock-tested only.
- Edge and WebKit verified loop phase continuity after DOM reinsertion, sleepy yawns/zzz,
  reduced-motion behavior, 16 swatches in exactly two rows, and 390-pixel light layouts.
  Working avatars measured 20 pixels and real running activity appeared in the sidebar.
- Browser checks verified that book details stay hidden during the early morph, build
  activity cycles hammer/saw/drill, five-minute inactivity sleeps, and active work stays awake.
  Sleep/yawn timing was advanced in the browser test rather than waiting several minutes.
- A simulated version mismatch displayed the reload indicator. Reload preserved an unsent
  composer draft and the selected bot. No message was submitted by this test.
- The user's already-open desktop window was observed running an old frontend despite
  the server reporting 0.4. Reloading that window exposed Marketplace, the compact
  computer panel and real CPU/memory/disk readings. Frontend version reporting now
  distinguishes the loaded UI from the server release.

The temporary model-check bot was archived. Piper's settings/history were preserved.
Live Composio catalog and consent still require a project key and user sign-in.

## Earlier version 0.4 acceptance (retained)


- Rust suite: **39 passed, 0 failed**, covering transactional history migration and rollback,
  multi-turn request/result continuations, mention routing, membership validation,
  whole-round cancellation, bounded collaboration, and previous provider/security checks.
- A live OpenAI subscription rehearsal used three bots for **seven completed turns**:
  Finch PM requested a subtotal from Moss Invoices, received it automatically,
  requested tax in a second exchange, then asked Reed Comms for a draft and returned
  the final result. **Three handoffs; zero failed tool results.** The result was
  $27.00 on a $25.00 subtotal with 8% tax. No email or other external message was sent.
- Edge and Playwright WebKit created a chat through the UI, routed a pasted
  `@Moss Invoices` mention to that bot alone, synchronized the result across clients,
  renamed the chat and observed the new name on the second client. Both use the
  real server and provider. No JavaScript errors were observed.
- The computer side panel is 350 pixels wide at a 1440-pixel viewport; the chat
  retains 830 pixels. VNC was checked for actual non-empty frame pixels, not merely
  a connected socket. Computer settings displayed a real screenshot and live metrics.
- Guest resource samples showed one vCPU, approximately 1.4 GiB usable memory,
  7.7 GiB filesystem capacity and 2.4 GiB used. CPU and memory usage are live samples,
  not reserved host resources. Resource polling and desktop viewing work together.
- The + menu, group member picker, inline avatar badges, desktop/mobile layouts,
  light/dark appearance, smaller working characters, and revised shapes were reviewed.
- Marketplace search, pagination, custom configuration selection, hosted link
  onboarding and connected-state UI passed **mocked-provider** browser acceptance.
  Rust mocks cover generic managed OAuth, API-key auth, custom OAuth bindings,
  credential-safe projections and case-insensitive auth schemes. No live Composio
  project key has been supplied, so live catalog/consent/third-party API success
  remains unverified.

Temporary rehearsal chats and bots were archived after acceptance. Existing Piper
history and account state were preserved.

## Earlier 0.3 evidence retained (not rerun in full)

- The 0.3 Rust suite had **29 passed, 0 failed**. Linux release build,
  formatting and JavaScript syntax checks passed.
- Cross-client acceptance used Edge and Playwright WebKit 26.5 on Windows against
  the actual Tailscale HTTPS server. A second client linked through a one-use URL
  without SSH or the permanent token. The URL fragment was cleared before consent;
  replay was rejected. Both clients could read the same workspace and see shared
  skill changes. Both watched the live computer, and the second took and returned
  control successfully. No JavaScript errors were observed.
- A task submitted by the first client completed after that client closed. The
  second client read the result from the server: `MULTI_DEVICE_OK`.
  Run: `d15a8fba-5a8d-4015-a37b-367fd3fb777f`.
- Opt-in remembered access survived a fresh WebKit browser context. Disconnect
  cleared both sessionStorage and localStorage credentials.
- Original body morphs were reviewed in the avatar picker: magnifier, hammer,
  wrench, pencil, book, envelope, clock, success, waiting, rest and worried states.
  WebKit rendered the morphs. Reduced motion suppresses animation. Desktop and
  390-pixel layouts were checked, including light and dark themes.
- All three Composio UI flows passed connect/check/test/disconnect acceptance with
  mocked provider responses. These are explicitly mock results, not Google consent.
- Live OpenAI subscription acceptance exercised **15 distinct common tool types**:
  apps_list, connectors_list, bots_list, skills_list, remember, skill_save,
  guest_exec, computer_open_url, computer_screenshot, computer_key, computer_type,
  computer_click, computer_scroll, routine_create and send_to_bot. There were
  **27 successful tool results and zero failed tool results**. The initial run hit
  the configured 24-action budget; it is correctly recorded as failed rather than
  claimed complete. A narrower follow-up completed the remaining checks.
  Runs: `9b28e24b-fdd5-488b-a137-aaed3653af4b`,
  `a6445eba-1ea3-482c-8695-c79e348f4440`.
  The recipient acknowledged its handoff in
  `4ffcdc25-9bc5-4bca-924d-0625fbf16805`.
- Test skills and routines were removed and temporary test bots archived. Existing
  Piper history and account state were preserved. No real email was sent.
- The test VM shut down gracefully and restarted using the actual app API.
  OpenAI account type remained `chatgpt`; screenshots returned and the exact file
  `/workspace/kindred-acceptance-v3/result.txt` content survived:
  `Kindred v3 build verified`. Takeover was returned to the bots afterward.
- The native Windows 0.3 client was built and opened through private HTTPS. A later
  automated native inspection was stopped by the user with Escape; no further
  native Computer Use was performed in that turn. Browser verification completed.

Current tests include configuration/TLS, HTTP authentication and origins, queue/cancellation,
approval single-use, restart recovery, routines, handoffs, provider tool contracts,
mocked OpenRouter privacy/error behavior, profile migration, VNC tickets, Composio
account/schema/version/policy boundaries, and expiring one-use device links.

## Previous live checks retained

Earlier 0.2 acceptance verified model-approved clicking/typing with fresh screenshots,
VNC watch/control and paste, persistent Chromium profiles, and server-enforced
view-only access even when the client changed its own viewOnly flag. Current tests
retain the corresponding server checks. Linux guest sign-in remains through the
installed official Codex app-server.

## Not yet verified

- Live Composio project authentication, Google OAuth consent, and real Gmail,
  Calendar or Drive API execution: a project key and user consent have not yet been
  supplied. Mock tests cannot establish that Google or a Workspace administrator
  permits the grant. See [connector setup](COMPOSIO.md).
- Live OpenRouter inference: this instance still has no OpenRouter key.
- A physical MacBook/Safari session or a signed native macOS application. WebKit
  on Windows is useful engine coverage, not a substitute for testing the actual Mac.
- Sustained heavy workloads, signed installers, a complete dependency audit,
  individual per-device account revocation, or full Grok Bot feature parity.

## Installed personal test service

The service runs on production-server at host loopback 7340 and is available privately at
[Kindred](https://kindred.example.com:9446/) through Tailscale Serve.
Other clients must be on the same permitted tailnet. This is not public internet
hosting. Full browser access includes chat, settings, approvals, connectors, skills,
routines, and the live shared VM. Each bot has a separate desktop and one task owner;
several bots can run concurrently. Closing a client does not stop the server or its tasks.

The `kindred-test` VM is Debian 13 with 2 vCPUs, 4096 MiB RAM and an 8 GiB thin
QCOW2 disk. Its autostart is disabled. Host service limits remain 128 MiB and 25% CPU.
Guest VNC uses loopback 5900 for control and 5901 for enforced view-only access.
The server database is `/var/lib/kindred/kindred.db`; configuration is
`/etc/kindred/kindred.toml`. Updates preserve online SQLite backups and previous
binaries/configuration under `/opt/kindred-test/backups`.

Packaged checksums are in the sibling `SHA256SUMS.txt`. Source and client archives
exclude provider keys, the server token, databases and local test credentials.


## 0.47.0 inbox monitors

All 144 backend tests pass, including persistent Gmail history and message deduplication, background run delivery, retry and pause behavior, authenticated Pub/Sub JWT verification, callback wakeups, and restricted custom Gmail OAuth scopes. The inbox monitor UI passes in Edge and WebKit with desktop dark and mobile light visual review; the existing user-flow suite also passes in Edge. Linux and Windows native release builds pass.

No real Gmail account is connected in the deployed workspace during verification, so live email delivery and Google Pub/Sub delivery remain unverified. The default mode checks Gmail history every 15 seconds without model calls when empty. Native push requires the documented Google Cloud and public HTTPS callback setup; setup alone does not prove delivery.


## 0.48.0 unified routines and guided inbox setup

All 147 backend tests pass. New integration checks cover a saved setup choice and its assigned continuation creating one Constant inbox routine, duplicate prevention, unified bot/API routine catalogues, missing and multiple Gmail account choices without premature reads or writes, and a custom answer saved as a real weekly schedule. Activity and timed triggers remain distinct.

The unified Routines UI passes in Edge and WebKit: scheduled and activity items together, Monitor activity creation, Constant frequency, pause/resume, native push details and missing-account setup. The existing decision-card and weekly-schedule suite passes in both engines. Desktop dark and mobile light layouts were visually reviewed. No live model conversation or real Gmail delivery was exercised; these checks use controlled integration fixtures.


## 0.48.1 conversation reliability and regression audit

Ordinary reloads now restore the selected direct or shared chat, unsent drafts and quoted replies. The resume record is limited to the browser tab and keyed by a SHA-256 digest of the session token; another session does not inherit it. Profile-switch restoration still takes precedence. Pending API fetches are cancelled on page exit. Mobile settings tabs wrap within the dialog, and interval labels use singular units correctly.

All 28 browser suites pass in Edge and WebKit (56 suite/engine combinations), including the new chat-reload regression: a second bot remains selected after reload, its draft survives, a shared quoted reply is restored and sent to the correct chat, and another token cannot recover those drafts. Mobile provider tests also verify that all five settings tabs fit and remain navigable. The history test now signals an upward reading gesture and waits for the actual restored scroll position; the teammate-draft test waits for the asynchronously populated dialog before checking its fields. These correct timing assumptions found during the broader run.

The complete 147-test backend suite passed with the pinned Pi SDK fixtures before this UI-only patch. Actual Windows native IPC tests passed for profile token isolation and protected storage, and for local access defaults, workspace limits, allow/deny, cancellation and disconnect handling. No native implementation changed.

An isolated full-server run exercised the real HTTP API, SQLite persistence, scheduler, Pi SDK and model tool loop with a deterministic local provider: one persisted DM reply after reload, a saved Not now answer followed by its continuation, one final scheduled report without intermediate chat commentary, a quiet routine with no chat message or notification, and a two-bot handoff with both results confined to its shared chat. No real model account or Gmail inbox was used in that test.

A read-only production database audit found zero duplicate result deliveries, wrong-chat results, missing terminal nonempty results, missing answered-question continuations or quiet-run messages. SQLite integrity and foreign-key checks passed. The audit covered 98 runs and 217 chat messages; it is a bounded persisted-state audit, not a guarantee of future delivery or external provider behavior.


## 0.48.2 durable message retry recovery

Chat sends accept an optional UUID `request_id`. The server commits its receipt, user message, uploads, quote and queued runs in one SQLite transaction. Repeating the same request returns the original run IDs; reusing its ID with a different payload is rejected. Failed transactions leave no reserved receipt. Existing clients without a request ID retain their existing API behavior.

The browser saves an uncertain send's ID before submitting it and reuses that ID when the unchanged message is retried. Its attachment references and quoted reply survive ordinary reloads, the web Update action, native update snapshots and profile draft transitions. A successful acknowledgement clears the pending send; a deliberate later message receives a fresh ID. A restored browser page refreshes its conversation, and page-exit cancellation no longer displays a request error. Pending uploads that have not finished are excluded from the saved attachment list.

All 151 backend tests pass with the pinned Pi SDK fixtures. Four new checks cover simultaneous authenticated HTTP retries, key/payload and chat mismatch rejection, restart persistence, failed-transaction rollback, bound uploads and quotes, and workspace transfer of send receipts. Exports include the new receipt table; both servers must use matching workspace schemas before transferring.

All 29 browser suites pass in Edge and WebKit (58 suite/engine combinations). The lost-response recovery suite also passes through the web Update action in both engines. It accepts the first send, drops the response, reloads, retries the same attachment and quoted reply with the same ID, and verifies one accepted message. It also verifies that successful sends clear their saved draft and a later intentional send gets a fresh ID.

An isolated 0.48.2 full-server test intentionally discarded an accepted HTTP response before reloading and retrying. The real SQLite database, scheduler and Pi SDK produced exactly one run and one final DM reply. Saved question continuations, final-only routine reporting, quiet routines and shared-chat bot handoffs also passed. Only the model responses were deterministic local fixtures; no production conversation, provider account or real Gmail inbox was used.


## 0.48.3 desktop connection recovery and usage clarity

A stale Windows taskbar pin was found pointing directly into the 0.24.0 version directory. The normal signed updater brought that installation forward, and its stable launcher restored the saved server connection. Version-specific shortcuts without custom connection arguments are now migrated during installation and launch. Stable launcher scripts are refreshed from the installed version. Future stale binaries forward to a newer current version, including manifests written with a Windows PowerShell UTF-8 BOM. Explicit custom connection shortcuts and other installations are preserved.

The native workspace window stays hidden until the loaded document identifies itself as the Kindred workspace or sign-in surface. A bundled connection screen offers retry, saved profiles and another server when loading does not complete within twelve seconds. Its public state contains the server and profile key, never the session token. A reload after a successful connection starts a new watchdog; redirects and automatic webview retries share the existing deadline. Closing the bundled screen during connection startup exits the pending application.

The Claude Code usage flyout has a compact account section, a direct provider usage link and one separate Kindred history section. Empty history is stated once. A failed quota request does not discard recorded usage. Subscription history sorts by tokens and omits API-cost controls and metrics. Account-wide quota values are never inferred from local request totals.

A bot-details control remains disabled until the conversation has loaded. The regression deliberately delays the initial bot response before checking it. Suspended pages reject new API requests, including asynchronous continuations that resume after pagehide, and refresh again on pageshow. A dedicated delayed-digest fixture verifies both lifecycle transitions in Edge and WebKit.

All 151 backend tests pass with the pinned Pi SDK fixtures. All 31 browser suites have passing Edge and WebKit results (62 combinations). The final broad run required a targeted rerun for a WebKit initial-navigation timeout; the profile fixture was also corrected to wait for actual conversation rendering before starting the next reload. Native Windows checks cover connection refusal, a non-Kindred proxy response, retry after recovery, failed reload recovery, protected saved sessions, cross-server token isolation, local access boundaries and cancellation. Shortcut tests use disposable installations and preserve unrelated links. Platform installer CI and published artifact status are recorded separately in release VERIFICATION.json.


## 0.48.4 Claude Code models and effort

Claude model discovery now uses the pinned official CLI's SDK initialize control response with no user turn. Kindred projects its model IDs, resolved versions, context variants, descriptions and supported effort levels. Verified resolved IDs are also offered as pinned choices; existing family aliases remain compatible with the CLI's extended-context entries. The catalogue preserves account-specific usage-credit notices and does not invent older models or infer access from API model lists.

The common model editor displays Claude's per-model thinking levels. Unsupported levels are rejected before launching a turn; selecting a different model resets effort to its default. Explicit Claude effort is passed using --effort and recorded in the selected-model event. Kimi keeps its default-only contract. The restricted tool set, MCP permissions and provider credential home are unchanged.

Five Linux bridge tests cover control-only catalogue discovery, resolved ID/context normalization, unsupported model/effort rejection, exact pinned-model/Maximum arguments, MCP denial and Kimi transport. The new browser suite covers saving pinned model and Maximum effort, model switches, draft preservation across providers and failed refreshes, default-only Haiku and usage-credit notices. It and both existing provider suites pass in Edge and WebKit. Live catalogue discovery was verified against installed Claude Code 2.1.263; no billable model prompt was submitted for these checks. All 151 backend tests pass with the pinned Pi SDK fixture runtime mounted. Release artifacts and rollout evidence are recorded in VERIFICATION.json.


## 0.48.5 routine avatars and spacing

Scheduled routines and inbox monitors show the assigned bot's existing character next to its name. Scheduled actions use consistent outlined buttons and align with monitor actions. Missing bot records keep the existing Assigned bot fallback.

The spacing pass covers settings copy/actions, marketplace empty and error states, device-link guidance, routine/paste submit buttons, provider account fields and actions, and inbox-monitor form fields. Grid-based forms no longer stack their gaps with global label margins. Shared computer actions wrap as one group. Text, controls and action rows have explicit separation; compact routine rows retain their own layout.

All 32 existing browser suites have passing Edge and WebKit results (64 combinations). A desktop and narrow-screen visual/geometry audit covers General, Connections, Routines, Skills, Computer and routine, monitor, skill and import dialogs. Nested modal closing now releases the underlying input immediately: the audit reproduced a visible settings button whose hit test returned the parent after delayed modal closing. The same click sequence passes with the fix, and the inbox-monitor regression now checks returning to Skills and opening its editor. Final focused checks and artifact/rollout evidence are recorded in release VERIFICATION.json. No backend or provider execution behavior changes in this release.


## 0.48.6 question handoff presence and activity labels

A completed question handoff no longer waits for a final result bubble that is intentionally suppressed. Empty and explicit quiet completions follow the same rule. The existing delayed-message protection remains for ordinary written results, and a successor already working for the same bot takes precedence over a departing avatar.

Activity follows recorded tool execution: local workflow scans, shell searches and supported connector searches say Searching; successful search returns say Reviewing search results. File reads, commands, app inspection and browser/computer actions have specific labels. Requested actions, approvals, failures and terminal states retain their distinct states. Activity responses expose only labels and timing, not command text or tool results.

All 152 backend tests pass, including an authenticated activity endpoint regression across tool requests, starts, successful results, failures and completion. Twelve browser suite/engine combinations pass across Edge and WebKit: question presence, chat transitions, conversation motion, decisions/schedules, public replies and chat history. The question fixture exercises the answered card with exactly one continuation avatar through polling, live label changes, mobile layout, empty and quiet completions. The chat transition fixture retains delayed-result arrival and reduced-motion checks. Signed package, live rollout and installed Windows upgrade evidence are recorded in release VERIFICATION.json.


## 0.48.7 live desktop-access diagnosis

Bots can always call local_access_status to inspect current global and per-bot settings, the selected desktop, its heartbeat and permission mode. The read-only tool and authenticated status endpoint return exact missing setup steps without granting access, queuing local operations or exposing device secrets. Every new turn includes a fresh status snapshot that supersedes older claims in chat. Bots are instructed to distinguish paired desktop access from connectors and VM tools, and to request a fresh turn if permissions changed after the available tools were selected.

General settings now explain that global access and native desktop permission do not enable every bot. Bot settings identify the missing desktop selection, bot toggle, global toggle, offline/disabled desktop and workspace-only limits. Selecting a desktop does not turn on a bot automatically.

All 154 backend tests pass, including disabled-tool status discovery, current-state reads with an older bot snapshot, zero local requests/approvals from diagnosis, denied file access, authenticated endpoint behavior, workspace/disabled/offline/unavailable device states and secret exclusion. Ten browser suite/engine combinations pass in Edge and WebKit across provider/local settings, profile settings, skill import, settings and user flows. Live rollout and installed signed Windows upgrade evidence are recorded in release VERIFICATION.json. Existing permission selections and conversation history are preserved.


## 0.48.10 live work, desktop routing and command feedback

All 163 backend tests pass with the pinned Pi SDK runtime. New coverage verifies same-task follow-up delivery, idempotent sends, retained ownership after restart, no automatic replay, queued late arrivals, progress reminders, quiet decision continuations, exact desktop selection, offline assignment, mandatory per-call routing for All, authenticated per-request progress, and single delivery to the selected desktop. Existing deferred-task tests now expect ordinary messages to join the active tool session. Five provider helper tests pass, including model-menu normalization with fixed selections retained.

Twenty browser suite/engine combinations pass across Edge and WebKit: live work, chat history, provider catalogue, account sessions, profiles, provider/local settings, profile settings, skill import, settings and user flows. Focused checks cover one active character, visible elapsed time, terminal morph, approval and failure states, follow-up delivery badges, an offline desktop retained while another is online, All saved explicitly, collapsed model variants, saved fixed models and frozen activity during a lost connection. The clock fixture installs its clock before page load so interval advancement is deterministic. Desktop dark/light and mobile captures were visually inspected.

An isolated real WebView2/native IPC fixture verifies permission defaults, workspace restrictions, explicit approval denial/allowance, permission-wait progress, multiline UTF-8 PowerShell, empty successful output, nonzero exits, bounded timeouts, cancellation, disconnect and stable desktop identity. It uses disposable processes/storage and restores its notification icon registration. Signed Windows upgrade, session continuity, deployment and artifact evidence are recorded in release VERIFICATION.json.

Live read-only WSL probes found a responsive version command and an independently stalled status command. This release improves transport, bounded failure information and diagnosis; WSL host recovery is not claimed. Model-written progress and quiet-decision behavior are instruction-guided. Follow-up delivery is guaranteed at completed tool boundaries, not during a blocked command or a tool-free final response. See LIVE_WORK.md for the full behavior and limits.


## 0.48.11 explicit WSL workflow scope

The shared operating guide (version 3) directs bots to clarify ambiguous WSL distributions and project roots using actual available names and relevant saved choices. A Windows default or home-only scan is not treated as proof of intent. Project-level and bounded nested-project discovery preserve their scope and limits; confirmed targets are reused without asking again. This is model guidance, not a guarantee of every provider response.

Windows local file and workflow tools now recognize only local WSL redirector hosts with a distribution registered to the current Windows user. General network hosts, unregistered WSL shares and device-path forms remain rejected. Existing workspace restrictions, outside-workspace approvals and link protections still apply. Discovery and bundles return reusable ordinary paths instead of internal Windows verbatim prefixes.

Backend regression tests, actual Windows WSL command/discovery/package-read checks, boundary checks, signed update verification and rollout evidence are recorded in release VERIFICATION.json. The native WSL test requires an explicitly supplied distribution and .claude root. It uses fixture accounts and permissions, reads the selected workflows without importing or executing them, and restores its own desktop registration state. Host WSL service recovery is separate from this product change.


## 0.48.12 source-linked workflow refresh

Local imports bind their actual desktop/path and a source baseline. The refresh tool lists all imported workflows, previews complete-package differences and applies reviewed updates using both source and current-copy hashes. Four new backend tests cover durable source links, external file changes, supporting assets, stable aliases/custom settings, frozen invocation receipts, unchanged retries, concurrent source/current edits, conflicts, explicit replacement, forged upload origin rejection, linking unchanged legacy copies, no silent source moves, actual local poll/receipt transport, missing source errors and offline-source behavior while another desktop is available.

A requested nonconflicting update proceeds without a second user decision per item. Conflicts preserve Kindred edits by default. Unlinked uploads and older imports require a verified source link; missing source workflows are retained. Updates do not execute scripts, create duplicates, change grants or schedule monitoring. Exact test counts, package, deployment and signed upgrade evidence are in release VERIFICATION.json.


## 0.48.13 persistent return-control notice

Two backend tests cover unknown historical flags, durable new cause/time/identity, invalid causes, workspace isolation, status listing of multiple paused bots, exact-screen return, stale-notice rejection, retries, preserved queued work and other holds. Browser regression covers a notice lasting beyond ordinary toast timeouts, dismiss without resume, retained chat action, view/rejoin without taking new control or cancelling a task, exact-bot return across navigation, persistent error/retry, reload and new-hold resurfacing. Dark/light desktop and narrow mobile renderings are inspected. Exact counts and signed upgrade evidence are in release VERIFICATION.json.

Live investigation found persisted manual-control flags but no historical acquisition audit. It cannot identify who or which earlier action created those flags. Deployment preserves existing queued work and manual holds. No queued user request is cancelled, completed or released merely to verify the change.


## 0.48.14 animation continuity polish

Working motion uses the task start rather than the changing step timer and keeps its current clock on same-state updates. A slightly gentler turn and a short settling transition preserve the existing character concept. The command monitor uses smooth output strokes and a softly pulsing cursor, with a quiet fade before the next loop and less body movement. Reduced motion remains static.

The focused browser regression exercises real activity refreshes without replacing the worker or changing its phase, same-state pose continuity, clearing a working tilt before tool motion, smooth command stroke progression and reduced motion. Existing face geometry, conversation transitions and live-work suites run in Edge and WebKit. This release changes presentation only; task execution, control ownership, local permissions and provider behavior are unchanged. Exact counts and signed update evidence are in release VERIFICATION.json.


## 0.48.16 chat status notices

Failed, cancelled and interrupted tasks appear as muted, labelled status text without bot avatars, reply bubbles or message reactions. Existing attachments and task activity remain available. Status metadata travels with paginated history so older failures do not depend on the recent-run list. Provider diagnostics retain their explicit CLI error marker; genuine replies before an error remain normal messages. An exact compatibility match recognizes the older Claude OAuth refresh diagnostic only on a failed Claude task's final assistant event, using its recorded provider even if the bot later changes providers. Stored messages and events are unchanged by this read projection.

Verification covers terminal outcomes, legitimate error discussion, explicit diagnostics, the older OAuth notice, history pagination, plain-text rendering, dark/light themes and narrow screens. The provider protocol fixture checks both error markers and preserves ordinary text/tool-denial behavior. Signed Windows update and native session evidence, release hashes and exact test counts are in VERIFICATION.json.


## 0.48.17 quiet send flow

The first message's brief scheduler hop no longer displays a waiting label or counts as a queued message. The arriving bot stays in place as actual work starts. Messages behind an active task, additional queued messages and manual control holds still show their real queue state. A single task still waiting after ten seconds gets a clear waiting-to-start label; stale connection feedback remains visible. This is a presentation change; scheduling, provider execution and control ownership are unchanged.

The focused browser test submits real messages against an isolated fixture, checks the quiet startup and retained avatar, then verifies active-task backlog, two queued messages, a delayed start and manual control. Existing conversation motion, live-work and control-notice suites run in Edge and WebKit. Exact counts, native signed-update evidence and release hashes are in VERIFICATION.json.


## 0.48.18 restrained response presentation

Shared guide version 5 adds two short presentation sentences to the always-included core prompt: avoid emoji headings, emoji bullets, decorative icons and excessive bolding unless requested, and prefer plain paragraphs with structure only when it helps readability. Both full and compact prompt tiers receive the same rule across providers. The application does not rewrite model output or modify existing messages.

Existing instruction-assembly tests verify that both prompt tiers include the core contract. Backend and provider-helper regression results, signed native session/upgrade checks and release hashes are recorded in VERIFICATION.json. UI rendering and provider execution code are unchanged; no new browser suite was needed and no paid model turn was started to verify a style preference. Model adherence remains model-dependent.


## 0.48.19 update saved routines through chat

The new routine_update tool edits an existing scheduled routine by exact ID under the normal approval policy. It checks bot ownership and patches only supplied fields in a transaction. Instruction-only changes preserve timing, next run, enabled state and unrelated instructions. Timing changes or resuming a paused routine calculate the next run; repeating the same timing does not postpone it. Optional expected_prompt detects intervening edits. No new routine or task is created by an edit, and existing run history remains unchanged. Duplicate creation now points to the update tool and the existing ID. Shared guide version 6 directs bots to save requested routine edits instead of substituting memory. Constant activity routines retain their existing inbox_monitor_save path.

An authenticated PATCH endpoint uses the same update implementation. Tests cover the actual tool/listing path, the next scheduled check using revised instructions, stable IDs and timestamps, paused-state preservation, repeated updates, timing validation, unknown IDs, cross-bot rejection, stale edits, normal approval/denial, HTTP authentication and persistence after database reopen. No UI rendering change or live model run is needed to verify this path. Release verification records the authorized correction to the existing daily Yankees routine separately from deployment.


## 0.48.20 live product review and workflow repairs

This release follows genuine, authenticated client tests with temporary bots using Claude Haiku, Codex Luna/low, and GLM-5.3-Flash through OpenRouter and the user's custom provider. The review reproduced Claude OAuth lock failures, missing routine lifecycle tools and one-time reminders, absent generated-file downloads, ambiguous successful command results, and queued follow-ups starting after cancellation. Exact live evidence and final release checks accompany the release record; live mailbox proof depends on a connected Gmail account.

Claude model discovery now closes stdin and allows graceful process cleanup before bounded termination. An official reconnect recovered the expired session and real Haiku tools succeeded. Connection status describes saved sign-in accurately; it does not claim a quota/live inference check. Command receipts now expose exit code, elapsed time and stdout/stderr, including explicit empty-output success. The guide clarifies approval completion and directs bots to use the normal tool gate without redundant choice cards.

Bots can pause, resume, run and remove their own scheduled and inbox routines through routine_control, under normal approval. Instruction edits preserve paused monitors and detector cursors. Run-now calls deduplicate within a task and reuse an active check. Scheduled one-time reminders use a persisted run_at timestamp and are consumed atomically when queued, waiting while the bot is busy and surviving restart without a repeat. Removal preserves task history and cancels queued checks; an already running check remains explicitly separate. The editor supports Once and patches existing fields rather than replacing the full definition.

share_file delivers verified regular files under /workspace as immutable chat downloads, with an 8 MB limit, SHA-256, same-task content deduplication, authenticated download, forced attachment disposition and no MIME sniffing. Export rejects symlinks, traversal, directories and non-regular files. Files persist across restart and workspace transfer. The live synthetic findings test ignored an embedded instruction, produced the correct severity counts and verified its report; post-deployment checks validate download bytes.

Chat now exposes Stop task and Continue task. Stop also cancels ordinary follow-ups waiting to join that active task, while independent commands, routines, decisions and other chats remain untouched. Continuation directs the bot to inspect completed effects and preserve denials; retries after uncertain responses use the same durable message receipt, including across reload. Tests cover these semantics, file persistence and authentication, command success/failure/timeout receipts, routine ownership/approval/timing, and Edge/WebKit interaction and mobile layout. Existing history and historical failures remain unchanged.


Follow-up live checks found that the official Claude CLI can finish a text reply without its Kindred MCP server connected. The provider handshake now requires the actual init event to report the Kindred server connected and every requested tool present. Actions and ordinary replies cannot pass an unverified handshake. A failed bridge gets a distinct connection error; fabricated tool JSON is not accepted as a tool execution. Fixtures exercise failed, missing and incomplete tool catalogues and denial transport. The live guest also had recorded kernel OOM kills before and during review; its existing disk/profile data was preserved through a graceful capacity increase from 4 GB / 2 vCPU to 8 GB / 4 vCPU. Deployment preserves the guest environment launcher and verifies a real guest RPC completion before starting the server.


## 0.48.24 — Dictation and conversation polish (2026-09-09)

- 207 backend tests passed with the pinned Pi Node runtime and SDK dependencies.
  The dictation HTTP tests verify authentication, model/audio validation, duration
  bounds, and that transcription never queues a bot task. Existing attention tests
  also verify the first unread message cursor.
- Edge and WebKit passed dictation controls, unread-divider placement/fading,
  manual-control notices, send recovery, and conversation reload/draft preservation.
  Edge used a synthetic microphone stream; no ambient microphone was recorded.
- Real native WebView2 IPC downloaded and transcribed the public JFK sample with
  quantized Whisper Base, Small, and Medium. The final runtime's smaller library
  extraction passed a further Base transcription, cancellation, disable, and
  forced-app-close check. Audio uses pipes and is not written to temporary files.
- Local model hashes and download sizes are pinned. The Windows x64 CPU runtime
  requires no GPU. Local dictation is not currently available on other platforms.
- The live server returned all three selected OpenRouter transcription models.
  Its existing zero-data-retention policy remains enabled, so no recording was
  submitted externally and external transcription is unavailable under that policy.
- Konsole launched from the normal VM dock on the temporary bot's own screen.
  Its process used about 74 MiB RSS while idle; Debian reported 328 MB additional
  package disk space. No full desktop environment or recommended packages were installed.
- The live screen test confirmed that taking control does not show the control
  toast, clicking away does, and returning to the pane hides it. The temporary bot
  and chat were archived, its screen was released, and other bots, chats, routines,
  control holds, and queued work were preserved.
- The actual signed Windows 0.48.21 to 0.48.24 update passed in an isolated install,
  preserving profiles and repairing its stale pinned shortcut. The native session
  regression also passed with zero new login requests. The user's running install
  was not replaced by either test.
- RSA signature, ZIP integrity, embedded server and native executable identity,
  and exact public feed/archive bytes were verified.

Deployment backup: `/opt/kindred-test/backups/20260909T232659Z-v80`.
Linux server SHA-256: `41863a99fb4b2cf58d4dab6d85ea833d08d72b80b40a8c45a8152aa032677d8f`.
Hosted CI outcomes and final source identities are recorded in the release's
VERIFICATION.json. Independent local tests are separate from hosted CI results.


## Settings, connector permissions, and pasted images (0.48.25)

General now groups Account, Appearance, System, Bot, and Dictation settings. Follow
System responds to OS theme changes. Microphone selection stays on the device;
hardware acceleration is a Windows preference that applies after restarting.
Computer lists saved local desktops and their execution permissions, while Bot
Computer contains the shared VM preview, resources, and controls. Renaming a saved
computer does not change its pairing or permission mode. Remote pages cannot grant
native execution access; changes must be made from that desktop.

The profile timezone is initialized from the opening device, with automatic and
fixed options. It supplies bot context, displayed times, new schedule defaults,
and the TZ environment of guest commands. Existing schedules retain their saved
zone. Local schedule conversion rejects missing or ambiguous daylight-saving times.

Claude connector controls appear only for Claude bots. Off means approval required
for each action, including in Full access mode. Previously an untouched grant could
inherit Full access; the absent-grant path now requires approval. Connected accounts
are described separately from execution permission. Existing account, bot, source,
read-only, and forced-approval boundaries remain enforced.

Pasting a raster image attaches it to the current draft with a thumbnail. It does
not send the message. Pending and sent images survive reloads, can be enlarged,
and remain bound to their original chat. The existing five-file and 8 MB limits
apply. Previews use authenticated requests without placing credentials in URLs.

Verification before release:

- 212 backend tests passed, including new timezone/DST, rename boundary, image
  persistence, and untouched connector approval tests.
- Edge and WebKit passed settings grouping, OS theme changes, microphone selection,
  download progress, saved computers, rename, provider changes and late responses,
  image paste/reload/enlargement, mobile layout, and existing send/reload regressions.
- Real Windows IPC verified acceleration off/restart/on/restart against actual
  WebView process arguments. Saved sign-in survived with zero login requests.
- The new native binary transcribed the public JFK sample with Whisper Base and
  passed cancellation, disable, worker cleanup on app close, and invalid input checks.

Signed update, deployment readbacks, hosted CI, and artifact hashes are recorded in
the release VERIFICATION.json. Test installs use isolated storage and processes.


## Windows setup EXE added to 0.48.25

A per-user NSIS setup EXE now wraps the unchanged signed 0.48.25 Windows ZIP.
It uses the same versions directory, stable launcher, and profile storage as the
in-app updater. No server or desktop runtime code changed for this installer.

- Real setup tests passed for a fresh install, repeated install, and upgrade from
  the signed 0.48.24 ZIP layout. Existing profiles and server settings were preserved.
- An active native Kindred process blocks setup until closed, including when the
  installer runs as a 32-bit process and Kindred is 64-bit. No process is force-closed.
- Downgrades, conflicting same-version files, and a corrupted signed package were
  rejected. Uninstall retained profiles, local workspace files, and local server data.
- The interactive welcome/finish flow passed, with Open Kindred selected by default.
- WebView2 dependency detection ran on a machine with the runtime installed. The
  bundled Microsoft bootstrapper signature was verified; installing the runtime on
  a machine without it was not exercised. No shared WebView2 runtime was removed.
- The setup EXE is not Windows publisher-signed. Its embedded Kindred payload is
  authenticated by the existing release key, and the WebView2 bootstrapper carries
  Microsoft's Authenticode signature.

The Windows CI job now produces the client binary for the signed release pipeline,
not a generic NSIS installer with a different installation layout. Hosted CI remains
subject to the existing account billing/spending-limit block. Local setup evidence,
package hashes, and the packaging source identity are included with the release.


## 0.48.26 — Local GPU dictation and composer/settings polish (2026-09-10)

Dictation is local only. OpenRouter speech routes and choices were removed.
Base, Small, Medium, Large v3 Turbo, and Large v3 use explicit + downloads;
selecting a downloaded model loads a resident worker. The loaded entry has a
checkmark and bold text. A 9px GPU/CPU indicator reflects the worker's actual
backend and expands to the device or fallback reason. Live transcriptions update
the current draft without sending it or adding an initial blank line.

The Windows runtime bundles Vulkan and CPU workers built from pinned Whisper and
Khronos sources. GPU kernel preparation happens before Ready. macOS builds use
the same resident protocol with Metal and CPU workers. Mac and other-vendor GPU
hardware are not claimed as tested by the Windows checks below.

Desktop permissions now open inside Computer settings with Back to Computer.
Its trusted native child webview retains permission-write authority; hosted content
cannot grant access, and this child cannot approve operations or change hardware
settings. Main-window IPC continues to work while the child exists. Settings
dropdowns use themed menus with pointer and keyboard controls.

Long user and assistant bubbles cross the conversation's center. Composer images
are 76px squares with hover remove/magnify controls and full-size previews. The
microphone keeps its circular background and shares a stable row with recording
controls, without a background-color flash when Send appears.

Verification:

- All 208 backend tests passed. The former four cloud speech tests were removed
  with their API implementation. Eight native Windows unit tests passed.
- Eighteen Edge/WebKit checks passed, including live dictation, model inventory,
  explicit downloads, loaded selection, CPU/GPU indicators, no cloud speech calls,
  no initial blank line, cancellation, image drafts/enlargement, overlap widths,
  themed pointer/keyboard selection, settings navigation, and mobile bounds.
- Actual WebView2/native IPC transcribed the public JFK sample on an NVIDIA
  GeForce RTX 4080 SUPER with Base, Large v3 Turbo, and Large v3. The Large models
  decoded the clip in 295ms and 423ms in the final native run. The process stayed
  resident between requests. Synthetic microphone words appeared before Stop.
- A separate native run with no compatible Vulkan driver selected CPU, produced
  a live transcript, and passed the same cancellation and cleanup checks.
- Explicit download cancellation, invalid audio rejection, disabling/unloading,
  and worker cleanup on app close passed. Audio was never persisted to files.
- The actual trusted embedded permissions page passed save/readback, Back/Close
  cleanup, hosted-content denial, child-command denial, navigation restrictions,
  and parent IPC checks.
- Setup passed fresh/repeat installation and 0.48.25 ZIP-layout upgrade with
  profiles/settings preserved. Active-app, downgrade, conflicting-version and
  tampered-payload protections passed; uninstall retained user data.
- The signed ZIP was verified with the existing public key; its native executable
  and standalone server exactly matched the tested final build artifacts.

The native microphone fixture also caught a picker closing during automatic
scroll after reopening settings. The picker now repositions on scroll, and the
native model-switching run and browser regression passed after that correction.
The actual user's running 0.48.25 app was preserved throughout fixture tests.

## Unreleased — Planning recovery hardening (2026-09-10)

Read-only browser interaction with the hosted 0.48.33 app reproduced an overview
that stayed on a transient error after successful planning responses resumed.
An isolated browser route supplied the single failed read; the live server and
its records were unchanged. All live API writes were intercepted.

The updated UI retains last-known lists during read failures, offers Retry,
recovers when identical data returns, restores failed checkbox changes, preserves
confirmed saves when conversation refresh fails, shows save errors inside open
dialogs, and stops polling if the dialog closes during its initial request.

- Five recovery scenarios passed in Edge and WebKit, including failed editor
  saves retaining their drafts. The original four failure scenarios were first
  reproduced against the unchanged UI.
- The existing planning suite passed in both engines: checklist progression,
  revision conflicts, idempotent work-request retry, list editing, reminder
  editing/cancellation, delayed delivery labels, source links and responsive
  light/dark layouts.
- Screenshot review covered the narrow-screen read-failure state, checkbox
  recovery, an editor retaining an unsaved draft, and the live recovery view.
- The local app.js overlay against the live server passed the same transient
  read-failure check without writing live data or deploying UI assets.
- Before this UI-only change, formatting, all 237 backend tests and the backend
  release build passed in an isolated local build container. No Rust behavior,
  version, release package, deployed asset or update feed changed in this lane.

Run `tools/frontend/test-planning-recovery.cjs` and `test-planning.cjs` with
`KINDRED_PLAYWRIGHT_MODULE` set to the local Playwright module. Set `WEBKIT=1`
for WebKit. These browser fixtures do not execute real bot work or send messages
to connected services. No GitHub workflow or publication was requested.

## Unreleased — Concurrent commands and task monitoring (2026-09-10)

Read-only inspection of the live workspace found no active tasks, one enabled
scheduled routine whose previous run completed, and an offline paired desktop.
The Routines UI opened without page errors. Existing Codex/Node/Brave shell
processes were identified by ownership and left untouched; they were not reported
as Kindred commands. All API writes from the live browser audit were intercepted.

Two defects were reproduced and fixed: the UI announced "Task stopped" while the
server was still cancelling, and native command capture silently clipped large
output. Stop requests now remain pending through a failed status refresh, while
command results explicitly distinguish partial output from the exit outcome.

- All 19 Windows native unit tests passed using a locally cross-compiled GNU
  test binary. Two actual PowerShell commands ran concurrently in isolated test
  directories. Cancelling one stopped its parent and child while the independent
  command finished successfully. Timeout retained partial output and stopped
  descendants; successful parent exit also cleaned up its background child.
  Already-cancelled commands did not start. Truncation flags, bounded capture and
  exit code 7 were checked with output exceeding both stream limits.
- All 238 backend tests passed. The added desktop-queue test verifies independent
  desktops, serialization on a busy desktop, cancellation without cross-device
  effects, and rejection of a late cancelled receipt. Formatting and the backend
  release build passed in an isolated local build container.
- Six browser suites passed: concurrent tasks, live work indicators, and task
  recovery/file downloads, each in Edge and WebKit. They cover precise stop
  targeting, pending cancellation, refresh failure, timers, connection loss,
  follow-up delivery, saved desktop selection, authenticated downloads and
  idempotent continuation across reload. The recovery test now uses the current
  file card's Download button. Screenshots were reviewed at mobile width.

These checks do not enable simultaneous bridge operations on one paired desktop,
execute commands in a user's real Kindred workspace, or prove native macOS/Linux
process cleanup. No version changed, no service was deployed, and no GitHub build,
source push, release publication or update-feed promotion was performed.

## 0.48.35 conversation and Linux viewer candidate — 2026-09-11

- All 262 backend tests pass on the final 0.48.35 source, followed by a successful
  optimized Linux server build. New cases cover named routing, concurrent group
  tasks, stopping descendants while preserving independent work, empty-group
  discovery, idempotent posts, membership revocation, durable shared history,
  long Unicode message retrieval and delivery to already-working teammates.
- Steering tests prove default queuing, authenticated explicit selection, same-bot
  and same-chat scope, single delivery, preserved attachments/quotes, and queued
  execution when the selected task finishes before another tool boundary.
  Instruction-edit tests prove one-time review in Full access, denial, stale-edit
  rejection, cancellation and preservation of all other bot fields.
- Seven browser suites pass in both Edge and WebKit: team-chat-controls,
  connector-artifacts, connector-edit-workflows, connector-catalog,
  concurrent-work, collaboration-notifications and live-work. They include
  15-record expansion, content scroll bounds, review controls, Markdown handoffs,
  explicit steering, hover/focus stop controls, independent work, saved edits,
  delayed or failed refreshes, desktop layout and mobile overflow checks.
- A fresh isolated profile VM was provisioned with two bot screens. The old
  viewer SSH command reproduced `Could not resolve hostname kindred-guest`.
  The corrected command uses the same profile SSH configuration and pinned host
  key as guest tools. Real WebSocket VNC sessions succeeded from the bot view and
  Bot Computer settings in Edge and WebKit against the final 0.48.35 server.
  The existing tested native Linux client also showed both live screens using
  those entry points under Debian 13, Xvfb and Openbox. It required no desktop
  takeover. This is client/server compatibility proof, not acceptance of the
  forthcoming 0.48.35 AppImage or DEB packages.
- Six release-gate tests, JavaScript syntax checks and whitespace checks pass.
  The standalone bundle contains the optimized server and has been verified as
  a valid credential-free ZIP. Platform packaging, installer acceptance and the
  complete-platform publication gate still belong to the separately approved
  manual build. No real provider messages, connector writes or user workspaces
  were used for these tests.

### Linux notification follow-up (unreleased, 2026-09-11)

- The owner reported missing notifications with the published 0.48.34 AppImage
  on KDE. Its native notification implementation matches the initially prepared
  0.48.35 source and does not suppress delivery for a focused window.
- Linux notifications now supply `desktop-entry=Kindred`, matching
  `Kindred.desktop`, and advertise the standard default click action. Desktop
  Settings > General exposes the existing native delivery status and test command.
  A successful test reports system acceptance, not proof that a popup was visible.
  The desktop entry must be installed for the shell to resolve its identity;
  this change does not install menu entries for arbitrary raw AppImage launches.
- `cargo fmt --check` and `cargo check --locked` pass for the Linux desktop in
  an isolated Debian builder (two pre-existing unused-item warnings). The focused
  notification-settings suite passes in Edge and WebKit: delivery errors, failed
  test/recovery, reconnection status and no preference writes. Syntax and
  whitespace checks also pass.
- This is not a reproduction or verified resolution on the owner's KDE session.
  Popup/history behavior and Do Not Disturb remain to be checked there. No build
  was dispatched to GitHub, and no package or installed application was changed.
  The earlier standalone bundle and exported sources must be refreshed before
  packaging these follow-up changes.

### Sound, settings and VM maintenance (unreleased, 2026-09-11)

- Added an original 210 ms two-note notification cue with a softer lower pitch,
  generated deterministically by `tools/generate-notification-sound.py`. Browser
  delivery suppresses the default tone, reuses decoded audio, and limits bursts
  to one cue per 750 ms after browser audio is unlocked. Native Linux uses the
  standard sound-file hint; macOS uses the application's sound file.
- Windows sends a silent toast and plays the cue through the system sound
  session only when notifications, application sound settings, shell state and
  `ToastNotificationManagerForUser.NotificationMode` permit it. Versions without
  the NotificationMode API remain silent. This follows the documented
  [notification-mode API](https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.toastnotificationmanagerforuser.notificationmode)
  and avoids unsupported file paths in
  [Windows custom toast audio](https://learn.microsoft.com/en-us/windows/apps/design/shell/tiles-and-notifications/custom-audio-on-toasts).
  The Windows module passes a GNU-target compile check, and a read-only query
  confirmed NotificationMode is available on the local Windows system. This is
  not Windows audio-output, Do Not Disturb toggle, or macOS runtime acceptance.
- Removed routine autosave, model, thinking, approval and paired-desktop helper
  text from Bot Details/Settings. Save failures, unavailable models and missing
  permissions remain visible. Connector status and permission sit together;
  refresh is in the heading and email controls align with their connection.
- Added default-enabled, VM-owned package maintenance, described in
  [VM maintenance](VM_MAINTENANCE.md). It waits for downtime and the running guest,
  reserves every screen and provider connection access, dispatches one durable
  systemd job, and never requests a reboot. New work queues until completion.
  The three-day interval covers attempted/failed/uncertain jobs; an explicitly
  deferred startup is not counted as a package attempt. Restart recovery retains
  reservations before clients or the scheduler can use the VM. An offline VM
  can still be started to resume verification.
- Eight focused server tests cover idle/interval gates, competing screens and
  connection operations, queued work, manual control, disable/transfer behavior,
  startup reservations, authenticated preferences, the public WAV, startup
  deferrals and completed guest responses. Eight guest tests cover deduplication,
  cooldown, interrupted jobs, active-service reconciliation, package order,
  failures, partial upgrades, configuration preservation and restart policy.
- The full backend suite passes: **270 tests** with the pinned Pi test runtime.
  The first broad run lacked that runtime and failed ten harness-dependent tests;
  it was corrected before the full successful run. The focused browser suites
  pass in Edge and WebKit: settings polish, Codex connector preferences,
  connector preferences, notification settings, collaboration notifications with
  custom audio, and maintenance controls/status (12 suite/engine combinations).
  Connector and narrow-screen layouts were visually inspected. Linux desktop
  `cargo check --locked` and the two notification unit tests pass; JavaScript
  syntax, sound regeneration and whitespace checks pass.
- On an isolated Debian 13 VM, the coordinator automatically dispatched job
  `6e2f6913-8ca7-48c9-a9b9-48e54c3d7050` after the guest idle window. Both real apt
  commands completed successfully; the already-current VM required no package
  changes. Its boot ID remained unchanged. Restarting the QA server before
  acknowledgment initially exposed rejection of an empty `error` on a successful
  guest response; that was fixed and regression-tested. The corrected server
  recovered the same completed job. Repeating its ID returned its existing
  result; a different ID was deferred exactly three days from the original
  attempt. The QA clock was seeded from the measured time since this otherwise
  idle VM booted; the 15-minute guest gate was not bypassed. This demonstrates a
  real no-op package upgrade and restart reconciliation, not a package-changing
  kernel upgrade. Partial-install and reboot-reminder behavior have unit coverage.
- The isolated QA VM and servers were stopped after verification. No user VM,
  host package installation, provider account or external connector was involved.
  Source is unreleased. No GitHub build, tag, release asset or stable-feed change
  was made. The earlier bundle and exported repositories remain stale and must
  be refreshed for an explicitly approved complete-platform build/release.

### Recovery and notification fault tests (unreleased, 2026-09-11)

- Recovery now requires the result to match the reserved job ID. Missing or
  malformed IDs, timestamps, errors and reboot flags cannot release a reservation
  or turn an older successful job into a new success. Unknown dispatches are
  reconciled under the guest dispatch lock and receive a durable rejection record
  in `maintenance/cancelled`; delayed requests with those IDs cannot start later.
  Interrupted starting states are persisted as terminal before releasing control,
  and workers acquire the same dispatch lock before entering their updating state.
- Server restarts reset unobserved idle time. Confirmed startup deferrals retain
  an existing reboot recommendation. These changes retain the three-day interval,
  no automatic reboot, and the existing disable/transfer boundaries.
- Browser delivery advances its cursor after each accepted notification, avoiding
  duplicates when a later item fails. Failed constructors release their portrait
  blobs. Permission and bot mute settings are rechecked after portrait fetching
  and audio decoding. Audio failures and autoplay blocking preserve the message.
- The full backend suite passes **273 tests**. All **14 guest tests pass on Linux**,
  including actual flock contention, dispatch timeouts, delayed worker rejection,
  stale completion, persistent cancellation and sanitized launch failures. The
  lock test is intentionally skipped by the Windows-only Python test run.
- Notification fault and existing collaboration-notification suites pass in Edge
  and WebKit. Coverage includes burst coalescing, a failed second delivery without
  repeating the first, sound download failure/recovery, muting during decode,
  permission revocation during portrait loading, blob cleanup and blocked audio.
- An isolated Debian VM and fresh QA server were seeded with an unknown dispatch
  ID while the guest retained its earlier completed job. The real SSH/helper path
  reported the new attempt as failed, preserved the older job, and rejected a
  delayed start with the cancelled ID. No new package operation was started.
  This was deliberate fault injection, not an observed production network failure.
  The test server and VM were stopped afterward. Native code and release artifacts
  were unchanged; platform-package acceptance remains pending.

### Closed-request replay and stalled portraits (unreleased, 2026-09-11)

- Regression tests reproduced late dispatch after a startup deferral, missing
  persistent state after a low-disk failure, replay of an older completed job
  after a later job replaced it, and an unfenced request when recovery adopted
  a different active job. Guest result records now close all of these requests
  while preserving the active job and the three-day attempt interval.
- Cooldown responses after a failed attempt now clear the old error from the
  deferral response so the server accepts it. Archived results clear reboot
  hints after a confirmed new boot, and legacy cancellation records still work.
- A browser regression reproduced an indefinitely stalled portrait blocking its
  notification. Portrait requests now have a three-second deadline, including
  reading their body; timeout falls back to the default icon and preserves the
  notification cursor. The new stalled-request case passes in Edge and WebKit,
  alongside the existing notification fault and collaboration-notification tests.
- The backend suite passes **273 tests** and the guest suite passes **22 tests
  on Linux**, including real flock contention. Windows runs the same guest suite
  with the Linux lock test explicitly skipped. Package and service operations
  are mocked in these guest tests; this round did not start a VM or install any
  packages. No native package build or publication was performed, and platform
  package acceptance remains pending.

### Teammate delivery and cancellation recovery (unreleased, 2026-09-11)

- Fault tests reproduced Stop losing to a ready provider response, a shared-chat
  handoff disappearing between durable completion and delivery, partially queued
  recipients when a conversation reached its turn limit, and archiving bots with
  unfinished work or enabled routines. The earlier code fails these regressions.
- Completion now saves pending delivery atomically with its terminal result.
  Restart and scheduler recovery consume that record with an idempotent receipt;
  a failed result write rolls back delivery and remains retryable. Existing
  historical messages without pending delivery are not replayed as new work.
  Pending delivery stays frozen during workspace transfer, travels with the
  exported workspace, and resumes once at the destination or after cancellation
  of the transfer. Stop before delivery prevents new teammate work while retaining
  results that had already completed.
- Stop remains authoritative when a provider reply becomes ready or completion
  reaches SQLite late. Group recipient dispatch rolls back together if a limit
  prevents full dispatch, retaining the visible pause notice. Archiving requires
  tasks to finish or stop and routines to be paused; legacy queued work cannot be
  claimed while its bot is archived. Restoring the bot preserves its history.
- The full backend suite passes **289 tests**, including 16 new regressions.
  A workflow runs the actual bundled Pi/Node bridge against scripted loopback
  provider responses: one bot requests two helpers, one helper waits for a user
  decision, the other receives HTTP 404, and the database restarts before the
  decision is answered. Repeating the answer creates one continuation, the saved
  preference survives another restart, and the requester receives both outcomes
  in its original chat. Five task runs make seven provider requests. This verifies
  tool execution and recovery with controlled responses, not live model judgment,
  real provider availability, or VM actions.
- Six browser suites pass in both Edge and WebKit: group sequences, team-chat
  controls, send recovery, concurrent work, task/file recovery, and chat reload.
  Send recovery also passes update-mode checks in both engines: **14 passing
  suite/engine combinations**. The initial task/file recovery runs exposed a
  test clicking a hover-hidden button; the fixture now hovers and waits for the
  control before clicking, and both reruns pass. Desktop and narrow-screen
  screenshots were inspected. No product UI code changed in this round.
- Verification used disposable test containers and local browser fixtures. No
  user VM, provider account, external connector, native package build or release
  was operated. Source remains unreleased; native package acceptance is pending.

### Connector stacks and isolated delivery faults (unreleased, 2026-09-11)

- Completed connector receipts now stack within consecutive activity from the
  same bot and task. The collapsed summary retains the apps, bot, count and latest
  record; expansion exposes every original receipt. Pending approvals, failures
  and interrupted outcomes remain standalone. Conversation messages, different
  bots/tasks, time gaps and the unread boundary prevent merging. Existing nested
  receipt DOM is retained across live updates to preserve reading position,
  record expansion and keyboard focus. Quoted receipts open their enclosing stack.
- Connector spacing, narrow-screen headers and action wrapping were tightened.
  Visual QA also found the mobile Stop toast covering the composer. Toasts now
  sit above the measured composer area and follow its changing height; their
  width is bounded by the viewport.
- A SQLite fault restricted to one result reproduced starvation of an independent
  pending completion against the previous code. Delivery now continues through
  its bounded batch before reporting the error. Startup and scheduling log a
  pending-delivery error and continue; the unsuccessful item remains retryable.
  Two fault regressions cover independent delivery, startup, retry after removing
  the fault and absence of duplicate results after another restart.
- The full backend suite passes **291 tests** using the pinned Pi test runtime.
  These are controlled database/provider fixtures; this round did not operate a
  user VM or connect to a real provider account. Browser stack checks pass in Edge
  and WebKit with 13 receipts, keyboard operation, live append, preserved nested
  reading state, quoted receipt navigation and six width/theme combinations per
  engine (1320, 900 and 390 pixels, dark and light). Connector writes are not issued
  by opening or inspecting stacks. Screenshots were visually inspected.
- The existing connector artifact, connector edit workflow, team chat control and
  chat transition suites were rerun in both engines. The artifact fixture now
  expands completed receipts before inspecting them, and the team-control fixture
  checks that the visible mobile toast clears the composer. All **10 final browser
  suite/engine combinations pass**, including the new stack suite. Source remains local
  and unreleased; no native package build or release publication was performed.

### Retry fairness and editor focus (unreleased, 2026-09-11)

- A stress regression reproduced 100 persistent receipt failures monopolizing
  every bounded delivery batch, leaving the healthy 101st reply undelivered.
  Pending delivery now follows queue order and atomically moves unsuccessful
  receipts to its tail. Collaboration dependency recovery still runs first.
  Freeze checks remain in place, and a concurrently delivered receipt is not
  reinserted. The regression proves the first batch stays bounded, the healthy
  reply arrives in the next batch, and all failed replies drain exactly once
  after the injected fault is removed.
- A browser regression reproduced a live sibling receipt update moving focus
  from a pending email editor to its enclosing message. Chat refresh now restores
  message-action focus only for identified message controls, leaving retained
  connector inputs focused. The draft and its editor remain intact.
- The full backend suite passes **292 tests**. After strengthening the fairness
  regression's bounded-batch and retry assertions, all **7 focused recovery tests**
  pass, including the actual Pi bridge with controlled provider responses.
- Browser checks cover completed receipts changing to failed, visible uncertain
  outcomes, keyboard expansion, draft/focus preservation, widths of 320/390/900
  pixels and CSS text-scale stress values of 1.4 and 1.6. Stack summaries and
  editor actions stay within the viewport and the save/cancel actions do not
  overlap. Screenshots were visually inspected. Existing checks cover history
  beyond 300 messages, paging/retry, cached scroll position, unread boundaries,
  message actions, quoting, and stack reading-state preservation. All **10 final
  browser suite/engine combinations pass** across Edge and WebKit.
- Testing used local browser fixtures and disposable test containers without
  operating a user VM, provider account or external connector. No release,
  native package build or stable-feed change was performed.
