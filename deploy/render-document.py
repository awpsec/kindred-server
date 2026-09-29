"""Render a private DOCX to PDF, without network or access to server data.

The converter needs Landlock ABI 3 (Linux 6.2+) and libseccomp. Unsupported
hosts fail closed; the client can still offer its local, approximate preview.
Input/output are bytes on stdin/stdout. Never log document contents.
"""
import ctypes as C
import errno
import io
import os
from pathlib import Path
import platform
import resource
import signal
import sys
import tempfile
import zipfile
from xml.parsers import expat

MAX_INPUT = 8 * 1024 * 1024
MAX_OUTPUT = 32 * 1024 * 1024


def validate(data):
    if len(data) > MAX_INPUT:
        raise ValueError('Document exceeds 8 MB')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) > 4096 or sum(x.file_size for x in entries) > 64 * 1024 * 1024:
            raise ValueError('Document is too complex to preview')
        if not {'word/document.xml', '[Content_Types].xml', '_rels/.rels'}.issubset(archive.namelist()):
            raise ValueError('Not a DOCX document')
        if any(x.flag_bits & 1 for x in entries):
            raise ValueError('Encrypted documents cannot be previewed')

        # Reject broken/hostile XML before OfficeKit can show an import dialog.
        # SAX validation avoids building a second large document tree.
        elements = 0
        for entry in entries:
            if not entry.filename.lower().endswith(('.xml', '.rels')):
                continue
            if entry.file_size > 12 * 1024 * 1024:
                raise ValueError('Document XML is too large')
            parser = expat.ParserCreate(namespace_separator="}")
            depth = 0
            first = True
            def start(name, attrs):
                nonlocal elements, depth, first
                if first and entry.filename == 'word/document.xml' and name not in (
                    'http://schemas.openxmlformats.org/wordprocessingml/2006/main}document',
                    'http://purl.oclc.org/ooxml/wordprocessingml/main}document',
                ):
                    raise ValueError('Invalid Word document root')
                first = False
                elements += 1
                depth += 1
                if elements > 500000 or depth > 256:
                    raise ValueError('Document XML is too complex')
            def end(name):
                nonlocal depth
                depth -= 1
            def doctype(*args):
                raise ValueError('Document XML cannot contain a DTD')
            parser.StartElementHandler = start
            parser.EndElementHandler = end
            parser.StartDoctypeDeclHandler = doctype
            with archive.open(entry) as part:
                while chunk := part.read(65536):
                    parser.Parse(chunk, False)
                parser.Parse(b'', True)


def restrict(directory):
    # Syscall numbers shared by the supported server architectures.
    if platform.machine() not in ('x86_64', 'aarch64'):
        raise RuntimeError('Unsupported preview architecture')
    libc = C.CDLL(None, use_errno=True)
    def checked(value):
        if value < 0:
            raise OSError(C.get_errno(), 'Document sandbox unavailable')
        return value
    abi = libc.syscall(444, 0, 0, 1)
    if abi < 3:
        raise RuntimeError('Document preview needs Landlock ABI 3')
    class Rules(C.Structure):
        _fields_ = [('fs', C.c_uint64), ('net', C.c_uint64), ('scoped', C.c_uint64)]
    class Beneath(C.Structure):
        _pack_ = 1
        _fields_ = [('access', C.c_uint64), ('fd', C.c_int32)]
    # Scope signals and abstract Unix sockets to this process and its children.
    # All filesystem mutation is denied except within the private scratch dir.
    rules = Rules((1 << (16 if abi >= 5 else 15)) - 1, 3 if abi >= 4 else 0, 3 if abi >= 6 else 0)
    fd = checked(libc.syscall(444, C.byref(rules), C.sizeof(rules), 0))
    read = 1 | 4 | 8
    def allow(path, access):
        if not os.path.exists(path):
            return
        node = os.open(path, os.O_PATH | os.O_CLOEXEC)
        try:
            if not os.path.isdir(path):
                access &= 1 | 2 | 4 | (1 << 14) | (1 << 15)
            checked(libc.syscall(445, fd, 1, C.byref(Beneath(access, node)), 0))
        finally:
            os.close(node)
    for path in ['/usr', '/lib', '/lib64', '/bin', '/etc/fonts', '/etc/libreoffice',
                 '/etc/ld.so.cache', '/etc/localtime', '/etc/passwd', '/proc/self',
                 '/proc/meminfo', '/proc/cpuinfo', '/var/cache/fontconfig', '/var/lib/libreoffice', '/var/spool/libreoffice', '/etc/nsswitch.conf']:
        allow(path, read)
    for path in ['/dev/null', '/dev/urandom', '/dev/random']:
        allow(path, 2 | 4)
    allow(directory, rules.fs)
    checked(libc.prctl(38, 1, 0, 0, 0))  # PR_SET_NO_NEW_PRIVS
    checked(libc.syscall(446, fd, 0))
    os.close(fd)
    # Landlock's TCP rules do not cover UDP. Block network sockets altogether,
    # and prevent process-memory and io_uring access outside this sandbox.
    sec = C.CDLL('libseccomp.so.2')
    sec.seccomp_init.restype = C.c_void_p
    sec.seccomp_init.argtypes = [C.c_uint32]
    sec.seccomp_rule_add.argtypes = [C.c_void_p, C.c_uint32, C.c_int, C.c_uint]
    sec.seccomp_load.argtypes = [C.c_void_p]
    sec.seccomp_release.argtypes = [C.c_void_p]
    class Compare(C.Structure):
        _fields_ = [('arg', C.c_uint), ('op', C.c_int), ('a', C.c_uint64), ('b', C.c_uint64)]
    ctx = sec.seccomp_init(0x7fff0000)
    if not ctx:
        raise RuntimeError('Document sandbox unavailable')
    try:
        deny = 0x50000 | errno.EPERM
        for name in [b'ptrace', b'process_vm_readv', b'process_vm_writev', b'pidfd_getfd',
                     b'io_uring_setup', b'mount', b'unshare', b'setns', b'execve', b'execveat',
                     b'fork', b'vfork', b'setsid', b'setpgid', b'socket', b'connect',
                     b'pidfd_send_signal', b'tkill', b'rt_sigqueueinfo', b'rt_tgsigqueueinfo']:
            call = sec.seccomp_syscall_resolve_name(name)
            if call >= 0 and sec.seccomp_rule_add(ctx, deny, call, 0) != 0:
                raise RuntimeError('Document sandbox unavailable')
        # OfficeKit needs threads, never new processes. clone3 falls back to
        # clone in glibc when unavailable; restrict clone to CLONE_THREAD.
        call = sec.seccomp_syscall_resolve_name(b'clone3')
        if call >= 0 and sec.seccomp_rule_add(ctx, 0x50000 | errno.ENOSYS, call, 0) != 0:
            raise RuntimeError('Document sandbox unavailable')
        if sec.seccomp_rule_add(ctx, deny, sec.seccomp_syscall_resolve_name(b'clone'),
                                1, Compare(0, 7, 0x10000, 0)) != 0:
            raise RuntimeError('Document sandbox unavailable')
        # On pre-ABI-6 kernels, explicitly prevent signals to other processes.
        # Thread runtimes use tgkill for their own thread group.
        for name in (b'kill', b'tgkill'):
            if sec.seccomp_rule_add(ctx, deny, sec.seccomp_syscall_resolve_name(name),
                                    1, Compare(0, 1, os.getpid(), 0)) != 0:
                raise RuntimeError('Document sandbox unavailable')
        if sec.seccomp_load(ctx) != 0:
            raise RuntimeError('Document sandbox unavailable')
    finally:
        sec.seccomp_release(ctx)
    resource.setrlimit(resource.RLIMIT_AS, (1536 * 1024 * 1024,) * 2)
    resource.setrlimit(resource.RLIMIT_CPU, (60, 60))
    resource.setrlimit(resource.RLIMIT_FSIZE, (64 * 1024 * 1024,) * 2)
    resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def render_kit(root):
    # The stable LibreOfficeKit C API avoids launching an interactive office or
    # connecting to any running instance. Only load/save/destroy are needed.
    pointer = C.c_void_p
    destroy = C.CFUNCTYPE(None, pointer)
    load = C.CFUNCTYPE(pointer, pointer, C.c_char_p)
    save = C.CFUNCTYPE(C.c_int, pointer, C.c_char_p, C.c_char_p, C.c_char_p)
    class OfficeClass(C.Structure):
        _fields_ = [('size', C.c_size_t), ('destroy', destroy), ('load', load)]
    class DocumentClass(C.Structure):
        _fields_ = [('size', C.c_size_t), ('destroy', destroy), ('save', save)]
    library = C.CDLL('/usr/lib/libreoffice/program/libmergedlo.so', mode=C.RTLD_GLOBAL)
    library.libreofficekit_hook_2.restype = pointer
    library.libreofficekit_hook_2.argtypes = [C.c_char_p, C.c_char_p]
    office = library.libreofficekit_hook_2(b'/usr/lib/libreoffice/program',
                                         (root / 'profile').as_uri().encode())
    if not office:
        raise RuntimeError('Office engine unavailable')
    api = C.cast(office, C.POINTER(C.POINTER(OfficeClass))).contents.contents
    document = api.load(office, (root / 'input.docx').as_uri().encode())
    if not document:
        raise RuntimeError('Document could not be opened')
    doc = C.cast(document, C.POINTER(C.POINTER(DocumentClass))).contents.contents
    if not doc.save(document, (root / 'input.pdf').as_uri().encode(), b'pdf', None):
        raise RuntimeError('Document could not be laid out')
    doc.destroy(document)
    api.destroy(office)


def convert(data, timeout=75):
    validate(data)
    with tempfile.TemporaryDirectory(prefix='kindred-document-') as temp:
        root = Path(temp)
        (root / 'input.docx').write_bytes(data)
        profile = root / 'profile' / 'user'
        profile.mkdir(parents=True)
        (profile / 'registrymodifications.xcu').write_text('''<?xml version="1.0"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Writer/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
</oor:items>''')
        env = {'PATH': '/usr/bin:/bin', 'HOME': temp, 'TMPDIR': temp,
               'LANG': 'C.UTF-8', 'SAL_USE_VCLPLUGIN': 'svp', 'SAL_DISABLE_JAVA': '1'}
        parent_pid = os.getpid()
        pid = os.fork()
        if pid == 0:
            try:
                os.setsid()
                libc = C.CDLL(None)
                if libc.prctl(1, signal.SIGKILL, 0, 0, 0) or os.getppid() != parent_pid:
                    os._exit(1)
                os.environ.clear()
                os.environ.update(env)
                os.chdir(temp)
                with (root / 'log').open('wb') as log:
                    os.dup2(log.fileno(), 1)
                    os.dup2(log.fileno(), 2)
                restrict(temp)
                render_kit(root)
                os._exit(0)
            except BaseException:
                os._exit(1)
        import time
        deadline = time.monotonic() + timeout
        try:
            while True:
                done, code = os.waitpid(pid, os.WNOHANG)
                if done:
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError('Document conversion timed out')
                time.sleep(0.05)
        finally:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
        output = root / 'input.pdf'
        if code or not output.is_file() or output.stat().st_size > MAX_OUTPUT:
            raise RuntimeError('Document could not be laid out')
        pdf = output.read_bytes()
        if not pdf.startswith(b'%PDF-'):
            raise RuntimeError('Invalid document preview')
        return pdf


if __name__ == '__main__':
    try:
        sys.stdout.buffer.write(convert(sys.stdin.buffer.read(MAX_INPUT + 1)))
    except Exception:
        print('Page preview unavailable on this server.', file=sys.stderr)
        sys.exit(1)
