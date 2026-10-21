#!/usr/bin/env python3
"""Persistent QEMU computers, one per Kindred profile. No Docker socket or host mounts.

Only a server-issued UUID is accepted. First use creates the disk and immutable
bootstrap seed; retries reuse them. Guest SSH host keys are pinned before boot.
"""
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import runpy
import shlex
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import uuid

ROOT = Path(os.environ.get('KINDRED_PROFILES_DIR', '/data/profiles')).resolve()
SOFTWARE = Path(os.environ.get('KINDRED_GUEST_SOFTWARE', '/opt/kindred/guest')).resolve()
CACHE = ROOT / '_images'
DEBIAN = 'https://cloud.debian.org/images/cloud/trixie/latest/'
IMAGE = 'debian-13-genericcloud-amd64.qcow2'

def available_host_port(port):
    # Host-network installations share ports with other host applications.
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        try: probe.bind(('127.0.0.1',port)); return True
        except OSError: return False

def run(args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=600, **kwargs)

def atomic(path, data):
    temp = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    with temp.open('x', encoding='utf-8') as stream:
        stream.write(json.dumps(data) if not isinstance(data, str) else data)
        stream.flush(); os.fsync(stream.fileno())
    os.replace(temp, path)

@contextlib.contextmanager
def locked(path):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with path.open('a') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield

def identifier(value):
    if str(uuid.UUID(value)) != value: raise ValueError('Invalid profile ID')
    return value

def directory(value):
    value = identifier(value)
    root = (ROOT / value / 'computer').resolve()
    if root.parent.parent != ROOT: raise ValueError('Invalid computer directory')
    with locked(ROOT/'_lifecycle'/value):
        if (ROOT/'_retired'/value).exists(): raise ValueError('This computer belongs to a removed account')
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
    return root

def download(url, path, limit):
    request = urllib.request.Request(url, headers={'User-Agent':'Kindred-computer-setup'})
    with urllib.request.urlopen(request, timeout=60) as response, path.open('xb') as out:
        size = 0
        while chunk := response.read(1024 * 1024):
            size += len(chunk)
            if size > limit: raise ValueError('Image download is too large')
            out.write(chunk)

def base_image():
    CACHE.mkdir(parents=True, exist_ok=True, mode=0o700)
    image = CACHE / IMAGE
    with locked(CACHE / 'download.lock'):
        if image.exists(): return image
        sums = urllib.request.urlopen(DEBIAN + 'SHA512SUMS', timeout=60).read(128*1024).decode()
        matches = [line.split()[0] for line in sums.splitlines() if line.split()[-1].lstrip('*') == IMAGE]
        if len(matches) != 1 or len(matches[0]) != 128: raise ValueError('Debian image checksum unavailable')
        temporary = CACHE / (uuid.uuid4().hex + '.download')
        try:
            download(DEBIAN + IMAGE, temporary, 1024*1024*1024)
            with temporary.open('rb') as stream: actual = hashlib.file_digest(stream, 'sha512').hexdigest()
            if actual != matches[0]: raise ValueError('Debian image checksum mismatch; retry setup')
            os.replace(temporary, image)
            atomic(CACHE / 'source.json', {'url':DEBIAN+IMAGE, 'sha512':actual, 'downloaded_at':int(time.time())})
        finally:
            if temporary.exists(): temporary.unlink()
    return image

def number(name, default, low, high):
    value = int(os.environ.get(name, default))
    if not low <= value <= high: raise ValueError('Invalid '+name)
    return value

def qmp(root, command):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(3);client.connect(str(root/'monitor.sock'))
        stream=client.makefile('rwb');json.loads(stream.readline())
        for name in ('qmp_capabilities',command):
            stream.write((json.dumps({'execute':name})+'\n').encode());stream.flush()
            while True:
                response=json.loads(stream.readline())
                if 'return' in response: break
                if 'error' in response: raise ValueError('Computer control was not accepted')
        return response['return']

def running(root):
    # A paused, faulted or shutting-down QEMU still owns its disk and port.
    # Never treat it as absent and start a second process or resize its disk.
    try: return bool(qmp(root,'query-status').get('status'))
    except PermissionError as error:
        raise ValueError('Kindred cannot access the bot computer control socket. Restore its ownership and permissions for the Kindred service account.') from error
    except (OSError,ValueError): return False

def reboot_process_start(process, pid):
    text = (process / 'stat').read_text()
    # comm may contain spaces or parentheses; starttime is field 22 after comm.
    fields = text[text.rindex(')') + 2:].split()
    if int(text.split(' ', 1)[0]) != pid: raise ValueError('PID changed')
    started = int(fields[19])
    if started <= 0: raise ValueError('Invalid process start time')
    return started

def reboot_process_identity(root, profile):
    """Bind the recorded QEMU PID to this disk before asking it to shut down."""
    try:
        pidfile = root / 'qemu.pid'
        if pidfile.is_symlink(): raise ValueError('redirected PID record')
        pid = int(pidfile.read_text().strip())
        if pid <= 1: raise ValueError('invalid PID')
        disk = root / 'disk.qcow2'
        metadata = root / 'computer.json'
        if disk.is_symlink() or not disk.is_file() or metadata.is_symlink() or not metadata.is_file():
            raise ValueError('existing computer files missing or redirected')
        disk_stat = disk.stat()
        process = Path(f'/proc/{pid}')
        started = reboot_process_start(process, pid)
        command = (process / 'cmdline').read_bytes().split(b'\0')
        expected_name = ('kindred-' + profile).encode()
        expected_disk = f'file={root}/disk.qcow2,if=virtio,format=qcow2'.encode()
        if ((process / 'exe').readlink().name != 'qemu-system-x86_64'
            or not command or command[0].rsplit(b'/', 1)[-1] != b'qemu-system-x86_64'
            or not any(command[i:i+2] == [b'-name', expected_name] for i in range(len(command)-1))
            or not any(command[i:i+2] == [b'-drive', expected_disk] for i in range(len(command)-1))
            or process.stat().st_uid != os.geteuid()
            or reboot_process_start(process, pid) != started
            or pidfile.is_symlink() or int(pidfile.read_text().strip()) != pid):
            raise ValueError('process does not match this computer')
        return pid, started, (disk_stat.st_dev, disk_stat.st_ino)
    except (OSError, ValueError, IndexError) as error:
        raise ValueError('Cannot verify the bot computer process identity. No replacement process was started.') from error

def reboot_process_exited(root, identity):
    """QMP loss is not exit. Unknown or reused process identity refuses restart."""
    try:
        pid, started, disk_identity = identity
        pidfile = root / 'qemu.pid'
        if pidfile.is_symlink(): raise ValueError('PID record redirected')
        try: recorded = int(pidfile.read_text().strip())
        except FileNotFoundError: recorded = None
        if recorded is not None and recorded != pid: raise ValueError('PID record changed')
        process = Path(f'/proc/{pid}')
        try:
            current = reboot_process_start(process, pid)
        except FileNotFoundError:
            # Missing stat is only exit if the process directory is also absent.
            try: process.stat()
            except FileNotFoundError: return True
            raise ValueError('process metadata missing')
        if current != started or recorded is None: raise ValueError('Process identity changed')
        return False
    except (OSError, ValueError, IndexError) as error:
        raise ValueError('Cannot verify the bot computer process identity. No replacement process was started.') from error

def prepare(root, profile):
    metadata=root/'computer.json'
    if metadata.exists(): return json.loads(metadata.read_text())
    resources=initial_resources(root)
    validate_resource_capacity(resources)
    base=base_image()
    for name in ('ssh_client','ssh_host_ed25519_key'):
        if not (root/name).exists(): run(['ssh-keygen','-q','-t','ed25519','-N','','-f',root/name])
    with locked(ROOT/'allocation.lock'):
        used={json.loads(p.read_text())['port'] for p in ROOT.glob('*/computer/computer.json')}
        port=next((p for p in range(22000,32000) if p not in used and available_host_port(p)),None)
        if port is None: raise ValueError('Computer port capacity reached')
        settings={'id':profile,'port':port,**resources,'created_at':int(time.time())}
        # Reserve the port before another profile can choose it.
        atomic(metadata,settings)
    return settings

def seed(root, profile, settings):
    if not (root/'disk.qcow2').exists():
        # An existing overlay must keep its original backing image. Downloading
        # today's Debian image cannot repair a missing old backing image.
        base=base_image()
        temporary=root/'disk.creating.qcow2'
        if temporary.exists(): temporary.unlink()
        run(['qemu-img','create','-f','qcow2','-F','qcow2','-b',base,temporary,str(settings['disk_gb'])+'G'])
        os.replace(temporary,root/'disk.qcow2')
    hostkey=(root/'ssh_host_ed25519_key.pub').read_text().strip()
    atomic(root/'known_hosts',f"[127.0.0.1]:{settings['port']} {hostkey}\n")
    atomic(root/'ssh_config',f'''Host kindred-guest
    HostName 127.0.0.1
    Port {settings['port']}
    User bot
    IdentityFile {root}/ssh_client
    IdentitiesOnly yes
    UserKnownHostsFile {root}/known_hosts
    StrictHostKeyChecking yes
    PasswordAuthentication no
    LogLevel ERROR
''')
    if not (root/'seed.iso').exists():
        bootstrap = '''#!/bin/sh
set -eu
export DEBIAN_FRONTEND=noninteractive
mkdir -p /mnt/kindred-software
mount -o ro /dev/sr1 /mnt/kindred-software
cp -a /mnt/kindred-software/. /root/kindred-install
cd /root/kindred-install
apt-get update -qq
apt-get install -y --no-install-recommends nftables
# Default guest network policy. A bot with guest sudo can change these rules;
# enforce any mandatory network isolation outside the guest as well.
cat >/etc/nftables.conf <<'RULES'
#!/usr/sbin/nft -f
flush ruleset
table inet kindred {
 chain output {
  type filter hook output priority 0; policy accept;
  oifname "lo" accept
  oifname "tailscale0" accept comment "kindred-tailnet"
  ct state established,related accept
  ip daddr 10.0.2.3 udp dport 53 accept
  ip daddr 10.0.2.3 tcp dport 53 accept
  udp sport 68 udp dport 67 accept
  ip daddr { 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4 } reject
  ip6 daddr { ::/128, ::1/128, fc00::/7, fe80::/10, ff00::/8 } reject
 }
}
RULES
systemctl enable --now nftables
sh ./install-guest.sh
sh ./install-providers.sh
python3 ./download-codex.py --install ./codex /usr/local/bin/codex
sudo -u bot python3 - /usr/local/bin/codex < ./check-codex.py
touch /var/lib/kindred-ready
'''
        data={'users':[{'name':'bot','shell':'/bin/bash','groups':[],'sudo':['ALL=(ALL:ALL) NOPASSWD:ALL'],'lock_passwd':True,'ssh_authorized_keys':[(root/'ssh_client.pub').read_text().strip()]}],
              'ssh_pwauth':False,'disable_root':True,'ssh_keys':{'ed25519_private':(root/'ssh_host_ed25519_key').read_text(),'ed25519_public':hostkey},
              'write_files':[{'path':'/root/kindred-bootstrap.sh','permissions':'0700','content':bootstrap}],
              'runcmd':[['sh','/root/kindred-bootstrap.sh']]}
        atomic(root/'user-data','#cloud-config\n'+json.dumps(data))
        atomic(root/'meta-data',f'instance-id: {profile}\nlocal-hostname: kindred-{profile[:8]}\n')
        temporary=root/'seed.creating.iso'
        run(['genisoimage','-quiet','-output',temporary,'-volid','cidata','-joliet','-rock',root/'user-data',root/'meta-data'])
        os.replace(temporary,root/'seed.iso')
    return software_image()

def software_fingerprint():
    digest=hashlib.sha256()
    for path in sorted(p for p in SOFTWARE.rglob('*') if p.is_file() and '__pycache__' not in p.parts):
        digest.update(json.dumps([path.relative_to(SOFTWARE).as_posix(),path.stat().st_size],separators=(',',':')).encode())
        with path.open('rb') as stream:
            for block in iter(lambda:stream.read(1024*1024),b''):digest.update(block)
    return digest.hexdigest()

def software_image():
    # Never replace media attached to a running computer. New computers and
    # future boots select the current content-addressed bundle after an update.
    with locked(CACHE/'software.lock'):
        if not (SOFTWARE/'kindred').is_file() or not (SOFTWARE/'codex').is_file(): raise ValueError('Guest software bundle is missing')
        checker=SOFTWARE/'check-codex.py'
        if not checker.is_file(): raise ValueError('Guest Codex runtime checker is missing. Restage the complete guest software bundle.')
        runpy.run_path(str(checker))['check'](SOFTWARE/'codex',execute=False)
        fingerprint=software_fingerprint();image=CACHE/('software-'+fingerprint+'.iso')
        if not image.exists():
            temporary=image.with_suffix('.creating.iso')
            run(['genisoimage','-quiet','-output',temporary,'-volid','kindred','-joliet','-rock',SOFTWARE])
            if software_fingerprint()!=fingerprint:raise ValueError('Guest software changed during packaging; retry setup')
            os.replace(temporary,image)
    return image

def legacy_cgroups(controller, mount, membership):
    roots={mount,mount/controller}
    if controller=='cpu': roots.add(mount/'cpu,cpuacct')
    entries=membership.read_text().splitlines() if membership.is_file() else []
    relative=next((line.split(':',2)[2] for line in entries if len(line.split(':',2))==3 and controller in line.split(':',2)[1].split(',')),None)
    groups=set(roots)
    if relative is not None:
        for root in roots:
            group=(root/relative.lstrip('/')).resolve()
            while group.is_relative_to(root):
                groups.add(group)
                if group==root:break
                group=group.parent
    return groups

def memory_available(meminfo=Path('/proc/meminfo'), cgroup_root=Path('/sys/fs/cgroup'), membership=Path('/proc/self/cgroup')):
    available=next(int(line.split()[1])*1024 for line in meminfo.read_text().splitlines() if line.startswith('MemAvailable:'))
    cgroup_root=cgroup_root.resolve()
    groups={cgroup_root}
    # On a native systemd host the service limit is below the cgroup mount.
    # Containers may expose their own cgroup directly at the mount root.
    if membership.is_file():
        unified=next((line[3:] for line in membership.read_text().splitlines() if line.startswith('0::')),None)
        if unified is not None:
            group=(cgroup_root/unified.lstrip('/')).resolve()
            while group.is_relative_to(cgroup_root):
                groups.add(group)
                if group==cgroup_root: break
                group=group.parent
    for group in groups:
        limit=group/'memory.max';used=group/'memory.current'
        if limit.is_file() and used.is_file() and limit.read_text().strip()!='max':
            available=min(available,int(limit.read_text())-int(used.read_text()))
    for group in legacy_cgroups('memory',cgroup_root,membership):
        limit=group/'memory.limit_in_bytes';used=group/'memory.usage_in_bytes'
        if limit.is_file() and used.is_file():
            available=min(available,int(limit.read_text())-int(used.read_text()))
    return max(0,available)

def require_running(root):
    status=qmp(root,'query-status').get('status','unknown')
    if status=='running': return
    if status=='io-error':
        raise ValueError('The bot computer paused because of a disk I/O error. Check free space on the server or Docker Desktop disk before restarting it.')
    raise ValueError('The bot computer is '+status+'. Shut it down before starting it again; its existing disk will be reused.')

def launch(root, args):
    try:
        run(args)
    except subprocess.CalledProcessError as error:
        detail=(error.stderr or b'').decode('utf-8',errors='replace')[-8192:]
        atomic(root/'startup-error.log',detail)
        lower=detail.lower()
        if 'cannot allocate memory' in lower or 'could not allocate' in lower:
            reason='Not enough memory to launch the bot computer. Reduce its memory allocation or increase memory available to Docker Desktop or the server.'
        elif 'no space left on device' in lower:
            reason='The server or Docker Desktop disk is full. Free disk space before starting the bot computer.'
        elif 'host forwarding rule' in lower or 'address already in use' in lower:
            reason='The bot computer connection port is already in use. Restart the local server before trying again.'
        elif 'failed to get' in lower and 'lock' in lower:
            reason='The bot computer disk is still in use. Wait for shutdown to finish before starting it again.'
        elif 'backing file' in lower and ('no such file' in lower or 'could not open' in lower):
            reason='The bot computer backing image is missing or unreadable. Restore the original image from backup; downloading a replacement can damage the existing disk.'
        elif 'kvm' in lower:
            reason='Hardware virtualization is unavailable. Check the server virtualization settings and KVM access.'
        else:
            reason='The bot computer could not launch. Details were saved in its startup-error.log on the server.'
        raise ValueError(reason) from error
    if not running(root):
        raise ValueError('The bot computer exited during startup. Check server memory and the computer console log before retrying.')
    require_running(root)
    (root/'startup-error.log').unlink(missing_ok=True)

def start(root, profile):
    settings=prepare(root,profile)
    # Existing running computers must not depend on downloading or repackaging
    # installation media after a server update.
    if running(root):
        require_running(root)
        return settings
    # Revalidate existing allocations too: host/container limits may have changed.
    validate_resource_capacity(settings)
    software=seed(root,profile,settings)
    with locked(ROOT/'capacity.lock'):
        if running(root):
            require_running(root)
            return settings
        active=sum(running(p.parent) for p in ROOT.glob('*/computer/computer.json'))
        if active>=number('KINDRED_VM_MAX_RUNNING',4,1,128): raise ValueError('All computer slots are in use. Stop an idle profile computer or ask the administrator to raise the limit.')
        if memory_available()<(settings['memory_mb']+512)*1024*1024: raise ValueError('Not enough memory to start this computer. Stop an idle computer or increase the Kindred service/container memory limit to cover the VM and server together.')
        accel=os.environ.get('KINDRED_VM_ACCEL','auto')
        if accel not in ('auto','kvm','tcg'): raise ValueError('Invalid computer accelerator')
        if accel=='auto': accel='kvm' if os.access('/dev/kvm',os.R_OK|os.W_OK) else 'tcg'
        args=['qemu-system-x86_64','-name','kindred-'+profile,'-machine','q35,accel='+accel,'-cpu','host' if accel=='kvm' else 'max',
              '-smp',settings['cpus'],'-m',settings['memory_mb'],'-display','none','-nodefaults','-device','VGA','-device','virtio-rng-pci',
              '-drive',f'file={root}/disk.qcow2,if=virtio,format=qcow2',
              '-drive',f'file={root}/seed.iso,media=cdrom,readonly=on,index=0',
              '-drive',f'file={software},media=cdrom,readonly=on,index=1',
              '-netdev',f"user,id=network,ipv6=off,hostfwd=tcp:127.0.0.1:{settings['port']}-:22",'-device','virtio-net-pci,netdev=network',
              '-qmp',f'unix:{root}/monitor.sock,server=on,wait=off','-serial',f'file:{root}/console.log','-pidfile',root/'qemu.pid','-daemonize']
        launch(root,args)
    return settings

# Update only Kindred's bridge, not provider packages, credentials or VM data.
PROVIDER_HELPER_UPDATE = r"""
import hashlib,json,os,pathlib,shutil,sys,tempfile
payload=json.load(sys.stdin)
data=payload['source'].encode('utf-8')
assert hashlib.sha256(data).hexdigest()==payload['sha256']
compile(data,'provider-cli.py','exec')
root=pathlib.Path(sys.argv[1]);target=root/'provider-cli.py'
old=target.read_bytes()
if old==data:
    print(json.dumps({'updated':False}))
else:
    backup=root/('provider-cli.py.backup-'+hashlib.sha256(old).hexdigest())
    if not backup.exists(): shutil.copy2(target,backup)
    name=None
    try:
        with tempfile.NamedTemporaryFile(dir=root,prefix='.provider-cli-',delete=False) as stage:
            name=stage.name;stage.write(data);stage.flush();os.fsync(stage.fileno())
        os.chmod(name,0o755);os.replace(name,target);name=None
    finally:
        if name: os.unlink(name)
    print(json.dumps({'updated':True}))
"""

def sync_provider_helper(root):
    source=(SOFTWARE/'provider-cli.py').read_text()
    compile(source,'provider-cli.py','exec')
    payload=json.dumps({'source':source,'sha256':hashlib.sha256(source.encode('utf-8')).hexdigest()})
    command='sudo -n python3 -c '+shlex.quote(PROVIDER_HELPER_UPDATE)+' /usr/local/lib/kindred'
    try:
        result=subprocess.run(['ssh','-F',str(root/'ssh_config'),'-T','-oBatchMode=yes',
            '-oStrictHostKeyChecking=yes','-oConnectTimeout=5','kindred-guest',command],
            input=payload,text=True,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=20,check=True)
        if not isinstance(json.loads(result.stdout).get('updated'),bool): raise ValueError('Invalid bridge receipt')
    except (subprocess.SubprocessError,ValueError) as error:
        raise ValueError('Could not update the bot computer provider bridge. Retry starting the computer; its data and provider sign-ins are preserved.') from error

def connection_status(root):
    # A saved SSH configuration means boot has been prepared, not that the
    # provider software is installed. This probe must never start a computer or
    # wait behind the long-running setup operation lock.
    if not (root/'ssh_config').is_file(): return 'not_started'
    if not running(root): return 'stopped'
    checker=SOFTWARE/'check-codex.py'
    if not checker.is_file(): return 'runtime_failed'
    check='python3 -c '+shlex.quote(checker.read_text())+' /usr/local/bin/codex --files-only >/dev/null 2>&1'
    try:
        result=subprocess.run(['ssh','-F',str(root/'ssh_config'),'-T','-oBatchMode=yes',
            '-oStrictHostKeyChecking=yes','-oConnectTimeout=3','kindred-guest',
            'if test -f /var/lib/kindred-ready; then '+check+' && echo ready || echo runtime_failed; '
            'elif test -f /var/lib/cloud/instance/boot-finished; then echo failed; '
            'else echo installing; fi'],check=True,stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=5)
        phase=result.stdout.decode().strip()
        return phase if phase in ('ready','failed','installing','runtime_failed') else 'starting'
    except (subprocess.CalledProcessError,subprocess.TimeoutExpired): return 'starting'

RESOURCE_LIMITS = {'cpus': (1, 32), 'memory_mb': (1024, 262144), 'disk_gb': (8, 2048)}

def cpu_capacity(cgroup_root=Path('/sys/fs/cgroup'), membership=Path('/proc/self/cgroup')):
    import math
    count=len(os.sched_getaffinity(0)) if hasattr(os,'sched_getaffinity') else (os.cpu_count() or 1)
    mount=cgroup_root.resolve();groups={mount}
    if membership.is_file():
        unified=next((line[3:] for line in membership.read_text().splitlines() if line.startswith('0::')),None)
        if unified is not None:
            group=(mount/unified.lstrip('/')).resolve()
            while group.is_relative_to(mount):
                groups.add(group)
                if group==mount:break
                group=group.parent
    for group in groups:
        quota=group/'cpu.max'
        if quota.is_file():
            maximum,period=quota.read_text().split()
            if maximum!='max':count=min(count,max(1,math.ceil(int(maximum)/int(period))))
    for group in legacy_cgroups('cpu',mount,membership):
        quota=group/'cpu.cfs_quota_us';period=group/'cpu.cfs_period_us'
        if quota.is_file() and period.is_file() and int(quota.read_text())>0:
            count=min(count,max(1,math.ceil(int(quota.read_text())/int(period.read_text()))))
    return max(1,count)

def resource_capacity():
    available=memory_available()//(1024*1024)
    return {'cpus':min(32,cpu_capacity()),'memory_available_mb':available,
            'memory_budget_mb':max(0,min(262144,available-512)),'reserved_memory_mb':512}

def validate_resource_capacity(values):
    capacity=resource_capacity()
    if values['cpus']>capacity['cpus']:
        raise ValueError(f"This server/container has {capacity['cpus']} CPUs available. Reduce the shared VM CPU count or increase the host/container CPU allocation.")
    if values['memory_mb']>capacity['memory_budget_mb']:
        raise ValueError(f"The shared VM requests {values['memory_mb']/1024:g} GB RAM, but only {capacity['memory_budget_mb']/1024:.1f} GB is available after reserving 0.5 GB for the server. Reduce shared VM RAM, stop another profile computer, or increase the service/container memory limit.")

def initial_resources(root):
    path=root/'resource-overrides.json'
    values=json.loads(path.read_text()) if path.exists() else {
        'cpus':number('KINDRED_VM_CPUS',2,1,32),'memory_mb':number('KINDRED_VM_MEMORY_MB',6144,1024,262144),
        'disk_gb':number('KINDRED_VM_DISK_GB',30,8,2048)}
    if not isinstance(values,dict) or set(values)!=set(RESOURCE_LIMITS):raise ValueError('Invalid initial VM resources')
    for key,(low,high) in RESOURCE_LIMITS.items():
        if type(values[key]) is not int or not low<=values[key]<=high:raise ValueError('Invalid initial VM resource '+key)
    return values

def resource_settings(root, profile):
    capacity=resource_capacity()
    limits={**RESOURCE_LIMITS,'cpus':(1,capacity['cpus']),'memory_mb':(1024,capacity['memory_budget_mb'])}
    if not (root/'computer.json').exists():
        return {'state':'shut off','provisioned':False,'resources':initial_resources(root),'limits':limits,'capacity':capacity}
    settings = json.loads((root/'computer.json').read_text())
    disk = root/'disk.qcow2'
    if disk.exists() and not running(root):
        actual = json.loads(run(['qemu-img', 'info', '--output=json', disk]).stdout)['virtual-size']
        # Reconcile an interrupted save after the disk expansion succeeded.
        size = (actual + 1024**3 - 1)//1024**3
        if size > settings['disk_gb']:
            settings['disk_gb'] = size
            atomic(root/'computer.json', settings)
    return {'state': 'running' if running(root) else 'shut off',
            'resources': {k: settings[k] for k in RESOURCE_LIMITS},
            'limits': limits,'capacity':capacity}

def resize_resources(root, profile, values):
    if not isinstance(values, dict) or set(values) != set(RESOURCE_LIMITS):
        raise ValueError('Specify CPU count, memory and disk size.')
    for key, (low, high) in RESOURCE_LIMITS.items():
        if type(values[key]) is not int or not low <= values[key] <= high:
            raise ValueError(f'{key} must be a whole number between {low} and {high}.')
    if running(root):
        raise ValueError('Shut down this computer before changing its resources.')
    validate_resource_capacity(values)
    if not (root/'computer.json').exists():
        atomic(root/'resource-overrides.json',values)
        return resource_settings(root,profile)
    current = resource_settings(root, profile)['resources']
    if values['disk_gb'] < current['disk_gb']:
        raise ValueError('Disk size can only be increased.')
    settings = json.loads((root/'computer.json').read_text())
    disk = root/'disk.qcow2'
    if disk.exists() and values['disk_gb'] > current['disk_gb']:
        run(['qemu-img', 'resize', disk, str(values['disk_gb'])+'G'])
    settings.update(values)
    atomic(root/'computer.json', settings)
    return resource_settings(root, profile)

def restore_computers():
    """Restore provisioned computers without blocking HTTP startup or creating new disks."""
    for config in sorted(ROOT.glob('*/computer/computer.json')):
        profile=config.parent.parent.name
        try:
            identifier(profile)
            with locked(ROOT/'_operations'/profile):
                if (ROOT/'_retired'/profile).exists(): continue
                root=directory(profile)
                intent=root/'power-intent.json'
                # Pre-feature computers have no intent record. Opt them into
                # recovery once; explicit shutdown persists an opt-out.
                if intent.exists() and json.loads(intent.read_text()).get('running') is not True: continue
                if not (root/'disk.qcow2').is_file(): continue
                start(root,profile)
                atomic(intent,{'running':True})
                print('Restored bot computer '+profile,flush=True)
        except Exception as error:
            # One unavailable computer must not prevent other profiles booting.
            print('Could not restore bot computer '+profile+': '+str(error),file=sys.stderr,flush=True)


def main():
    os.umask(0o077)
    if sys.argv[1:]==['restore']: return restore_computers()
    if len(sys.argv)!=3: raise ValueError('Expected action and profile ID')
    action,profile=sys.argv[1:]
    if action=='retire': identifier(profile)
    if action=='retire' and (ROOT/'_retired'/profile).exists():
        root=ROOT/profile/'computer'
    else: root=directory(profile)
    if action not in ('ensure','start','domstate','connection-status','shutdown','reboot','resource-settings','resize-resources','retire'): raise ValueError('Unknown computer action')
    if action=='connection-status':
        print(json.dumps({'setup':connection_status(root)}));return
    if action=='domstate':
        print(json.dumps({'state':'running' if running(root) else 'shut off','provisioned':(root/'computer.json').exists()}));return
    with locked(ROOT/'_operations'/profile):
        if action=='retire':
            with locked(ROOT/'_lifecycle'/profile):
                (ROOT/'_retired').mkdir(exist_ok=True,mode=0o700)
                atomic(ROOT/'_retired'/profile,{'retired':True})
            if running(root):
                qmp(root,'system_powerdown')
                deadline=time.monotonic()+60
                while running(root) and time.monotonic()<deadline: time.sleep(.5)
                if running(root): qmp(root,'quit')
                deadline=time.monotonic()+5
                while running(root) and time.monotonic()<deadline: time.sleep(.1)
                if running(root): raise ValueError('Computer has not stopped; retry account removal')
            # A lost QMP socket must not allow deletion of a live VM's disk.
            pidfile=root/'qemu.pid'
            if pidfile.exists():
                pid=int(pidfile.read_text().strip())
                try: command=Path(f'/proc/{pid}/cmdline').read_bytes()
                except FileNotFoundError: command=b''
                if str(root).encode() in command: raise ValueError('Computer process is still exiting; retry account removal')
            print(json.dumps({'state':'retired'}));return
        if (ROOT/'_retired'/profile).exists(): raise ValueError('This computer belongs to a removed account')
        if action == 'resource-settings':
            print(json.dumps(resource_settings(root, profile)));return
        if action == 'resize-resources':
            print(json.dumps(resize_resources(root, profile, json.loads(sys.stdin.read(4096)))));return
        if action in ('ensure','start'):
            settings=start(root,profile)
            atomic(root/'power-intent.json',{'running':True})
            if action=='ensure':
                deadline=time.monotonic()+1770
                while time.monotonic()<deadline:
                    phase=connection_status(root)
                    # ensure starts the shared computer, not a specific provider.
                    # Codex's RPC preflight rejects a broken Codex runtime; other
                    # working providers on an already-installed guest may run.
                    if phase in ('ready','runtime_failed'):
                        sync_provider_helper(root)
                        break
                    if phase=='failed': raise ValueError('Bot computer software setup failed. Ask the server administrator to check /var/log/cloud-init-output.log inside this profile computer, then retry sign-in after setup is repaired.')
                    if not running(root): raise ValueError('Computer stopped during setup. Inspect its console log.')
                    time.sleep(3)
                else: raise ValueError('The computer is still setting up. Wait a few minutes and retry; its disk and setup progress are preserved.')
            print(json.dumps({'state':'running','id':profile,'resources':{k:settings[k] for k in ('cpus','memory_mb','disk_gb')}}))
        else:
            is_running=running(root)
            if action=='reboot' and not is_running:
                raise ValueError('The bot computer is off. Start the computer before restarting it.')
            identity = reboot_process_identity(root, profile) if action=='reboot' else None
            atomic(root/'power-intent.json',{'running':action!='shutdown'})
            if is_running: qmp(root,'system_powerdown')
            if is_running:
                # Keep the operation lock until QEMU exits. A message sent during
                # shutdown then starts the same disk after shutdown has finished.
                deadline=time.monotonic()+60
                if action=='reboot':
                    while not reboot_process_exited(root, identity) and time.monotonic()<deadline: time.sleep(.5)
                    if not reboot_process_exited(root, identity):
                        raise ValueError('The computer is still shutting down. No hardware reset was sent; wait a moment and retry.')
                else:
                    while running(root) and time.monotonic()<deadline: time.sleep(.5)
                    if running(root): raise ValueError('The computer is still shutting down. No hardware reset was sent; wait a moment and retry.')
            if action=='reboot':
                # A hardware reset bypasses Chromium and filesystem shutdown.
                # Boot the same existing disk only after ACPI shutdown completed.
                disk = root / 'disk.qcow2'
                metadata = root / 'computer.json'
                if disk.is_symlink() or not disk.is_file() or metadata.is_symlink() or not metadata.is_file():
                    raise ValueError('The existing computer files are unavailable. No replacement process was started.')
                disk_stat = disk.stat()
                if (disk_stat.st_dev, disk_stat.st_ino) != identity[2]:
                    raise ValueError('The existing computer disk changed. No replacement process was started.')
                start(root,profile)
            print(json.dumps({'state':'shut off' if action=='shutdown' else 'restarting'}))

if __name__=='__main__':
    try: main()
    except Exception as error:
        # Subprocess output can include cloud-init paths and diagnostics. Retain
        # it locally in console.log; only sanitized control errors reach clients.
        message=str(error) if isinstance(error,ValueError) else 'Computer setup failed. Check server logs and retry.'
        print(message,file=sys.stderr);sys.exit(1)
