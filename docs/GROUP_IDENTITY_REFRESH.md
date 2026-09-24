# Current bot identities in group chats

Shared-room participant records used to retain the bot name and avatar captured when the room was created. Room listing and the shared-chat sync worker now refresh the existing participant's public name and avatar from its owning profile, without changing IDs, membership or permissions. Only the same public avatar fields already exposed by the directory are copied. Updated snapshots are persisted so attribution, routing, headers and avatar clusters agree.

New shared rooms record whether their title was generated. Generated titles follow participant names; explicitly named rooms retain their titles. Legacy rooms infer this once by comparing their saved title with the original participant snapshot. A legacy custom name exactly identical to the generated title is indistinguishable and follows names. Renaming a shared room explicitly turns automatic naming off.

Local workspace groups already render avatars from the current bot objects. Bot saves now update their title when it exactly matches the old ordered member names, retaining custom names. Previously stale local titles that no longer match any current member names are left intact rather than guessing what the owner intended.

Regression coverage includes legacy shared title repair, new avatar/name data, persisted refreshed records, private-data exclusion, and preservation of custom shared/local titles. Changes are server-side; the existing desktop refresh/render path consumes them.
