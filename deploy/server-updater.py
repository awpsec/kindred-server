#!/usr/bin/env python3
"""Host-owned updater. The app can request only the latest public stable release.
Run outside the server process/container, with a root-owned JSON configuration.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socketserver
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
import zipfile

RELEASES = 'https://api.github.com/repos/awpsec/kindred-server/releases/latest'

def version(value):
    if not isinstance(value, str) or not re.fullmatch(r'\d{1,5}\.\d{1,5}\.\d{1,5}', value):
        raise ValueError('Invalid release version')
    return tuple(map(int, value.split('.')))

def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=600, **kwargs).stdout.strip()

def release():
    req = urllib.request.Request(RELEASES, headers={'User-Agent': 'Kindred-Server-Updater'})
    with urllib.request.urlopen(req, timeout=15) as response:
        data = json.loads(response.read(1048576))
    target = data['tag_name'].removeprefix('v')
    version(target)
    if data.get('prerelease') or data.get('draft'):
        raise ValueError('Expected a published stable release')
    name = f'Kindred-{target}-Server-Bundle.zip'
    asset = next(a for a in data['assets'] if a['name'] == name)
    url = f'https://github.com/awpsec/kindred-server/releases/download/v{target}/{name}'
    digest = asset.get('digest', '')
    if asset['browser_download_url'] != url or not re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
        raise ValueError('Release has no verified download digest')
    return {'version': target, 'url': url, 'digest': digest[7:]}

class Updater:
    def __init__(self, config):
        self.config = config
        self.lock = threading.RLock()
        self.phase = 'idle'
        self.progress = 0
        self.error = ''
        self.target = None
        self.latest = None
        self.checked = 0
        self.operation = None

    def status(self):
        with self.lock:
            active = self.phase in ('downloading', 'installing', 'restarting')
            if not active and time.monotonic() - self.checked > 60:
                self.latest = release()
                self.checked = time.monotonic()
            return {'supported': True, 'phase': self.phase, 'progress': self.progress,
                    'error': self.error, 'version': (self.target if active else self.latest or {}).get('version'),
                    'operation': self.operation}

    def start(self, target):
        version(target)
        with self.lock:
            if self.phase in ('downloading', 'installing', 'restarting'):
                return self.status()
            latest = release()
            if latest['version'] != target:
                raise ValueError('The available release changed. Check again before updating.')
            current = self.current_version()
            if version(target) <= version(current):
                raise ValueError('This server already has this release or a newer version.')
            self.latest = self.target = latest
            self.operation = uuid.uuid4().hex
            self.phase, self.progress, self.error = 'downloading', 0, ''
            threading.Thread(target=self.update, daemon=True).start()
            return self.status()

    def current_version(self):
        if self.config['mode'] == 'compose':
            output = run(self.compose() + ['exec', '-T', 'server', '/usr/local/bin/kindred', '--version'])
        else:
            output = run([self.config['binary'], '--version'])
        value = output.split()[-1]
        version(value)
        return value

    def compose(self):
        directory = self.config['compose_directory']
        command = ['docker', 'compose', '--project-directory', directory]
        for filename in self.config.get('compose_files', ['compose.yaml', 'compose.updater.yaml']):
            command += ['-f', str(Path(directory)/filename)]
        return command

    def ready(self):
        until = time.monotonic() + 180
        while time.monotonic() < until:
            try:
                with urllib.request.urlopen(self.config.get('health_url', 'http://127.0.0.1:9444/health'), timeout=3) as r:
                    if r.status == 200:
                        return
            except (OSError, ValueError):
                pass
            time.sleep(2)
        raise RuntimeError('The server did not become healthy after restarting.')

    def update(self):
        try:
            if self.config['mode'] == 'compose':
                # Use the owner's compose project and configuration; never remove volumes.
                # Refuse pinned/build overrides: changing those is a host administration action.
                compose = self.compose()
                resolved = json.loads(run(compose + ['config', '--format', 'json']))
                image = resolved['services']['server'].get('image', '')
                if image != 'ghcr.io/awpsec/kindred-server:latest' or resolved['services']['server'].get('build'):
                    raise ValueError('This deployment pins or builds its image. Update its compose configuration first.')
                target_image = 'ghcr.io/awpsec/kindred-server:' + self.target['version']
                run(['docker', 'pull', target_image])
                # Snapshot SQLite databases before replacing the container. VM disks stay in place.
                backup_script = """
import pathlib, sqlite3, sys
root = pathlib.Path('/data')
out = root / 'server-update-backups' / sys.argv[1]
out.mkdir(parents=True, exist_ok=True)
for database in list(root.rglob('*.db')):
    if 'server-update-backups' in database.parts:
        continue
    target = out / database.relative_to(root)
    target.parent.mkdir(parents=True, exist_ok=True)
    source = sqlite3.connect(str(database))
    destination = sqlite3.connect(str(target))
    try:
        source.backup(destination)
    finally:
        source.close()
        destination.close()
"""
                run(compose + ['exec', '-T', 'server', 'python3', '-c', backup_script, self.operation])
                self.phase, self.progress = 'installing', 65
                # A private override chooses the approved version without editing the owner's files.
                with tempfile.TemporaryDirectory(prefix='kindred-update-') as directory:
                    override = Path(directory)/'release.json'
                    override.write_text(json.dumps({'services': {'server': {'image': target_image}}}))
                    self.phase, self.progress = 'restarting', 85
                    run(compose + ['-f', str(override), 'up', '-d', '--no-deps', 'server'])
                    self.ready()
                    run(['docker', 'image', 'tag', target_image, image])
            else:
                self.native_update()
            self.phase, self.progress = 'complete', 100
        except Exception as error:
            # Log detail on the host; avoid returning command output or host paths to browsers.
            print(f'Server update failed: {error}', flush=True)
            self.error = str(error) if isinstance(error, ValueError) else 'The update failed. Check the host updater log before retrying.'
            self.phase = 'failed'

    def native_update(self):
        binary = Path(self.config['binary'])
        service = self.config.get('service', 'kindred.service')
        backups = Path(self.config['backup_directory'])/self.operation
        backups.mkdir(parents=True, mode=0o700)
        with tempfile.TemporaryDirectory(prefix='kindred-update-', dir=binary.parent) as directory:
            stage = Path(directory)
            archive = stage/'bundle.zip'
            digest = hashlib.sha256()
            req = urllib.request.Request(self.target['url'], headers={'User-Agent': 'Kindred-Server-Updater'})
            with urllib.request.urlopen(req, timeout=60) as response, archive.open('wb') as output:
                length = int(response.headers.get('Content-Length', 0)); total = 0
                while chunk := response.read(1024*1024):
                    total += len(chunk)
                    if total > 1024**3:
                        raise ValueError('Release exceeds the download limit')
                    output.write(chunk); digest.update(chunk)
                    self.progress = min(60, int(total/max(length, total)*60))
            if digest.hexdigest() != self.target['digest']:
                raise ValueError('Release checksum verification failed; nothing was installed.')
            candidate = stage/'kindred'
            with zipfile.ZipFile(archive) as bundle:
                info = bundle.getinfo('kindred')
                if info.file_size > 256*1024*1024:
                    raise ValueError('Server executable exceeds the size limit')
                with bundle.open(info) as source, candidate.open('wb') as destination:
                    shutil.copyfileobj(source, destination)
            candidate.chmod(0o755)
            if run([str(candidate), '--version']).split()[-1] != self.target['version']:
                raise ValueError('Downloaded server version did not match the release')
            self.phase, self.progress = 'installing', 65
            shutil.copy2(binary, backups/'kindred')
            run(['systemctl', 'stop', service])
            saved = []
            saved_ownership = {}
            try:
                # Back up the registry and every profile database with the server stopped.
                data = Path(self.config['data_directory'])
                for database in data.rglob('*.db'):
                    if database.is_symlink() or not database.resolve().is_relative_to(data.resolve()):
                        raise ValueError('Database path is not a regular file within the data directory')
                    relative = database.relative_to(data)
                    destination = backups/'databases'/relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    for suffix in ('', '-wal', '-shm'):
                        source = Path(str(database)+suffix)
                        if source.exists():
                            shutil.copy2(source, str(destination)+suffix)
                            stat = source.stat()
                            saved_ownership[str(source)] = (stat.st_uid, stat.st_gid)
                    saved.append((database, destination))
                os.replace(candidate, binary)
                self.phase, self.progress = 'restarting', 85
                run(['systemctl', 'start', service])
                self.ready()
            except Exception:
                run(['systemctl', 'stop', service])
                shutil.copy2(backups/'kindred', binary)
                for database, source in saved:
                    for suffix in ('', '-wal', '-shm'):
                        destination = Path(str(database)+suffix)
                        destination.unlink(missing_ok=True)
                        original = Path(str(source)+suffix)
                        if original.exists():
                            shutil.copy2(original, destination)
                            original_stat = saved_ownership[str(database)+suffix]
                            os.chown(destination, *original_stat)
                run(['systemctl', 'start', service])
                raise

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(30)
        try:
            raw = self.rfile.readline(4097)
            if len(raw) > 4096:
                raise ValueError('Request too large')
            request = json.loads(raw)
            updater = self.server.updater
            if request.get('action') == 'status':
                result = updater.status()
            elif request.get('action') == 'start':
                result = updater.start(request.get('version'))
            else:
                raise ValueError('Unknown update action')
        except Exception as error:
            print(f'Updater request failed: {error}', flush=True)
            result = {'supported': True, 'error': str(error) if isinstance(error, ValueError) else 'Cannot check for server updates right now. Try again shortly.'}
        self.wfile.write(json.dumps(result).encode()+b'\n')

if __name__ == '__main__':
    import grp
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', default='/etc/kindred/updater.json')
    args = parser.parse_args()
    config_path = Path(args.config)
    if config_path.stat().st_uid != 0 or config_path.stat().st_mode & 0o022:
        raise SystemExit('Updater configuration must be root-owned and not group/world writable')
    config = json.loads(config_path.read_text())
    if config['mode'] not in ('native', 'compose'):
        raise SystemExit('Unsupported deployment mode')
    socket = Path(config.get('socket', '/run/kindred-updater/control.sock'))
    socket.parent.mkdir(parents=True, exist_ok=True)
    socket.unlink(missing_ok=True)
    server = socketserver.ThreadingUnixStreamServer(str(socket), Handler)
    os.chown(socket, 0, grp.getgrnam(config.get('group', 'kindred')).gr_gid)
    socket.chmod(0o660)
    server.updater = Updater(config)
    server.serve_forever()
