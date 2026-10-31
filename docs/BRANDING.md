# Kindred identity

The approved Team shadows identity is shared with the desktop repository.
Its three bots are a periwinkle square, happy orange circle and smaller curious
green triangle with long, subtly tinted cast shadows. Preserve the artwork and
expressions in `ui/brand-icon*.svg`; do not substitute the previous logo.

`ui/favicon.svg` adapts to system appearance, with simplified geometry for tabs.
`ui/brand-icon.png` is the notification fallback and `ui/apple-touch-icon.png`
is the web bookmark icon. The first-run icon follows the app-selected theme; its
outlined wordmark inherits the app's text color. These assets have explicit
public SVG/PNG routes in `src/web.rs`.

The desktop repository's `branding/team-shadows.json` and `branding/generate.py`
are the canonical source and generator. Keep shared UI assets identical and run
the desktop release shared-UI verifier on exact desktop/server source commits.
The macOS package deliberately uses the LIGHT composition in every appearance.
iOS uses light/dark/tinted AppIcon entries and light/dark native loading artwork.

This source update does not publish packages or deploy a server. Native Apple
acceptance and release approval remain part of the existing release workflow.
