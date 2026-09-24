#!/usr/bin/env python3
"""Stage and verify provider updates before switching managed entrypoints."""
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import uuid


def metadata(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def switch(target, destination):
    temporary = target.with_name('.' + target.name + '-' + uuid.uuid4().hex)
    try:
        temporary.symlink_to(destination)
        os.replace(temporary, target)
    finally:
        temporary.unlink(missing_ok=True)


def run(args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, stdin=subprocess.DEVNULL, timeout=480, **kwargs)


def update_tailnet_firewall(config=Path('/etc/nftables.conf')):
    """Migrate only Kindred's guest policy; never reload/flush Tailscale's rules."""
    if not config.exists():
        return
    source = config.read_text()
    if 'table inet kindred {' not in source:
        return  # Externally managed firewall.
    rule = 'oifname "tailscale0" accept comment "kindred-tailnet"'
    anchor = '  oifname "lo" accept\n'
    if rule not in source:
        if source.count(anchor) != 1:
            raise RuntimeError('Kindred firewall was customized; cannot safely add tailnet rule')
        updated = source.replace(anchor, anchor + '  ' + rule + '\n', 1)
        temporary = config.with_name(config.name + '.kindred-staged')
        try:
            temporary.write_text(updated)
            shutil.copymode(config, temporary)
            run(['nft', '--check', '--file', temporary])
            backup = config.with_name(config.name + '.before-kindred-tailnet')
            if not backup.exists():
                shutil.copy2(config, backup)
            os.replace(temporary, config)
        finally:
            temporary.unlink(missing_ok=True)
    # Insert just this rule into the running chain. No flush, restart or changes
    # to unrelated rules, routes, Tailscale login state or tailnet ACLs.
    current = subprocess.run(['nft', 'list', 'chain', 'inet', 'kindred', 'output'],
                             capture_output=True, text=True, check=True, timeout=20)
    if rule not in current.stdout:
        run(['nft', 'insert', 'rule', 'inet', 'kindred', 'output', rule])


def guest(root=Path('/opt/kindred/providers')):
    root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.update-', dir=root) as directory:
        stage = Path(directory)
        info = metadata('https://registry.npmjs.org/@anthropic-ai/claude-code-linux-x64/latest')
        dist = info['dist']
        if not dist['tarball'].startswith('https://registry.npmjs.org/'):
            raise ValueError('Unexpected Claude package source')
        with urllib.request.urlopen(dist['tarball'], timeout=120) as response:
            data = response.read()
        if 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode() != dist['integrity']:
            raise ValueError('Claude integrity check failed')
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            entries = [m for m in archive.getmembers() if m.isfile() and m.name.endswith('/claude')]
            if len(entries) != 1:
                raise ValueError('Claude executable missing')
            claude = stage / 'claude'
            claude.write_bytes(archive.extractfile(entries[0]).read())
            claude.chmod(0o755)
        run([claude, '--version'])
        # Build the venv at its permanent location: script shebangs are absolute.
        version = metadata('https://pypi.org/pypi/kimi-cli/json')['info']['version']
        if not re.fullmatch(r'[0-9]+(?:\.[0-9]+)+', version):
            raise ValueError('Expected a stable Kimi release')
        kimi = root / ('kimi-update-' + version)
        existing = kimi.exists()
        try:
            if not existing:
                run([sys.executable, '-m', 'venv', kimi])
                run([kimi / 'bin/python', '-m', 'pip', 'install', '--disable-pip-version-check', '--index-url', 'https://pypi.org/simple', 'kimi-cli==' + version])
            run([kimi / 'bin/kimi', '--version'])
        except Exception:
            if not existing:
                shutil.rmtree(kimi, ignore_errors=True)
            raise
        destination = root / ('claude-update-' + hashlib.sha256(claude.read_bytes()).hexdigest()[:20])
        if not destination.exists():
            os.replace(claude, destination)
        switch(root / 'claude-current', destination)
        switch(root / 'kimi-current', kimi)


def pi(current):
    if not current.exists():
        return  # Optional harness is not installed on this server.
    if not current.is_symlink():
        raise ValueError('Pi updates require the managed current symlink')
    root = current.parent / 'releases'
    root.mkdir(parents=True, exist_ok=True)
    stage = root / ('update-' + uuid.uuid4().hex)
    try:
        shutil.copytree(current.resolve(), stage, ignore=shutil.ignore_patterns('node_modules'))
        environment = dict(os.environ, PATH=str(stage / 'node/bin') + ':' + os.environ.get('PATH', ''))
        run([stage / 'node/bin/npm', 'install', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund',
             '--registry=https://registry.npmjs.org', '@earendil-works/pi-coding-agent@latest', '@earendil-works/pi-ai@latest'], cwd=stage, env=environment)
        sdk = json.loads((stage / 'node_modules/@earendil-works/pi-coding-agent/package.json').read_text())['version']
        if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', sdk):
            raise ValueError('Expected a stable Pi SDK')
        session = stage / 'session.mjs'
        source, count = re.subn(r"export const SDK_VERSION = '[^']+';", 'export const SDK_VERSION = ' + json.dumps(sdk) + ';', session.read_text())
        if count != 1:
            # Previously updated sessions use JSON double quotes.
            source, count = re.subn(r'export const SDK_VERSION = "[^"]+";', 'export const SDK_VERSION = ' + json.dumps(sdk) + ';', session.read_text())
        if count != 1:
            raise ValueError('Unrecognized Pi harness version declaration')
        session.write_text(source)
        run([stage / 'node/bin/node', stage / 'worker.mjs', '--check'], cwd=stage, env=environment)
        lock = stage / 'package-lock.json'
        old_lock = current / 'package-lock.json'
        if lock.exists() and old_lock.exists() and lock.read_bytes() == old_lock.read_bytes():
            shutil.rmtree(stage)
            return
        switch(current, stage)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise


if __name__ == '__main__':
    if sys.argv[1] == 'guest':
        update_tailnet_firewall()
        guest()
    elif sys.argv[1] == 'pi':
        pi(Path(sys.argv[2]))
    else:
        raise ValueError('Unknown harness update target')
