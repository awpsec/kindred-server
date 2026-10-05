"""Disposable local X11 fixture: requires compiled Rust tests, Xvfb, xdotool, import, Playwright.
Run through operations/run-heavy. Never uses a VM, existing desktop or real account.
"""
import os, pathlib, socket, subprocess, tempfile, time, shutil, shlex, json, hashlib
root=pathlib.Path(__file__).resolve().parents[1]
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0)); address='127.0.0.1:'+str(sock.getsockname()[1])
screen=next(n for n in range(32,1,-1) if not pathlib.Path(f'/tmp/.X11-unix/X{n}').exists() and not pathlib.Path(f'/tmp/.X{n}-lock').exists())
env=dict(os.environ, DISPLAY=f':{screen}', KINDRED_EXECUTOR_FIXTURE_SCREEN=str(screen), KINDRED_EXECUTOR_FIXTURE_ADDRESS=address)
binaries=[p for p in (root/'target/debug/deps').glob('kindred-*') if p.is_file() and os.access(p,os.X_OK) and not p.suffix]
binary=pathlib.Path(env['KINDRED_EXECUTOR_TEST_BINARY']) if env.get('KINDRED_EXECUTOR_TEST_BINARY') else max(binaries,key=lambda p:p.stat().st_mtime)
if env.get('KINDRED_FIXTURE_MODE') != 'baseline':
    listing=subprocess.check_output([str(binary),'--list'],text=True)
    assert 'transport_uncertainty_survives_tool_error_serialization' in listing, 'Stale/baseline executable; rebuild candidate with a separate target or clean this package'
artifacts=pathlib.Path(env['KINDRED_TEST_ARTIFACTS']);artifacts.mkdir(parents=True,exist_ok=True)
(artifacts/'execution.json').write_text(json.dumps({'cwd':str(root),'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'guest_source_sha256':hashlib.sha256((root/'src/guest.rs').read_bytes()).hexdigest(),'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'mode':env.get('KINDRED_FIXTURE_MODE'),'command':['node',str(root/'tools/test-computer-executor.cjs')],'display':env['DISPLAY'],'geometry':'1440x900x24','chrome':env.get('KINDRED_TEST_CHROME'),'playwright_module':env.get('KINDRED_PLAYWRIGHT_MODULE')},indent=2))
# Fault only fixture-local command wrappers, retaining the real tools beneath.
command_dir=artifacts/'commands';command_dir.mkdir(exist_ok=True)
for name in ['import','xdotool']:
    real=shutil.which(name);assert real, f'Missing {name}'
    fault=artifacts/('fail-capture' if name=='import' else 'fail-input')
    env['KINDRED_EXECUTOR_'+('CAPTURE' if name=='import' else 'INPUT')+'_FAULT']=str(fault)
    if name=='import': script=f'#!/bin/sh\n[ ! -f {shlex.quote(str(fault))} ] || exit 42\nexec {shlex.quote(real)} "$@"\n'
    else: script=f'#!/bin/sh\n{shlex.quote(real)} "$@"\nstatus=$?\nif [ "$1" != getdisplaygeometry ] && [ -f {shlex.quote(str(fault))} ]; then exit 42; fi\nexit "$status"\n'
    (command_dir/name).write_text(script);(command_dir/name).chmod(0o755)
env['PATH']=str(command_dir)+':'+env['PATH']
with (artifacts/'xvfb.log').open('w') as xlog, (artifacts/'executor.log').open('w') as elog:
    xvfb=subprocess.Popen(['Xvfb',f':{screen}','-screen','0','1440x900x24','-nolisten','tcp','-noreset'],stdout=xlog,stderr=xlog)
    executor=None
    try:
        for _ in range(100):
            if pathlib.Path(f'/tmp/.X11-unix/X{screen}').exists():break
            time.sleep(.05)
        executor=subprocess.Popen([str(binary),'--exact','guest::browser_tests::local_executor_fixture','--ignored','--nocapture'],env=env,stdout=elog,stderr=elog)
        for _ in range(100):
            if executor.poll() is not None:raise RuntimeError('Executor exited; inspect executor.log')
            # Listener readiness from /proc TCP, without opening an incomplete JSON request.
            port_hex=f'{int(address.split(":")[1]):04X}'
            if any(line.split()[1].endswith(':'+port_hex) for line in pathlib.Path('/proc/net/tcp').read_text().splitlines()[1:]):break
            time.sleep(.05)
        subprocess.run(['node',str(root/'tools/test-computer-executor.cjs')],env=env,check=True,timeout=120)
        executor.wait(timeout=5)
        if executor.returncode:raise RuntimeError('Executor test failed')
    finally:
        if executor and executor.poll() is None:executor.terminate();executor.wait(timeout=5)
        xvfb.terminate();xvfb.wait(timeout=5)
