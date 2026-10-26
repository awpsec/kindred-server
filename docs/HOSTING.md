# Hosting Kindred

## Linux self-hosted server

For a new manual installation, use the copyable `compose.yml` in the README or
save `deploy/compose.linux.yaml` as `compose.yml`. This setup targets Linux hosts.
It uses host networking and starts with Kindred listening on `127.0.0.1:9444`.
There is no Docker `ports` mapping in this mode. The dedicated host entrypoint
makes older images fail to start instead of accidentally exposing every interface.
Desktop Standalone retains its existing platform-specific Docker networking.

Create the administrator account locally (or through an SSH tunnel), then open
**Server admin → Networking → Set up access**. For private HTTPS, use the Tailscale
steps below and keep the backend listener on this host only. Public deployments
use an HTTPS reverse proxy. Advanced settings can also bind plain HTTP to an
existing Tailscale IP or all IPv4 interfaces; those choices do not enable HTTPS.
Kindred does not install Tailscale or change the host's Serve routes. Loopback
remains available for health checks and host administration. Allowed URLs control
browser origins, not the source networks allowed to connect.

Save persists the settings and restarts the server process. Active tasks may be
interrupted; the VM supervisor and running profile computers stay alive. The page
reconnects and refreshes when its URL remains allowed. If that URL is removed, use
the new-address link and sign in there. The previous configuration is restored if
a saved web generation cannot start. Check container logs if a reconnect times out.

Compose can also configure `KINDRED_ACCESS` (`local`, `tailnet`, `all`),
`KINDRED_TAILNET_ADDRESS`, `KINDRED_PUBLIC_URL`, and `KINDRED_ALLOWED_ORIGINS` (a JSON
array). Unchanged environment values preserve web overrides. Changing a value in
Compose and recreating the container overrides only that saved field. The web
page's **Reset to Compose settings** restores all the environment defaults.
Settings and the last working generation live in the persistent data volume.
Invalid Compose networking values stop startup without replacing saved settings;
correct the environment and recreate the container.

Existing installations using bridge-mode `compose.yaml` keep that topology.
Their web page can change URLs but explains that Docker interface bindings remain
in Compose. Do not silently replace a running bridge deployment with host mode:
review reverse-proxy routing and host port usage first. VM SSH forwards use host
loopback ports 22000–31999; new allocations skip occupied ports.

The server image is Linux x86_64. Assign enough host memory for at least one 6 GB
profile computer plus server overhead. QEMU uses KVM when available, otherwise TCG
software emulation. On a Linux VPS, nested virtualization availability is controlled
by the provider. The Computers page reports startup acceleration and warns when
the container falls back to slow software emulation.

Keep `compose.kvm.yaml` in your existing Compose project. Configure KVM once from
that directory, naming the **base file you already use**:

```sh
python3 scripts/server-setup.py configure-kvm --compose-file compose.yaml
```

If you saved the Linux host-network template as `compose.yml`, pass that filename
instead. Repeat `--compose-file` for other existing overlays, in their existing
order. If `.env` already defines `COMPOSE_FILE`, omit the flag; the helper preserves
that list. It backs up `.env`, records the actual `/dev/kvm` group, and appends the
KVM overlay. It does not change networking mode, start containers or enable host
virtualization. Review its output before `docker compose up -d server`. Subsequent
plain `docker compose pull` / `up -d` commands keep KVM because `.env` retains the
overlay. An explicit `-f` argument or shell `COMPOSE_FILE` overrides that selection;
include all overlays if you use those overrides. The overlay now refuses a missing
group ID instead of guessing a distribution-specific number.

For a read-only setup check, run:

```sh
python3 scripts/server-setup.py doctor
```

It checks host/container KVM access, available storage, container memory limits,
running binary versus checkout version, and outbound HTTPS **from inside the
running server container**. DNS alone does not verify HTTPS downloads. It does
not print credentials or change firewall rules. Run it from the original Compose
project directory, using the same shell/Compose selection as your deployment.
See [hosting troubleshooting](HOSTING-TROUBLESHOOTING.md) for recovery and host
administration checks.

## Private team server with Tailscale HTTPS

Open **Server admin → Networking → Set up access → My devices with Tailscale**.
Hosted and Standalone offer access guidance, a shareable address and phone pairing.
Standalone can apply its managed local setup; on a hosted server, run host setup
steps on that server. Changing **Primary server URL** only changes the address
Kindred advertises and allows. It does **not** install a certificate, enable TLS,
open a firewall or change Docker port publishing.

1. Sign into Tailscale on the server and each client device. Create your Kindred
   administrator account through localhost or an SSH tunnel first.
2. Keep the backend local: in Linux host-network mode choose the host-only listener.
   For bridge Compose use `KINDRED_BIND=127.0.0.1` in your existing `.env`, then
   recreate that project's server if the binding changed. Preserve its volumes.
3. Run `tailscale serve status` on the host. If another service owns the HTTPS root
   on port 443, preserve it and use another HTTPS port. For a free port 443:

   ```sh
   sudo tailscale serve --bg --https=443 --set-path=/ http://127.0.0.1:9444
   ```

   Replace the backend port if your bridge Compose mapping uses a different host
   port. Follow Tailscale's HTTPS prompt when requested. Do not start the Caddy
   profile on a port already used by Serve or another proxy.
4. Copy the **HTTPS URL printed by Serve** into **Primary server URL**. With default
   HTTPS port 443 it looks like `https://your-server.your-tailnet.ts.net`, without
   `:9444`. Only include a port when the HTTPS service actually uses that port.
5. Choose **Check address**, then **Save and restart**. After reconnecting, check
   the saved address again. Open it on the colleague's device, confirm the expected
   workspace, and enter that same URL in **Kindred → Add account**. For an iPhone,
   the Networking page's **Connect mobile app** uses the checked saved address.

The check requests Kindred sign-in metadata from the server, with certificate
validation and without credentials or redirects. Success confirms that a Kindred
endpoint answered from that server; it does not prove which workspace is behind
it or that another device can reach it. The page shows your current connection
separately. A URL that has only been saved is never described as verified.

An HTTPS handshake sent to a plain HTTP port is a protocol mismatch, not evidence
that the client needs a CA installed. Use the HTTPS proxy's address and port.
Tailscale Serve can use a chosen HTTPS port; the restricted list 443/8443/10000
belongs to public Funnel. See the official [Serve command reference](https://tailscale.com/docs/reference/tailscale-cli/serve)
and [Funnel reference](https://tailscale.com/docs/reference/tailscale-cli/funnel).

## Team server with HTTPS

Point a domain at the server and allow inbound ports 80 and 443. Copy `.env.example`
to `.env` and set:

```dotenv
KINDRED_DOMAIN=kindred.example.com
KINDRED_PUBLIC_URL=https://kindred.example.com
```

For existing bridge-mode installations, start the included HTTPS proxy with `docker compose --profile public up -d`.
Keep the persisted KVM overlay configured as above. Caddy obtains and renews
the certificate. Keep the application port bound to localhost. Alternatively use
an existing HTTPS reverse proxy with WebSocket support and set the matching public
URL. Desktop clients accept HTTPS remote origins and loopback HTTP.

For the Linux host-network template, configure a host HTTPS proxy to forward to
`127.0.0.1:9444`. A proxy in its own bridged container cannot reach the server via
`server:9444`; use host networking for that proxy or configure an explicit host
route. Do not reuse the bridge-mode Caddy service unchanged.

Every colleague installs the desktop app, enters that address and creates an account
(or uses the administrator's invitation). They connect their own provider. No SSH
account, host token, guest provisioning command or manually configured model ID is
needed. Use the same account to restore profiles on another device.

## Limits and persistence

CPU, memory and disk values size the VM shared by all bots in one profile.
They do not allocate a VM per bot or limit the number of bot screens.
The resource editor shows available host/container CPU and RAM and permits changes
before first startup. Save and startup check those limits, including cgroup v1/v2
CPU quotas and memory limits. Memory is not reserved by saving; capacity can change
as other workloads run, so startup checks again. A Linux server that is itself a
VM still needs adequate resources assigned by its hypervisor administrator.
For multi-user hosting, `KINDRED_VM_MAX_RUNNING` (default 4) caps concurrently
running **profile VMs across the server**. It is an advanced server capacity limit,
not a limit on bots within a VM. User and profile limits are also configurable. CPU/memory/disk defaults are captured when each computer is first created;
changing these values affects new computers. QEMU rejects starts that exceed the
configured running-computer limit or available host/container memory. No other
profile's computer is automatically evicted. Stop an idle computer from its controls.

The `kindred-data` named volume contains the account registry, profile databases,
credentials, disks, guest keys and image cache. A normal `docker compose down`
followed by `up -d` preserves it. Do not use `down -v` to update. Container shutdown
stops scheduling and requests guest shutdown, with a bounded final QEMU disk flush.

To update, back up your data, then run:

```sh
docker compose pull
docker compose up -d
```

The default image is `ghcr.io/awpsec/kindred-server:latest`, updated with each stable release. Set `KINDRED_VERSION=0.75.0` in `.env` to pin a version. Keep the same project directory/name and named volume when updating. Do not use `down -v`. Existing source-build installations need the current `compose.yaml` once to switch to registry images. For development builds, use `docker compose -f compose.yaml -f compose.build.yaml up -d --build` instead. Desktop Standalone continues to use its bundled server and in-app update controls.

Existing
guest disks and accounts are retained. Software already installed inside existing
guest disks is retained too; this release does not silently rebuild those computers.
New computers use a software image matched to the current guest bundle. Older
images remain available while existing computers may still have them attached.
For an older guest that lacks the updated helper or guest sudo policy, its
administrator must run the current `install-guest.sh` inside that guest as root,
following [guest setup](SETUP.md). Updating the server alone does not install
software or change permissions inside an existing disk.

## Backups and recovery

Stop the Compose project during a consistent full-volume backup. Back up the entire
named volume, not just SQLite files: provider credentials and guest files also live
there. Encrypt and restrict the backup as you would the server. Restore the volume
with the same Compose project name. Keep its ownership at UID/GID 1000 and preserve
private file modes. Never restore one profile's files into another profile directory.

First startup downloads a verified Debian cloud image; the software image includes
the server guest helper and verified official Codex CLI. Guest package installation
runs once. A slow setup can time out while the VM keeps its disk and continues
installing. Inspect the profile computer console, wait and retry the message; no
replacement disk is created. Failed model runs remain in chat history.

Inspect status with `docker compose ps` and server logs with `docker compose logs
--tail 100 server`. Profile console logs live under `profiles/<id>/computer` in the
volume. Do not paste registry files, credential files, session tokens or SSH private
keys into support messages.

## Migrating an existing native service

Existing personal installations may keep their original libvirt guest while enabling
`profiles.enabled = true`, `profiles.import_legacy = true` and an absolute profiles
directory. Install the managed VM helper, guest software bundle, QEMU and genisoimage;
set matching `KINDRED_PROFILES_DIR` and `KINDRED_GUEST_SOFTWARE` environment variables.
The service needs a writable profiles directory, KVM group access when available,
and a memory/CPU budget that includes its managed guests. Its old single-process
128 MB budget cannot contain a 6 GB QEMU computer.

The supplied `scripts/native-stop-computers.py` can be installed as a systemd
ExecStop helper with `$MAINPID` and a 90-second stop timeout. It stops the server
scheduler, requests managed guest shutdown and flushes remaining QEMU processes at
the deadline. It does not stop or modify the separately managed legacy libvirt VM.
Back up the existing config, binary, database and credential files before enabling
migration. Claim the owner account from an already authenticated device; old tokens
remain restricted to the original profile. New installations should use Compose.

## Change computer resources

In **Server administration → Computers**, administrators can change
CPU count, RAM and disk capacity for their current workspace. Shut down the
computer there, save the resources, then start it again. Stop active bot work and
return any controlled screens first. Existing disks and sign-ins are retained;
disks can grow but cannot shrink. For a fresh, smaller disk, shut down the computer
and choose **Recreate computer…**. Specify the new resources and type `RECREATE`.
Active/queued work, controlled screens, sign-in operations and maintenance block
recreation. This is a separate operation from saving resources: it archives the
old guest, including its files, installed apps and guest sign-ins, and leaves a
fresh, stopped computer. Workspace chats, bots, provider settings and account
credentials outside the guest remain. Start the fresh computer to provision it.

Archives stay in the data volume under
`profiles/_computer-backups/<profile-id>/<archive-id>/computer`, with a receipt.
They contain sensitive guest data and are not automatically pruned. They retain
the original image backing files, so keep the image cache too. Archive moves stay
on the same filesystem and require no second copy of a large disk. A failed switch
restores the old directory. A host administrator must restore an archive while
the server and that computer are stopped, checking its saved port is still free;
do not overwrite a running guest or combine files from different computers.

The managed Debian guest uses
[cloud-init growth modules](https://docs.cloud-init.io/en/latest/reference/modules.html)
to expand its root filesystem on boot.

This works for managed computers on standalone and hosted installations. Compose
variables `KINDRED_VM_CPUS`, `KINDRED_VM_MEMORY_MB` and `KINDRED_VM_DISK_GB` are
defaults for **new** computers, not resize commands for existing ones. After
editing Compose or its `.env`, run `docker compose up -d` to apply the new
configuration; `docker compose restart` alone does not apply it. Docker Desktop
and the host must also have enough available RAM and disk space. Set the container
memory limit above the total RAM of all concurrently running computers, with
headroom for the server and QEMU. `KINDRED_VM_MAX_RUNNING` limits concurrent
computers; it does not allocate additional container memory. Externally
managed VMs remain configurable through their host VM manager.

## Bot computers on a tailnet

Install and sign into Tailscale inside the bot computer to give that workspace
access to your tailnet. Its bots share that computer and its network access.
Tailnet permissions still control which services the computer can reach.

Kindred’s guest firewall permits traffic through `tailscale0`, including private
subnet routes, while retaining its private-address restrictions on other interfaces.
New computers receive this rule during setup. Existing managed computers receive
it through **Settings → Bot Computer → Update now** after upgrading the server.
The update preserves unrelated firewall rules and saves the rule for reboots; it
does not sign into Tailscale or change your tailnet permissions. Custom external
firewalls remain the operator’s responsibility.
