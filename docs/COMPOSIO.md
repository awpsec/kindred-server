# Apps marketplace through Composio

Kindred 0.4 searches the live Composio v3.1 toolkit catalog, rather than presenting
three fixed connectors. Search, pagination, installed apps, hosted sign-in, custom
OAuth configurations, and tool discovery all use the same private Composio project.
The installed instance needs a project API key before live sign-in can be tested.

## Connect

1. Create or select a project in the [Composio dashboard](https://dashboard.composio.dev/).
2. In **Settings → Connections**, save its project API key in the private field.
   The host validates it and stores it atomically in a mode-600 credentials file.
   Existing OpenRouter keys are preserved. `COMPOSIO_API_KEY` is also supported.
3. Open **Marketplace** from the sidebar, search for an app, and choose **Add**.
4. Choose access, then **Connect** and **Continue in your browser**. Complete the
   provider's consent or credential form on Composio's hosted page.
5. Return to Kindred and choose **Check connection**. The app shows Added only
   after the server has a connection record; its detail page shows the actual status.
   An initiated connection is not an active account.

Managed OAuth is preferred where the toolkit advertises it, accepting both upper-
and lowercase scheme names. API-key, bearer-token, Basic and no-auth toolkits can
use a custom config with credentials supplied at connect time. Apps needing a
custom OAuth client can use **Use a custom auth configuration**: create the config
in Composio first, then select it. Config listings expose only names, IDs and schemes.
Narrowly scoped Google connections default to managed auth. Gmail also accepts
a custom OAuth configuration with explicit scopes matching the selected access,
for native push setup in the owner's Google Cloud project. Other apps use their configured permissions. Tools outside Kindred's reviewed
read-only allowlist follow the bot's effective approval setting.

The project key is never returned by the settings API. Provider passwords, tokens
and one-time codes must never be pasted into a Kindred conversation.

## Access

The account's **Read-only / Read and write** choice controls allowed connector
operations. The bot's effective approval setting separately controls prompts:
**Full access** skips them for write-enabled accounts; **Ask for approval** and
**Approve for me** still review connector mutations. Full access cannot exceed the
provider's granted OAuth scopes or the account's read-only restriction. Expanding
an existing OAuth grant requires reauthentication; changing prompt policy does not.

| App | Read-only scopes | Additional/change scope for approved actions |
| --- | --- | --- |
| Gmail | `gmail.readonly` | `gmail.modify` replaces read-only; excludes immediate permanent deletion |
| Calendar | `calendar.readonly` | `calendar.events` |
| Drive | `drive.readonly` | `drive.file`, limited to files created/opened with this OAuth app |

Scope names above have the prefix `https://www.googleapis.com/auth/`.
Google consent is authoritative about the grant actually requested. Disconnect and
reconnect to change access. Broader grants do not remove Kindred's individual
approval requirement for changes, including for bots that auto-approve computer
actions. Unknown tool operations are hidden for read-only connections and require
approval for connections that allow changes. Some catalog tools need additional
Google scopes and will correctly fail with the intentionally limited grant.

The model discovers available tools and their current schema/version before
execution. Account and user identifiers are resolved by the server. Approval is
bound to the proposed tool/arguments, and account state is checked again before
dispatch. HTTP success alone is insufficient: Composio must report `successful: true`.
An uncertain or failed mutation is never automatically retried.

## When Google blocks a connection

Kindred requests Composio-managed OAuth where the toolkit reports support. The
consent screen can show Composio's name. Sign-in opens outside the embedded desktop
webview, consistent with [Google's OAuth policies](https://developers.google.com/identity/protocols/oauth2/policies).
Kindred's localhost/Tailscale address is not registered as Google's OAuth redirect.
The hosted Connect Link handles consent; Kindred polls the bound connected account.

This avoids embedded-browser and private-callback mistakes but cannot guarantee
Google will allow every account, scope or organization. If consent fails:

- A work/school administrator may need to allow the OAuth application and scopes.
- Check the Google error in the browser and the Composio project's logs.
- If managing a custom OAuth application outside this UI, check approved test users,
  enabled APIs, exact redirect registration and Google's verification requirements.
  Kindred's connection button creates managed configurations, not custom ones.
- A project requiring callback identity verification needs its own supported public
  verifier. Kindred does not silently disable that policy.
- Expired or failed connections need a fresh sign-in. Reconnect rather than repeatedly
  executing tools against a stale grant.

**Disconnect** removes the Composio connected account. It does not promise to revoke
every Google grant. Review Google's connected-app settings if full grant revocation
is needed. Remove existing app connections before switching to another Composio
project; account/config identifiers belong to their original project.

## Verification boundary

Mocked API tests cover managed configuration/scopes, account identity, discovery,
version pinning, schema checks, read-only policy, approval/denial/cancellation,
changed connections, provider-reported failure and credential preservation. Browser
tests cover search, pagination, connection onboarding, custom config selection and
connected-state changes with mocked provider replies. These tests
do not establish that Google accepted live consent. The installed instance has not
yet supplied a Composio project key; live consent and read-only API checks remain.

Reference: [Composio authentication](https://docs.composio.dev/docs/authentication),
[managed versus custom OAuth](https://docs.composio.dev/docs/authentication/custom-app-vs-managed-app),
[scope controls](https://docs.composio.dev/docs/authentication/controlling-scopes),
[versioned execution](https://docs.composio.dev/reference/api-reference/tools/postToolsExecuteByToolSlug).

## Inbox monitoring

[Inbox monitors](INBOX_MONITORS.md) use account-bound Gmail history reads and optional
native Google push delivery. This is separate from Composio's polling Gmail trigger.
Empty checks do not run a model.

### Managed Gmail scope compatibility (2026-09-22)

Managed Gmail sign-in now leaves Composio's OAuth scopes unchanged. Overriding
these with `gmail.readonly` or `gmail.modify` triggered Google's app-blocked
screen in the owner's personal-account flow; a separate default-scope connection
completed OAuth and passed `GMAIL_GET_PROFILE` after the API key received
`tool_execution` write permission. No messages were fetched or sent in that test.

The cache key `gmail:managed-default-v1` avoids reusing older overridden configs.
Existing accounts are preserved; remove unfinished blocked accounts and add them
again after upgrading. Managed accounts store an empty scopes list (provider
defaults, not a claimed narrow grant). Kindred's `permission` remains the bot's
local access policy. The UI discloses that default Google authorization includes
full Gmail plus contacts/profile access. Custom OAuth configs retain explicit
scope validation. Do not describe local read-only policy as a read-only Google
token. The diagnostic connection is isolated and is not a bot connection.
