# Long-running tasks

Healthy tasks can keep working for hours. Kindred no longer ends every task after
30 minutes. This changes the server runtime; it does not depend on a VM reinstall.
The server must stay awake and running, and provider quotas still apply.

## Separate limits

- **Model requests:** `request_timeout_seconds = 600` bounds each Pi model request
  and the subscription-provider frame wait. It is not a total task duration.
  Provider-specific connection and tool timeouts still apply.
- **Inactivity:** `idle_timeout_seconds = 1800` pauses a task without recorded work.
  Repeated keepalives and usage packets are not progress. An outstanding tool uses
  its execution timeout, with a no-progress backstop of at least one hour
  (or the configured idle window if longer). Status-label changes alone do not
  renew the window.
- **Task duration:** `task_timeout_seconds = 0` means no total duration cap. Set a
  positive value to apply an active-time cap, up to seven days. Waiting for a
  person or approval does not consume it. It is not a wall-clock deadline.
- **Actions:** `task_action_limit = 10000` bounds tool actions across a task and its
  safe provider retries. A nonzero `max_steps` can impose a lower limit. This is
  an action ceiling, not a dollar budget or a claim that every loop is detected.
- **Native Claude connectors:** a dispatched native connector call has a fixed
  one-hour execution deadline. Intermediate frames do not extend it. A timeout
  leaves its external outcome unconfirmed; Kindred does not replay it blindly.
  Long external processes should use a background job plus a durable wait/check
  instead of one blocking connector call.

A runtime or action limit preserves Activity and interrupts the task. **Continue**
starts a deliberate continuation using the existing recovery record. Clicking it
twice cannot queue two continuations. Completed external actions are not undone,
and an unconfirmed write must be checked before repeating it. Stop and account
revocation remain authoritative.

## Existing installations

The old `run_timeout_seconds` field is still accepted so existing TOML files load.
An omitted value, zero, or the former shipped default of `1800` no longer imposes
a total task cap. Other legacy values preserve the customized cap. The new
`task_timeout_seconds` explicitly overrides that legacy setting, including zero.
To deliberately retain a 30-minute total cap, set `task_timeout_seconds = 1800`.

The request timeout is independent of both fields. No database or workspace
migration is required. Existing running tasks are not silently restarted.

## Scheduling and restarts

"Work overnight" can now span many model and tool calls without the former
blanket cutoff. It does **not** create a scheduled wake-up, guarantee completion,
or turn a testing window written in chat into a server-enforced boundary. Use
saved routines and command waits for durable scheduled/background work. An
external scan already launched may continue after Kindred stops its turn.

A server restart still interrupts an in-flight model turn. Existing recovery and
background-job records are retained; Kindred does not automatically replay an
interrupted task or an external action with an uncertain result.

## Rollback

Before rolling back to a server predating this policy, restore its backed-up
configuration. Older binaries reject the new configuration keys and reject
`run_timeout_seconds = 0`; remove the new keys and restore the previous valid
legacy timeout. This update does not rewrite the configuration file.

The CLI helper also drops its old streaming-event count limit. Existing guests
receive that helper through the normal bot-computer update mechanism; until
updated, their old helper can still stop after 10,000 streaming events.
