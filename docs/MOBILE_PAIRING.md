# Connect the mobile app

From the account menu or Settings → General, choose **Connect mobile app**.
The code represents the signed-in account and active profile on that server.
In the phone app choose **Add account → Scan pairing code**, scan it, check the server
address, and connect. Pasting a pairing link is also supported.

Pair additional accounts the same way. Different servers and different accounts
on the same server remain separate. Pairing an account already saved on the phone
refreshes its session rather than adding a duplicate. Existing other accounts are
preserved. Phone sign-in gets a separate session; it does not copy the computer's
session credential.

## Reachability

The mobile apps require a reachable HTTPS origin with a trusted certificate.
A localhost address refers to the phone itself, so it cannot identify a server on
a computer. Enter the computer/server's reachable HTTPS address in the pairing
dialog instead. A QR code does not change listening interfaces, configure TLS,
join a VPN, or bypass a firewall.

For direct network access to Standalone, Server admin → Network can change the
listening address to All interfaces. Save and restart when active work permits.
This only changes the listener; HTTPS still requires a trusted proxy or a service
such as Tailscale Serve. Add that HTTPS origin to Network → Connection addresses
(or the hosted server’s `public_url`/`allowed_origins`) so the phone web view can
make authenticated requests. When using a VPN, the phone must also have access to that
network. A loopback-only backend behind a reachable HTTPS proxy is valid and does
not require exposing its underlying HTTP port.

The phone shows a connection error with expandable help for unreachable servers.
An expired or used pairing code is a separate error: create a fresh code on the
computer. If a claim response is lost, the code may already have been consumed;
Kindred does not retry the sign-in automatically.

## Lifecycle and account boundary

- Codes expire after five minutes and are consumed atomically once.
- Generating another code from the same session replaces its older code.
- Closing the pairing dialog cancels its unused code. If the client disconnects
  before cancellation arrives, the five-minute expiry still applies.
- Pending codes are bound to the issuing session. Sign-out, password reset,
  session expiry, account disablement, or profile removal prevent claims.
- QR generation happens on the Kindred server with no external QR service.
  Only the code's digest is stored. The raw code is in a custom-link fragment,
  never an HTTP URL query or a reusable account password/session token.
- The phone validates the link locally, requires confirmation of the origin,
  refuses redirects and verifies the claimed account/profile before saving.
- Legacy shared-owner-token sessions must first be converted to an account.

Pairing does not enable production push delivery by itself. APNs/FCM setup and
notification permission remain separate. iOS compilation and camera acceptance
require macOS/Xcode and a device or simulator; Linux shared-UI or core tests are
not a substitute for that native acceptance.
