# Linux host and desktop setup

These steps describe a new Linux host installation. For Docker hosting, see
[HOSTING.md](HOSTING.md). Desktop updates are covered in
[GITHUB_UPDATES.md](GITHUB_UPDATES.md).

## 1. Prepare the host and keys

Use an x86_64 Linux host with working libvirt/KVM, the `default` NAT network, QEMU
tools, virt-install, UEFI firmware, Python 3, and OpenSSH. Provisioning does not install
host packages or delete/replace existing domains. Obtain a Debian 13 generic cloud
QCOW2 from [Debian's official images](https://cloud.debian.org/images/cloud/trixie/)
and verify its published checksum before use.

As an administrator, create a system `kindred` user with home `/var/lib/kindred`, a
private `.ssh` directory, and a dedicated ed25519 SSH key. Create a second guest
administration key in a root-only directory. The service must never receive the
administration private key. Use a new domain name and disk path:

```sh
sh deploy/provision-vm.sh kindred-bots /path/to/verified-debian.qcow2 \
  /var/lib/libvirt/images/kindred-bots.qcow2 \
  /var/lib/kindred/.ssh/id_ed25519.pub /root/kindred-admin.pub
```

This creates a 1-vCPU, 1536-MiB, 8-GiB thin VM for a single light workload.
For several simultaneous screens, allocate at least 2 vCPUs and 4 GiB, or set
`max_parallel_runs = 1` for the smaller guest. Cloud-init creates a `bot` account
with passwordless guest sudo and a separate `kindred-admin` account, and installs a
pre-generated SSH host key. The script prints its root-only cloud-init state directory.
Keep the state confidential: it includes the guest host private key. The resulting
seed media also contains that provisioning key. The script does not enable VM autostart.

Find the new guest lease with `virsh net-dhcp-leases default`. Reserve that exact
MAC/IP using libvirt's `net-update` without replacing the network configuration.
Build known_hosts from the **pre-generated** `ssh_host_ed25519_key.pub` in the
reported state directory, using the guest IP as the host field. Do not blindly
trust ssh-keyscan output. Copy `deploy/ssh-config.example` to the service account's
`.ssh/config`, set the actual IP, and set ownership/mode 600 for keys and SSH config.

## 2. Install inside the VM

Build the Linux binary. Copy it as `kindred`, together with `install-guest.sh`,
`start-desktop.sh`, `start-desktop-shell`, `desktop-launch`, the complete `desktop/`
asset directory, `ensure-screen`, `wait-screen-ready`,
`kindred-screen@.service`, `kindred-guest-desktop.service`, `kindred-vnc.service`,
and `kindred-vnc-view.service`, into a staging directory in
the guest using the separate administrator key and pinned host key. Run there:

```sh
sudo sh install-guest.sh
```

This installs Debian browser/desktop packages and a service running as `bot`, prepares
`/workspace`, and installs the guest-only CLI wrapper. The Chromium sandbox is enabled.
On older installations, an administrator must run the current installer inside
the guest as root to apply guest software and sudo changes. A server upgrade
preserves the existing guest disk and does not apply these changes by itself.

Verify that `sudo -n id` as `bot` returns guest root and that guest screenshot RPC works through
the service account's pinned SSH alias before configuring inference.

Install the official [Codex CLI release](https://github.com/openai/codex/releases)
inside the VM at `/usr/local/bin/codex` if using subscription authentication. Version
0.153.4 was tested. Verify the release archive against its official SHA256 metadata.
The CLI is substantially larger than Kindred and is not bundled in this repository.

The reusable provisioner was syntax checked; its revised separate-admin-key flow
was not used to create a second VM. The existing test installation was provisioned
with the same key/account separation through a dedicated setup script.

## 3. Install the server

Install the compiled Linux executable at `/usr/local/bin/kindred`. Create
`/etc/kindred` and a mode-700 `/var/lib/kindred` owned by the service user. Copy
`deploy/kindred.example.toml` to `/etc/kindred/kindred.toml` and set the actual domain
and pinned guest SSH alias. Keep listen/public_url on localhost for the SSH tunnel.

Generate the application token once into a protected environment file:

```sh
umask 077
printf 'KINDRED_TOKEN=%s\n' "$(/usr/local/bin/kindred token)" > /etc/kindred/kindred.env
chmod 600 /etc/kindred/kindred.env
```

Do not run that command over an existing installation unless intentionally rotating
the token. For OpenRouter, use Settings → Connections, or add `OPENROUTER_API_KEY` to this protected environment file
through a trusted administrator workflow. No provider key belongs in kindred.toml.

## 4. Scope VM control

The recommended configuration uses a root-owned helper at
`/usr/local/libexec/kindred-virsh`. Substitute your actual domain and its verified
UUID from `virsh domuuid kindred-bots` in this example:

```sh
#!/bin/sh
set -eu
[ "$#" -eq 1 ] || exit 2
case "$1" in domstate|start|shutdown|reboot) ;; *) exit 2;; esac
uuid=REPLACE_WITH_VERIFIED_VM_UUID
[ "$(/usr/bin/virsh --connect qemu:///system domuuid kindred-bots)" = "$uuid" ] || exit 1
exec /usr/bin/virsh --connect qemu:///system "$1" --domain "$uuid"
```

Make the helper root-owned and mode 755. The sudoers rule should authorize only:

```text
kindred ALL=(root) NOPASSWD: /usr/local/libexec/kindred-virsh domstate, /usr/local/libexec/kindred-virsh start, /usr/local/libexec/kindred-virsh shutdown, /usr/local/libexec/kindred-virsh reboot
```

Validate the mode-440 sudoers file with `visudo -cf` before continuing. Do not grant
generic sudo or libvirt-group membership. Copy `deploy/kindred.service` into
`/etc/systemd/system`. The base service sets NoNewPrivileges=true. To allow only
this explicitly configured sudo helper, create a service drop-in:

```ini
[Service]
NoNewPrivileges=false
MemoryMax=128M
CPUQuota=25%
```

That weakens the service's no-new-privileges restriction, so the helper and sudoers
scope matter. The quota caps the small host service, not the VM. Check configuration,
then enable/start the service:

```sh
/usr/local/bin/kindred --config /etc/kindred/kindred.toml check
systemctl daemon-reload
systemctl enable --now kindred.service
```

## 5. Connect

The included Windows launcher expects passwordless existing SSH access, a pinned
server SSH key, a default localhost port of 7340, and permission to read the application
token. Its default target is the authorized test host root@production-server. For another host:

```powershell
.\Start-Kindred.ps1 -Server admin@yourhost
```

That SSH account must be able to read the protected token; a normal unprivileged
account cannot. The launcher never changes SSH trust or authentication settings.
For manual browser access, create a loopback SSH tunnel and paste the server access
token into Kindred's connection screen. A non-loopback public URL must use HTTPS
and match the configured public_url Origin.

## Operations and backup

Closing the desktop does not stop tasks, the host service, or the VM. Disable unwanted
routines before leaving it unattended. Shutting down the VM retains files and browser
sessions. Keep VM autostart disabled when occasional testing is the goal.

Stop the service before taking an ordinary file-copy database backup, or use SQLite's
online backup API. Back up guest workspace/profile data separately; the server database
does not contain them. Treat both backups as sensitive. Restart does not automatically
replay interrupted tasks. Inspect any external side effects before manually resubmitting.

OpenAI credentials are managed from Connections and stored only in the guest Codex
home. Restarting the Kindred service interrupts a pending device login; start a fresh
login if its code expires. Guest package updates and Codex CLI updates remain operator
responsibilities. Review provider protocol changes before updating Codex.

## Live desktop and Tailscale

The guest installer enables `kindred-vnc.service` on loopback 5900 and
`kindred-vnc-view.service` on loopback 5901. Both need the existing Xvfb desktop on
`:1`. Do not expose those raw ports on the guest network. The service SSH identity
needs forwarding to the guest-loopback port pairs (5900 through 5963 for the
32 supported screens). Keep any SSH `PermitOpen` list aligned with those ports.

The installer also enables linger for the unprivileged `bot` account and installs
`/etc/systemd/user/kindred-screen@.service`. Additional desktops start on demand;
no root privileges are granted to the bot. For an existing installation, copy
`deploy/ensure-screen`, `deploy/start-desktop.sh`, and `deploy/wait-screen-ready` into `/usr/local/lib/kindred/`
(mode 755), install the template in `/etc/systemd/user/`, run `loginctl enable-linger bot`,
and reload the bot user manager. Update both the host and guest Kindred binary.
The desktop shell also requires tint2, feh, xterm, pcmanfm, and librsvg2-bin. Install
`start-desktop-shell` and `desktop-launch` beside the screen scripts, and the
`deploy/desktop/` files in `/usr/local/share/kindred/desktop/`. Convert each SVG
there to a matching PNG with `rsvg-convert` as shown in `install-guest.sh`.
The template waits for the dock and, when restoring a saved browser profile,
Chromium's first visible window before navigation can run. New profiles begin
on the wallpaper with the Browser, Files and Terminal dock. For existing live
screens, run `start-desktop-shell` as bot with that screen's DISPLAY; its per-screen
lock prevents duplicate docks. No screen or browser restart is needed to add it.
If custom VNC ports are used, the view port must follow the control port; also set
`KINDRED_VNC_BASE_PORT` in the user template and update the two primary VNC units.

For a private remote UI, first inspect `tailscale serve status` and choose an unused
HTTPS port. This installation uses 9446; do not replace an existing Serve listener.
Set the host configuration before restarting Kindred:

```toml
listen = "127.0.0.1:7340"
public_url = "https://YOUR-SERVER.YOUR-TAILNET.ts.net:9446"
allowed_origins = ["http://127.0.0.1:7340"]
```

Then on the host run `tailscale serve --bg --https=9446 http://127.0.0.1:7340`.
This uses private tailnet Serve, not Funnel. The application still requires its
access token. To launch the supplied desktop directly over HTTPS:

```powershell
.\Start-Kindred.ps1 -ServerUrl https://kindred.example.com:9446
```

The launcher still uses the configured SSH account to retrieve the token securely.
Without `-ServerUrl`, it defaults to the localhost SSH tunnel. A generic Kindred
client also accepts a server URL argument and manual token entry.

## Composio marketplace

See [COMPOSIO.md](COMPOSIO.md) for project-key setup, marketplace search, external-browser consent,
scopes, read-only checks and troubleshooting. No public callback or Google password
is required on the Kindred host. The private Tailscale address is not a Google OAuth
redirect URI. `COMPOSIO_API_KEY` is an optional service-environment alternative to the
private Settings field. Keep it out of logs, transcripts and source control.

## Composio key troubleshooting

Connections shows only the apps actually linked to this workspace. Browser shortcuts
are not API connections. Paste a **project API key** from the Composio dashboard into
Connections. Leading/trailing whitespace is removed; a rejected replacement does not
overwrite the saved key. Marketplace discovery needs Toolkit Read permission. App
onboarding and execution additionally need the appropriate auth-config,
connected-account and tool-execution permissions. A scoped key can receive a generic
401 even when the key exists. The inline error retains the provider's sanitized code
and request id for comparison with dashboard logs. Never paste keys into chat or issue
reports. OAuth sign-in remains an explicit browser step by the user.

HTTP 401 with code 801 (`APIKey_InvalidAPIKey`) means Composio rejected the key
as invalid. Copy the full, unmasked project key again; if it cannot be revealed,
create an additional project key. Existing keys used by other clients need not be
revoked. Kindred does not display the key fragments contained in that error.
