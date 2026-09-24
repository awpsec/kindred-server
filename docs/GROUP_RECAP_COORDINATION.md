# Group recap coordination

Guide version 24 distinguishes a single team recap from individual status requests. A general "fill me in" can still reach every member, but the expected speaker is the explicitly addressed bot, established coordinator or clear task owner. Specialists add only missing firsthand details or corrections. Uninvolved bots finish quietly, including instead of posting "nothing on my end."

If the appropriate speaker is unclear, all members receive the same fallback_responder nomination in conversation context. It is selected deterministically from current bot participant IDs, excluding people and locally known archived bots. Sorting by ID makes it independent of roster order and renaming. This is a tie-breaker, not a configured leadership role, and does not override direct questions, round-robin updates or distinct assignments. Local members also include their public role labels.

This changes model guidance and context, not server delivery or a hard reply lock. Models still decide whether a contribution is relevant; this does not guarantee zero overlapping responses. No extra model call or public election conversation is added. Server rosters supply their current participant list; eligibility there follows the synchronized roster.

Regression expectations:
- An established coordinator summarizes once; the author of the work adds only an omitted material fact.
- Without a known coordinator/owner, the common nominee answers; others do not independently self-select.
- "Each of you, report your own progress" still permits separate relevant updates.
- A named specialist's question overrides the fallback.
- Quiet participants do not emit a public non-participation explanation.
- Normal private chats do not receive a fallback nominee.

Tests cover stable nomination across roster order, exclusion of people/archived bots, no eligible nominee, and existing full/compact instruction context tests. Behavioral examples are expectations, not claims of a live multi-model evaluation.
