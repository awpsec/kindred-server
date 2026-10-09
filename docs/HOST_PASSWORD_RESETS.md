# Recover an account from the trusted server console

A person with trusted shell access to the Kindred server can list, approve or deny a password-reset request, including a request for the sole administrator. This is a local operator capability. It does not add an unauthenticated HTTP route, and signed-in web administrators still cannot approve their own request.

First, use **Forgot password** in the requesting browser. Keep that browser open. The host command uses the request **ID**, not the browser's secret token or a reset link. After approval, that same browser polls the existing status endpoint and asks for the new password and confirmation. The host command never accepts a password or completes the reset itself.

## Docker Compose

With the shipped `compose.yaml`, run these commands from the directory containing your deployment's Compose file:

```sh
docker compose exec --user 1000:1000 server /usr/local/bin/kindred --config /data/server.toml password-reset list
docker compose exec --user 1000:1000 server /usr/local/bin/kindred --config /data/server.toml password-reset approve REQUEST_ID
docker compose exec --user 1000:1000 server /usr/local/bin/kindred --config /data/server.toml password-reset deny REQUEST_ID
```

Replace `REQUEST_ID` with an ID from the list. The standard entrypoint writes `/data/server.toml`, selects `/data/profiles/accounts.db`, and runs the server as user 1000. `exec` runs the installed binary directly: it does not run the entrypoint again or restart the container. Use your actual Compose file/service if your deployment differs. The installed image must contain this command; updating source on the host does not update a running container.

## Native server

The example systemd service uses `/etc/kindred/kindred.toml`:

```sh
cd /var/lib/kindred
/usr/local/bin/kindred --config /etc/kindred/kindred.toml password-reset list
/usr/local/bin/kindred --config /etc/kindred/kindred.toml password-reset approve REQUEST_ID
/usr/local/bin/kindred --config /etc/kindred/kindred.toml password-reset deny REQUEST_ID
```

Run as the server's OS user, or a trusted administrator who can access that user's registry. Select the **same configuration and working directory as the running server**. A relative `profiles.directory` is relative to the process working directory, as it is for `serve`; it is not relative to the configuration file. An absolute directory avoids ambiguity. Do not guess a historical `accounts.db` or copy a live database into another location.

The command requires account profiles to be enabled and opens the existing `profiles.directory/accounts.db` without creating a registry or directory. It supports `import_legacy` without opening the legacy workspace, requiring its server token, starting listeners, providers, schedulers or VMs. A profiles-disabled personal server has no account registry to recover with this command. Only reset schema/audit migrations and the requested decision are written; profiles and computer files are untouched.

## Read the result

`list` prints JSON containing request ID, username (or null for an unavailable account), creation/expiry Unix timestamps, effective status, observed connection IP, any existing location, disabled-account flag and decision attribution. Metadata is not identity proof; an IP can belong to a reverse proxy. Expired requests can appear until the existing incoming-request cleanup removes them. No reset token, digest, password or session secret is listed.

A pending request lasts 24 hours. Approving or denying replaces that expiry with the existing 15-minute decision window. Approval still requires the requesting browser's token, matching new-password fields and an enabled account. Successful completion is single-use, invalidates account sessions and device links, and preserves the account's workspaces. Unknown/unavailable, expired and already-handled requests cannot be approved. Concurrent decisions serialize; only one can change a pending request. A nonzero command result is a refusal or database/configuration failure, not an approval. If another operator already handled it, list again to see its state.

Incoming requests emit a sanitized `password_reset_requested` JSON log event with ID, username, time and observed IP. Decisions emit `password_reset_decision`, including `local-host` or `web-admin` attribution and outcome. Decision outcomes are also recorded atomically in the registry's `password_reset_audit` table; completion or ordinary expiry cleanup does not delete that audit. Console stderr from `docker compose exec` appears in the operator's terminal, not automatically in the main server process's Docker logs. Keep operational audit backups under the same protections as the registry. User-controlled controls/bidi characters are removed, metadata is bounded, and URL/credential-shaped metadata is redacted. Do not paste reset links, passwords, tokens or registry dumps into a support report.
