# Native macOS window frame

Main, Accounts and Local Access windows retain AppKit decorations on macOS so
AppKit owns rounded outer corners, resizing, shadows and fullscreen behavior.
The main window uses Tauri's Overlay titlebar style with a hidden title and native
traffic lights inset into the existing sidebar header. Its startup script sets
`__KINDRED_MAC_OVERLAY`; shared UI omits the HTML traffic-light buttons only when
that flag is present. Older installed clients retain their existing controls.

Accounts and Local Access use the standard native titlebar and the existing
`__KINDRED_NATIVE_FRAME` path to hide their HTML chrome. The updater already uses
a native decorated window. The special transparent notch surface stays borderless.

The builder APIs and macOS gates were checked against the pinned Tauri/Wry source.
Browser tests exercise both legacy and overlay startup flags, checking that no
extra top row or duplicate HTML controls are introduced. Browser screenshots do
not include native corners or native traffic lights.

A rebuilt Apple Silicon client is required; UI-only repackaging cannot change
NSWindow construction. Before release, verify all four corners in both themes,
traffic-light alignment/actions, resizing, maximize/restore, fullscreen entry/exit,
and the Accounts and Local Access windows on macOS. Native acceptance remains
pending; Linux compilation cannot validate the macOS-only builder configuration.

Validation: Chromium and WebKit UI checks passed. A cold Linux desktop dependency
build was stopped before completion; it does not verify the macOS-only branch.
