const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await(process.env.WEBKIT?webkit:chromium).launch();
 try{
  for(const platform of ['linux','macos'])for(const preference of [null,{sidebar:480,computer:700}]){
   const context=await browser.newContext({viewport:{width:1440,height:900}}),p=await context.newPage();
   await p.addInitScript(({token,platform,preference})=>{sessionStorage.setItem('kindred-token',token);window.__KINDRED_DESKTOP={platform};window.__TAURI__={core:{invoke:async()=>null}};if(preference)localStorage.setItem('kindred-pane-sizes',JSON.stringify(preference));},{token,platform,preference});
   await p.goto('http://127.0.0.1:'+server.address().port);await p.locator('#show-computer').click();
   const geometry=()=>p.evaluate(()=>{const rect=s=>document.querySelector(s).getBoundingClientRect();return {sidebar:rect('.sidebar').width,chat:rect('.conversation').width,right:rect('#computer-panel').width,overflow:document.documentElement.scrollWidth>innerWidth,prefs:localStorage.getItem('kindred-pane-sizes')};});
   await p.waitForTimeout(250);const original=await geometry();
   for(const width of [1100,1000,950,900,850,800,761]){
    await p.setViewportSize({width,height:900});await p.waitForTimeout(100);
    const g=await geometry();assert(g.chat>=419.5,`${platform} ${width}px keeps at least 420px for chat: ${JSON.stringify(g)}`);assert(!g.overflow,'No horizontal page overflow');
    assert.equal(g.prefs,original.prefs,'Automatic fitting must not overwrite the preferred sizes');
    if(width===900){
     const handle=await p.getByRole('separator',{name:'Resize sidebar'}).boundingBox();
     await p.mouse.move(handle.x+handle.width/2,300);await p.mouse.down();await p.mouse.move(650,300);await p.waitForTimeout(40);
     assert((await geometry()).chat>=419.5,'Dragging the automatically fitted sidebar cannot squeeze the chat');
     await p.keyboard.press('Escape');await p.mouse.up();
     assert.equal((await geometry()).prefs,original.prefs,'Cancel restores the saved preference');
    }
   }
   await p.setViewportSize({width:1440,height:900});await p.waitForTimeout(100);const restored=await geometry();assert.equal(restored.sidebar,original.sidebar);assert.equal(restored.right,original.right);
   await context.close();
  }
  console.log('Pane viewport fitting: defaults and saved sizes preserve chat, restore preferences, and avoid overflow on Linux/macOS layouts');
 }finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
