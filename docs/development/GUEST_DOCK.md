# Guest dock

The guest now uses `deploy/kindred-dock`, a GTK 3 / libwnck X11 panel,
launched by the existing per-display supervisor. It reserves a 64px strip across
the bottom of the screen. Maximized windows respect its EWMH strut.

Applications, Browser, Files, and Terminal remain available. Running windows are
grouped by WM_CLASS, including minimized windows and other workspaces. Each open
application has a dot; the active application has a highlighted background.
Left-click restores a single window or opens the instance list for multiple
windows. Right-click lists instances with Show window and Close window actions.
Close requests go through the window manager, allowing normal save prompts.
Closing the last window does not guarantee its process exits: background-only
processes are deliberately outside the dock's window list.

The clock uses the guest's configured local timezone and displays that timezone,
time, weekday, and full date. It does not infer the user's timezone. App icons
scroll horizontally if there are more than fit; the clock stays visible.

## Installation and existing computers

Fresh guests install the panel and GTK/libwnck dependencies through
`deploy/install-guest.sh`. Existing VM disks are not silently rewritten by a server
update. For an approved guest upgrade, extract matching deploy sources into the
guest and run `sudo python3 refresh-desktop.py --panel --source <deploy-directory>`.
This backs up the managed supervisor and prior panel, installs dependencies, and
stages the panel for the next computer restart. It does not stop applications or
restart X. Custom supervisors are rejected for manual integration. The old tint2
configuration stays intact; rollback restores the backed-up supervisor before the
next restart. No guest upgrade was executed during development.

## Validation

From the server checkout:

```
/usr/bin/python3 tools/test-guest-dock.py
KINDRED_TEST_DOCK_WIDTH=640 /usr/bin/python3 tools/test-guest-dock.py
python3 -m py_compile deploy/kindred-dock deploy/refresh-desktop.py
sh -n deploy/install-guest.sh deploy/start-desktop-shell
git diff --check
```

Passed locally at 1024px and 640px. The native test starts isolated Xvfb/Openbox
processes, creates actual GTK windows (Chromium-class fixtures, not Chromium),
right-clicks via xdotool, checks minimized-window discovery, activates restore and
close menu items, checks grouping/cleanup, date/time, full-width geometry, and the
reserved maximized-window area. Screenshot: `/tmp/kindred-dock-acceptance.png`.
Requires GTK 3, libwnck introspection, Xvfb, Openbox, and xdotool locally.

Still to verify in an approved guest candidate: real Chromium/PCManFM/Konsole
window classes, save prompts, screen-resolution changes, large window counts,
and staging/rollback on an existing disk. This is guest source only; desktop
standalone bundles have not been rebuilt or published. Opus 5.5 was unavailable
in the development agent environment; no Opus review is claimed.
