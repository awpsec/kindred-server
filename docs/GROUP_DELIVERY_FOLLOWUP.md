# Group delivery follow-up

The September 21 conversation review showed three related failure modes: a posted result followed by a second final recap, queued older messages answered after newer results had resolved them, and public explanations of why a bot supposedly had nothing to say. These messages could themselves wake teammates.

The shared-server task prompt now agrees with ordinary delivery: a normal reply already appears in the room. `chat_post` remains available for explicit addressing and cross-chat delivery. Its tool definition and successful receipts (including idempotent retries) explain that the message is already delivered. Same-chat receipts direct completed work to `finish_quietly`; cross-chat receipts permit a short requested confirmation without copying the report.

Both local and shared-server wake prompts tell bots to read the latest conversation before acting on an older queued request. Core and communication guidance prohibit queue/sequence narration and repetitive unchanged reports. Guide version 23 includes these changes even for providers using the reduced core guide.

This complements the existing real group `finish_quietly` path, named sender attribution, and private owner question delivery. It does not discard queued work, suppress replies using text matching, or claim semantic deduplication. Models still decide whether information is new; live quality should be checked after release. No production conversations were modified by this review.

Validation: team-chat routing and delivery tests cover current-room and cross-room receipts, retry idempotency, participant routing and handoffs. The existing quiet-group test checks completion creates no public bubble. Source changes require a release before installed clients/servers benefit.
