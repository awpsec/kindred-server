#!/usr/bin/env python3
"""Opt-in real QEMU rehearsal using ONLY new synthetic profiles and a fresh guest.
Run in the disposable Linux test container, never against an owner profiles dir.
Keeps disks and receipts for inspection; shuts down only its own guest on exit.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time
import uuid

ROOT=Path('/data/profiles')
assert os.environ.get('KINDRED_MIGRATION_VM_TEST')=='synthetic-only'
assert ROOT.is_dir() and (ROOT/'.synthetic-migration-rehearsal').is_file()
assert not list(ROOT.glob('*/kindred.db')), 'Only a fresh disposable profile directory is allowed'
DEPLOY=Path('/migration-code/deploy')
sys.path.insert(0,str(DEPLOY))
import test_migration_files as fixture
spec=importlib.util.spec_from_file_location('migration_worker',DEPLOY/'migration-worker.py')
worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
manager=worker.vm_manager()
source_id,target_id,transfer_id=[str(uuid.uuid4()) for _ in range(3)]
source,target=ROOT/source_id,ROOT/target_id
source.mkdir();target.mkdir()
keys=ROOT/'.account-synthetic.credentials'
keys.write_text('{"test-only":"no-live-credential"}')
record={'synthetic_only':True,'source':source_id,'destination':target_id,'transfer':transfer_id,'steps':[]}

def save(stage,**extra):
    record['steps'].append({'stage':stage,'at':int(time.time()),**extra})
    worker.journal.atomic(ROOT/'receipt.json',record)
    print(stage,flush=True)

def call(root,profile,action,**kw):
    return worker.dispatch(root,profile,{'action':action,'id':transfer_id,**kw},'synthetic-rehearsal',keys)

def ssh(computer,command):
    return subprocess.check_output(['ssh','-F',str(computer/'ssh_config'),'-oBatchMode=yes','-oConnectTimeout=5','kindred-guest',command],stderr=subprocess.PIPE,timeout=30).decode().strip()

f=fixture.MigrationFilesTests();f.setUp()
with worker.files.database(source/'kindred.db') as db:
    f.db.backup(db);db.execute("DELETE FROM settings WHERE key='_migration_gate'")
with worker.files.database(target/'kindred.db') as db:
    f.db.backup(db)
    for table in ('bots','runs','chats','settings'):db.execute(f'DELETE FROM {table}')
f.doCleanups()
try:
    save('Provisioning a new synthetic computer')
    subprocess.run([sys.executable,str(DEPLOY/'vm-manager.py'),'ensure',source_id],check=True,timeout=1900)
    computer=source/'computer'
    sentinel='kindred-synthetic-migration-'+transfer_id
    ssh(computer,"mkdir -p /workspace/migration-test && printf '%s' '"+sentinel+"' > /workspace/migration-test/receipt.txt && sync")
    save('Synthetic computer ready; sentinel saved')
    package=call(source,source_id,'prepare',peer={'server':'https://destination.invalid','profile':target_id})
    assert not manager.running(computer)
    original=worker.journal.file_digest(computer/'disk.qcow2')
    save('Source stopped; complete snapshot verified',files=len(package['manifest']['files']))
    reserved=call(target,target_id,'reserve',manifest=package['manifest'],peer={'server':'https://source.invalid','profile':source_id})
    call(source,source_id,'bind',peer={'server':'https://destination.invalid','profile':target_id},receipt=reserved['receipt'])
    total=0
    for index,item in enumerate(package['manifest']['files']):
        offset=0
        while offset<item['size']:
            chunk=call(source,source_id,'read',index=index,offset=offset)
            offset=call(target,target_id,'write',index=index,offset=offset,**chunk)['offset']
        total+=offset
    save('All files copied through resumable chunks',bytes=total)
    original_status=manager.connection_status
    def checked_status(root):
        phase=original_status(root)
        if root.parent.name==target_id and phase=='ready':
            assert ssh(root,'cat /workspace/migration-test/receipt.txt')==sentinel
            # During validation the guest must not reach external services.
            assert ssh(root,'if curl -fsS --max-time 4 https://example.com >/dev/null 2>&1; then echo reachable; else echo blocked; fi')=='blocked'
        return phase
    manager.connection_status=checked_status
    worker.vm_manager=lambda:manager
    verified=call(target,target_id,'verify')
    assert not manager.running(target/'computer') and not manager.running(computer)
    assert worker.journal.file_digest(computer/'disk.qcow2')==original
    save('Copied guest booted in isolation; sentinel and network isolation verified')
    proof=call(source,source_id,'commit',receipt=verified['receipt'])
    staged=call(target,target_id,'activate_prepare',proof=proof)
    with worker.files.database(staged['database']) as db:
        with worker.files.database(target/'kindred.db') as destination:db.backup(destination)
    call(target,target_id,'activated')
    try:manager.start(computer,source_id)
    except ValueError as error:assert 'migration' in str(error)
    else:raise AssertionError('Source was allowed to boot after cutover')
    manager.connection_status=original_status
    manager.start(target/'computer',target_id)
    deadline=time.monotonic()+240
    while original_status(target/'computer')!='ready':
        assert time.monotonic()<deadline,'Activated guest did not become ready'
        time.sleep(2)
    assert ssh(target/'computer','cat /workspace/migration-test/receipt.txt')==sentinel
    ssh(target/'computer',"printf '%s' 'new destination work' > /workspace/migration-test/after.txt && sync")
    assert call(target,target_id,'activate_prepare',proof=proof)['already_restored'] is True
    assert ssh(target/'computer','cat /workspace/migration-test/after.txt')=='new destination work'
    save('Activated destination works; original stays fenced; repeated activation preserves new files')
    record['passed']=True
except Exception as error:
    record['passed']=False;record['error']=type(error).__name__+': '+str(error)
    raise
finally:
    for profile,root in ((source_id,source),(target_id,target)):
        if (root/'computer/computer.json').exists() and manager.running(root/'computer'):
            subprocess.run([sys.executable,str(DEPLOY/'vm-manager.py'),'shutdown',profile],check=True,timeout=100)
    worker.journal.atomic(ROOT/'receipt.json',record)
