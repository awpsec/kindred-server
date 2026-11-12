const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const engine=process.env.WEBKIT?'webkit':'chromium',browser=await({chromium,webkit}[engine]).launch();
 const out=process.env.KINDRED_TEST_ARTIFACTS||'/opt/kindred/testing/pane-motion';fs.mkdirSync(out,{recursive:true});
 try{
  const p=await browser.newPage({viewport:{width:1320,height:820}}),errors=[];p.on('pageerror',e=>errors.push(e.message));
  await p.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);
  await p.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../ui/app.js'),'utf8')+'\nexport {showPane,hidePane,setComputerExpanded,finishComputerTransition,layoutPanes};'}));
  await p.goto(origin);await p.locator('#prompt').waitFor();
  await p.evaluate(async()=>{window.fixture=await import('/app.js');});
  await p.locator('#prompt').fill('Please keep this draft while I inspect the details. '.repeat(4));await p.waitForTimeout(300);
  for(const width of [1320,1000,780,390]){
   await p.setViewportSize({width,height:820});await p.waitForTimeout(60);
   for(const pane of ['details-panel','computer-panel'])for(const open of [true,false]){
    const proof=await p.evaluate(({pane,open})=>{
     const panel=document.getElementById(pane);
     const rect=n=>{const r=n.getBoundingClientRect();return {x:r.x,y:r.y,bottom:r.bottom,right:r.right,width:r.width};};
     const sample=()=>({pane:rect(panel),chat:rect(document.querySelector('.conversation')),composer:rect(document.querySelector('#composer')),button:rect(document.querySelector('#composer-actions')),send:rect(document.querySelector('#send')),editor:rect(document.querySelector('#prompt')),menu:rect(document.querySelector('#bot-details'))});
     const before=sample();fixture[open?'showPane':'hidePane'](panel);
     const moving=[...document.querySelectorAll('.pane-layout-frame,.conversation-header,#content,#composer-area,.composer-resize-surface,#composer>* ,#composer .composer-bottom>*')].flatMap(n=>n.getAnimations()).filter(a=>a.effect.getTiming().duration===240);
     if(!moving.length)throw new Error('No coordinated motion');
     if(moving.some(a=>a.effect.getKeyframes().some(k=>['width','height','left','padding','margin'].some(key=>key in k))))throw new Error('Pane motion must not animate layout');moving.forEach(a=>a.pause());
     const frames=[];for(const t of [0,30,60,120,180,239]){moving.forEach(a=>a.currentTime=t);frames.push(sample());}
     fixture.finishComputerTransition();return {before,frames,after:sample(),hidden:panel.hidden,leaks:document.querySelectorAll('.pane-layout-frame,.pane-layout-reservation').length};
    },{pane,open});
    fs.writeFileSync(path.join(out,`${engine}-${width}-${pane}-${open?'open':'close'}.json`),JSON.stringify(proof,null,2));
    assert.equal(proof.hidden,!open);assert.equal(proof.leaks,0);
    assert(proof.frames.every(f=>Math.abs(f.chat.width-proof.after.chat.width)<2),'Transcript width is settled once, not animated');
    for(const item of ['composer','menu']){
     assert(Math.abs((proof.before[item].x+proof.before[item].width/2)-(proof.frames[0][item].x+proof.frames[0][item].width/2))<2,`${width} ${pane} ${open} ${item} start center snapped`);
     assert(Math.abs(proof.after[item].width-proof.frames[0][item].width)<2,`${item} must reserve final width once`);
     assert(Math.abs(proof.after[item].x-proof.frames.at(-1)[item].x)<2,`${item} end x snapped`);
     assert(Math.abs(proof.after[item].width-proof.frames.at(-1)[item].width)<2,`${item} end width snapped`);
     const start=proof.frames[0][item].right,end=proof.after[item].right;
     if(Math.abs(end-start)>5){assert(proof.frames.slice(1,-1).some(f=>Math.abs(f[item].right-start)>2&&Math.abs(f[item].right-end)>2),`${item} must move through intermediate positions`);for(const f of proof.frames)assert(f[item].right>=Math.min(start,end)-2&&f[item].right<=Math.max(start,end)+2,`${item} must not overshoot`);}
    }
    for(const key of ['button','send','editor']){assert(Math.abs(proof.before[key].x-proof.frames[0][key].x)<2,`${key} start x continuity`);assert(Math.abs(proof.before[key].y-proof.frames[0][key].y)<2,`${key} start y continuity`);}
    const travel=Math.abs(proof.frames.at(-1).pane.x-proof.frames[0].pane.x);
    assert(travel>150,'Pane must slide full distance: '+travel);
   }
  }
  await p.setViewportSize({width:1320,height:820});await p.waitForTimeout(60);
  const reversal=await p.evaluate(()=>{const pane=document.querySelector('#computer-panel');fixture.showPane(pane);for(const a of pane.getAnimations()){a.pause();a.currentTime=100;}const x=pane.getBoundingClientRect().x;fixture.hidePane(pane);const diff=Math.abs(x-pane.getBoundingClientRect().x);fixture.showPane(pane);fixture.finishComputerTransition();return {diff,hidden:pane.hidden};});assert(reversal.diff<2&&!reversal.hidden,JSON.stringify(reversal));
  for(const expanded of [true,false]){await p.evaluate(v=>fixture.setComputerExpanded(v),expanded);await p.waitForTimeout(480);assert.equal(await p.locator('#computer-panel').evaluate(n=>n.style.cssText),'');}
  await p.evaluate(()=>{fixture.showPane(document.querySelector('#details-panel'));fixture.hidePane(document.querySelector('#computer-panel'));});await p.waitForTimeout(480);assert(await p.locator('#computer-panel').evaluate(n=>n.hidden));assert(await p.locator('#details-panel').isVisible());
  await p.screenshot({path:path.join(out,engine+'-details-open.png')});
  await p.emulateMedia({reducedMotion:'reduce'});await p.evaluate(()=>fixture.hidePane(document.querySelector('#details-panel')));assert(await p.locator('#details-panel').evaluate(n=>n.hidden));assert.equal(await p.locator('.pane-layout-reservation').count(),0);
  // Native iOS's important overlay rules must yield to the temporary frame.
  await p.emulateMedia({reducedMotion:'no-preference'});
  for(const mode of ['overlay','side']){
   await p.setViewportSize({width:mode==='overlay'?390:1320,height:820});await p.waitForTimeout(80);
   const result=await p.evaluate(mode=>{
    window.__KINDRED_MOBILE_PLATFORM='ios';document.documentElement.dataset.iosComputer=mode;document.documentElement.dataset.iosLayout=mode==='side'?'regular':'compact';
    fixture.layoutPanes();const pane=document.querySelector('#computer-panel');fixture.showPane(pane);const a=pane.getAnimations()[0];if(!a)throw new Error(JSON.stringify({mode,hidden:pane.hidden,inert:pane.inert,focus:document.hasFocus(),motion:document.documentElement.dataset.motion,reduce:matchMedia('(prefers-reduced-motion:reduce)').matches,app:document.querySelector('#app').hidden}));a.pause();a.currentTime=0;const start=pane.getBoundingClientRect().x;a.currentTime=319;const last=pane.getBoundingClientRect().x;fixture.finishComputerTransition();const end=pane.getBoundingClientRect().x;fixture.hidePane(pane);fixture.finishComputerTransition();delete window.__KINDRED_MOBILE_PLATFORM;delete document.documentElement.dataset.iosComputer;delete document.documentElement.dataset.iosLayout;return {start,last,end};
   },mode);assert(Math.abs(result.last-result.end)<2,JSON.stringify({mode,...result}));assert(Math.abs(result.start-result.end)<2,'Native iOS retains its opacity-only transition');
  }
  assert.equal(await p.locator('#prompt').evaluate(n=>n.value),'Please keep this draft while I inspect the details. '.repeat(4),'Pane changes preserve the entire draft');
  assert.deepEqual(errors,[]);console.log(JSON.stringify({passed:true,engine,fullSlide:true,headerAndComposerContinuous:true,desktopTabletPhone:true,reversal:true,switching:true,expandCollapse:true,reducedMotion:true}));
 }finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);server.close();process.exitCode=1});
