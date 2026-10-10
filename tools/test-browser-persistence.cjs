// Actual headed Chromium and desktop bus, isolated profiles and loopback auth.
// No owner paths, providers, credentials, browser encryption override or HOME change.
const {chromium}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'/opt/aldabra-dev-tools/node_modules/playwright');
const {spawn,execFileSync}=require('node:child_process'),fs=require('node:fs'),os=require('node:os'),path=require('node:path'),http=require('node:http'),assert=require('node:assert/strict');
const root=path.resolve(__dirname,'..'),chrome=process.env.KINDRED_TEST_CHROME||'/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
if(!process.env.KINDRED_PERSISTENCE_CHILD){
 const runtime=fs.mkdtempSync(path.join(os.tmpdir(),'kindred-bus-fixture-'));
 try{process.stdout.write(execFileSync('dbus-run-session',['--',process.execPath,__filename],{env:{...process.env,XDG_RUNTIME_DIR:runtime,XDG_DATA_HOME:path.join(runtime,'data'),XDG_CONFIG_HOME:path.join(runtime,'config'),XDG_CACHE_HOME:path.join(runtime,'cache'),KINDRED_PERSISTENCE_CHILD:'1'},timeout:180000,encoding:'utf8',maxBuffer:2*1024*1024}));}
 finally{fs.rmSync(runtime,{recursive:true,force:true});}
}else (async()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'kindred-auth-fixture-'));let xvfb,openbox,busHost;const browsers=new Set();
 const server=http.createServer((q,r)=>{if(q.url==='/seed')r.setHeader('Set-Cookie',['persistent=synthetic; Max-Age=3600; HttpOnly; SameSite=Lax; Path=/','session=synthetic; HttpOnly; SameSite=Lax; Path=/']);r.setHeader('Content-Type','text/html');r.end('<title>Synthetic auth</title><pre>'+JSON.stringify({persistent:/(?:^|; )persistent=/.test(q.headers.cookie||''),session:/(?:^|; )session=/.test(q.headers.cookie||'')})+'</pre>');});await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 async function launch(screen,mode='restore'){
  // Exercise the actual shared preparation function with ONLY the root relocated
  // into this disposable fixture. /proc, UID, process identity and bus are real.
  const prepare="import importlib.util,json,sys;from pathlib import Path;s=importlib.util.spec_from_file_location('launch',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);print(json.dumps(m.prepare(int(sys.argv[2]),sys.argv[3],root=Path(sys.argv[4]))))";
  const policy=JSON.parse(execFileSync('python3',['-c',prepare,path.join(root,'deploy/browser-launch.py'),String(screen),mode,path.join(dir,'profiles')],{env:{...process.env,DISPLAY:':'+screen},encoding:'utf8'}));
  const profile=policy.args.find(x=>x.startsWith('--user-data-dir=')).slice(16),endpoint=path.join(profile,'DevToolsActivePort');fs.rmSync(endpoint,{force:true});
  const child=spawn(chrome,[...policy.args,'--no-sandbox'],{env:{...process.env,...policy.env},stdio:'ignore',detached:true});browsers.add(child);
  for(let i=0;i<150;i++){if(fs.existsSync(endpoint)){try{const port=fs.readFileSync(endpoint,'utf8').split('\n')[0];const browser=await chromium.connectOverCDP('http://127.0.0.1:'+port,{timeout:1000});return {child,browser,profile,policy};}catch{}}if(child.exitCode!==null)throw Error('Fixture Chromium exited');await sleep(100);}throw Error('Fixture endpoint deadline');
 }
 async function stop(x,crash=false){
  if(crash){process.kill(-x.child.pid,'SIGKILL');}else{
   // Send normal WM_DELETE_WINDOW only to windows of this owned process/display.
   execFileSync('python3',[path.join(root,'tools/close-fixture-browser.py'),String(x.child.pid)],{env:{...process.env,DISPLAY:x.policy.env.DISPLAY},timeout:5000});
  }
  for(let i=0;i<100&&x.child.exitCode===null&&x.child.signalCode===null;i++)await sleep(50);
  if(!crash)assert.equal(x.child.exitCode,0,'Normal close must not be a forced kill');
  await x.browser.close().catch(()=>{});browsers.delete(x.child);
 }
 async function seed(x){const context=x.browser.contexts()[0],page=context.pages()[0]||await context.newPage();await page.goto(origin+'/seed');await page.evaluate(async()=>{localStorage.setItem('auth-fixture','synthetic');await new Promise((resolve,reject)=>{const q=indexedDB.open('auth-fixture',1);q.onupgradeneeded=()=>q.result.createObjectStore('state');q.onerror=()=>reject(q.error);q.onsuccess=()=>{const db=q.result,tx=db.transaction('state','readwrite');tx.objectStore('state').put('synthetic','auth');tx.oncomplete=()=>{db.close();resolve();};tx.onerror=()=>reject(tx.error);};});});}
 async function inspect(x){const context=x.browser.contexts()[0],tabs=context.pages().filter(p=>p.url().startsWith(origin)).length,page=context.pages().find(p=>p.url().startsWith(origin))||await context.newPage();await page.goto(origin+'/check');return {tabs,...await page.evaluate(async()=>({cookies:JSON.parse(document.querySelector('pre').textContent),localStorage:localStorage.getItem('auth-fixture')==='synthetic',indexedDB:await new Promise((resolve,reject)=>{const q=indexedDB.open('auth-fixture',1);q.onupgradeneeded=()=>q.result.createObjectStore('state');q.onerror=()=>reject(q.error);q.onsuccess=()=>{const db=q.result,tx=db.transaction('state');const read=tx.objectStore('state').get('auth');read.onsuccess=()=>{resolve(read.result==='synthetic');db.close();};read.onerror=()=>reject(read.error);};})}))};}
 try{
  let screen;for(let n=20;n<=32;n++){if(fs.existsSync('/tmp/.X11-unix/X'+n))continue;const child=spawn('Xvfb',[':'+n,'-screen','0','1440x900x24','-nolisten','tcp'],{stdio:'ignore'});await sleep(150);if(child.exitCode===null){xvfb=child;screen=n;break;}}assert(screen,'No disposable display available');
  openbox=spawn('openbox',['--sm-disable'],{env:{...process.env,DISPLAY:':'+screen},stdio:'ignore'});await sleep(300);
  const results=[];let x=await launch(screen);await seed(x);const inode=fs.statSync(x.profile).ino;await stop(x);
  x=await launch(screen,'new');let state=await inspect(x);assert(state.cookies.persistent&&state.cookies.session&&state.localStorage&&state.indexedDB);assert(state.tabs>=1);assert.equal(fs.statSync(x.profile).ino,inode);results.push({boundary:'normal-close/cross-action-new',...state,profileInodePreserved:true,busSelected:!!x.policy.env.DBUS_SESSION_BUS_ADDRESS});
  await stop(x,true);x=await launch(screen);state=await inspect(x);assert(state.cookies.persistent&&state.localStorage&&state.indexedDB);assert(state.tabs>=1);results.push({boundary:'crash/restore',...state});await stop(x);
  // Recreate the disposable desktop/bus without changing profile/XDG data.
  // This is a desktop-session restart, not a VM reboot or keyring proof.
  const firstBus=x.policy.env.DBUS_SESSION_BUS_ADDRESS;
  openbox.kill('SIGTERM');await new Promise(r=>openbox.once('exit',r));openbox=null;
  busHost=spawn('dbus-run-session',['--','sh','-c','openbox --sm-disable & wait'],{env:{...process.env,DISPLAY:':'+screen},stdio:'ignore',detached:true});await sleep(350);
  x=await launch(screen);assert.notEqual(x.policy.env.DBUS_SESSION_BUS_ADDRESS,firstBus);state=await inspect(x);
  assert(state.cookies.persistent&&state.cookies.session&&state.localStorage&&state.indexedDB);assert(state.tabs>=1);
  results.push({boundary:'desktop-session-bus-restart',...state,distinctBus:true});await stop(x);
  // A second desktop has the same UID and bus, but a distinct assigned profile.
  const other=screen===32?19:screen+1;assert(!fs.existsSync('/tmp/.X11-unix/X'+other));const secondX=spawn('Xvfb',[':'+other,'-screen','0','1440x900x24','-nolisten','tcp'],{stdio:'ignore'});let secondBox;
  try{await sleep(200);secondBox=spawn('openbox',['--sm-disable'],{env:{...process.env,DISPLAY:':'+other},stdio:'ignore'});await sleep(300);x=await launch(other);state=await inspect(x);assert(!state.cookies.persistent&&!state.cookies.session&&!state.localStorage&&!state.indexedDB);results.push({boundary:'separate-screen',...state});await stop(x);}finally{secondBox?.kill('SIGTERM');secondX.kill('SIGTERM');}
  console.log(JSON.stringify({chrome,helperSha256:require('node:crypto').createHash('sha256').update(fs.readFileSync(path.join(root,'deploy/browser-launch.py'))).digest('hex'),results,keyringReadabilityProved:false,rebootProved:false,realSiteLoginProved:false},null,2));
 }finally{for(const child of browsers){try{process.kill(-child.pid,'SIGKILL');}catch{}}openbox?.kill('SIGTERM');if(busHost){try{process.kill(-busHost.pid,'SIGTERM');}catch{}}xvfb?.kill('SIGTERM');await new Promise(r=>server.close(r));fs.rmSync(dir,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exitCode=1;});
