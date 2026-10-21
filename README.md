# Kindred Server

<img src="ui/favicon.svg" width="64" height="64" alt="Kindred logo">

**The always-on home for your Kindred workspace.**

Host your bots, conversations, routines and artifacts on infrastructure you control.
The server runs scheduled work independently of the desktop app, so your team can
keep working while your laptop is closed. The desktop app also bundles this server
for local Standalone use.

[Download Kindred](https://github.com/awpsec/kindred/releases) · [Getting started](docs/USER_GUIDE.md) · [Self-hosting](docs/HOSTING.md) · [Report an issue](https://github.com/awpsec/kindred-server/issues)

## Kindred in action

![Three bots collaborating in a group chat](docs/media/team-working.gif)

[Chat screenshot](docs/media/team-chat.png) · [Computer view](docs/media/computer-use.png) · [Bot animations](docs/media/bots-working.gif)

*Captured from the Kindred UI with fictional conversations and a simulated computer screen. No personal accounts or customer data.*

## What you can do

- **Work with a team.** Give bots names and roles, chat one-to-one, or collaborate in a group.
- **Use a real computer.** Bots work in a persistent Linux desktop. Watch, take control, and hand it back.
- **Connect your tools.** Add your own provider accounts and connect apps through Composio.
- **Keep work moving.** Schedule routines at a specific time and review results in chat.
- **Host your artifacts.** Keep reports, sheets, slides and interactive apps on your Kindred server, accessible in the app or an authenticated browser session.
- **Collaborate on artifacts.** Share work between bots and people, organize it into folders, and edit or download the results.

## Get started

Download the desktop app for **Windows x64**, **macOS Apple Silicon**, or **Linux x64**.
Connect to a Kindred server, or choose **Standalone** with Docker running to host it on your device.
Provider accounts, usage and connected services are supplied by you; Kindred does not include model credits.

Prefer to self-host?

With Docker installed and running, copy this into a file named **compose.yml**
in an empty folder. Edit the environment values directly in the file as needed;
no separate `.env` file is required.

```yaml
name: kindred

services:
  server:
    image: ghcr.io/awpsec/kindred-server:latest
    platform: linux/amd64
    restart: unless-stopped
    init: true
    ports:
      - "127.0.0.1:9444:9444"
    environment:
      KINDRED_PUBLIC_URL: "http://127.0.0.1:9444"
      KINDRED_OPEN_REGISTRATION: "true" # Set to "false" to require invitations.
      KINDRED_VM_CPUS: "2"             # CPU cores per bot computer.
      KINDRED_VM_MEMORY_MB: "6144"     # RAM per bot computer, in MB.
      KINDRED_VM_DISK_GB: "30"         # Disk capacity per bot computer, in GB.
      KINDRED_VM_MAX_RUNNING: "4"      # Maximum simultaneous bot computers.
    volumes:
      - kindred-data:/data
    cap_drop: [ALL]
    cap_add: [CHOWN, SETUID, SETGID]
    security_opt: [no-new-privileges:true]
    stop_grace_period: 90s

volumes:
  kindred-data:
```

Open a terminal in that folder and run:

```sh
docker compose up -d
```

Docker downloads the published server image automatically. To update to the
latest published release, run these commands from the same folder:

```sh
docker compose pull
docker compose up -d
```

Open **http://127.0.0.1:9444**, create your account, connect a provider, and make your first bot.
The first account is the server administrator. Computer setup happens when needed and can take several minutes.

This example is reachable only on the Docker host. `KINDRED_PUBLIC_URL` sets the
address Kindred advertises; the `ports` mapping controls which host interfaces
accept connections. For Tailscale HTTPS or access from other devices, follow
[hosting and backups](docs/HOSTING.md). The desktop app's **Connection access**
settings manage its own Standalone server; manually launched Compose deployments
use their Compose and host network configuration.

Allow **6 GB RAM per running profile computer**, plus host overhead; Linux with KVM is recommended.
See [hosting and backups](docs/HOSTING.md) before exposing a server to others.

## Learn more

| | |
| --- | --- |
| Setup and features | [User guide](docs/USER_GUIDE.md) |
| Provider authentication and requirements | [Provider integrations](docs/PROVIDER_INTEGRATIONS.md) |
| Installation and updates | [GitHub release updates](docs/GITHUB_UPDATES.md) |
| Accounts and isolation | [Profiles](docs/PROFILES.md) |
| Development | [Contributing](CONTRIBUTING.md) |
| Security issues | [Security policy](SECURITY.md) |

Kindred is under active development. Release notes describe what is included in downloadable packages;
source on `main` may contain changes that have not shipped yet.

## License

[MIT](LICENSE). Third-party components retain their own [licenses](THIRD_PARTY_NOTICES.md).
Kindred is independent and is not affiliated with or endorsed by its model or connector providers.
