"""Opt-in regression check against a cached server image; no builds or downloads."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import uuid

@unittest.skipUnless(os.environ.get('KINDRED_TEST_CONTAINER_IMAGE'), 'Set a cached container image to run')
class PrivateDirectoryRestart(unittest.TestCase):
    def test_private_profile_directory_survives_container_restart(self):
        image=os.environ['KINDRED_TEST_CONTAINER_IMAGE']
        name='kindred-entry-test-'+uuid.uuid4().hex[:12]
        def docker(*args,**kwargs):
            return subprocess.run(['docker',*args],check=True,capture_output=True,text=True,timeout=30,**kwargs)
        with tempfile.TemporaryDirectory(prefix='kindred-entry-test-') as folder:
            root=Path(folder);data=root/'data';profiles=data/'profiles';profiles.mkdir(parents=True)
            for path in [data,profiles]:os.chown(path,1000,1000);path.chmod(0o700)
            try:
                docker('run','--pull=never','-d','--name',name,'--network','none','--security-opt','no-new-privileges:true','--cap-drop','ALL','--cap-add','CHOWN','--cap-add','SETUID','--cap-add','SETGID','-v',f'{data}:/data','-v',f'{Path(__file__).with_name("container-entry.py").resolve()}:/opt/kindred/source/deploy/container-entry.py:ro',image)
                for attempt in range(2):
                    for _ in range(30):
                        try:
                            docker('exec',name,'python3','-c',"import urllib.request; assert b'ok' in urllib.request.urlopen('http://127.0.0.1:9444/health').read()")
                            break
                        except subprocess.CalledProcessError:time.sleep(.2)
                    else:self.fail('Server did not become healthy with private profile directories')
                    if attempt==0:docker('restart','-t','5',name)
            finally:
                subprocess.run(['docker','rm','-f',name],capture_output=True,timeout=30)
