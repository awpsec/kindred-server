const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const engine=process.env.WEBKIT?'webkit':'chromium',browser=await({chromium,webkit}[engine]).launch();
 try{
  const page=await browser.newPage({viewport:{width:1200,height:850}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);
  await page.goto('http://127.0.0.1:'+server.address().port);await page.locator('#prompt').waitFor();await page.waitForTimeout(300);
  await page.locator('#prompt').fill('Keep this draft');await page.waitForTimeout(250);
  const result=await page.evaluate(async()=>{
   const form=document.querySelector('#composer'),surface=form.querySelector('.composer-resize-surface'),editor=document.querySelector('#prompt');
   const read=()=>({height:surface.getBoundingClientRect().height,bottom:surface.getBoundingClientRect().bottom,y:editor.getBoundingClientRect().y,x:editor.getBoundingClientRect().x});
   const reply=document.querySelector('[data-message] [data-message-action=reply]');if(!reply)throw Error('Reply fixture missing');
   const old=read();reply.click();await Promise.resolve();
   const first=read(),effects=form.getAnimations({subtree:true}).filter(a=>a.effect.getKeyframes().some(k=>'transform'in k));
   if(!effects.length)throw Error('No painted reply transition');
   if(effects.some(a=>a.effect.getKeyframes().some(k=>'height'in k||'width'in k)))throw Error('Layout animation on reply');
   effects.forEach(a=>{a.pause();a.currentTime=80;});const mid=read();
   document.querySelector('#composer-reply button').click();const reversed=read();
   await new Promise(r=>setTimeout(r,420));
   return {old,first,mid,reversed,final:read(),draft:editor.value,morphing:form.classList.contains('is-morphing'),ghosts:form.querySelectorAll('.composer-reply-ghost').length};
  });
  assert(Math.abs(result.old.height-result.first.height)<2,'Paint starts from existing height');
  assert(Math.abs(result.old.y-result.first.y)<2,'Editor retains visual position');
  assert(result.mid.height>result.old.height+2,'Reply grows through intermediate painted sizes');
  assert(Math.abs(result.mid.height-result.reversed.height)<2,'Reversal starts at current paint height');
  assert(Math.abs(result.final.height-result.old.height)<2,'Collapse returns to natural size');
  assert.equal(result.draft,'Keep this draft');assert(!result.morphing&&!result.ghosts,'Effects clean up');
  await page.locator('#prompt').focus();await page.keyboard.press('End');await page.keyboard.press('Shift+Enter');await page.keyboard.insertText('Another line');await page.waitForTimeout(300);
  assert.equal(await page.locator('#prompt').evaluate(n=>n.value),'Keep this draft\nAnother line');
  assert(await page.locator('#prompt').evaluate(n=>n.contains(getSelection().anchorNode)),'Caret retained');
  for(const system of [true,false]){
   if(system)await page.emulateMedia({reducedMotion:'reduce'});else await page.evaluate(()=>document.documentElement.dataset.motion='off');
   await page.locator('[data-message] [data-message-action=reply]').first().evaluate(n=>n.click());
   assert.equal(await page.locator('#composer').evaluate(n=>n.classList.contains('is-morphing')),false);
   await page.locator('#composer-reply button').click();await page.emulateMedia({reducedMotion:'no-preference'});
  }
  assert.deepEqual(errors,[]);console.log(JSON.stringify({passed:true,engine,...result}));
 }finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.closeAllConnections();server.close();});
