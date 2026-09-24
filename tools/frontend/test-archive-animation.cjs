const {server,token}=require('./fixtures/desktop.cjs');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
(async()=>{await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
const browser=await(process.env.WEBKIT?webkit:chromium).launch({headless:true,args:process.env.WEBKIT?[]:['--no-sandbox']});const page=await browser.newPage({viewport:{width:900,height:640}});const out='/tmp/kindred-archive-animation';fs.mkdirSync(out,{recursive:true});
try{
await page.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../ui/app.js'),'utf8')+'\nexport {archiveBot,state,refresh};'}));
await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);await page.goto(origin);await page.locator('.nav-entry [data-sidebar-id="piper"]').first().waitFor();
await page.evaluate(async()=>{const m=await import('/app.js');m.state.routines.push({id:'archive-routine',bot_id:'piper',enabled:true});window.archivePromise=m.archiveBot('piper');});
await page.locator('.archive-coffin').waitFor();assert.equal(await page.locator('.archive-coffin .character').getAttribute('data-action'),'coffin');
const ink=await page.evaluate(()=>{const root=document.documentElement,old=root.dataset.theme,path=document.querySelector('.archive-coffin-solid path');const colors=[];for(const theme of ['dark','light']){root.dataset.theme=theme;colors.push(getComputedStyle(path).stroke);}root.dataset.theme=old;return colors;});assert.deepEqual(ink,['rgb(0, 0, 0)','rgb(255, 255, 255)']);
assert.equal(await page.locator('.archive-coffin-solid polygon[fill=none]').count(),0,'No decorative lid outline');assert.equal(await page.locator('.nav-entry [data-sidebar-id="piper"]').count()>0,true);
await page.evaluate(async()=>{const m=await import('/app.js');await m.refresh(true);});assert.equal(await page.locator('.archive-coffin').count(),1,'Polling cannot remove an in-flight archive animation');
for(let i=0;i<28;i++){await page.screenshot({path:path.join(out,`frame-${String(i).padStart(2,'0')}.png`),clip:{x:0,y:60,width:270,height:250}});await page.waitForTimeout(65);}
await page.evaluate(()=>window.archivePromise);assert.equal(await page.locator('.nav-entry [data-sidebar-id="piper"]').count(),0);
await page.evaluate(async()=>{const m=await import('/app.js'),b=m.state.bots.find(b=>b.id==='piper');b.profile.archived=false;await fetch('/api/bots/piper',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(b)});await m.refresh(true);});
assert.equal(await page.locator('.nav-entry [data-bot-id="piper"]').evaluate(n=>getComputedStyle(n).opacity),'1','Restoring a bot must not reuse a faded avatar');
await page.route(origin+'/api/bots/piper',r=>r.fulfill({status:400,json:{error:'Cannot archive fixture'}}));
const failed=await page.evaluate(async()=>{try{await (await import('/app.js')).archiveBot('piper');return false;}catch{return true;}});assert(failed);assert.equal(await page.locator('.archive-coffin').count(),0);
await page.unroute(origin+'/api/bots/piper');
await page.evaluate(async()=>{const m=await import('/app.js');m.state.general.reduced_motion=true;await m.archiveBot('piper');});assert.equal(await page.locator('.archive-coffin').count(),0);
console.log('Archive animation, polling, restored avatar, failed save and reduced motion passed');
}finally{await browser.close();server.closeAllConnections();server.close();}})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
