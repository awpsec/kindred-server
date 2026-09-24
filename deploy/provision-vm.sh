#!/bin/sh
# Explicitly creates a NEW VM from an operator-supplied, verified Debian cloud image.
# No downloads, replacements, deletion, existing-domain mutation or host package installation.
set -eu
umask 077
[ "$(id -u)" -eq 0 ] || { echo 'Run as root on the libvirt host.' >&2; exit 1; }
if [ "$#" -ne 5 ]; then
    echo 'Usage: provision-vm.sh NEW_DOMAIN VERIFIED_BASE_QCOW2 NEW_DISK_PATH BOT_PUBLIC_KEY ADMIN_PUBLIC_KEY' >&2
    exit 2
fi
domain=$1
base=$2
disk=$3
pubkey=$4
adminkey=$5
case "$domain" in ''|-*|*[!A-Za-z0-9._-]*) echo 'Invalid domain name' >&2; exit 2;; esac
case "$disk" in /*) ;; *) echo 'Use an absolute disk path' >&2; exit 2;; esac
[ -f "$base" ] && [ -f "$pubkey" ] && [ -f "$adminkey" ] || { echo 'Base image or public keys not found' >&2; exit 2; }
[ ! -e "$disk" ] || { echo 'Destination disk already exists; refusing to replace it' >&2; exit 1; }
# Fail if libvirt is unavailable; only proceed after enumerating domains successfully.
domains=$(virsh --connect qemu:///system list --all --name)
if printf '%s\n' "$domains" | grep -Fxq "$domain"; then echo 'Domain already exists' >&2; exit 1; fi
state=$(mktemp -d)
# Keep generated setup data on failure for diagnosis; never clean or remove a VM disk.
printf 'instance-id: %s\nlocal-hostname: %s\n' "$domain" "$domain" > "$state/meta-data"
ssh-keygen -q -t ed25519 -N '' -f "$state/ssh_host_ed25519_key"
python3 - "$state" "$pubkey" "$adminkey" <<'PY'
import json, pathlib, subprocess, sys
state, botfile, adminfile = map(pathlib.Path, sys.argv[1:])
keys = []
for file in (botfile, adminfile):
    value = file.read_text().strip()
    if '\n' in value or not value.startswith(('ssh-ed25519 ', 'ssh-rsa ', 'ecdsa-sha2-')):
        raise SystemExit('Expected a single SSH public key per file')
    subprocess.run(['ssh-keygen','-lf',str(file)],check=True,stdout=subprocess.DEVNULL)
    keys.append(value)
if keys[0].split()[:2] == keys[1].split()[:2]:
    raise SystemExit('Use separate bot and administrator keys')
data = {'users': [
    {'name':'bot','shell':'/bin/bash','lock_passwd':True,'sudo':['ALL=(ALL:ALL) NOPASSWD:ALL'],'ssh_authorized_keys':[keys[0]]},
    {'name':'kindred-admin','shell':'/bin/bash','lock_passwd':True,
     'sudo':['ALL=(ALL) NOPASSWD:ALL'],'ssh_authorized_keys':[keys[1]]}],
    'ssh_pwauth':False,'disable_root':True,
    'ssh_keys':{'ed25519_private':(state/'ssh_host_ed25519_key').read_text(),
                'ed25519_public':(state/'ssh_host_ed25519_key.pub').read_text().strip()}}
(state/'user-data').write_text('#cloud-config\n'+json.dumps(data,indent=2)+'\n')
PY
qemu-img convert -O qcow2 "$base" "$disk"
qemu-img resize "$disk" 8G
virt-install --connect qemu:///system --name "$domain" --memory 1536 --vcpus 1 --cputune shares=128 --import \
    --disk "path=$disk,format=qcow2,bus=virtio" --os-variant debian13 \
    --network network=default,model=virtio --boot uefi --graphics spice,listen=127.0.0.1 \
    --cloud-init "user-data=$state/user-data,meta-data=$state/meta-data" --noautoconsole
echo "Created $domain. Cloud-init setup data: $state"
echo "Pin the guest host key from $state/ssh_host_ed25519_key.pub before connecting. Keep this state root-only."
echo 'Use the separate kindred-admin key to install guest packages. Never give the service that key.'
