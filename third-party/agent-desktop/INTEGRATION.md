Kindred includes the unmodified Vercel Labs agent-desktop 1.0.2 Linux x86_64 CLI and clipboard helper, from the pinned source/release in SOURCE.json. The Apache-2.0 license is retained. No upstream source is modified. Kindred adds a separate launcher that selects the assigned guest desktop and preserves screen ownership.

Fetch the archive at build time with deploy/install-agent-desktop.py --download --stage deploy/vendor/agent-desktop.tar.gz. Packaging verifies archive and executable hashes. Guest installation and updates use the bundled archive offline; there is no startup download or automatic upstream upgrade.

Linux bot guests are Debian 13 with AT-SPI and X11. This is a guest capability, regardless of whether the Kindred client runs on Windows, macOS, Linux or iOS. It does not grant access to the client computer. Chromium semantic text entry is not universally available; keep existing input tools.
