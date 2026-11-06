#!/usr/bin/env python3
"""Profile snapshot, verification and installation for the migration journal.

Callers fence the source database and stop its managed computer before snapshot.
The destination remains fenced throughout restore. Original files are never
removed by these operations, including after a successful migration.
"""
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time

spec = importlib.util.spec_from_file_location('migration_journal', Path(__file__).with_name('migration-journal.py'))
journal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(journal)
require = journal.require

COMPUTER_FILES = ('seed.iso', 'ssh_client', 'ssh_client.pub', 'ssh_host_ed25519_key',
                  'ssh_host_ed25519_key.pub', 'computer.json', 'user-data', 'meta-data')


@contextlib.contextmanager
def database(*args, **kwargs):
    connection = sqlite3.connect(*args, **kwargs)
    try:
        with connection:
            yield connection
    finally:
        connection.close()


def tables(connection):
    return {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}


def check_database(path):
    with database(path.as_uri() + '?mode=ro', uri=True) as connection:
        require(connection.execute('PRAGMA quick_check').fetchone() == ('ok',), 'Workspace database integrity check failed')
        require(connection.execute('PRAGMA foreign_key_check').fetchone() is None, 'Workspace references are incomplete')
        counts = {}
        for name in sorted(tables(connection)):
            require('"' not in name, 'Invalid workspace table name')
            counts[name] = connection.execute(f'SELECT COUNT(*) FROM "{name}"').fetchone()[0]
        return counts


def set_gate(connection, transfer_id, role):
    value = json.dumps({'id': transfer_id, 'role': role, 'state': 'staged'})
    connection.execute("INSERT INTO settings(key,value) VALUES('_migration_gate',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", (value,))


def gate_matches(connection, transfer_id):
    row = connection.execute("SELECT value FROM settings WHERE key='_migration_gate'").fetchone()
    return bool(row and json.loads(row[0]).get('id') == transfer_id)


def sanitize_copy(path, transfer_id):
    with database(path) as connection:
        names = tables(connection)
        # Keep historical local execution receipts, but discard the credentials
        # that would grant another machine access. Account sessions live outside
        # this database and are never part of a profile snapshot.
        if 'local_devices' in names:
            connection.execute("UPDATE local_devices SET secret='',mode='off'")
        if 'local_requests' in names:
            connection.execute("UPDATE local_requests SET status='expired' WHERE status IN ('queued','claimed')")
        # Existing Gmail history/receipts remain intact. A push subscription's
        # callback is tied to the old host, so continue with the no-model fast
        # history watcher until the owner reconnects push at the new address.
        if 'mail_watches' in names:
            connection.execute("UPDATE mail_watches SET body=json_set(body,'$.mode','fast','$.public_url','','$.next_check',0,'$.status','active','$.error','') WHERE json_extract(body,'$.mode')='push'")
        for name in ('mobile_push_outbox', 'mobile_devices', 'mobile_push_cursor', 'workspace_transfers'):
            if name in names:
                connection.execute(f'DELETE FROM "{name}"')
        connection.execute("UPDATE bots SET profile=json_set(profile,'$.local_access',json('false'),'$.local_device_id','')")
        connection.execute("UPDATE settings SET value=json_set(value,'$.local_access',json('false')) WHERE key='general'")
        connection.execute("DELETE FROM settings WHERE key IN ('_workspace_import','takeover','quiet_until')")
        connection.execute("INSERT INTO settings(key,value) VALUES('_account_disabled','false') ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        if 'screens' in names:
            connection.execute('UPDATE screens SET takeover=0,quiet_until=0')
        set_gate(connection, transfer_id, 'destination')
        connection.execute("INSERT INTO settings(key,value) VALUES('_migration_import',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", (json.dumps({'id': transfer_id}),))
    # Export one self-contained file, including the sanitized committed state.
    with database(path) as connection:
        require(connection.execute('PRAGMA journal_mode=DELETE').fetchone() == ('delete',),
                'Workspace snapshot could not be made self-contained')
    return check_database(path)


def terminate_with_parent():
    # The server can terminate a timed-out worker. Do not leave its qemu-img
    # child writing a staging file that a resumed transfer will reuse.
    import ctypes
    import signal
    parent = os.getppid()
    require(ctypes.CDLL(None).prctl(1, signal.SIGTERM) == 0, 'Could not protect the computer snapshot worker')
    if os.getppid() != parent: os.kill(os.getpid(), signal.SIGTERM)


def cancelled(migration):
    require(not (migration.root / 'cancel-requested.json').exists(), 'Migration cancellation requested')


def copy_verified(source, destination, migration=None):
    require(source.is_file() and not source.is_symlink(), 'Migration source is not a regular file')
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = destination.with_name(destination.name + '.partial')
    with source.open('rb') as reader, temporary.open('wb') as writer:
        os.chmod(temporary, 0o600)
        while data := reader.read(journal.CHUNK):
            if migration is not None: cancelled(migration)
            writer.write(data)
        writer.flush()
        os.fsync(writer.fileno())
    require(journal.file_digest(source) == journal.file_digest(temporary), 'Snapshot file changed during copying')
    os.replace(temporary, destination)
    fd = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
    try: os.fsync(fd)
    finally: os.close(fd)
    return destination


def snapshot(migration, version, account_credentials=None):
    state = migration.read()
    require(state['role'] == 'source' and state['state'] in ('preparing', 'prepared'), 'Source is not preparing')
    if state['state'] == 'prepared':
        return json.loads((migration.root / 'manifest.json').read_text())
    root = migration.profile
    staging = migration.root / 'snapshot'
    staging.mkdir(exist_ok=True, mode=0o700)
    backup = migration.root / 'source-backup.db'
    source = root / 'kindred.db'
    require(source.is_file() and not source.is_symlink(), 'Workspace database is unavailable')
    with database(source.as_uri() + '?mode=ro', uri=True) as connection:
        require(gate_matches(connection, migration.id), 'Pause source work before copying')
        active = connection.execute("SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')").fetchone()[0]
        require(active == 0, 'Finish active work before migration')
        # SQLite's backup API includes committed WAL contents. Copying kindred.db
        # alone would lose recent conversations on a running server.
        with database(backup) as saved:
            connection.backup(saved)
    os.chmod(backup, 0o600)
    check_database(backup)
    copy_verified(backup, staging / 'kindred.db')
    counts = sanitize_copy(staging / 'kindred.db', migration.id)
    cancelled(migration)
    for source in root.iterdir():
        if source.name.startswith('.migration') or source.name in ('kindred.db', 'kindred.db-wal', 'kindred.db-shm', 'kindred.db.lock', 'computer'):
            continue
        require(source.is_file() and not source.is_symlink(), 'An unsupported profile file needs review before migration')
        copy_verified(source, staging / source.name, migration)
    computer = root / 'computer'
    has_computer = (computer / 'disk.qcow2').is_file()
    if has_computer:
        # The VM manager owns shutdown and fencing. Require its persistent fence
        # and no live process; qemu-img also refuses a write-locked disk.
        require((root / '.migration-vm.json').is_file(), 'Pause the source computer before copying')
        pid_path = computer / 'qemu.pid'
        if pid_path.exists():
            pid = int(pid_path.read_text().strip())
            command = Path(f'/proc/{pid}/cmdline')
            require(not command.exists() or str(computer).encode() not in command.read_bytes(),
                    'The source computer has not stopped')
        target = staging / 'computer'
        target.mkdir(exist_ok=True, mode=0o700)
        flat = target / 'disk.qcow2.partial'
        # Flatten the entire backing chain; never replace an old base image with
        # a newly downloaded image at the destination.
        info = json.loads(subprocess.check_output(['qemu-img', 'info', '--output=json', str(computer / 'disk.qcow2')], timeout=30))
        require(shutil.disk_usage(staging).free > info['virtual-size'] + 64 * 1024**2,
                'The source needs more free storage to save a complete computer backup')
        with subprocess.Popen(['qemu-img', 'convert', '-O', 'qcow2', str(computer / 'disk.qcow2'), str(flat)],
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, preexec_fn=terminate_with_parent) as child:
            try:
                deadline = time.monotonic() + 1800
                while child.poll() is None:
                    cancelled(migration)
                    require(time.monotonic() < deadline, 'Computer backup timed out; the original disk is retained')
                    time.sleep(.2)
                require(child.returncode == 0, 'Computer backup failed; the original disk is retained')
            finally:
                if child.poll() is None:
                    child.terminate()
                    try: child.wait(timeout=5)
                    except subprocess.TimeoutExpired: child.kill(); child.wait()

        subprocess.run(['qemu-img', 'check', str(flat)], check=True, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=300)
        info = json.loads(subprocess.run(['qemu-img', 'info', '--output=json', str(flat)], check=True,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30).stdout)
        require(not info.get('backing-filename'), 'Computer snapshot still depends on its source')
        os.chmod(flat, 0o600)
        with flat.open('rb') as stream: os.fsync(stream.fileno())
        os.replace(flat, target / 'disk.qcow2')
        for name in COMPUTER_FILES:
            if (computer / name).exists():
                copy_verified(computer / name, target / name)
        for name in ('seed.iso', 'ssh_client', 'ssh_host_ed25519_key.pub', 'computer.json'):
            require((target / name).is_file(), 'Computer identity is incomplete')
    if account_credentials is not None and account_credentials.exists():
        copy_verified(account_credentials, staging / 'account-credentials.json')
    files = []
    for file in sorted(staging.rglob('*')):
        if file.is_file():
            require(not file.is_symlink() and not file.name.endswith('.partial'), 'Snapshot contains unfinished files')
            files.append({'name': str(file.relative_to(staging)), 'size': file.stat().st_size,
                          'sha256': journal.file_digest(file)})
    manifest = {'format': 'kindred-profile-migration', 'version': 1, 'server_version': version,
                'id': migration.id, 'files': files, 'database_counts': counts, 'computer': has_computer}
    return migration.source_ready(manifest)


def read_chunk(migration, index, offset):
    require(migration.read()['role'] == 'source' and migration.read()['state'] in ('prepared', 'committed'),
            'Source snapshot is unavailable')
    item = migration.item(index)
    require(type(offset) is int and 0 <= offset <= item['size'], 'Invalid source copy position')
    path = migration.root / 'snapshot' / item['name']
    require(path.is_file() and not path.is_symlink() and path.stat().st_size == item['size'], 'Source snapshot changed')
    with path.open('rb') as stream:
        stream.seek(offset)
        return stream.read(min(journal.CHUNK, item['size'] - offset))


def verified_database(migration):
    manifest = json.loads((migration.root / 'manifest.json').read_text())
    files, _ = journal.manifest_files(manifest)
    candidates = [(i, item) for i, item in enumerate(files) if item['name'] == 'kindred.db']
    require(len(candidates) == 1, 'Migration contains no workspace database')
    index, item = candidates[0]
    path = migration.root / 'files' / str(index)
    require(journal.file_digest(path) == item['sha256'], 'Workspace checksum changed')
    require(check_database(path) == manifest['database_counts'], 'Workspace row counts do not match')
    with database(path.as_uri() + '?mode=ro', uri=True) as connection:
        require(gate_matches(connection, migration.id), 'Imported workspace is not fenced')
    return path


def account_key_merge(migration, destination_path):
    """Return a merge plan without overwriting an existing account's API keys."""
    manifest = json.loads((migration.root / 'manifest.json').read_text())
    source = next((migration.root / 'files' / str(i) for i, f in enumerate(manifest['files'])
                   if f['name'] == 'account-credentials.json'), None)
    old = json.loads(destination_path.read_text()) if destination_path.exists() else {}
    incoming = json.loads(source.read_text()) if source is not None else {}
    require(isinstance(old, dict) and isinstance(incoming, dict), 'Account credentials have an unsupported format')
    for key, value in incoming.items():
        require(isinstance(value, str), 'Invalid account credential')
        require(not old.get(key) or not value or old[key] == value,
                'The destination account has different API credentials. Use a separate destination account or reconcile its connection settings before migrating.')
    return {**old, **{key: value for key, value in incoming.items() if value}}
