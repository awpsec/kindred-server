"""A restored browser without windows must not kill an otherwise ready desktop."""
import os,subprocess,tempfile,unittest
from pathlib import Path
class ScreenReady(unittest.TestCase):
 def test_dock_ready_without_browser_window(self):
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory)
   profile=root/'browser-5';profile.mkdir();(profile/'Local State').write_text('{}')
   script=root/'wait';script.write_text(Path(__file__).with_name('wait-screen-ready').read_text().replace('/home/bot/.local/share/kindred/browser',str(root/'browser')))
   for name,body in [('xdpyinfo','exit 0'),('xdotool','case "$*" in *chromium*) exit 1;; *) exit 0;; esac')]:
    p=root/name;p.write_text('#!/bin/sh\n'+body+'\n');p.chmod(0o755)
   result=subprocess.run(['sh',str(script),'5'],env={**os.environ,'PATH':str(root)+':'+os.environ['PATH']},capture_output=True,timeout=2)
   self.assertEqual(result.returncode,0,result.stderr)
 def test_missing_display_does_not_pass(self):
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory)
   for name,body in [('xdpyinfo','exit 1'),('sleep','exit 0')]:
    p=root/name;p.write_text('#!/bin/sh\n'+body+'\n');p.chmod(0o755)
   result=subprocess.run(['sh',str(Path(__file__).with_name('wait-screen-ready')),'5'],env={**os.environ,'PATH':str(root)+':'+os.environ['PATH']},capture_output=True,timeout=3)
   self.assertNotEqual(result.returncode,0)
if __name__=='__main__': unittest.main()
