# Browser sign-in and verification

Bot browser profiles are persistent and screen-specific, both on a hosted server and in a standalone VM. Existing Chromium launches do not disable password saving. The user can accept the browser's Save password prompt; bots may use normal masked autofill for the verified service/account. Password-manager unlocking and revealed-secret screens stay with the user. Do not inspect/export credential stores. Per-screen profiles are not a security boundary against privileged tools in the shared VM or its host.

The operating guide directs bots encountering an expired session to try the authorized saved login, then request human help rather than silently treating an inaccessible inbox as empty. Existing user-action notifications respect notification and mute preferences. This is a response to an observed login failure, not a background session-expiry detector.

`request_user_action` accepts optional `authentication` metadata:

- `service`: display name (required)
- `method`: `signin`, `sms`, `email`, `authenticator`, `push`, or `security_key` (required)
- `destination`: the observed masked delivery detail, if present
- `code_length`: exact observed character count (4–16), omitted when unknown; the UI uses one accessible input rendered as individual slots
- `submission`: `enter` (default) or `automatic` for sites that submit on code entry

Code methods render a masked 4–16 character alphanumeric field. The bot focuses the real website code field and observes the page before requesting help. The UI stays in chat and sends input to `/api/user-tasks/{id}/code`. The endpoint checks the pending task, bot, run, method, absence of manual control and code format under the screen lease. A durable attempt marker prevents repeated delivery, including after uncertain failures. It sends text to the guest without a model tool event, database code storage, or returned guest output. Browser requests and guest process memory necessarily carry the input transiently. No code is added to chat or a draft. Never log request bodies at a reverse proxy.

Submitting a code types into the prepared field, presses Return unless automatic submission was specified, and readies the existing task for resumption. The bot must observe the resulting page before continuing; submission does not prove successful authentication. Sites may render entered codes on the screen, so the private route prevents text/history disclosure but cannot promise that a website will never display a code. Manual takeover remains available when needed. Push/security-key methods retain the human handoff without a code input. Expired/cancelled tasks cannot accept input. An uncertain delivery error leaves the task paused and requires checking the screen; the same request cannot inject a code twice. A rejected/expired code can get a fresh handoff request after the current step is resolved.

Validation uses backend task-boundary tests and WebKit/Chromium UI fixtures. It does not establish acceptance by any live service or encrypted password-store behavior on every guest image.
