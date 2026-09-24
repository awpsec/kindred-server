# Computer updates

Kindred updates packages inside the shared bot VM during downtime. This is enabled by default and can be turned off in **Settings > Bot Computer > Computer updates**.

The coordinator waits for at least 15 minutes without queued or active tasks, manual screen control, screen recovery or workspace transfer. It also reserves all bot screens and the provider connection lock before starting. It does not start an offline computer for maintenance. The guest must have finished setup and been running for at least 15 minutes.

One package attempt is allowed every three days, including failures and uncertain dispatches. A confirmed startup deferral is not a package attempt. The guest also enforces this interval, so a server restart or repeated request cannot launch a duplicate update.

The dedicated Debian-family VM runs `apt-get update` followed by `apt-get --yes --with-new-pkgs upgrade`. Existing configuration files are retained. This installs updates and new dependencies without removing packages, changing the distribution release or rebooting. No AI provider is used for maintenance, and it does not update the server operating system or the person's desktop. The managed Pi harness on the server is updated alongside the guest provider runtimes.

New tasks wait while packages install. Turning automatic updates off stops future attempts; it does not interrupt an installation. A systemd service owns the package process so an SSH disconnect or Kindred restart does not terminate dpkg. The server retains its reservation until the guest confirms the service has finished. If the VM went offline, **Start computer** restores the check; reboot and shutdown remain unavailable during an active update.

The status shows the latest successful update or a concise failure. A reboot recommendation remains until the guest confirms a new boot, including after a partial upgrade. Automatic updates leave rebooting to the owner; manual updates restart after successful installation. Detailed package output stays inside the VM at `/var/lib/kindred/maintenance/packages.log` and is bounded to about 1 MB. The helper requires the normal Kindred guest installation and its passwordless sudo permission; missing prerequisites are reported without blocking ordinary tasks.

Maintenance state belongs to the VM and is excluded from workspace transfers. The operating guide tells bots not to create duplicate package-update routines. Task-specific dependency installation still follows the task's existing permissions.

If dispatch cannot be confirmed, Kindred reconciles the exact request ID under the guest's dispatch lock. Each closed request receives a small durable result in `/var/lib/kindred/maintenance/results`, including startup deferrals, low-disk failures and requests superseded by an already-running update. A delayed copy of a closed request cannot start after normal work resumes. Completed jobs are archived before later jobs replace them, and earlier cancellation records in `maintenance/cancelled` remain honored. A fresh request ID is required after a deferral; low-disk failures retain the three-day cooldown. An older completed job is never reported as the new attempt's success. Restarting Kindred also begins a fresh observed idle window before a new update may start.


## Manual computer updates (2026-09-22)

Bot Computer settings exposes **Update now** beside **Update automatically**.
The authenticated POST `/api/computer/maintenance` durably requests one update,
even when automatic updates are disabled or the three-day cooldown has not
elapsed. Repeated clicks coalesce. Active runs, commands, screen control and
workspace transfers still block installation; requests for a stopped VM wait
until it is started. The updater checks for pending requests every five seconds.

Both update paths now update all managed provider runtimes: the complete verified
Codex package, Claude Code, Kimi CLI, and the server-side Pi SDK harness (when
installed). Pi uses the configured worker installation directory; a managed
`current` symlink is required. New runtimes are staged and checked before their
entrypoints switch. Account homes, credentials, bot profiles and documents are
not replaced. Codex and Claude downloads are checked against upstream package
digests; Pi and Kimi use their official registries. A failed runtime check retains
that runtime's previous entrypoint and fails the update; this is not a transaction
that rolls back already successful updates to other providers.

After a successful **manual** update, the guest restarts. Maintenance stays busy
through the reboot, and queued tasks resume only after the guest reports a new
boot ID. Failed installs do not reboot. Automatic updates retain the existing
reboot-recommended behavior. Provider entitlement still controls which models
are offered; refresh the model picker after updating.

Validation: offline guest lifecycle, installer integrity/failure tests, provider
bridge tests, authenticated maintenance API/state tests and WebKit settings tests.
These tests do not install packages or reboot a production VM.

### Live owner VM update

On 2026-09-22, the owner requested an immediate runtime update of the existing
production-server profile VM. Codex 0.156.0, Claude Code 2.1.280 and Kimi CLI 1.52.0
were installed, the guest rebooted, and its authenticated Codex catalog returned
GPT-6 Luna, Sol and Astra. Guest readiness and server health passed. The original
4 CPU / 8192 MiB / 75 GiB configuration was preserved. Backup on production-server:
`/var/backups/kindred/runtime-20260923T012213Z`. The existing server remains
0.63.0; server-side Pi needs the new compatibility gate in the next deployment.
Legacy provider paths redirect to the updated runtimes, with their original
files/directories retained under `.before-20260922`, so 0.63.0's bridge refresh
cannot silently select the older Claude/Kimi runtimes.

Live validation exposed new optional voice resources in the official Codex
package. Extraction now preserves safe regular files under `codex-resources/`,
while still requiring every core runtime file and rejecting traversal, links,
duplicate files and invalid sizes. Seven installer tests and the actual 0.156.0
offline tool protocol check passed.

The owner subsequently found Codex unavailable and VM controls ineffective.
The manual maintenance restart had launched QEMU as root, creating a root-owned
monitor socket/PID file inaccessible to the Kindred service user. The guest was
gracefully stopped and restarted as `kindred` inside the existing service cgroup.
Service-user `domstate`, `connection-status`, Start, Reboot and authenticated
Codex model discovery then passed; public HTTPS health passed too. This was an
operations ownership error, not a Codex runtime/sign-in failure. Future manual
maintenance must preserve both the service UID/groups and its cgroup. VM manager
source now reports permission errors explicitly instead of treating inaccessible
control sockets as stopped computers; settings show control failures inline and
block overlapping actions. These UI/manager changes remain unreleased.
