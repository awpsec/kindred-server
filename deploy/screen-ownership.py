#!/usr/bin/env python3
"""Account-local screen ownership fence. Never delete browser state or stop a service."""
import argparse, contextlib, fcntl, hashlib, json, os, pwd, re, socket, stat, subprocess, sys, tempfile
from pathlib import Path
ROOT = Path('/home/bot/.local/share/kindred')
EXPECTED_SOURCE_SHA256 = None
HELPER = Path('/usr/local/lib/kindred/screen-ownership.py')

class UnreadableProcess(RuntimeError): pass
UUID = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z')

def private(path, create=False):
    for ancestor in [path, *path.parents]:
        if ancestor.is_symlink():
            raise RuntimeError('Screen ownership path is redirected')
    if create and not path.exists():
        path.mkdir(mode=0o700)
    info = path.lstat()
    # Older guests retain this non-writable shared state root at 0755.
    # Only its metadata may be public; ownership records and profiles stay private.
    legacy_root = path == ROOT and stat.S_IMODE(info.st_mode) == 0o755
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or (info.st_mode & 0o077 and not legacy_root):
        raise RuntimeError('Screen ownership directory is not private')

def read_record(path):
    if not os.path.lexists(path):
        return None
    info=path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Invalid screen ownership record')
    fd=os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as stream:
        value=json.load(stream)
    if not isinstance(value,dict) or set(value)!= {'owner','generation'} or not all(isinstance(value[k],str) and UUID.fullmatch(value[k]) for k in value):
        raise RuntimeError('Invalid screen ownership record')
    return value

def empty(slot, root=ROOT, proc=Path('/proc'), xroot=Path('/tmp'), service=None):
    profile=root / ('browser' if slot==1 else f'browser-{slot}')
    if os.path.lexists(profile):
        raise RuntimeError('This screen retains browser state')
    if os.path.lexists(xroot / '.X11-unix' / f'X{slot}') or os.path.lexists(xroot / f'.X{slot}-lock'):
        raise RuntimeError('This screen has a display')
    if service is None:
        env=dict(os.environ, XDG_RUNTIME_DIR=f'/run/user/{os.getuid()}',DBUS_SESSION_BUS_ADDRESS=f'unix:path=/run/user/{os.getuid()}/bus')
        result=subprocess.run(['systemctl','--user','show',f'kindred-screen@{slot}.service','--property=LoadState,ActiveState,SubState,Job'],env=env,capture_output=True,timeout=5)
        if result.returncode:
            raise RuntimeError('Screen service ownership could not be verified')
        service=dict(line.split('=',1) for line in result.stdout.decode().splitlines() if '=' in line)
    if service.get('ActiveState')!='inactive' or service.get('SubState') not in ('dead','failed') or service.get('Job') not in ('','0') or service.get('LoadState') not in ('loaded','not-found'):
        raise RuntimeError('Screen service is active or uncertain')
    try:
        inspect_processes(slot,os.getuid(),root,proc)
    except UnreadableProcess:
        if proc!=Path('/proc') or root!=ROOT: raise RuntimeError('Guest process inspection is incomplete') from None
        privileged_process_receipt(slot)
    ports={5900+(slot-1)*2,5901+(slot-1)*2}
    for name in ('tcp','tcp6'):
        for line in (proc/'net'/name).read_text().splitlines()[1:]:
            fields=line.split()
            if int(fields[1].rsplit(':',1)[1],16) in ports:
                raise RuntimeError('This screen has a network connection')

def process_identity(entry):
    info=entry.stat()
    fields=(entry/'stat').read_text().rsplit(')',1)[1].split()
    return info.st_uid,info.st_ino,fields[0],fields[19]

def inspect_processes(slot,uid,root=ROOT,proc=Path('/proc')):
    profile=root/('browser' if slot==1 else f'browser-{slot}')
    initial={entry.name for entry in proc.iterdir() if entry.name.isdigit()}
    inspected={}
    for name in initial:
        entry=proc/name
        try:
            before=process_identity(entry)
            if before[0]!=uid: continue
            if before[2]=='Z': continue
            argv=(entry/'cmdline').read_bytes().split(b'\0')
            environ=(entry/'environ').read_bytes().split(b'\0')
            if not any(argv): raise RuntimeError('A guest process cannot be identified')
            after=process_identity(entry)
            if before!=after: raise RuntimeError('Guest process identity changed')
            inspected[name]=after
            display=f':{slot}'.encode()
            if any(a==display or a==display+b'.0' or a==b'DISPLAY='+display or a==b'DISPLAY='+display+b'.0' for a in argv+environ):
                raise RuntimeError('A process owns this display')
            if f'KINDRED_SCREEN_SLOT={slot}'.encode() in environ:
                raise RuntimeError('A guest request owns this screen')
            if any(str(profile).encode() in a for a in argv+environ):
                raise RuntimeError('A process owns this browser profile')
        except PermissionError:
            raise UnreadableProcess('Guest process inspection is incomplete') from None
        except (FileNotFoundError,IndexError,ValueError):
            raise RuntimeError('Guest process inspection is incomplete') from None
    final={entry.name for entry in proc.iterdir() if entry.name.isdigit()}
    if final!=initial: raise RuntimeError('Guest process inventory changed')
    for name,expected in inspected.items():
        try:
            if process_identity(proc/name)!=expected: raise RuntimeError('Guest process identity changed')
        except OSError: raise RuntimeError('Guest process inspection is incomplete') from None

def validate_installed_helper():
    for ancestor in [HELPER,*HELPER.parents]:
        info=ancestor.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid!=0 or info.st_mode&0o022:
            raise RuntimeError('Privileged process inspector is unavailable')
    info=HELPER.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_mode&0o777!=0o755:
        raise RuntimeError('Privileged process inspector is unavailable')
    expected=EXPECTED_SOURCE_SHA256
    if expected is None:
        expected=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    if hashlib.sha256(HELPER.read_bytes()).hexdigest()!=expected:
        raise RuntimeError('Privileged process inspector does not match')

def privileged_process_receipt(slot):
    validate_installed_helper()
    try:
        result=subprocess.run(['/usr/bin/sudo','-n','/usr/bin/python3','-I','-B',str(HELPER),'--inspect-proc','--slot',str(slot)],capture_output=True,timeout=5)
        if result.returncode or len(result.stdout)>512: raise ValueError()
        receipt=json.loads(result.stdout)
        if receipt!={'protocol':1,'slot':slot,'uid':os.getuid(),'status':'clear'}: raise ValueError()
    except (OSError,ValueError,subprocess.TimeoutExpired):
        raise RuntimeError('Complete guest process ownership could not be verified') from None

def verify_parent_lock(slot,uid):
    records=ROOT/'screen-owners'; lockpath=records/f'{slot}.lock'
    for ancestor in ROOT.parents:
        if ancestor.is_symlink():
            raise RuntimeError('Process inspector lock is unavailable')
    for path in (ROOT,records,lockpath):
        info=path.lstat()
        # Match private(ROOT): the legacy state root alone may remain 0755.
        legacy_root = path == ROOT and stat.S_ISDIR(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o755
        if stat.S_ISLNK(info.st_mode) or info.st_uid!=uid or (info.st_mode&0o077 and not legacy_root):
            raise RuntimeError('Process inspector lock is unavailable')
    info=lockpath.lstat()
    if not stat.S_ISREG(info.st_mode): raise RuntimeError('Process inspector lock is unavailable')
    ancestors=set();pid=os.getppid()
    while pid>1:
        if Path(f'/proc/{pid}').stat().st_uid==uid: ancestors.add(pid)
        pid=int(Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[1])
    for line in Path('/proc/locks').read_text().splitlines():
        fields=line.split()
        if len(fields)>=8 and fields[1]=='FLOCK' and fields[3]=='WRITE' and int(fields[4]) in ancestors:
            device=fields[5].split(':')
            if len(device)==3 and int(device[0],16)==os.major(info.st_dev) and int(device[1],16)==os.minor(info.st_dev) and int(device[2])==info.st_ino:
                return
    raise RuntimeError('Process inspector requires its ancestor slot lock')


def root_inspector(argv):
    # This branch has no ownership, profile, process-launch or permission writes.
    if len(argv)!=3 or argv[:2]!=['--inspect-proc','--slot'] or not argv[2].isdigit() or not 1<=int(argv[2])<=32:
        raise RuntimeError('Invalid process inspector request')
    slot=int(argv[2])
    uid=pwd.getpwnam('bot').pw_uid
    if os.geteuid()!=0 or os.environ.get('SUDO_UID')!=str(uid):
        raise RuntimeError('Process inspector caller is invalid')
    validate_installed_helper()
    receipt={'protocol':1,'slot':slot,'uid':uid,'status':'unknown'}
    try:
        verify_parent_lock(slot,uid)
        inspect_processes(slot,uid)
        verify_parent_lock(slot,uid)
        receipt['status']='clear'
    except Exception:
        print(json.dumps(receipt));return 1
    print(json.dumps(receipt));return 0


def inherited_lock(slot, lockpath, current):
    """Accept only a live ancestor supervisor holding this exact lock and identity."""
    env=os.environ
    if env.get('KINDRED_SCREEN_SLOT') != str(slot) or current != {'owner':env.get('KINDRED_SCREEN_OWNER'),'generation':env.get('KINDRED_SCREEN_GENERATION')}:
        return False
    try:
        pid=int(env['KINDRED_SCREEN_LOCK_PID']); fd=int(env['KINDRED_SCREEN_LOCK_FD'])
        ancestor=os.getppid()
        while ancestor>1 and ancestor!=pid:
            ancestor=int(Path(f'/proc/{ancestor}/stat').read_text().rsplit(')',1)[1].split()[1])
        if ancestor!=pid or Path(f'/proc/{pid}').stat().st_uid!=os.getuid(): return False
        actual=Path(f'/proc/{pid}/fd/{fd}').stat(); expected=lockpath.stat()
        if (actual.st_dev,actual.st_ino)!=(expected.st_dev,expected.st_ino): return False
        # An open FD is insufficient: the supervisor must own the flock.
        for line in Path('/proc/locks').read_text().splitlines():
            fields=line.split()
            if len(fields)>=8 and fields[1]=='FLOCK' and fields[3]=='WRITE' and fields[4]==str(pid):
                device=fields[5].split(':')
                if len(device)==3 and int(device[0],16)==os.major(expected.st_dev) and int(device[1],16)==os.minor(expected.st_dev) and int(device[2])==expected.st_ino:
                    return True
    except (OSError,ValueError,KeyError):
        pass
    return False

@contextlib.contextmanager
def launch_guard(slot, root=ROOT, desktop_context=None, startup=False):
    """Hold the claim lock until a browser directory pins the slot permanently.

    A startup creates only a desktop/profile, never delivers an old request.
    Browser actions require the ancestor request or this display's generation.
    """
    if not 1<=slot<=32: raise RuntimeError('Invalid screen')
    private(root)
    records=root/'screen-owners'; private(records,True)
    path=records/f'{slot}.json'; lockpath=records/f'{slot}.lock'
    current=read_record(path)
    borrowed=inherited_lock(slot,lockpath,current) if lockpath.exists() else False
    descriptor=None
    if not borrowed:
        descriptor=os.open(lockpath,os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
        info=os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode&0o077:
            os.close(descriptor); raise RuntimeError('Invalid screen lock')
        try: fcntl.flock(descriptor,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except Exception: os.close(descriptor); raise
    try:
        current=read_record(path)
        if current is not None and not borrowed and not startup:
            context=desktop_context or {}
            if context.get('KINDRED_SCREEN_OWNER')!=current['owner'] or context.get('KINDRED_SCREEN_GENERATION')!=current['generation']:
                raise RuntimeError('Desktop ownership changed')
        profile=root/('browser' if slot==1 else f'browser-{slot}')
        private(profile,True)
        yield current
    finally:
        if descriptor is not None: os.close(descriptor)


def main():
    p=argparse.ArgumentParser()
    p.add_argument('--slot',type=int,required=True)
    p.add_argument('--owner',required=True); p.add_argument('--generation',required=True)
    p.add_argument('--previous-owner'); p.add_argument('--previous-generation')
    p.add_argument('--mode',choices=('dispatch','claim-empty','probe-empty'),required=True)
    p.add_argument('--legacy',action='store_true'); p.add_argument('command',nargs=argparse.REMAINDER)
    a=p.parse_args()
    if not 1<=a.slot<=32 or not UUID.fullmatch(a.owner) or not UUID.fullmatch(a.generation):
        raise RuntimeError('Invalid screen identity')
    private(ROOT)
    records=ROOT/'screen-owners'; private(records,True)
    lockpath=records/f'{a.slot}.lock'
    fd=os.open(lockpath,os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
    info=os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode&0o077:
        raise RuntimeError('Invalid screen lock')
    fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
    path=records/f'{a.slot}.json'; current=read_record(path)
    desired={'owner':a.owner,'generation':a.generation}
    if a.mode in ('claim-empty','probe-empty'):
        previous={'owner':a.previous_owner,'generation':a.previous_generation}
        if current is not None and current not in (desired,previous):
            raise RuntimeError('Screen ownership changed')
        if a.slot==1: raise RuntimeError('The primary desktop cannot be reclaimed')
        if current!=desired:
            empty(a.slot)
    elif current is None and not a.legacy:
        raise RuntimeError('Screen ownership has not been verified')
    elif current is not None and current!=desired:
        raise RuntimeError('Screen ownership changed')
    if a.mode=='probe-empty':
        print(json.dumps({'slot':a.slot,'empty':True})); return
    if current!=desired:
        out,name=tempfile.mkstemp(prefix=f'.{a.slot}-',dir=records)
        temp=Path(name)
        with os.fdopen(out,'w') as stream:
            json.dump(desired,stream); stream.flush(); os.fsync(stream.fileno())
        os.replace(temp,path)
        directory=os.open(records,os.O_DIRECTORY); os.fsync(directory); os.close(directory)
    if a.mode=='claim-empty':
        print(json.dumps(dict(desired,slot=a.slot,verified_claim=True,empty=True if current!=desired else None))); return
    command=a.command[1:] if a.command[:1]==['--'] else a.command
    if not command: raise RuntimeError('Missing owned screen command')
    os.environ['KINDRED_SCREEN_SLOT']=str(a.slot)
    os.environ['KINDRED_SCREEN_OWNER']=a.owner
    os.environ['KINDRED_SCREEN_GENERATION']=a.generation
    # The supervisor owns the lock; detached browser/command children never inherit it.
    os.environ['KINDRED_SCREEN_LOCK_PID']=str(os.getpid())
    os.environ['KINDRED_SCREEN_LOCK_FD']=str(fd)
    result=subprocess.run(command,close_fds=True)
    sys.exit(result.returncode)

if __name__=='__main__':
    try:
        if '--inspect-proc' in sys.argv[1:]: sys.exit(root_inspector(sys.argv[1:]))
        if os.geteuid()==0: raise RuntimeError('Ownership changes require the guest user')
        main()
    except Exception as error:
        print('Screen ownership could not be verified: '+str(error),file=sys.stderr)
        sys.exit(1)
