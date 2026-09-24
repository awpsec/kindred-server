# Routine connector visibility

Read-only connector calls now publish a chat receipt when created, just like other connector actions. Previously reads were absent during execution and only appeared on completion if a result could be normalized into records; scheduled runs explicitly skipped even that publication.

All newly created tracked calls now use the existing inline disclosure and counted connector stack, including scheduled checks, empty results and failures. Completion updates the same receipt without duplicating it. Completion also inserts a missing receipt for a call created under the earlier behavior, retaining its original timestamp. Already completed historical calls are not backfilled.

Permissions and routine notification policy are unchanged. The visibility applies to calls routed through Kindred’s Claude, Codex and native connector integrations; arbitrary external scripts are not inferred as connector calls.

Validation includes a database regression for quiet routine reads against Confluence, Monday, Slack and Google Calendar, live preparing receipts and empty completed results without duplicates. Existing browser checks cover live current-call visibility, collapsed counts, expansion, approvals/errors, refresh retention, unread boundaries, light/dark and narrow layouts. No production connector actions were performed for testing. Source-only change; not deployed.
