# Mac package validation

Run `scripts/release/desktop-validation-ci.yml` manually with the original build run, application source SHA, and version. The two native Mac jobs validate Apple Silicon and Intel packages using a fresh profile. They check the DMG, bundle signature, architecture, bundled standalone payload, onboarding, chat rendering, and native Accounts bridge. Chat traffic uses local fixtures; this does not prove provider authentication, Docker setup, or a full agent/computer session.

The packaging workflow also runs these checks before accepting each Mac package.
The chat probe waits for the loading overlay to leave, the content to become
visible and interactive, and the expected message text to appear. A message row
can exist while initial history and scroll restoration are still in progress.
`node tools/frontend/test-native-smoke-probe.cjs` checks this timing boundary
with delayed animation frames and verifies that missing expected content is
still rejected. Set `WEBKIT=1` to exercise WebKit. These browser fixture tests
do not replace native Mac validation. Native failures retain a window capture
and chat readiness diagnostics when available.

The first 0.48.35 check (run 34636241617) passed those functional checks but found invalid or absent bundle signatures. That run collected signature failures as diagnostics. The validator now requires a valid bundle signature, and the complete-release gate requires corresponding Mac manifest evidence. The original 0.48.35 DMGs must not be published as validated candidates.

The optional `repair_signature` input copies the existing app, adds an ad-hoc bundle signature, verifies it, and creates a replacement DMG before repeating the functional checks. It does not recompile application code. Input hashes, unchanged resource counts, tool commit, and replacement hashes are recorded in the new manifests. Successful packages are retained as workflow artifacts; this workflow cannot publish releases. One additional runner invocation requires owner approval under AGENTS.md.

Ad-hoc signing does not provide Apple Developer ID or notarization. Even a valid repaired bundle can require the user's **Privacy & Security > Open Anyway** approval after a browser download. Do not describe this as frictionless Gatekeeper acceptance. Developer ID signing and notarization are a separate distribution requirement if automatic first-open acceptance is desired. See [Apple's opening guidance](https://support.apple.com/en-gb/102445) and [Tauri's signing documentation](https://v2.tauri.app/distribute/sign/macos/).
