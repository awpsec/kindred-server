# Concurrent work and lightweight group routing

## What changed

General group messages no longer queue a full model turn for every member just
so each bot can decide whether to answer. Both local and server-shared chats pick
recipients before enqueueing, with no router model or provider call:

1. Explicit mentions/direct addresses and explicit everyone/each-of-you requests.
2. The author of a quoted reply (existing membership and delegation rules apply).
3. One named task owner, such as “summarize Atlas’s work”.
4. A single coordinator named in the chat description (for example “Piper
   coordinates”, “Piper leads”, “coordinator: Piper”, or “head bot Piper”).
5. The most recent eligible bot speaker among the last 12 messages.
6. A stable member-ID tie-breaker if there is no usable conversational context.

This is deterministic selection, not semantic understanding of arbitrary prose.
Explicitly addressing someone remains the strongest choice. The selected bot can
still delegate, hand off, or ask teammates for missing context. Everyone/each-bot
updates still fan out. A greeting such as “Morning everyone” is not a broadcast
request and does not wake bots in a mentions-only room. Bot-originated directed collaboration retains its existing
routing and loop protections. Shared history remains available to every member;
unselected bots do not run merely to acknowledge receipt. Archived members are
excluded from automatic shared-chat selection. General shared-chat routing still
respects the existing all_messages setting.

## Desktop ownership

Correction to the initial latency hypothesis: bots already have separate display
slots/browser profiles inside their shared VM. A shared VM did not imply that all
bots were serialized on one screen. However, the scheduler held each display for
its entire model task and prevented non-desktop work during human control.

The scheduler now starts model tasks without reserving their displays. The
configured parallel-run limit remains (default four), and each individual bot's
conversation queue remains serial to protect its session/state. Independent bots
can reason, read context, call connectors, and execute headless commands while
screens are held elsewhere or under manual control.

Desktop tools acquire ownership lazily. A screenshot/navigation/action sequence
retains its display across successive desktop calls, preventing someone else from
changing the page between a screenshot and click. computer_release, a subsequent
non-desktop tool, or task completion releases ownership. Reentry requires a fresh
screenshot before coordinate/keyboard actions. Per-run action gates serialize
simultaneous desktop calls. Human handoffs release ownership and resume the same
provider turn, with the existing post-handoff observation requirement.

Foreground guest_exec is headless unless use_desktop=true. Display-using scripts
participate in desktop ownership; they cannot run as unmanaged background desktop
actions. Background commands remain headless. The server also unsets display
variables inside headless guest commands for compatibility with older guest
binaries. These are tool semantics, not a security sandbox against a deliberately
written script that overrides them. Shared files and services still need bot
coordination when edits overlap.

Cancellation/timeout/aborted drivers release ownership. An interrupted owned
desktop gets the existing recovery interval before another action can start;
cancelling chat-only work does not unnecessarily pause the screen. Queuing time is
now recorded in run_started events to distinguish scheduling delay from model or
tool time in later investigations.

## Validation

New tests cover deterministic single-recipient routing, broadcasts, named owners,
coordinators, recent speakers, shared quoted replies, retained member history,
parallel model/context work with both screens locked, release/re-observation,
manual-control guards, cancellation, cleanup, and cancellable desktop acquisition.
All 161 targeted tests passed: 138 in the backend coordination preflight, plus
seven provider protocol tests, eleven maintenance tests, two screen-control tests,
and three guest/browser tests. The provider checks use the real Pi worker against
local mock endpoints and Codex protocol fixtures, not live accounts. Pi tests used
`KINDRED_PI_TEST_NODE=/usr/local/bin/node` as required by this test host.

Evidence: `/opt/kindred/testing/concurrency-routing-final-2026-09-21/`. The initial
pass caught a broadcast-greeting regression; the fix and regression assertion are
included in the passing final run. No live-provider latency improvement is claimed
without a timed run on the affected instance. No packages, release or deployment
were performed.
