#!/usr/bin/env python3
"""Read-only hosting checks and an explicit, non-restarting KVM configuration step."""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import uuid


def read_environment(path):
    if path.is_symlink():
        raise ValueError('Refusing a redirected .env file. Use the actual Compose project directory.')
    text = path.read_text() if path.exists() else ''
    values = {}
    for line in text.splitlines():
        match = re.match(r'^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$', line)
        if match and match[1] in ('COMPOSE_FILE', 'COMPOSE_PATH_SEPARATOR', 'KINDRED_KVM_GID'):
            parts = shlex.split(match[2], comments=True)
            if len(parts) > 1:
                raise ValueError('Quote Compose environment values containing spaces.')
            values[match[1]] = parts[0] if parts else ''
    return text, values


def configure_kvm(directory, files, device=Path('/dev/kvm')):
    directory = directory.resolve()
    path = directory / '.env'
    text, values = read_environment(path)
    if not device.exists():
        raise ValueError('/dev/kvm is missing on the host. Enable nested virtualization or choose a host with KVM first.')
    # The group must match the actual host, not a distribution-specific default.
    gid = str(device.stat().st_gid)
    for key in ('COMPOSE_FILE', 'COMPOSE_PATH_SEPARATOR', 'KINDRED_KVM_GID'):
        if key in os.environ:
            raise ValueError('Unset shell variable '+key+' first; it overrides .env and can hide the saved configuration.')
    separator = values.get('COMPOSE_PATH_SEPARATOR', ':')
    if separator != ':':
        raise ValueError('This Linux helper requires the default Compose path separator (:).')
    existing = values.get('COMPOSE_FILE', '')
    if '$' in existing or '\n' in existing:
        raise ValueError('Resolve variable references in COMPOSE_FILE before using this helper.')
    selected = existing.split(':') if existing else list(files)
    if not selected:
        raise ValueError('Specify --compose-file with the base file already used by this project. The helper will not guess or change networking mode.')
    if existing and files and list(files) != [p for p in selected if p != 'compose.kvm.yaml']:
        raise ValueError('COMPOSE_FILE is already configured. Omit --compose-file to preserve those files.')
    if 'compose.kvm.yaml' not in selected:
        selected.append('compose.kvm.yaml')
    for name in selected:
        if not name or ':' in name or '\n' in name or '$' in name or not (directory / name).is_file():
            raise ValueError('Every selected Compose file must exist; keep all overlays in the project before configuring KVM.')
    updates = {'KINDRED_KVM_GID': gid, 'COMPOSE_FILE': ':'.join(selected)}
    lines = text.splitlines()
    result = []
    for line in lines:
        match = re.match(r'^\s*(?:export\s+)?(KINDRED_KVM_GID|COMPOSE_FILE)\s*=', line)
        if not match:
            result.append(line)
    for key, value in updates.items():
        result.append(key+'="'+value.replace('\\', '\\\\').replace('"', '\\"')+'"')
    rewritten = '\n'.join(result)+'\n'
    if rewritten == text:
        return {'changed': False, 'kvm_gid': int(gid), 'compose_files': selected}
    backup = None
    if path.exists():
        backup = directory / ('.env.kindred-backup-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-'+uuid.uuid4().hex[:8])
        descriptor = os.open(backup, os.O_WRONLY|os.O_CREAT|os.O_EXCL, 0o600)
        with os.fdopen(descriptor, 'w') as stream:
            stream.write(text); stream.flush(); os.fsync(stream.fileno())
    descriptor, name = tempfile.mkstemp(prefix='.env.kindred-', dir=directory)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            if path.exists() and os.geteuid() == 0:
                original = path.stat(); os.fchown(stream.fileno(), original.st_uid, original.st_gid)
            stream.write(rewritten); stream.flush(); os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)
    return {'changed': True, 'backup': backup.name if backup else None, 'kvm_gid': int(gid), 'compose_files': selected,
            'next': 'Review .env, then use docker compose up -d server in this same project. No container was restarted.'}


CONTAINER_PROBE = r'''
import json,os,pathlib,ssl,urllib.request,urllib.error,socket,subprocess
result={}
device=pathlib.Path('/dev/kvm')
result['kvm_device_present']=device.exists()
result['kvm_device_gid']=device.stat().st_gid if device.exists() else None
try:
    fd=os.open(device,os.O_RDWR);os.close(fd);result['kvm_accessible']=True
except OSError:result['kvm_accessible']=False
result['configured_acceleration']=os.environ.get('KINDRED_VM_ACCEL','auto')
try:result['running_binary_version']=subprocess.run(['/usr/local/bin/kindred','--version'],capture_output=True,text=True,timeout=5).stdout.strip()[:80]
except (OSError,subprocess.TimeoutExpired):result['running_binary_version']='unavailable'
memory={}
for line in pathlib.Path('/proc/meminfo').read_text().splitlines():
    key,_,value=line.partition(':')
    if key in ('MemAvailable','SwapTotal','SwapFree'):memory[key+'_mb']=int(value.split()[0])//1024
result['host_memory']=memory
for key in ('memory.max','memory.current'):
    p=pathlib.Path('/sys/fs/cgroup')/key
    if p.exists():result[key]=p.read_text().strip()
result['outbound_https']=[]
for name,url in [('Debian image checksums','https://cloud.debian.org/images/cloud/trixie/latest/SHA512SUMS'),('Guest package repository','https://deb.debian.org/debian/'),('Runtime downloads','https://github.com/')]:
    item={'name':name,'status':'ok'}
    try:
        with urllib.request.urlopen(url,timeout=10) as response:response.read(1)
    except urllib.error.HTTPError:item['status']='HTTP error (network reached the remote server)'
    except urllib.error.URLError as error:
        item['status']='DNS failed' if isinstance(error.reason,socket.gaierror) else 'TLS validation failed' if isinstance(error.reason,ssl.SSLCertVerificationError) else 'HTTPS connection failed; check egress and established reply traffic'
    except (OSError,TimeoutError):item['status']='HTTPS connection failed; check egress and established reply traffic'
    result['outbound_https'].append(item)
print(json.dumps(result))
'''


def doctor(directory):
    directory = directory.resolve()
    _, env = read_environment(directory / '.env')
    device = Path('/dev/kvm')
    result = {'host_kvm_present': device.exists(), 'host_kvm_gid': device.stat().st_gid if device.exists() else None,
              'saved_kvm_gid': env.get('KINDRED_KVM_GID'), 'saved_compose_files': env.get('COMPOSE_FILE', 'automatic base-file discovery'),
              'storage_free_gb': round(shutil.disk_usage(directory).free / 1024**3, 1), 'warnings': []}
    if device.exists() and env.get('KINDRED_KVM_GID') != str(device.stat().st_gid):
        result['warnings'].append('Saved KVM group does not match /dev/kvm. Run configure-kvm for this project.')
    for key in ('COMPOSE_FILE', 'KINDRED_KVM_GID'):
        if key in os.environ:
            result['warnings'].append('Shell '+key+' overrides .env; checks use the shell value.')
    cargo = directory / 'Cargo.toml'
    if cargo.is_file():
        match = re.search(r'^version\s*=\s*"([0-9.]+)"', cargo.read_text(), re.M)
        if match:result['checkout_version'] = match[1]
    try:
        probe = subprocess.run(['docker', 'compose', 'exec', '-T', 'server', 'python3', '-c', CONTAINER_PROBE],
                               cwd=directory, capture_output=True, text=True, timeout=50)
        if probe.returncode:
            result['warnings'].append('Could not check the running server container. Run docker compose ps and verify this is its original project directory. No container configuration or logs were printed.')
        else:
            result['container'] = json.loads(probe.stdout)
            if not result['container']['kvm_accessible']:
                result['warnings'].append('The container cannot use KVM. Auto mode falls back to slow software emulation; keep the KVM overlay enabled on updates.')
            if result.get('checkout_version') and result['checkout_version'] not in result['container']['running_binary_version']:
                result['warnings'].append('Checkout and running binary versions differ. Diagnose the running image, not the old checkout.')
    except (OSError, subprocess.TimeoutExpired, ValueError):
        result['warnings'].append('Container check unavailable. Docker Compose must be installed and the existing server project must be running.')
    result['scope'] = 'Read-only checks. No credentials, environment dump, guest files, firewall changes, restarts or network exposure changes.'
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, default=Path.cwd(), help='Existing Compose project directory')
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('doctor', help='Check the running container, KVM and outbound HTTPS without changing configuration')
    configure = commands.add_parser('configure-kvm', help='Back up .env and persist the host KVM group and existing overlays; never restart')
    configure.add_argument('--compose-file', action='append', default=[], help='Base Compose file already used by this project (repeat for existing overlays)')
    args = parser.parse_args()
    try:
        result = doctor(args.directory) if args.command == 'doctor' else configure_kvm(args.directory, args.compose_file)
        print(json.dumps(result, indent=2))
    except (OSError, ValueError) as error:
        parser.exit(1, str(error)+'\n')


if __name__ == '__main__':
    main()
