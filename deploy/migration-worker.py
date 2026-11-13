#!/usr/bin/env python3
"""Authenticated-server worker. Arguments are server-owned profile identities.

JSON on stdin carries one bounded operation. No owner credentials or cutover
secrets are written to stdout except the specific authenticated transfer reply.
"""
import base64
import contextlib
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


files = module('migration_files', 'migration-files.py')
journal = files.journal
require = journal.require


@contextlib.contextmanager
def operation(root):
    with (root / '.migration-operation.lock').open('a') as stream:
        os.chmod(stream.name, 0o600)
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield


def vm_manager():
    return module('migration_vm', 'vm-manager.py')


def stage(migration, label):
    with migration.locked():
        value = migration.read()
        steps = value.setdefault('steps', [])
        if steps and steps[-1]['state'] == 'working':
            steps[-1]['state'] = 'complete'
        steps.append({'label': label, 'state': 'complete' if label in ('Snapshot ready', 'Destination verified', 'Workspace active here') else 'working', 'at': int(time.time())})
        migration.save(value)


def cancel_requested(migration):
    require(not (migration.root / 'cancel-requested.json').exists(), 'Migration cancellation requested')


def schema(connection):
    # Upgraded and fresh workspaces can acquire equivalent columns in a
    # different order. Storage ordinals are not schema incompatibilities;
    # retain types, defaults, nullability and primary-key positions.
    return journal.digest({table: sorted((tuple(row[1:]) for row in connection.execute(f'PRAGMA table_info("{table}")')),
                                        key=lambda row: row[0])
                           for table in sorted(files.tables(connection)) if table != 'sqlite_sequence'})


def preflight(root, version):
    require(root.is_dir() and not root.is_symlink(), 'Profile is unavailable')
    with files.database((root / 'kindred.db').as_uri() + '?mode=ro', uri=True) as connection:
        active = connection.execute("SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')").fetchone()[0]
        names = files.tables(connection)
        if 'command_jobs' in names:
            active += connection.execute("SELECT COUNT(*) FROM command_jobs WHERE status IN ('starting','running')").fetchone()[0]
        if 'voice_calls' in names:
            active += connection.execute('SELECT COUNT(*) FROM voice_calls WHERE finished=0').fetchone()[0]
        if 'screens' in names:
            active += connection.execute('SELECT COUNT(*) FROM screens WHERE takeover=1 OR quiet_until>?', (int(time.time()),)).fetchone()[0]
        active += connection.execute("SELECT COUNT(*) FROM settings WHERE (key='takeover' AND value='true') OR (key='quiet_until' AND CAST(value AS INTEGER)>?)", (int(time.time()),)).fetchone()[0]
        bots = connection.execute('SELECT COUNT(*) FROM bots').fetchone()[0]
        rows = connection.execute('SELECT COUNT(*) FROM runs').fetchone()[0]
        info = {'protocol': 1, 'version': version, 'schema': schema(connection), 'active_work': active,
                'empty': bots == 0 and rows == 0, 'computer': (root / 'computer/disk.qcow2').exists(),
                'free_bytes': files.shutil.disk_usage(root).free}
    return info


def fence(root, transfer_id, role, require_empty=False):
    with files.database(root / 'kindred.db') as connection:
        connection.execute('BEGIN IMMEDIATE')
        old = connection.execute("SELECT value FROM settings WHERE key='_migration_gate'").fetchone()
        require(not old or json.loads(old[0]).get('id') == transfer_id, 'Another migration already owns this profile')
        require(connection.execute("SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')").fetchone()[0] == 0,
                'Finish active work before migrating')
        names = files.tables(connection)
        if 'command_jobs' in names:
            require(connection.execute("SELECT COUNT(*) FROM command_jobs WHERE status IN ('starting','running')").fetchone()[0] == 0,
                    'Finish running computer commands before migrating')
        if 'voice_calls' in names:
            require(connection.execute('SELECT COUNT(*) FROM voice_calls WHERE finished=0').fetchone()[0] == 0,
                    'End the voice call before migrating')
        if 'screens' in names:
            require(connection.execute('SELECT COUNT(*) FROM screens WHERE takeover=1 OR quiet_until>?', (int(time.time()),)).fetchone()[0] == 0,
                    'Release control of the bot computer before migrating')
        require(connection.execute("SELECT COUNT(*) FROM settings WHERE (key='takeover' AND value='true') OR (key='quiet_until' AND CAST(value AS INTEGER)>?)", (int(time.time()),)).fetchone()[0] == 0,
                'Release control of the bot computer before migrating')
        if require_empty:
            require(all(connection.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0] == 0 for table in ('bots', 'runs', 'chats')),
                    'Migration requires a new empty destination profile')
        files.set_gate(connection, transfer_id, role)
    journal.atomic(root / '.migration-vm.json', {'id': transfer_id, 'role': role})


def unfreeze(root, transfer_id):
    marker = root / '.migration-vm.json'
    if marker.exists():
        require(json.loads(marker.read_text())['id'] == transfer_id, 'Another migration owns the computer fence')
    with files.database(root / 'kindred.db') as connection:
        receipt = connection.execute("SELECT value FROM settings WHERE key='_migration_import'").fetchone()
        require(receipt and json.loads(receipt[0]).get('id') == transfer_id, 'Destination has not restored this workspace')
        gate = connection.execute("SELECT value FROM settings WHERE key='_migration_gate'").fetchone()
        require(not gate or files.gate_matches(connection, transfer_id), 'Another migration owns this workspace')
        marker.unlink(missing_ok=True)
        connection.execute("DELETE FROM settings WHERE key='_migration_gate'")


def stop_computer(root, profile):
    manager = vm_manager()
    computer = root / 'computer'
    if not (computer / 'computer.json').is_file():
        return
    # The normal shutdown path waits for graceful guest shutdown. It does not
    # send a hard reset or discard a live browser's unsaved state.
    subprocess.run([sys.executable, str(Path(__file__).with_name('vm-manager.py')), 'shutdown', profile],
                   check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=90)
    require(not manager.running(computer), 'The computer has not stopped')


def prepare_source(migration, profile, request, version, account_path):
    migration.begin_source(request['peer'])
    if migration.read()['state'] in ('prepared', 'committed'):
        return {'manifest': json.loads((migration.root / 'manifest.json').read_text()), 'status': migration.status()}
    stage(migration, 'Pausing new work')
    fence(migration.profile, migration.id, 'source')
    cancel_requested(migration)
    computer = migration.profile / 'computer'
    with migration.locked():
        value = migration.read()
        if 'computer_was_running' not in value:
            value['computer_was_running'] = (computer / 'computer.json').is_file() and vm_manager().running(computer)
            migration.save(value)
    stage(migration, 'Saving the bot computer')
    stop_computer(migration.profile, profile)
    cancel_requested(migration)
    stage(migration, 'Backing up and verifying the workspace')
    manifest = files.snapshot(migration, version, account_path)
    cancel_requested(migration)
    stage(migration, 'Snapshot ready')
    return {'manifest': manifest, 'status': migration.status()}


def install_staged_files(migration, profile):
    manifest = json.loads((migration.root / 'manifest.json').read_text())
    # Validation boots legitimately change the copied disk. Once installation
    # finished, retries must preserve that disk instead of copying it again.
    installed = migration.root / 'installed.json'
    if installed.exists():
        require(json.loads(installed.read_text()).get('digest') == journal.digest(manifest), 'Installed inventory changed')
        return
    for index, item in enumerate(manifest['files']):
        name = item['name']
        if name in ('kindred.db', 'account-credentials.json', 'computer/computer.json'):
            continue
        source = migration.root / 'files' / str(index)
        destination = migration.profile / name
        require(not any(parent.is_symlink() for parent in (destination, *destination.parents)), 'Destination contains a linked path')
        if destination.exists():
            require(destination.is_file() and journal.file_digest(destination) == item['sha256'],
                    'Destination files changed; existing files will not be overwritten')
        else:
            files.copy_verified(source, destination)
    if manifest['computer']:
        manager = vm_manager()
        source_info = next(migration.root / 'files' / str(i) for i, item in enumerate(manifest['files'])
                           if item['name'] == 'computer/computer.json')
        source_settings = json.loads(source_info.read_text())
        resources = {key: source_settings[key] for key in ('cpus', 'memory_mb', 'disk_gb')}
        manager.validate_resource_capacity(resources)
        computer = migration.profile / 'computer'
        # prepare allocates a new host port and profile identity while preserving
        # the copied disk, seed and guest SSH host keys.
        settings = manager.prepare(computer, profile)
        settings.update(resources)
        manager.atomic(computer / 'computer.json', settings)
        manager.seed(computer, profile, settings)
    journal.atomic(installed, {'digest': journal.digest(manifest)})


def verify_destination(migration, profile, account_path):
    if migration.read()['state'] == 'verified':
        return {'receipt': migration.receipt(), 'status': migration.status()}
    migration.verify_files()
    cancel_requested(migration)
    stage(migration, 'Verifying conversations, memory and monitor receipts')
    files.verified_database(migration)
    files.account_key_merge(migration, account_path)
    install_staged_files(migration, profile)
    manifest = json.loads((migration.root / 'manifest.json').read_text())
    if manifest['computer']:
        stage(migration, 'Checking the copied computer without external network access')
        manager = vm_manager()
        computer = migration.profile / 'computer'
        try:
            manager.start(computer, profile, migration_check=True)
            deadline = time.monotonic() + 240
            while time.monotonic() < deadline:
                cancel_requested(migration)
                if manager.connection_status(computer) in ('ready', 'runtime_failed'):
                    require(manager.connection_status(computer) == 'ready', 'The copied computer needs a runtime repair')
                    break
                time.sleep(2)
            else:
                raise ValueError('The copied computer did not become ready; the source is retained')
        finally:
            stop_computer(migration.profile, profile)
    receipt = migration.ready({'database': True, 'computer': True, 'credentials': True})
    stage(migration, 'Destination verified')
    return {'receipt': receipt, 'status': migration.status()}


def dispatch(root, profile, request, version, account_path):
    action = request['action']
    if action == 'preflight':
        return preflight(root, version)
    migration = journal.Journal(root, request['id'])
    if action == 'status':
        return migration.status() if migration.path.exists() else {'id': migration.id, 'state': 'absent'}
    if action == 'cancel_signal':
        journal.atomic(migration.root / 'cancel-requested.json', {'requested': True})
        return {'requested': True}
    with operation(root):
        if action == 'prepare':
            return prepare_source(migration, profile, request, version, account_path)
        if action == 'reserve':
            require(request['manifest']['server_version'] == version, 'Update both servers to the same version before migrating')
            if not migration.path.exists():
                info = preflight(root, version)
                require(info['empty'] and not info['computer'], 'Choose a new empty destination profile')
            receipt = migration.begin_destination(request['manifest'], request['peer'])
            # Idempotent retry may have already restored the copied database.
            if migration.read()['state'] in ('receiving', 'copied', 'verified'):
                fence(root, migration.id, 'destination', require_empty=True)
            return {'receipt': receipt, 'status': migration.status()}
        if action == 'read':
            data = files.read_chunk(migration, request['index'], request['offset'])
            return {'data': base64.b64encode(data).decode(), 'sha256': journal.digest(data)}
        if action == 'write':
            data = base64.b64decode(request['data'], validate=True)
            return {'offset': migration.chunk(request['index'], request['offset'], data, request['sha256'])}
        if action == 'bind':
            migration.bind_destination(request['receipt'], request['peer'])
            return migration.status()
        if action == 'verify':
            return verify_destination(migration, profile, account_path)
        if action == 'commit':
            if migration.read()['state'] == 'committed':
                return migration.commit_source(request['receipt'])
            with files.database(root / 'kindred.db') as connection:
                require(files.gate_matches(connection, migration.id), 'The source is not paused for this migration')
            require(json.loads((root / '.migration-vm.json').read_text())['id'] == migration.id, 'The source computer is not paused')
            if (root / 'computer/computer.json').is_file():
                require(not vm_manager().running(root / 'computer'), 'The source computer is still running')
            manifest = json.loads((migration.root / 'manifest.json').read_text())
            keys = next((item for item in manifest['files'] if item['name'] == 'account-credentials.json'), None)
            require((keys is None and not account_path.exists()) or
                    (keys is not None and account_path.is_file() and journal.file_digest(account_path) == keys['sha256']),
                    'Source account keys changed during migration. Cancel and restart the move to include them.')
            return migration.commit_source(request['receipt'])
        if action == 'activate_prepare':
            # A lost activation reply must never restore yesterday's snapshot
            # over work already performed at the destination.
            if migration.read()['state'] in ('activating', 'active'):
                migration.begin_activation(request['proof'])
                with files.database(root / 'kindred.db') as connection:
                    receipt = connection.execute("SELECT value FROM settings WHERE key='_migration_import'").fetchone()
                    if receipt and json.loads(receipt[0]).get('id') == migration.id:
                        return {'already_restored': True, 'status': migration.status()}
            # Recheck bytes before irrevocably entering activation.
            database = files.verified_database(migration)
            merged = files.account_key_merge(migration, account_path)
            migration.begin_activation(request['proof'])
            install_staged_files(migration, profile)
            # Server restores the database using its locked SQLite connection.
            return {'database': str(database), 'account_credentials': merged, 'status': migration.status()}
        if action == 'activated':
            # Database restore must precede this action; its imported gate must
            # still match the exact transfer. Never unfreeze another workspace.
            unfreeze(root, migration.id)
            migration.activated()
            stage(migration, 'Workspace active here')
            return migration.status()
        if action == 'abort':
            value = migration.read()
            require(value['role'] == 'destination' and value['state'] in ('receiving', 'copied', 'verified', 'aborted'),
                    'Destination may already be active; reconcile cutover first')
            stop_computer(root, profile)
            return migration.abort_destination()
        if action == 'cancel':
            migration.cancel_source(request.get('proof'))
            value = migration.read()
            marker = root / '.migration-vm.json'
            if marker.exists():
                require(json.loads(marker.read_text())['id'] == migration.id, 'Another migration owns the computer')
                marker.unlink()
            with files.database(root / 'kindred.db') as connection:
                if files.gate_matches(connection, migration.id):
                    connection.execute("DELETE FROM settings WHERE key='_migration_gate'")
            if value.get('computer_was_running'):
                vm_manager().start(root / 'computer', profile)
                journal.atomic(root / 'computer/power-intent.json', {'running': True})
            return migration.status()
        raise ValueError('Unknown migration operation')


def main():
    os.umask(0o077)
    require(len(sys.argv) == 4, 'Expected profile, version and account credential path')
    profile, version, credentials = sys.argv[1:]
    journal.identifier(profile)
    base = Path(os.environ.get('KINDRED_PROFILES_DIR', '/data/profiles')).resolve()
    root = base / profile
    require(root.is_dir() and not root.is_symlink(), 'Unknown migration profile')
    account_path = Path(credentials)
    require(account_path.parent.resolve() == base and account_path.name.startswith('.account-')
            and account_path.name.endswith('.credentials'), 'Invalid destination account store')
    request = json.loads(sys.stdin.buffer.read(6 * 1024 * 1024 + 1))
    print(json.dumps(dispatch(root, profile, request, version, account_path)))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        message = str(error) if isinstance(error, ValueError) else 'Migration stopped safely. The original workspace is retained; check storage and retry or cancel.'
        print(json.dumps({'error': message}))
        sys.exit(1)
