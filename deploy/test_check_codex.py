"""Readiness must reject damaged packages, broken IPC and incomplete callbacks."""
from pathlib import Path
import runpy
import tempfile
import time
import unittest
import test_download_codex as fixture

checker = runpy.run_path(str(Path(__file__).with_name('check-codex.py')))


class RuntimeCheck(unittest.TestCase):
    def test_every_component_must_be_present_executable_and_correct_architecture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); archive = root / 'package.tar.gz'
            fixture.CodexPackage().archive(archive)
            fixture.package.extract(archive, root / 'runtime')
            for name in checker['FILES']:
                path = root / 'runtime' / name; saved = path.read_bytes()
                path.unlink()
                with self.assertRaises((ValueError, FileNotFoundError)): checker['check'](root / 'runtime/bin/codex', execute=False)
                path.write_bytes(saved); path.chmod(0o755)
                if name != 'codex-package.json':
                    path.chmod(0o644)
                    with self.assertRaisesRegex(ValueError, 'not executable'): checker['validate'](root / 'runtime')
                    path.chmod(0o755)
                    path.write_bytes(saved[:18] + b'\xb7\x00' + saved[20:])  # ARM64, not this guest.
                    with self.assertRaisesRegex(ValueError, 'x86_64'): checker['validate'](root / 'runtime')
                    path.write_bytes(saved)

    def test_round_trip_rejects_missing_callback_failed_result_and_bad_frames(self):
        # A protocol peer, not a fake model. The official binary is separately
        # exercised by download/install on every package build.
        script = r'''#!/usr/bin/env python3
import json,os,struct,sys,time
mode = os.path.basename(sys.argv[0])
def send(v):
    data=json.dumps(v).encode();sys.stdout.buffer.write(struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
def read():
    size,=struct.unpack('<I',sys.stdin.buffer.read(4));return json.loads(sys.stdin.buffer.read(size))
read()
if mode=='hang': time.sleep(10)
if mode=='oversized': sys.stdout.buffer.write(struct.pack('<I',65537));sys.stdout.buffer.flush();time.sleep(10)
if mode=='eof': sys.exit(0)
send({'type':'connection/ready','selectedVersion':1});read()
send({'type':'operation/response','id':1,'result':{'status':'ok','value':{'type':'session/ready'}}});read()
if mode!='no-callback':
    send({'type':'delegate/request','id':1,'request':{'type':'tool/invoke','invocation':{'tool_name':{'name':'kindred_check','namespace':None},'input':{'value':41 if mode!='wrong-call' else 99}}}})
    read()
send({'type':'execute/initialResponse','result':{'status':'ok','value':{'Result':{'error_text':'failed' if mode=='failed' else None,'content_items':[{'type':'input_text','text':'42'}]}}}})
time.sleep(10)
'''
        with tempfile.TemporaryDirectory() as temporary:
            for mode in ('success', 'no-callback', 'failed', 'wrong-call', 'oversized', 'eof', 'hang'):
                host = Path(temporary) / mode; host.write_text(script); host.chmod(0o755)
                if mode == 'success':
                    checker['probe'](host, timeout=1)
                else:
                    start = time.monotonic()
                    with self.assertRaises(ValueError): checker['probe'](host, timeout=.2)
                    self.assertLess(time.monotonic() - start, 2)


if __name__ == '__main__': unittest.main()
