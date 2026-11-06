#!/usr/bin/env python3
"""Opt-in rollback check using ONLY the successful synthetic VM rehearsal."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid
root=Path('/data/profiles')
assert os.environ.get('KINDRED_MIGRATION_VM_TEST')=='synthetic-only'
assert (root/'.synthetic-migration-rehearsal').is_file()
previous=json.loads((root/'receipt.json').read_text())
assert previous.get('synthetic_only') is True and previous.get('passed') is True
profile=str(uuid.UUID(previous['destination']));folder=root/profile
assert not (root/'cancel-receipt.json').exists(),'Use the retained result instead of repeating the check'
spec=importlib.util.spec_from_file_location('migration_worker',Path('/migration-code/deploy/migration-worker.py'))
worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
manager=worker.vm_manager();computer=folder/'computer';transfer=str(uuid.uuid4())
record={'synthetic_only':True,'source':profile,'transfer':transfer,'passed':False}
def call(action,**kw):
    return worker.dispatch(folder,profile,{'action':action,'id':transfer,**kw},'synthetic-rehearsal',root/'.account-synthetic.credentials')
def ready():
    deadline=time.monotonic()+240
    while manager.connection_status(computer)!='ready':
        assert time.monotonic()<deadline,'Computer did not become ready'
        time.sleep(2)
def read():
    return subprocess.check_output(['ssh','-F',str(computer/'ssh_config'),'-oBatchMode=yes','kindred-guest','cat /workspace/migration-test/after.txt'],stderr=subprocess.PIPE,timeout=30).decode().strip()
try:
    manager.start(computer,profile);ready();assert read()=='new destination work'
    print('Original synthetic workspace running with its new work',flush=True)
    call('prepare',peer={'server':'https://unused-destination.invalid','profile':str(uuid.uuid4())})
    assert not manager.running(computer) and manager.migration_paused(computer)
    print('Source paused with a complete backup',flush=True)
    assert call('cancel')['state']=='cancelled'
    ready();assert read()=='new destination work'
    assert not manager.migration_paused(computer)
    assert manager.migration_paused(root/previous['source']/'computer')
    record['passed']=True
    print('Cancel restored the source computer and its files; older recovery copy remains fenced',flush=True)
except Exception as error:
    record['error']=type(error).__name__+': '+str(error);raise
finally:
    if manager.running(computer):
        subprocess.run([sys.executable,'/migration-code/deploy/vm-manager.py','shutdown',profile],check=True,timeout=100)
    worker.journal.atomic(root/'cancel-receipt.json',record)
