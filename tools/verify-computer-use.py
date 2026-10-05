"""Focused offline checks. Run through operations/run-heavy with native fixture PNG path.
Cargo, Pi and CLI fixtures use only disposable databases/local transports.
"""
import json, os, pathlib, subprocess, re
root=pathlib.Path(__file__).resolve().parents[1]
proof=pathlib.Path(os.environ['KINDRED_TEST_ARTIFACTS']);proof.mkdir(parents=True,exist_ok=True)
assert pathlib.Path(os.environ['KINDRED_EXECUTOR_IMAGE_PATH']).is_file()
commands=[['/root/.cargo/bin/cargo','test','--locked','--offline',name,'--','--nocapture'] for name in [
 'guest::browser_tests','vm::computer_transport_tests','runtime::computer_receipt_tests',
 'provider_retry::tests','desktop_sessions::tests','user_tasks::tests','providers::tests',
 'instructions::tests','pi_cancellation_during_human_step']]
commands += [['node','--test','harness/pi/test/session.test.mjs'],['node','tools/test-computer-image-transport.mjs'],['python3','-m','unittest','deploy/test_provider_cli.py']]
results=[]
for index,command in enumerate(commands):
    log=proof/f'{index:02d}.log'
    with log.open('w') as output: result=subprocess.run(command,cwd=root,stdout=output,stderr=subprocess.STDOUT)
    if command[0].endswith('cargo') and not re.search(r'test result: ok\. [1-9][0-9]* passed',log.read_text()):
        raise RuntimeError(f'No passing tests executed: {log}')
    results.append({'command':command,'exit_code':result.returncode,'log':str(log)})
    print(json.dumps(results[-1]),flush=True)
    (proof/'checks.json').write_text(json.dumps(results,indent=2))
    if result.returncode:raise SystemExit(result.returncode)
