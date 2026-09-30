# Codex browser worker and Decisions integration status

Owner preference: Codex-powered bots should prefer OpenAI Decisions for browser
execution when supported access is available. Their existing computer tools are
the fallback. Other providers retain their existing computer-use paths.

## What is implemented

The server has a bounded browser worker, a provider-specific tool gate, a
Kindred-owned selector interface, and a persistent guest-local browser driver.
With an installed, verified Decisions adapter, `computer_browser_task` is exposed
only to Codex bots. Codex supplies the goal, exact site origin and named form
values. Each observation supplies finite concrete choices from the current page;
the selector returns only a choice ID. The worker observes after each action and
returns its receipts and final screenshot to Codex. It does not call the main
Codex model after every click.

The driver attaches through an existing Chromium profile's loopback CDP endpoint.
It requires exactly one focused tab at the supplied origin. It never launches,
restarts or replaces a browser, switches tabs, changes profile
settings, or exports browser storage. The driver and observer are embedded in the
server and sent over one SSH channel; they require Python's standard library in
the guest, with no new package installation.

When a verified Decisions adapter is installed, the server's Codex URL-opening
path enables a random-port loopback debugging connection for a newly launched
browser. This flag is not exposed to model arguments and is stripped for other
providers. Chromium keeps an already-running browser's launch settings, so a
browser without that connection continues through fallback; it is never killed
or restarted to enable the feature.

An isolated JavaScript world constructs labelled button, link, checkbox, field
and select choices. Exact supplied text is inserted through Chromium input;
checkboxes have a desired state, and field/checkbox/select values are verified.
Disabled, hidden, password, one-time-code and file controls are excluded. Visible
password or one-time-code inputs require the normal human-interaction flow;
these views are not captured for the decision model. Browser
frames and unsupported/custom controls require ordinary computer tools. Open
shadow roots are inspected; closed/custom widgets may still need fallback.
Choice and input sizes are bounded. Stale DOM state, changed tab focus, covered
targets and replayed action IDs cannot execute a new action.

Kindred's existing display lease, per-bot profile, Stop task, maintenance lock,
human takeover and action budget remain in force. Exact actions use the existing
approval machinery; clicks, field events, checkbox changes and selects are
classified as external because they may submit or autosave. Scroll and wait are
routine VM actions. Denial stops the worker without route substitution. Returning
from human sign-in still requires a fresh desktop screenshot.

Every attempted action has a correlated request/result receipt. If a connection
is lost after dispatch, the result is uncertain and blocks automatic provider
replay. Fallback continues from the observed state and existing receipts; it
does not replay the goal. A pre-action screenshot is not returned as current
state after dispatch. A finish selection is not proof of remote success.
Denial and takeover also set the existing stopped-result recovery marker, so a
later provider disconnect cannot restart a browser task that was stopped.

## What remains unavailable

As checked on September 30, 2026, the
[official DevDay announcement](https://openai.com/index/devday-2026-recap/)
describes Decisions as a limited-preview Luna-based selection API with text and
image context. A public Decisions request schema, endpoint and supported Codex
authentication method were not found. Pro subscription coverage and preview
access for the owner's account were not verified.

`App.decisions` therefore defaults to `None`. The preferred tool is not advertised
and Codex continues using its existing computer tools, with no speculative API
probe or additional paid model call. A direct stale call returns unavailable
without touching a VM. The normal production runtime has no configuration switch
that can turn a placeholder into a live Decisions connection.

The `Decisions` trait and its `DecisionInput` are **internal Kindred interfaces**,
not a proposed OpenAI payload. Completing the integration requires implementing
an adapter against the published/preview contract, verifying supported account
authentication and billing, and installing that adapter in the profile runtime.
Ordinary Luna structured output is not used as an impersonation of Decisions.
No subscription token is extracted or sent to an undocumented endpoint.

Existing browser profiles also need a supported observation connection before
the fast worker can run. This change deliberately does not restart an existing
signed-in browser to enable CDP. Missing connections select fallback. The
Codex-only launch flag does not enable debugging on other providers' browsers
or expose a debugging port beyond guest loopback.

## Verification

Run the worker's server tests with:

```sh
KINDRED_PI_TEST_NODE=/path/to/node cargo test --locked browser_use -- --test-threads=1
```

Run real local Chromium checks with:

```sh
python3 tools/test-browser-worker.py --chrome /path/to/chrome --playwright-module /path/to/playwright
```

The browser test uses an installed Node/Playwright launcher to prepare an
isolated temporary Chromium profile and local assessment form. This developer
test dependency is not installed into VMs or used by the guest driver.
It checks exact multiline/Unicode target entry, input events, checkbox precision,
preserved unrelated settings, select changes, local save feedback, screenshots,
credential omission from text/candidates, authentication-view fallback,
stale/covered controls, frame fallback, action replay and
the production JSONL handshake. It uses no account, real scan, existing VM or
external website. Server tests use deterministic selector/browser fixtures for
unavailability, malformed choices, action receipts, lost responses, denials,
budgets, takeover, origin changes and other-provider isolation.

These checks validate the worker and fallback machinery. They are not live
Decisions testing, a model accuracy/latency benchmark, or native Windows/macOS
acceptance. Source changes do not publish or deploy a release.

Local verification on September 30, 2026: the complete server suite passed
(487 passed, 2 existing ignored tests); the eight browser-worker server tests
were rechecked after the final stop/recovery marker change. Five real Chromium
checks passed. The existing deployment Python suite also passed (77 run,
2 skipped). Syntax and diff checks passed. No live Decisions call, account
entitlement check, production VM operation or release/deployment was performed.
