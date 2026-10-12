"""Apply the real runtime installer only to the supplied disposable fixture."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
from unittest.mock import patch

repo = Path(__file__).resolve().parents[1]
root = Path(sys.argv[1])
spec = importlib.util.spec_from_file_location('update', repo / 'deploy/update-guest-runtime.py')
update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update)
units = root / 'systemd'
for kind in ('system', 'user'):
    (units / kind).mkdir(parents=True)
data = b'#!/usr/bin/python3\nimport sys,json\nprint("kindred fixture" if sys.argv[1]=="--version" else json.dumps({"commands":[]}))\n'
target = root / 'kindred-bin'
target.write_bytes(data)
target.chmod(0o755)
payload = {}
names = ('browser-launch.py', 'stop-browser.py', 'desktop-launch', 'start-desktop.sh',
         'ensure-screen', 'screen-ownership.py', 'kindred-guest-desktop.service', 'kindred-screen@.service', 'browser-session.json')
for name in names:
    source = (repo / 'deploy' / name).read_text()
    old = None if name in ('stop-browser.py','browser-session.json') else subprocess.check_output(['git', 'show', 'HEAD:deploy/' + name], cwd=repo)
    if old is not None:
        destination = update.launcher_target(root, name, units)
        destination.write_bytes(old)
        destination.chmod(0o644 if name.endswith(('.service','.json')) else 0o755)
    payload[name] = {'source': source, 'previous_sha256': hashlib.sha256(old).hexdigest() if old is not None else None}
with patch.object(update, 'reload_units'):
    receipt = update.install(io.BytesIO(data), target, hashlib.sha256(data).hexdigest(), 'fixture', payload, units)
print(json.dumps(receipt))
