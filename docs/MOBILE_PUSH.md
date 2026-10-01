# Native mobile push

Kindred servers in account mode (`[profiles] enabled = true`) can send native
push alerts to the Android and iOS apps. The server delivers directly to Apple
Push Notification service (APNs) and Firebase Cloud Messaging (FCM). There is no
relay. Push is off until credentials are configured.

## What a push contains

Every alert is generic: title `Kindred`, body `You have a new update.` Chat
text, bot names, previews and the server address are never sent to Apple or
Google. The custom data has five strings:

| Key                 | Value                                               |
|---------------------|-----------------------------------------------------|
| `account_id`        | Kindred account UUID that registered the device     |
| `installation_uuid` | The `{installation_uuid}` used to register          |
| `profile_id`        | Owned profile where the event happened              |
| `chat_id`           | Conversation to open                                |
| `event_id`          | Notification feed event ID (decimal string)         |

The app routes a push by `installation_uuid` first, which identifies the saved
backend registration on that phone. `account_id` is a consistency check; it can
repeat across migrated or cloned servers. A push that is unknown or ambiguous
must not open a different account. The app switches to `profile_id` (one of
that account's profiles) before opening `chat_id`. It then fetches real content
over its normal authenticated connection. On iOS the data keys are
top-level next to `aps`; `aps.thread-id` is the chat ID. On Android they are FCM
`data` with a `notification` block and high priority. Alerts expire after one
hour at the provider.

## Configuration

Credentials are read once at startup from files named by environment variables.
Paths must be absolute; files must be regular and at most 64 KiB. Store them
outside Git and the data directory, readable only by the Kindred service user,
for example with `chmod 600`. When the container is used, mount them read-only
and set the variables in the Compose environment.

### APNs (iOS)

| Variable               | Value                                              |
|------------------------|----------------------------------------------------|
| `KINDRED_APNS_KEY_FILE` | Path to the `.p8` token-signing key (ES256)       |
| `KINDRED_APNS_KEY_ID`  | 10-character key ID                                |
| `KINDRED_APNS_TEAM_ID` | 10-character Apple Developer team ID               |
| `KINDRED_APNS_TOPIC`   | App bundle identifier                              |

Kindred signs a provider token and reuses it for 40 minutes. Each registration
chooses `api.push.apple.com` (`production`) or `api.sandbox.push.apple.com`
(`sandbox`). Development builds must register as `sandbox`; TestFlight and App
Store builds use `production`. The host is not otherwise configurable.

### FCM (Android)

| Variable                           | Value                                  |
|------------------------------------|----------------------------------------|
| `KINDRED_FCM_SERVICE_ACCOUNT_FILE` | Google service-account JSON key        |

The account needs permission to send Firebase Cloud Messaging messages for
that project, for example the Firebase Cloud Messaging API Admin role. Kindred
uses only `type`, `project_id`, `client_email` and `private_key`. Any
`token_uri` in the file is ignored; access tokens always come from
`https://oauth2.googleapis.com/token`, and messages always go to
`https://fcm.googleapis.com/v1/projects/<project_id>/messages:send`. Redirects
are refused.

If any APNs variable is set but the set is incomplete or invalid, or the FCM
file is invalid, that platform is disabled and one reason is logged without
credential contents. The server still starts. Check the startup log after
changing credentials.

## API

All routes need an account session (`Authorization: Bearer <token>`). Legacy
personal-server tokens receive `401`.

- `PUT /api/mobile/devices/{installation_uuid}` with
  `{"platform":"android"|"ios","token":"…","environment":"production"|"sandbox","account_id":"<uuid>"}`.
  `account_id` must be the signed-in account: the `account_id` returned by
  `GET /identity/profiles`, not a client-local ID. The installation must be a
  hyphenated UUID. iOS tokens are 64–200 hex characters. FCM tokens are
  16–4096 characters of letters, digits and `-_:.`; unknown fields are rejected.
  The installation is registered in every profile the account owns. The
  response is `{"registered":true,"installation_uuid":…,"platform":…,"profiles":n,"delivery_enabled":bool}`.
  `delivery_enabled` is false when that platform has no credentials. The
  registration is still stored.
- `DELETE /api/mobile/devices/{installation_uuid}` removes the installation
  from every owned profile and returns `{"removed":bool}`. Repeating it is safe.
- `GET /api/mobile/push-status[?installation_uuid=…]` returns
  `{"enabled":bool,"platforms":{"ios":bool,"android":bool}}`. With an
  installation it also returns `"registered"`, `"profiles"` and
  `"registered_profiles"`. `registered` is true only when every owned profile
  has the installation.

Each profile can have 32 devices. Registering a token already used by another
installation in that profile moves it to the new installation.

## Scope: every profile the account owns

A registration covers the signed-in account's owned profiles, not just the open
one. Each owned profile's private database stores its own copy of the
installation, with its own join point, cursor and outbox. Alerts from any owned
profile carry that `profile_id`. A profile created later, through **New
profile** or sign-in with a new profile name, copies the account's live
registrations, starting at its own present. Other users' profiles are never
touched. A registration is live only in a profile its account still owns.

Covered: everything in each owned profile's private notification feed. That
includes results from your own bots in shared rooms (`server-…` chats).

Not covered: messages from other people, and other people's bots, in shared
multi-account rooms. They are not in your private feed, so mobile push does not
send them.

## Session binding

Each registration is stored in that profile's private database with the
SHA-256 digest of the session that created it. A device receives pushes only
while that session is unexpired and unrevoked, its account is enabled, and that
account still owns the profile. The session may be on any owned profile.

- Signing out removes that session's registrations immediately. **Sign out
  everywhere** removes the account's registrations from all its profiles.
- Switching profiles rotates the session. Registrations in every owned profile
  move to the new session in the same registry transaction.
- Changing the password on the phone does the same. Other devices are signed out
  and lose their registrations.
- Expiry, an administrator disable or account removal ends the session. The
  worker deletes those registrations on its next pass.

Clients should `PUT` again after signing in, receiving a new provider token or
any token change they did not get from `/identity/switch` or
`/identity/password`. Repeating the call with the
same values is safe.

## Delivery

One gateway worker serves all profiles. It starts only when at least one
platform is configured. Every two seconds it:

1. removes registrations whose sessions have ended;
2. reads new items from that profile's notification feed (`Db::notifications`),
   the same feed used for desktop alerts, including its notification level,
   bot and chat mutes, disabled bot notifications, archived bots, quiet routine
   runs and resolved questions or approvals;
3. adds one outbox row per item and registered device in one transaction with a
   persisted cursor;
4. sends up to 32 due alerts per profile, eight at a time, with a 30-second limit
   per send. Before a retry, it checks the event against the current feed rules.
   It drops the alert if the chat or bot is now muted, the notification level
   changed, or the question or approval was resolved.

There is no historical flood:

- Each installation records the latest event when it first registers. It never
  receives earlier events, even if other devices on the profile have not yet
  been sent those events. Refreshing a token or session keeps this join point.
  Deleting and re-registering an installation resets it to the present.
- Events more than 15 minutes old when first read are skipped.
- Removing the last device clears the shared cursor and outbox.

Results are recorded per alert:

| Provider result                                                | Action |
|----------------------------------------------------------------|--------|
| Accepted                                                       | Row removed |
| Network failure, timeout, `429`, `5xx`                         | Retry after 20 s, doubling to 30 min; `Retry-After` honored |
| APNs `403` / FCM `401`, `403` (credentials)                    | Retry with backoff and log; FCM drops its cached token, APNs re-signs at most every 20 min |
| APNs `410`, `BadDeviceToken`, `DeviceTokenNotForTopic`; FCM `404`, `UNREGISTERED`, invalid token argument, `SENDER_ID_MISMATCH` | Registration with that token deleted |
| Other `4xx`                                                    | Alert dropped and logged |

An alert is dropped after eight attempts or one hour. Each profile has at most
512 pending alerts; the oldest are dropped first.

## Limitations

- Account mode only. Single-user servers without `[profiles]` have no account
  sessions to bind, so these routes do not exist there.
- Only owned profiles' private notification feeds are pushed. Messages from
  other people, and other people's bots, in shared multi-account rooms are not.
  This is account-wide for your own bots, not full shared-room coverage.
- New-profile inheritance happens when the profile is created. If copying fails,
  it is logged, and the phone covers that profile after its next `PUT`.
- First attempts use the eligibility from when the event was read, about two
  seconds earlier. Only retries are re-checked.
- There is no separate quiet-hours schedule. "Quiet" covers the existing
  notification level, mutes and quiet routine rules.
- Delivery polls every two seconds, so alerts arrive within about two seconds
  plus provider latency. Alerts are not sent while the server is down, and
  events missed for more than 15 minutes are not delivered later.
- Push tokens are stored in plaintext in each private profile database, because
  they are needed for sending. They are never returned by the API, logged or
  included in workspace transfers.
- Credentials are read only at startup. Restart after rotating keys.
- Adding HTTP/2 support for APNs means other outbound HTTPS clients may now use
  HTTP/2 when a server offers it.
- These checks use simulated APNs/FCM responses and locally generated keys. A
  real delivery has not been verified with Apple or Google credentials or with
  physical devices.
