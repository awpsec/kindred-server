# Guest dock

The guest now uses `deploy/kindred-dock`, a GTK 3 / libwnck X11 panel,
launched by the existing per-display supervisor. It reserves a 64px strip across
the bottom of the screen. Maximized windows respect its EWMH strut.

Layout: an Applications pill (opens the rofi application search) on the left,
the app icons centred in the strip, and the clock on the right. Below 820px the
pill drops its label and centring gives way to available space. Icons that do not
fit scroll horizontally (mouse wheel or keyboard focus, no scrollbar), and the
pill and clock stay visible. The dock styles only its own process: its windows,
menus and tooltips. App palettes are untouched.

Browser, Files and Terminal stay pinned. Other running windows are grouped by
WM_CLASS, including minimized windows and other workspaces, and appear after a
thin divider in first-opened order. Icon-less apps get a muted letter tile rather
than the window manager's stock icon. State updates come from libwnck signals, and
items are updated in place, so hover, focus and transitions stay stable.

Indicators, in the macOS style, sit under each icon: one small dot per open window,
up to three. The active app has a subtle tile background and a wider bright lead
dot. An app whose windows are all minimized shows hollow dots and a slightly dimmed
icon. The tooltip and accessible name spell this out, e.g.
`Browser · 2 windows · 1 minimized`. Hover transitions are 160ms background and
icon-lift transitions.

- Left-click: launch if closed; show and focus a single window, un-minimizing it
  and switching workspace if needed, and bringing forward any pending dialog. The
  click never hides a focused app, which keeps bot clicks idempotent. With several
  windows it opens a chooser above the icon: windows topmost first, the focused one
  marked, minimized ones labelled, then New window.
- Right-click, Menu key or Shift+F10: a titled menu with Show window, Minimize and
  Close window for each window, plus New window (or Open) and Close all windows.
  Close requests go through the window manager, allowing normal save prompts.
- Middle-click on a pinned app opens a new window.
- Launch contract: opening a closed app uses the plain `desktop-launch` action, so
  `browser` still restores Chromium's last session. New window and middle-click use
  explicit new-window actions: `browser-new` (`--new-window`, same profile),
  `files` (`--new-win`) and `terminal` (`--separate`).
- Keyboard: Tab/arrow keys move between items, with a visible focus ring. Enter or
  Space acts like left-click, Down opens the chooser, and keyboard-opened menus
  preselect their first entry.

Closing the last window does not guarantee its process exits: background-only
processes are deliberately outside the dock's window list. Launches go through
GLib, which reaps the launcher, so finished apps leave no zombies under the dock.

The clock uses the guest's configured local timezone. It shows 24h time over
`Sat 26 Sep · EDT`; the tooltip gives the full date, the IANA zone and the UTC
offset. It does not infer the user's timezone.

## Installation and existing computers

Fresh guests install the panel and GTK/libwnck dependencies through
`deploy/install-guest.sh`. Existing VM disks are not silently rewritten by a server
update. For an approved guest upgrade, extract matching deploy sources into the
guest and run `sudo python3 refresh-desktop.py --panel --source <deploy-directory>`.
This backs up the managed supervisor, prior panel and launcher, installs
dependencies, adds only the `browser-new` action to an existing launcher (keeping
local edits; a launcher without the stock `browser)` entry is rejected), and
stages the panel for the next computer restart. It does not stop applications or
restart X. Custom supervisors are rejected for manual integration. The old tint2
configuration stays intact; rollback restores the backed-up supervisor before the
next restart. No guest upgrade was executed during development.

## Validation

From the server checkout:

```
/usr/bin/python3 tools/test-desktop-launch.py
/usr/bin/python3 tools/test-guest-dock.py
KINDRED_TEST_DOCK_WIDTH=640 /usr/bin/python3 tools/test-guest-dock.py
python3 -m py_compile deploy/kindred-dock deploy/refresh-desktop.py
sh -n deploy/install-guest.sh deploy/start-desktop-shell deploy/desktop-launch
git diff --check
```

Passed locally at 640px, 1024px and 1920px on 2026-09-26. The native test starts
isolated Xvfb/Openbox processes with the guest wallpaper and creates actual GTK
windows (Chromium/Konsole-class fixtures, not the real apps). It drives real
clicks and keys through xdotool. It never calls the dock's refresh directly, so
every state change must come from WM signals. It checks:

- grouping, dot counts, and active and minimized states;
- the chooser, including that it opens above the strip;
- manage-menu actions and single-window restore;
- idempotent focus clicks;
- Applications, closed-app, middle-click and New window launch actions (via a
  stub launcher);
- that the final screenshot has the pointer parked and no tooltip showing;
- keyboard menu access;
- the clock and timezone;
- full-width geometry, strut and maximized work area;
- overflow scrolling that keeps the clock visible;
- cleanup.

`test-desktop-launch.py` runs a copy of `deploy/desktop-launch` that changes only
its working directory, with stub `chromium`, `pcmanfm`, `konsole` and `rofi` on
PATH. It checks the arguments of every action, per-screen browser profiles,
rejected actions and screens, and the staging merge. It does not show that real
Chromium opens a window.

Screenshots go to `$KINDRED_TEST_DOCK_SHOTS` (default `/tmp`):
`kindred-dock-final.png`, `kindred-dock-chooser.png`, `kindred-dock-overflow.png`,
each with a `-strip.png` crop. Requires GTK 3, libwnck introspection, Xvfb,
Openbox and xdotool; feh and rsvg-convert are optional (wallpaper only).

Still to verify in an approved guest candidate: real Chromium, PCManFM and Konsole
window classes and icons, save prompts, screen-resolution changes, and staging and
rollback on an existing disk. This is guest source only; desktop standalone bundles
have not been rebuilt or published. Reviewed and revised by Opus 5.5 on
2026-09-26; changes are reviewed and saved on the development branch; no guest rollout or release has been performed.

Maintainer verification also passed at 1440px, the new default guest width,
including window restoration, overflow, keyboard actions and the launcher argument
contract. Screenshots are local fixture previews, not a production guest capture.
