# Routine delivery repair for 0.48.35

Duplicate saved terminal results for the same run now leave one visible result.
The repair retains the original message rows, reaction records and quoted-message
IDs. Identical text from a later routine run remains a separate visible report.
It runs before migration links results to assistant events, avoiding a reproduced
`UNIQUE constraint failed: chat_messages.source_event_seq` startup failure.
An existing event link is never assigned to another result row.

The file-backed regression inserts an old duplicate, then opens the database
twice. It covers results with and without assistant events, retained reactions
and quote resolution, and two independent runs with identical reports. Before
the fix, the event-backed case failed during startup. After the fix, all 295
Rust tests passed, including the actual Pi SDK routine delivery fixture.
Scheduling checks also confirm eight concurrent due ticks create one run and
a one-time routine remains consumed across repeated ticks and restart.

Version 0.48.34 already suppresses intermediate routine assistant messages.
The new repair addresses historical repeated terminal rows; these synthetic
reproductions do not identify the exact message sequence in the owner's app.
Routine delivery receipts and scheduling remain the primary live idempotency
guards. This change does not replay historical work or erase its evidence.

Validation ran in the isolated Linux builder against the 0.48.35 source.
Publishing and changing the installed app require the separate release approval
defined in AGENTS.md.
