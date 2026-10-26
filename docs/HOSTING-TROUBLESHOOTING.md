# Hosted setup troubleshooting

Start from the original Compose project, not an old checkout or a newly created
directory. Preserve its project name, volumes, `.env` and overlays. An image tag
or checkout version alone does not identify the running binary. Use **Server
administration → Updates**, or `python3 scripts/server-setup.py doctor`, to check.
Never use `docker compose down -v` as a repair step.

## Choose one working connection address

**Networking** shows the current connection separately from the advertised URL.
Saving an HTTPS URL does not start TLS, publish a Docker port, alter a firewall,
or create a certificate. Set up the actual listener/proxy first, then check and
save its complete HTTPS URL. For Tailscale, use the URL printed by Serve, including
its HTTPS port only if one is printed. Do not append the backend's HTTP port to
an HTTPS proxy URL. Preserve existing Serve routes and avoid running Caddy on a
port already used by another HTTPS service.

A TLS handshake sent to an HTTP listener is a protocol mismatch. Installing a
CA or bypassing validation cannot fix that. Genuine certificate validation
failures are reported separately by the desktop client. Check the saved URL from
another device too: a server-side address check cannot prove client reachability
or that the responding endpoint is the intended workspace.

For existing bridge deployments, `.env` controls Docker's published bind address;
the UI cannot change Docker publishing. The newer Linux host-network template
provides listener controls in the UI. Both show setup guidance and address
verification. Changing networking topology requires an explicit deployment change;
Kindred does not silently migrate an existing server.

## Separate provider failures from computer setup

The first computer startup downloads a checksum-verified Debian image, then the
guest installs its software. `doctor` tests HTTPS from the container, including
the image and package hosts. Successful DNS does not prove TCP replies can get
through. If downloads fail, inspect outbound connectivity, the appropriate Docker
network path, and established reply traffic in host firewall policy. Do not copy
a blanket interface DROP rule into a production firewall: it can block replies
to the container's own outbound connections. Host and bridge networking follow
different packet paths. Have the host administrator review the existing policy;
the helper does not rewrite it.

Image checksum/download errors now identify that stage. An incomplete image is
discarded on failure, while existing disks and their backing images are retained.
The image cache's empty `download.lock` file is a normal advisory lock marker;
deleting it is not a repair. A first-boot timeout may leave installation running.
Inspect the computer console and retry after it finishes; do not delete the disk
or the account. Provider sign-in is a separate step after computer setup completes.

## Check acceleration and resource headroom

KVM requires virtualization support on the VPS host, an exposed `/dev/kvm`, and
permission for the server's service account. The numeric device group differs by
host. Use the KVM configuration helper in [hosting](HOSTING.md); do not assume
group 108 or forget the overlay on the next update. Explicit KVM mode fails before
image downloads when the device is inaccessible. Auto mode remains compatible
with hosts without KVM, but the Computers page warns about slow TCG emulation.

The RAM setting belongs to the VM shared by this workspace's bots. Capacity checks
respect container limits and other workloads; saving resources does not reserve
that RAM for later. Leave headroom for the host, server, guest desktop and heavy
applications. Swap can reduce out-of-memory risk but does not replace RAM or KVM.
Adding swap or resizing a paid VPS is a host administrator decision. Kindred does
not do either automatically. Large applications may also need their own documented
heap settings; changing VM RAM cannot correct every application limit.

An existing disk can grow, never shrink in place. To start over with a smaller
disk, use the separate **Recreate computer…** flow. It requires stopped/idle work,
explicit confirmation and archives the old computer. Deleting a workspace to
rebuild its guest is unnecessary. Preserve the entire data volume and image cache
in backups, including computer archives.

## Host and third-party administration

SSH recovery access, public services such as Apache, multicast name resolution,
firewall policy and SSH key rotation belong to the VPS administrator. Establish
and test a recovery path before removing remote access. They are not controlled
by a Kindred public URL setting or repaired by recreating a bot computer.

Third-party application licensing, telemetry preferences and JVM heap options
belong to those applications. Check their supported configuration files; launchers
may regenerate others. To stop one application, target its known process rather
than using a broad command-line substring that can also match the SSH session
running it. These operations are separate from Kindred server setup.
