# Workflow audit — 2026-09-11

## Scope

Browser regression exercise of the existing workflow suites in Edge and bundled
WebKit. The suites use local HTTP fixtures and do not call external providers,
start model turns, or use live credentials. This browser audit changed only its stale assertion. The integrated implementation and backend validation are recorded below; no release or deployment was performed.

## Results

All seven suites passed in both engines (14 successful runs):

- `test-user-flows.cjs`
- `test-codex-connector-preferences.cjs`
- `test-connector-preferences.cjs`
- `test-send-recovery.cjs`
- `test-task-recovery-files.cjs`
- `test-planning-recovery.cjs`
- `test-question-presence.cjs`

The passing coverage includes account/provider switching, Codex preference and
permission gating, duplicate connector source selection, interactive account
cards and inputs, retry/cancel and recovery, concurrent planning recovery,
mobile layout, and stale reload handling as defined by each suite.

The initial `test-question-presence.cjs` failure was a stale assertion. The live
DOM correctly rendered one continuation worker with the label `Searching`, but
the `.work-label` text also includes the elapsed timer (for example,
`Searching14s`). The test compared the entire textContent to `Searching` and
timed out in both engines. The minimal test-only correction checks the label's
text span, retaining the exact worker-count and label behavior. The later
`Reviewing search results` assertion received the same correction. No product
code or fixture behavior changed.

## Visual inspection

I inspected the fresh connector settings screenshots:

- `test-results/edge-connector-settings.png`
- `test-results/webkit-connector-settings.png`
- `test-results/edge-codex-provider-mobile.png`
- `test-results/webkit-codex-provider-mobile.png`

Both show the preferred-source control, provider connector cards, per-connector
permission controls, email sending selector, refresh action, and narrow-width
layout without visible clipping. The Codex mobile views visibly retain the
Codex Gmail card's per-call approval and the Kindred Slack card below it.
Fresh conversation, takeover, and account screenshots were also produced by the
passing user-flow suites.

## Reproduction commands

With `KINDRED_PLAYWRIGHT_MODULE` set to
`C:\Users\Alex\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules\playwright`:

```powershell
$env:KINDRED_TEST_BROWSER = 'edge'
node tools/frontend/test-question-presence.cjs

$env:WEBKIT = '1'
node tools/frontend/test-question-presence.cjs
```

The other six suite names above were run with the same engine settings and
returned their suite-level `passed: true` records. After the assertion fix,
`test-question-presence.cjs` returned `passed: true` in both engines with
question handoff, one continuation avatar, live activity labels, empty and quiet
completion, and mobile checks passing.


## Integrated card and input validation

Unreleased source based on `51c1cf9e7ef332c75d9ebf63611f3a7dd7df9513`.
The installed desktop version remains 0.48.34; no native desktop package,
GitHub workflow, release asset, stable feed or hosted service was changed.

The catalogue now has 34 adapters. New services are Linear, ClickUp, Airtable,
GitLab, Dropbox, Box, Todoist and Stripe. Backend-generated fixtures exercise
all 34 through card completion/readback, with stable fixture identities only;
record content is not hand-authored in the rendered fixture file.

Supported draft edits now include Graph-shaped Outlook recipients, subject and
HTML/text body, and existing string fields for tasks, calendar events, meetings,
messages, issues and support requests. Recipients with extra unknown object
fields, MIME, sender changes and attachment editing remain unsupported. Saved
edits retain routing, envelope options, unknown fields, attachment data and
existing matching recipient names. Date/time strings are not coerced by edits.

`cargo fmt --check` and all **249 Rust tests passed** (34.90 seconds). The
optimized backend build passed with the same three existing dead-code warning
groups. The final UI source was included in a subsequent optimized build;
that final build passed in 43.49 seconds. Eight changed Rust, UI and catalogue
files were verified by SHA-256 against the passing builder snapshot. This is an
isolated build, not a deployment or desktop release.

The three affected browser suites also passed in Edge and bundled WebKit:

- `test-connector-catalog.cjs`: 34 generated cards, typed content, source-link
  safety, proposed/completed distinction, sheet grid, marketplace shortcuts,
  UTC date display with original timestamps, and narrow layout.
- `test-connector-edit-workflows.cjs`: nested Outlook save/readback, reload,
  task save/approve, calendar and message edits, failed-save draft retention,
  cancellation, duplicate save, Escape during a pending save, read-only input,
  feedback and future-email approval locking, and attachment data filtering.
- `test-connector-artifacts.cjs`: existing saved email edits, stale save errors,
  feedback, one approval per double click, forced Codex review, safe source
  links and completed record discussion.

Together with the seven audit suites, this is **10 browser suites passed in
both engines**. Repeated development runs are not counted as extra coverage.
Root visual inspection included saved Outlook at 390px, a saved calendar card,
Stripe source amounts, ClickUp assignment/deadline, and Airtable fields.

The first integration run found missing Dropbox/Box `entries` traversal. That
adapter defect was fixed, together with missing ClickUp username extraction.
A mismatched test catalogue origin and a stale Stripe-label assertion were
corrected as fixture/test defects. Airtable metadata collisions and sensitive
field names are excluded from its typed record. Review found a separate
approval button outside the card footer; all approval paths now lock while
an editor is open, including feedback and future-email approval.

These checks used isolated database/RPC peers and local browser HTTP fixtures.
They did not authenticate or write to real mailboxes, projects, payment accounts
or other external services. A live provider read and separately approved write
remain acceptance work, not a claim established by this test matrix.
