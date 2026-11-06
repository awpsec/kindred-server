#!/usr/bin/env python3
"""Durable, profile-scoped migration handoff and resumable file staging.

The authenticated server owns this module. Public status never returns handoff
secrets. A failed or uncertain handoff stays fenced until its receipt is resolved.
"""
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import secrets
import shutil
import time
import uuid

CHUNK = 4 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def identifier(value):
    require(str(uuid.UUID(value)) == value, 'Invalid migration identity')
    return value


def digest(value):
    data = value if isinstance(value, bytes) else json.dumps(value, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(data).hexdigest()


def atomic(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_name(path.name + '.' + secrets.token_hex(8) + '.tmp')
    try:
        with temporary.open('xb') as stream:
            os.chmod(temporary, 0o600)
            stream.write(json.dumps(value, sort_keys=True).encode())
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        temporary.unlink(missing_ok=True)


def relative_name(value):
    require(isinstance(value, str) and value and len(value) <= 1024, 'Invalid migration file name')
    path = PurePosixPath(value)
    require(bool(path.parts) and not path.is_absolute() and str(path) == value and not any(p in ('..', '.') for p in path.parts)
            and '\\' not in value and '\x00' not in value, 'Invalid migration file path')
    return value


def manifest_files(manifest):
    require(manifest.get('format') == 'kindred-profile-migration' and manifest.get('version') == 1,
            'Update both servers before migrating')
    identifier(manifest['id'])
    files = manifest.get('files')
    require(isinstance(files, list) and 0 < len(files) <= 10000, 'Invalid migration inventory')
    names = set()
    total = 0
    for item in files:
        name = relative_name(item['name'])
        require(not any(part.startswith('.migration') for part in PurePosixPath(name).parts)
                and name not in ('kindred.db-wal', 'kindred.db-shm', 'kindred.db.lock')
                and (not name.startswith('computer/') or name.split('/', 1)[1] in
                     ('disk.qcow2', 'seed.iso', 'ssh_client', 'ssh_client.pub', 'ssh_host_ed25519_key',
                      'ssh_host_ed25519_key.pub', 'computer.json', 'user-data', 'meta-data')),
                'Inventory contains a reserved migration or computer control file')
        require(name not in names, 'Duplicate migration file')
        names.add(name)
        size = item['size']
        require(type(size) is int and 0 <= size <= 2 * 1024**4, 'Invalid migration file size')
        require(isinstance(item.get('sha256'), str) and len(item['sha256']) == 64
                and all(c in '0123456789abcdef' for c in item['sha256']), 'Invalid migration file checksum')
        total += size
    # Prevent file/directory aliasing before any bytes are accepted.
    for name in names:
        require(not any(str(p) in names for p in PurePosixPath(name).parents if str(p) != '.'),
                'Conflicting migration paths')
    require(total <= 4 * 1024**4, 'Migration exceeds supported storage capacity')
    return files, total


def file_digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        while chunk := stream.read(CHUNK): checksum.update(chunk)
    return checksum.hexdigest()


class Journal:
    def __init__(self, profile_root, transfer_id):
        self.profile = Path(profile_root)
        require(not self.profile.is_symlink(), 'Migration profile must not be a link')
        self.id = identifier(transfer_id)
        require(not (self.profile / '.migrations').is_symlink(), 'Migration storage must not be a link')
        self.root = self.profile / '.migrations' / self.id
        require(not self.root.is_symlink(), 'Migration journal must not be a link')
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        require(self.root.resolve() == self.profile.resolve() / '.migrations' / self.id,
                'Migration storage must stay inside its profile')
        self.path = self.root / 'journal.json'

    @contextlib.contextmanager
    def locked(self):
        with (self.profile / '.migration.lock').open('a') as lock:
            os.chmod(lock.name, 0o600)
            fcntl.flock(lock, fcntl.LOCK_EX)
            yield

    def read(self):
        require(self.path.is_file() and not self.path.is_symlink(), 'Migration journal unavailable')
        value = json.loads(self.path.read_text())
        require(value['id'] == self.id, 'Migration identity changed')
        return value

    def save(self, value, state=None):
        if state is not None:
            value['state'] = state
        value['updated'] = int(time.time())
        atomic(self.path, value)

    def reserve(self):
        pointer = self.profile / '.migration-current.json'
        if pointer.exists():
            previous = json.loads(pointer.read_text())['id']
            if previous != self.id:
                old = Journal(self.profile, previous).read()
                require(old['state'] in ('cancelled', 'active', 'aborted'),
                        'Finish or recover the existing migration first')
        atomic(pointer, {'id': self.id})

    def begin_source(self, peer):
        with self.locked():
            if self.path.exists():
                old = self.read()
                require(old['role'] == 'source' and old['peer'] == peer and old['state'] != 'cancelled',
                        'Resume this migration with its original destination')
                return self.status()
            self.reserve()
            self.save({'id': self.id, 'role': 'source', 'peer': peer, 'state': 'preparing',
                       'activation_secret': secrets.token_hex(32), 'steps': [], 'created': int(time.time())})
            return self.status()

    def source_ready(self, manifest):
        manifest_files(manifest)
        with self.locked():
            value = self.read()
            require(value['role'] == 'source' and value['state'] in ('preparing', 'prepared'),
                    'Source is not preparing a migration')
            require(manifest['id'] == self.id, 'Wrong migration inventory')
            # Only the hash travels with the copy. The source retains the key
            # until it has durably relinquished execution authority.
            manifest = dict(manifest, activation_hash=digest(value['activation_secret'].encode()))
            if value['state'] == 'prepared':
                require(value['manifest_digest'] == digest(manifest), 'Prepared inventory changed')
            atomic(self.root / 'manifest.json', manifest)
            value['manifest_digest'] = digest(manifest)
            self.save(value, 'prepared')
            return manifest

    def begin_destination(self, manifest, source):
        files, total = manifest_files(manifest)
        require(manifest['id'] == self.id and len(manifest.get('activation_hash', '')) == 64,
                'Invalid activation identity')
        with self.locked():
            if self.path.exists():
                value = self.read()
                require(value['role'] == 'destination' and value['peer'] == source
                        and value['manifest_digest'] == digest(manifest) and value['state'] != 'aborted',
                        'Destination belongs to another or cancelled migration')
                return self.receipt(value)
            require(shutil.disk_usage(self.profile).free >= 2 * total + max(total // 20, 64 * 1024**2),
                    'The destination needs more free storage before migration')
            self.reserve()
            atomic(self.root / 'manifest.json', manifest)
            value = {'id': self.id, 'role': 'destination', 'peer': source, 'state': 'receiving',
                     'manifest_digest': digest(manifest), 'activation_hash': manifest['activation_hash'],
                     'abort_secret': secrets.token_hex(32), 'files': {}, 'steps': [], 'created': int(time.time())}
            self.save(value)
            return self.receipt(value)

    def receipt(self, value=None):
        value = value or self.read()
        return {'id': self.id, 'digest': value['manifest_digest'], 'state': value['state'],
                'abort_hash': digest(value['abort_secret'].encode())}

    def bind_destination(self, receipt, peer):
        with self.locked():
            value = self.read()
            require(value['role'] == 'source' and value['state'] in ('prepared', 'committed') and value['peer'] == peer,
                    'Source is not ready for this destination')
            require(receipt['id'] == self.id and receipt['digest'] == value['manifest_digest']
                    and len(receipt.get('abort_hash', '')) == 64, 'Destination receipt does not match')
            previous = value.get('abort_hash')
            require(previous is None or previous == receipt['abort_hash'], 'Destination identity changed')
            value['abort_hash'] = receipt['abort_hash']
            self.save(value)

    def item(self, index):
        manifest = json.loads((self.root / 'manifest.json').read_text())
        files, _ = manifest_files(manifest)
        require(type(index) is int and 0 <= index < len(files), 'Unknown migration file')
        return files[index]

    def chunk(self, index, offset, data, checksum):
        require(type(offset) is int and offset >= 0 and isinstance(data, bytes)
                and 0 < len(data) <= CHUNK and digest(data) == checksum, 'Invalid migration chunk')
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] == 'receiving', 'Copy is not accepting data')
            item = self.item(index)
            require(offset + len(data) <= item['size'], 'Chunk exceeds declared file size')
            # Store by inventory index. Remote paths never become write targets.
            path = self.root / 'files' / str(index)
            path.parent.mkdir(exist_ok=True, mode=0o700)
            received = value['files'].get(str(index), 0)
            if offset < received:
                require(offset + len(data) <= received and path.is_file(), 'Chunk overlaps the saved cursor')
                with path.open('rb') as stream:
                    stream.seek(offset)
                    require(stream.read(len(data)) == data, 'Retried chunk changed')
                return received
            require(offset == received, 'Resume from the saved copy position')
            require(not path.is_symlink(), 'Invalid staged file')
            with path.open('r+b' if path.exists() else 'x+b') as stream:
                os.chmod(path, 0o600)
                # A crash may have persisted bytes before its journal update.
                stream.truncate(received)
                stream.seek(received)
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            for directory in (path.parent, self.root):
                fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            value['files'][str(index)] = received + len(data)
            self.save(value)
            return value['files'][str(index)]

    def verify_files(self):
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] in ('receiving', 'copied', 'verified'),
                    'Destination is not ready for verification')
            files, _ = manifest_files(json.loads((self.root / 'manifest.json').read_text()))
            for index, item in enumerate(files):
                path = self.root / 'files' / str(index)
                if item['size'] == 0 and not path.exists():
                    path.parent.mkdir(exist_ok=True, mode=0o700)
                    path.touch(mode=0o600)
                require(value['files'].get(str(index), 0) == item['size'] and path.is_file()
                        and not path.is_symlink() and path.stat().st_size == item['size']
                        and file_digest(path) == item['sha256'], 'Staged file did not pass verification')
            if value['state'] != 'verified':
                self.save(value, 'copied')
            return self.receipt(value)

    def ready(self, checks):
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] in ('copied', 'verified'),
                    'Verify copied files before checking workspace readiness')
            require(all(checks.get(key) is True for key in ('database', 'computer', 'credentials')),
                    'Database, computer and connection checks must pass before cutover')
            value['checks'] = checks
            self.save(value, 'verified')
            return self.receipt(value)

    def commit_source(self, receipt):
        with self.locked():
            value = self.read()
            require(value['role'] == 'source' and value['state'] in ('prepared', 'committed'), 'Source cannot commit')
            require(receipt['id'] == self.id and receipt['digest'] == value['manifest_digest']
                    and receipt['state'] == 'verified' and value.get('abort_hash') == receipt.get('abort_hash')
                    and bool(value.get('abort_hash')), 'Verified destination receipt is required')
            self.save(value, 'committed')
            return {'id': self.id, 'digest': value['manifest_digest'], 'activation': value['activation_secret']}

    def begin_activation(self, proof):
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] in ('verified', 'activating', 'active'),
                    'Destination is not available for activation')
            require(proof['id'] == self.id and proof['digest'] == value['manifest_digest']
                    and secrets.compare_digest(digest(proof.get('activation', '').encode()), value['activation_hash']),
                    'The source has not confirmed cutover')
            if value['state'] != 'active':
                self.save(value, 'activating')
            return self.status()

    def activated(self):
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] in ('activating', 'active'), 'Cutover was not started')
            self.save(value, 'active')
            return self.status()

    def abort_destination(self):
        with self.locked():
            value = self.read()
            require(value['role'] == 'destination' and value['state'] in ('receiving', 'copied', 'verified', 'aborted'),
                    'Destination may already be active; reconcile cutover before moving back')
            self.save(value, 'aborted')
            return {'id': self.id, 'digest': value['manifest_digest'], 'abort': value['abort_secret']}

    def cancel_source(self, proof=None):
        with self.locked():
            value = self.read()
            require(value['role'] == 'source' and value['state'] in ('preparing', 'prepared', 'committed', 'cancelled'),
                    'Source cannot cancel this migration')
            if value.get('abort_hash'):
                require(isinstance(proof, dict) and proof.get('id') == self.id
                        and proof.get('digest') == value['manifest_digest']
                        and secrets.compare_digest(digest(proof.get('abort', '').encode()), value['abort_hash']),
                        'Confirm the destination is inactive before resuming the source')
            self.save(value, 'cancelled')
            return self.status()

    def status(self):
        value = self.read()
        # Deliberate allowlist: no provider credentials or cutover preimages.
        return {key: value[key] for key in ('id', 'role', 'peer', 'state', 'manifest_digest', 'files',
                                          'steps', 'created', 'updated') if key in value}
