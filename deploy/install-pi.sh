#!/bin/sh
# Install the optional non-Codex harness on a Linux server. No provider keys needed.
set -eu
umask 022
prefix=${1:-/opt/kindred/pi}
mode=${2:-activate}
case "$mode" in activate|--stage-only) ;; *) echo "Use --stage-only to prepare without changing the active harness" >&2; exit 1;; esac
case "$prefix" in /*) ;; *) echo 'Install prefix must be an absolute path' >&2; exit 1;; esac
source=$(CDPATH= cd -- "$(dirname -- "$0")/../harness/pi" && pwd)
version=0.44.5
node_version=24.14.0
case "$(uname -m)" in x86_64) arch=x64;; aarch64|arm64) arch=arm64;; *) echo 'Supported Linux architectures: x86_64 and arm64' >&2; exit 1;; esac
release="$prefix/releases/$version"
test ! -e "$release" || { echo "Release $version already exists; use its existing installation or publish a new version." >&2; exit 1; }
mkdir -p "$prefix/releases"
stage=$(mktemp -d "$prefix/releases/.install-XXXXXX")
cleanup() { case "$stage" in "$prefix"/releases/.install-*) rm -rf -- "$stage";; esac; }
trap cleanup EXIT HUP INT TERM
archive="node-v$node_version-linux-$arch.tar.xz"
curl --proto '=https' --tlsv1.2 -fsSL "https://nodejs.org/dist/v$node_version/SHASUMS256.txt" -o "$stage/SHASUMS256.txt"
curl --proto '=https' --tlsv1.2 -fsSL "https://nodejs.org/dist/v$node_version/$archive" -o "$stage/$archive"
(cd "$stage" && awk -v archive="$archive" '$2 == archive' SHASUMS256.txt | sha256sum -c -)
mkdir "$stage/node"
tar -xJf "$stage/$archive" -C "$stage/node" --strip-components=1
rm -- "$stage/$archive"
cp "$source/package.json" "$source/package-lock.json" "$source/worker.mjs" "$source/session.mjs" "$source/opencode-models.json" "$stage/"
export PATH="$stage/node/bin:$PATH"
(cd "$stage" && npm ci --omit=dev --ignore-scripts --no-audit --no-fund && node worker.mjs --check)
chmod 755 "$stage"
mv -- "$stage" "$release"
if [ "$mode" = activate ]; then
    ln -s "releases/$version" "$prefix/current.new"
    mv -Tf -- "$prefix/current.new" "$prefix/current"
fi
# The server stages verified updates and atomically switches current as its own user.
# Released bundles remain readable; only the containing directories need ownership.
if [ "$(id -u)" -eq 0 ] && id kindred >/dev/null 2>&1; then
    chown kindred:kindred "$prefix" "$prefix/releases"
fi
printf 'Installed Kindred Pi harness %s with Pi SDK 0.85.1 and Node %s\n' "$version" "$node_version"
