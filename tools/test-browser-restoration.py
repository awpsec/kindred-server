"""Real headed attachment using the production restoration launch arguments.

Run under run-heavy. Requires cached Chromium, Node and Playwright paths.
Only disposable X11/profile/local form; no API, owner browser or credentials.
"""
import argparse, importlib.util, json, os, queue, shlex, subprocess, tempfile, threading, time, unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('browser_fixture',ROOT/'tools/test-browser-worker.py')
f=importlib.util.module_from_spec(spec);spec.loader.exec_module(f)
# Read the shared production policy used by all three launch paths.
launch_spec=importlib.util.spec_from_file_location('browser_launch',ROOT/'deploy/browser-launch.py')
launch=importlib.util.module_from_spec(launch_spec);launch_spec.loader.exec_module(launch)
args=launch.browser_arguments('$KINDRED_BROWSER_PROFILE')
assert args.count('--user-data-dir=$KINDRED_BROWSER_PROFILE')==1
args.remove('--user-data-dir=$KINDRED_BROWSER_PROFILE')
assert f.BOOTSTRAP.count("args: ['--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', '--ozone-platform=x11']")==1
f.BOOTSTRAP=f.BOOTSTRAP.replace("args: ['--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', '--ozone-platform=x11']",'handleSIGTERM: false, args: '+json.dumps(args+['--ozone-platform=x11','--no-sandbox']))
# Fixture launcher explicitly focuses its synthetic tab; the production driver
# must still refuse background/foreign tabs and never perform this focus step.
f.BOOTSTRAP=f.BOOTSTRAP.replace('// Wait for the actual compositor surface', 'await context.pages()[0].evaluate(()=>document.title="Selected restoration fixture"); await context.pages()[0].bringToFront();\n  // Wait for the actual compositor surface')
class RestorationTests(f.BrowserTests):
 def test_existing_browser_does_not_gain_endpoint_or_change_process(self):
  with tempfile.TemporaryDirectory(prefix='kindred-existing-browser-') as profile:
   bootstrap=f.BOOTSTRAP.replace('"--remote-debugging-address=127.0.0.1", ', '').replace('"--remote-debugging-port=0", ', '')
   browser=subprocess.Popen([f.NODE,'-e',bootstrap,f.PLAYWRIGHT_MODULE,profile,f.CHROME,self.origin+'/settings'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
   def parents():
    matches=[]
    for entry in Path('/proc').iterdir():
     if not entry.name.isdecimal():continue
     try:
      argv=(entry/'cmdline').read_bytes().split(b'\0')
      if f'--user-data-dir={profile}'.encode() in argv and not any(x.startswith(b'--type=') for x in argv):matches.append(entry.name)
     except OSError:pass
    return matches
   try:
    q=queue.Queue();threading.Thread(target=lambda:q.put(browser.stdout.readline()),daemon=True).start()
    self.assertEqual(q.get(timeout=25).strip(),'ready')
    before=parents();self.assertEqual(len(before),1)
    self.assertFalse(Path(profile,'DevToolsActivePort').exists())
    completed=subprocess.run([f.CHROME,f'--user-data-dir={profile}',*args,'--no-sandbox'],capture_output=True,timeout=10)
    self.assertEqual(completed.returncode,0)
    self.assertEqual(parents(),before)
    self.assertFalse(Path(profile,'DevToolsActivePort').exists())
    with self.assertRaises(FileNotFoundError):f.module.Driver(profile,(ROOT/'deploy/browser-observer.js').read_text(),os.environ['DISPLAY'])
   finally:
    browser.terminate()
    try:browser.wait(timeout=8)
    except subprocess.TimeoutExpired:browser.kill();browser.wait()
    browser.stdin.close();browser.stdout.close()
 def test_z_cookie_survives_graceful_restart_and_attachment(self):
  self.driver.evaluate('document.cookie="fixture_session=synthetic; max-age=3600; path=/"')
  self.assertIn('fixture_session=synthetic',self.driver.evaluate('document.cookie'))
  self.driver.ws.socket.close();self.stop_browser()
  self.assertEqual(self.browser.returncode,0,'Original browser must close gracefully, not be killed')
  browser=subprocess.Popen([f.NODE,'-e',f.BOOTSTRAP,f.PLAYWRIGHT_MODULE,self.directory.name,f.CHROME,self.origin+'/settings'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
  try:
   q=queue.Queue();threading.Thread(target=lambda:q.put(browser.stdout.readline()),daemon=True).start()
   self.assertEqual(q.get(timeout=25).strip(),'ready')
   driver=f.module.Driver(self.directory.name,(ROOT/'deploy/browser-observer.js').read_text(),os.environ['KINDRED_BROWSER_FIXTURE_DISPLAY'])
   try:
    # Explicit fixture user focus after restart; the driver never activates it.
    targets=driver.call('Target.getTargets')['targetInfos']
    selected=[t for t in targets if t['type']=='page' and t['url'].startswith(self.origin+'/') and t['title']=='Selected restoration fixture']
    self.assertEqual(len(selected),1)
    tab=selected[0]
    driver.call('Target.activateTarget',{'targetId':tab['targetId']})
    windows=subprocess.check_output(['xdotool','search','--all','--onlyvisible','--pid',str(driver.binding[0]),'--name','Selected restoration fixture'],text=True).splitlines()
    self.assertEqual(len(windows),1)
    subprocess.run(['xdotool','windowactivate','--sync',windows[-1]],check=True,timeout=5)
    # Record actual focus/visibility before worker observation. This is a
    # read-side fixture oracle; it neither selects nor executes a page action.
    for target in driver.call('Target.getTargets')['targetInfos']:
     if target['type']=='page':
      session=driver.call('Target.attachToTarget',{'targetId':target['targetId'],'flatten':True})['sessionId']
      # Playwright may emulate focus across restored pages; disable that
      # fixture-only emulation so the worker sees actual native tab focus.
      driver.call('Emulation.setFocusEmulationEnabled',{'enabled':False},session)
      print('RESTORE FOCUS ORACLE',json.dumps(driver.call('Runtime.evaluate',{'expression':'JSON.stringify({title:document.title,focus:document.hasFocus(),visibility:document.visibilityState,url:location.href})','returnByValue':True},session)),flush=True)
      driver.call('Target.detachFromTarget',{'sessionId':session})
    deadline=time.monotonic()+5
    while True:
     try:observation=driver.observe(self.origin,{});break
     except RuntimeError:
      if time.monotonic()>deadline:raise
      time.sleep(.1)
    self.assertIn('Safe checks',observation['text'])
    self.assertIn('fixture_session=synthetic',driver.evaluate('document.cookie'))
    endpoint=Path(self.directory.name,'DevToolsActivePort').read_text().splitlines()
    self.assertTrue(endpoint[0].isdigit())
    # Production binding verifies the exact process owns this loopback listener.
    self.assertTrue(driver.binding_current())
   finally:driver.ws.socket.close()
  finally:
   browser.terminate()
   try:browser.wait(timeout=8)
   except subprocess.TimeoutExpired:browser.kill();browser.wait()
   browser.stdin.close();browser.stdout.close()
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--chrome',required=True);p.add_argument('--playwright-module',required=True);a=p.parse_args()
 f.CHROME=a.chrome;f.PLAYWRIGHT_MODULE=a.playwright_module;f.NODE='node'
 screen=next(n for n in range(32,1,-1) if not Path(f'/tmp/.X11-unix/X{n}').exists() and not Path(f'/tmp/.X{n}-lock').exists())
 with tempfile.TemporaryDirectory(prefix='kindred-restoration-env-') as temporary:
  os.environ.update(DISPLAY=f':{screen}',KINDRED_BROWSER_FIXTURE_DISPLAY=f':{screen}',XDG_CONFIG_HOME=temporary+'/config',XDG_DATA_HOME=temporary+'/data',XDG_CACHE_HOME=temporary+'/cache')
  xvfb=subprocess.Popen(['Xvfb',f':{screen}','-screen','0','1440x900x24','-nolisten','tcp','-noreset'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  box=None
  try:
   time.sleep(.4);box=subprocess.Popen(['openbox'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);time.sleep(.4)
   result=unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(RestorationTests))
  finally:
   for child in [box,xvfb]:
    if child and child.poll() is None:child.terminate();child.wait(timeout=5)
  raise SystemExit(not result.wasSuccessful())
