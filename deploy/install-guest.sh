#!/bin/sh
# Run explicitly as root INSIDE a dedicated Debian VM after reviewing this script.
set -eu
[ "$(id -u)" -eq 0 ] || { echo 'Run as root inside the dedicated guest.' >&2; exit 1; }
[ -f /etc/debian_version ] || { echo 'This guest setup targets Debian.' >&2; exit 1; }
[ -f ./kindred ] || { echo 'Put the compiled Linux kindred binary beside this script.' >&2; exit 1; }
[ -f ./kindred-guest-desktop.service ] && [ -f ./start-desktop.sh ] || exit 1
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends sudo openssh-server ca-certificates curl chromium openbox dbus-x11 xvfb x11-utils xdotool x11vnc imagemagick coreutils libpam-systemd tint2 rofi feh xterm konsole fonts-dejavu-core pcmanfm librsvg2-bin util-linux python3-docx python3-venv python3-pip libreoffice-writer poppler-utils fonts-liberation
id bot >/dev/null 2>&1 || useradd --create-home --shell /bin/bash bot
# The bot administers this dedicated VM. This grants no host privileges.
install -d -m 0750 /etc/sudoers.d
printf '%s\n' 'bot ALL=(ALL:ALL) NOPASSWD: ALL' >/etc/sudoers.d/kindred-bot
chmod 0440 /etc/sudoers.d/kindred-bot
visudo -cf /etc/sudoers.d/kindred-bot
install -d -o bot -g bot -m 700 /home/bot/.ssh /home/bot/.local /home/bot/.local/share /home/bot/.local/share/kindred /home/bot/.local/run /home/bot/.local/share/kindred/codex /home/bot/.local/share/kindred/browser
install -d -o bot -g bot -m 700 /workspace
install -d /usr/local/lib/kindred
install -m 755 ./kindred /usr/local/lib/kindred/kindred-bin
cat > /usr/local/bin/kindred <<'WRAPPER'
#!/bin/sh
export KINDRED_GUEST=1 DISPLAY=:1
exec /usr/local/lib/kindred/kindred-bin "$@"
WRAPPER
chmod 755 /usr/local/bin/kindred
install -m 755 ./start-desktop.sh ./start-desktop-shell ./desktop-launch ./ensure-screen ./wait-screen-ready /usr/local/lib/kindred/
install -d /usr/local/share/kindred/desktop
install -m 644 ./desktop/* /usr/local/share/kindred/desktop/
install -d /usr/local/share/konsole
install -m 644 ./desktop/Kindred.colorscheme /usr/local/share/konsole/Kindred.colorscheme
for asset in wallpaper browser files terminal applications; do
    rsvg-convert "/usr/local/share/kindred/desktop/$asset.svg" -o "/usr/local/share/kindred/desktop/$asset.png"
done
install -d /etc/systemd/user
install -m 644 ./kindred-screen@.service /etc/systemd/user/
loginctl enable-linger bot
install -m 644 ./kindred-guest-desktop.service /etc/systemd/system/kindred-guest-desktop.service
install -m 644 ./kindred-vnc.service ./kindred-vnc-view.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now kindred-guest-desktop.service
systemctl enable --now kindred-vnc.service kindred-vnc-view.service
echo 'Install the official Codex CLI at /usr/local/bin/codex, add the server SSH public key to /home/bot/.ssh/authorized_keys, and verify SSH host keys before connecting.'
