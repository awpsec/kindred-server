#!/usr/bin/env python3
"""systemd ExecStop helper: stop scheduling, then flush managed guest shutdowns."""
import importlib.util,os,signal,sys,time
from pathlib import Path
pid=int(sys.argv[1]);assert pid>1
try:os.kill(pid,signal.SIGINT)
except ProcessLookupError:pass
spec=importlib.util.spec_from_file_location('manager','/usr/local/lib/kindred/vm-manager.py')
manager=importlib.util.module_from_spec(spec);spec.loader.exec_module(manager)
computers=[p.parent for p in manager.ROOT.glob('*/computer/computer.json')]
for computer in computers:
 try:manager.qmp(computer,'system_powerdown')
 except (OSError,ValueError):pass
deadline=time.monotonic()+60
while time.monotonic()<deadline and any(manager.running(p) for p in computers):time.sleep(.5)
for computer in computers:
 if manager.running(computer):
  try:manager.qmp(computer,'quit')
  except (OSError,ValueError):pass
