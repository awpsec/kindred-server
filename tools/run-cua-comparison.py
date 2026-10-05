"""Three matched scripted executor samples per mode, never model metrics."""
import json, os
from pathlib import Path
import subprocess, sys
root=Path(__file__).resolve().parents[1]
proof=Path(os.environ['KINDRED_TEST_ARTIFACTS']);proof.mkdir(parents=True,exist_ok=True)
results=[]
for index in range(1,4):
    for mode in ('native','target'):
        directory=proof/f'{mode}-{index}'
        env={**os.environ,'KINDRED_CUA_COMPARISON_MODE':mode,'KINDRED_TEST_ARTIFACTS':str(directory)}
        if mode=='native':
            env['KINDRED_EXECUTOR_TEST_BINARY']=os.environ['KINDRED_BASELINE_EXECUTOR_BINARY']
            env['KINDRED_EXECUTOR_SOURCE_ROOT']=os.environ['KINDRED_BASELINE_SOURCE_ROOT']
        with (proof/f'{mode}-{index}.log').open('w') as log:
            subprocess.run(['dbus-run-session','--',sys.executable,str(root/'tools/run-cua-executor-fixture.py')],env=env,stdout=log,stderr=log,check=True)
        results.append(json.loads((directory/'results.json').read_text()))
(proof/'summary.json').write_text(json.dumps({'samples':results,'provider_requests':None,'model_tokens':None,'limits':'Matched scripted fixture values and native geometry oracle; no model performance or production task evidence'},indent=2))
print(json.dumps({'passed':True,'samples':len(results)}))
