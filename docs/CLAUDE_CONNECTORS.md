# Claude account connectors

Sign in to Claude from Kindred Settings → Connections. Remote connectors connected
to that Claude account are discovered at the start of each Claude bot task. Add or
reconnect them in Claude's connector settings; no connector credentials are copied
into Kindred. In Connections, choose **Refresh Claude connectors** to inspect their
current status without making a model request.

Claude bots can use both sources in the same task:

| Connection source | Available to | Managed in |
| --- | --- | --- |
| Claude account | Claude bots using this profile's Claude sign-in | Claude connector settings |
| Kindred connected apps | All authorized model providers, including GPT and Claude | Kindred Connections |

For example, a Claude bot can search Confluence through Claude and use a Gmail
account connected through Kindred. A GPT bot can use Gmail and Confluence when
both are connected through Kindred. Claude sign-in does not grant GPT access to
Claude-only connectors. Kindred's marketplace supports additional apps through
the configured Composio project; some require a custom authentication configuration.

When a Claude bot discovers the same service through both sources, it asks which
source to prefer. Choose **Prefer Claude**, **Prefer Kindred**, or **Ask when needed**.
The choice is saved for that bot. You can still name another source in a message.
Multiple accounts within the chosen source still require an account choice when
the intended account is unclear. Choosing a source does not grant permission.

Connector approvals show the app and source, such as **Confluence (via Claude)**,
with **Always allow**, **Allow once**, and **Deny**. Always allow covers the entire
named connection, including changes, for that bot. For example, a Confluence call
through the Atlassian connection offers permission for the whole Atlassian
connection; the card states this scope. Denial must not cause a retry through
another account or source.

To grant all Claude connections together, tell the bot: “Please always allow all
connectors from my Claude account.” It presents one **Yes (always allow)**
confirmation covering current and future connectors on that Claude account for
that bot. The bot cannot save this permission without your confirmation, even in
full permission mode. Claude and connector authentication requirements still apply.

Open the bot’s settings → **Connectors** to change its preferred source or revoke
an individual or account-wide grant. An individual revocation overrides an
account-wide grant. Turning off the account-wide grant clears its individual
grants and returns the account to asking. These choices persist across restarts;
they are tied to the bot, connection source, and account identity. A different
Claude account or replacement connection does not inherit the previous account’s
individual grants. Moving a workspace requires setting up connector permissions
again on the destination.

Kindred applies the bot's current approval preference to every inherited connector
call. A further permission request from Claude or the connector always requires
explicit approval, even when the bot otherwise has full permission. Organization
blocked tools remain withheld by Claude. Activity records distinguish requested
calls from completed or failed calls; stopping a task does not undo completed
external actions.

Discovery uses the official CLI's subscription login. API keys and setup tokens do
not provide inherited account connectors. A connector marked **Reconnect in Claude**
needs attention in Claude. A new task refreshes discovery; an existing task does
not silently switch to a newly connected or different account.

Reference: [Claude Code account connectors](https://code.claude.com/docs/en/mcp#use-mcp-servers-from-claudeai).
