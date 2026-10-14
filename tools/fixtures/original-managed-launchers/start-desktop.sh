#!/bin/sh
set -eu
screen=${1:-1}
case "$screen" in ''|*[!0-9]*) exit 2;; esac
[ "$screen" -ge 1 ] && [ "$screen" -le 32 ] || exit 2
export DISPLAY=:$screen
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/home/bot/.local/run}
browser=/home/bot/.local/share/kindred/browser
[ "$screen" -eq 1 ] || browser=$browser-$screen
mkdir -p "$XDG_RUNTIME_DIR" "$browser"
chmod 700 "$XDG_RUNTIME_DIR" "$browser"
Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp &
xpid=$!
trap 'kill "$xpid" 2>/dev/null || true' EXIT INT TERM
i=0
until xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; do
    i=$((i+1)); [ "$i" -lt 75 ] || exit 1
    sleep 0.2
done
if [ "$screen" -gt 1 ]; then
    port=$((${KINDRED_VNC_BASE_PORT:-5900}+(screen-1)*2))
    x11vnc -display "$DISPLAY" -rfbport "$port" -localhost -nopw -forever -shared -noxdamage -repeat -xkb -quiet &
    x11vnc -display "$DISPLAY" -rfbport "$((port+1))" -localhost -nopw -forever -shared -viewonly -noxdamage -quiet &
fi
# Independent browser profiles prevent Chromium from forwarding a new window
# to another bot's existing process. Files and connected API apps remain shared.
export KINDRED_BROWSER_PROFILE="$browser"
dbus-run-session -- sh -c '
    openbox-session &
    /usr/local/lib/kindred/start-desktop-shell &
    # New computers begin on the desktop. Existing browsers restore their session.
    if [ -f "$KINDRED_BROWSER_PROFILE/Local State" ]; then
        chromium --user-data-dir="$KINDRED_BROWSER_PROFILE" --no-first-run --window-size=1120,680 --restore-last-session &
    fi
    wait
'
