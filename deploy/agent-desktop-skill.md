---
name: agent-desktop
description: Read and operate accessible controls on your assigned Kindred Bot Computer.
---

Use `guest_exec` with `use_desktop=true` for every `agent-desktop` command. This uses Kindred's normal approval, screen ownership and human-control checks. Never set DISPLAY, a bus address or AGENT_DESKTOP_HOME yourself; never run this on the user's physical computer. Background commands cannot control a screen.

1. Run `agent-desktop status`, then `agent-desktop skills get platform` once when using it in a task. If unavailable, use the existing computer tools; do not install another version or replace the desktop session.
2. Run `agent-desktop list-apps` and use the returned exact app name. Find a known control with `find --app NAME --role ROLE --name LABEL --exact`. Otherwise map with `snapshot --app NAME --skeleton -i --compact`, then drill into a returned ref with `snapshot --root REF -i --compact`.
3. Act on the exact qualified reference. Prefer semantic actions, without `--headed`. Observe the result before the next action. A successful delivery does not prove the task succeeded.
4. After a UI change or `STALE_REF`, observe again. Never guess or reuse another screen's references. Only repeat a failed action when its disposition says `delivery: not_delivered` and `retry: safe`. Otherwise inspect the live state first; the action may already have happened.

Chromium exposes useful controls but some fields do not support semantic typing. A refusal is not a broken connection. After verifying that nothing was delivered, use the existing Kindred computer/browser tools for the needed input, with a fresh observation. Do not silently turn on `--headed` or bypass a permission denial. Use screenshots for visual layout, canvas apps, verification and handing over to the user. Do not equate a partial accessibility tree with an empty page.

Keep text, snapshots and screenshots within the task's scope; accessibility output can contain private on-screen text. Never dump a vault or hidden fields. Release the computer with `computer_release` when done. Connected API tools remain preferable when they directly support the task.

Detailed syntax is available on demand through `agent-desktop COMMAND --help` and `agent-desktop skills get desktop commands-interaction`. Do not load every reference into context.
