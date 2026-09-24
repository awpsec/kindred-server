const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict'),fs=require('node:fs');
(async()=>{await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await(process.env.WEBKIT?webkit:chromium).launch({headless:true});
try{
 const page=await browser.newPage({viewport:{width:1120,height:800}}),errors=[];page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(12000);
 const list={id:'complete-list',chat_id:'dm-piper',bot_id:'piper',revision:1,title:'Ready for the project review',items:[{id:'report',title:'Review the report',state:'done',owner:'user'},{id:'notes',title:'Add the final notes',state:'pending',owner:'user'}]};
 await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);
 await page.route('**/api/runs',r=>r.fulfill({json:[]}));
 await page.route('**/api/chats/dm-piper*',r=>r.fulfill({json:{chat:{id:'dm-piper',name:'Piper',members:['piper']},messages:[{seq:1,sender:'piper',kind:'message',text:'The report is reviewed. Just the final notes left.',created:1789050000},{seq:2,sender:'piper',kind:'checklist',text:list.title,planning:list,created:1789050001},{seq:3,sender:'piper',kind:'message',text:'Everything stays in this list, so we can pick it up again later.',created:1789050002}],page:{has_before:false,has_after:false}}}));
 await page.route('**/api/chats/dm-piper/checklists/complete-list',r=>{const v=r.request().postDataJSON();list.items.find(i=>i.id===v.item_id).state=v.state;list.revision++;return r.fulfill({json:list});});
 await page.goto('http://127.0.0.1:'+server.address().port+'/');const card=page.locator('#content [data-list="complete-list"]');await card.waitFor();
 const out=process.env.KINDRED_TEST_ARTIFACTS||'/tmp/kindred-decision-receipts';fs.mkdirSync(out,{recursive:true});let frame=0;
 const capture=async(n=1)=>{for(let i=0;i<n;i++){await page.screenshot({path:`${out}/frame-${String(frame++).padStart(3,'0')}.png`});await page.waitForTimeout(30);}};
 await capture(5);const expanded=(await card.boundingBox()).height;
 await card.getByRole('checkbox',{name:'Mark Add the final notes done'}).check();await capture(12);
 assert.equal(await card.locator('.decision-receipt-disclosure').evaluate(n=>n.open),false);assert((await card.boundingBox()).height<expanded-50);
 assert.equal(await card.getByRole('checkbox').count(),0,'Hidden detail controls must leave accessibility tree');
 await card.locator('.decision-receipt-summary').hover();
 assert.equal(await card.locator('.decision-receipt-summary').evaluate(n=>getComputedStyle(n).borderTopLeftRadius),await card.evaluate(n=>getComputedStyle(n).borderTopLeftRadius));
 await card.screenshot({path:out+'/completed-dark.png'});
 await card.locator('.decision-receipt-summary').click();await capture(12);assert(await card.getByRole('checkbox').first().isVisible());
 await card.screenshot({path:out+'/details-dark.png'});
 await card.locator('.decision-receipt-summary').focus();await page.keyboard.press('Enter');await capture(12);assert(await card.locator('.decision-receipt-body').evaluate(n=>n.inert));
 assert.equal(await card.locator('.decision-receipt-summary').evaluate(n=>getComputedStyle(n).outlineStyle),'none');
 assert.equal(await card.locator('.decision-receipt-toggle').evaluate(n=>getComputedStyle(n).textDecorationLine),'underline');
 await page.mouse.click(1050,650);assert.equal(await card.locator('.decision-receipt-toggle').evaluate(n=>getComputedStyle(n).textDecorationLine),'none');
 // A fresh terminal history entry starts compact, without replaying its completion.
 await page.reload();await card.locator('.decision-receipt-summary').waitFor();assert.equal(await card.locator('.decision-receipt-disclosure').evaluate(n=>n.open),false);
 await page.evaluate(()=>document.documentElement.dataset.theme='light');await card.screenshot({path:out+'/completed-light.png'});
 // Exercise reduced motion and stale-render reconciliation directly through the shared component.
 const result=await page.evaluate(async()=>{
  const {decisionReceipt}=await import('/decision-receipts.js');document.documentElement.dataset.motion='off';
  const host=document.createElement('div');document.body.append(host);
  const render=terminal=>{const el=document.createElement('section');el.className='planning-card';el.innerHTML='<p>Decision details</p><button>Action</button>';return decisionReceipt(el,{key:'test-reconcile',title:'Review proposal',outcome:'Allowed',terminal});};
  host.append(render(false));await new Promise(requestAnimationFrame);host.replaceChildren(render(true));await new Promise(requestAnimationFrame);
  const noMotion=host.querySelector('section').getAnimations().length===0;
  host.querySelector('summary').click();const signature=host.firstChild.dataset.decisionSignature;
  const ignored=render(true); // App can build then discard a matching render.
  host.querySelector('summary').click();host.querySelector('summary').click();host.replaceChildren(render(true));
  const preserved=host.querySelector('details').open&&host.firstChild.dataset.decisionSignature===signature;
  host.replaceChildren(render(false));const reopened=!host.querySelector('details');host.remove();return{noMotion,preserved,reopened};
 });assert.deepEqual(result,{noMotion:true,preserved:true,reopened:true});
 assert.deepEqual(errors,[]);console.log('Decision receipts: live checklist completion, keyboard disclosure, hidden controls, history, reduced motion, reconciliation and reopening passed.');
}finally{await browser.close();server.closeAllConnections();server.close();}})().catch(e=>{console.error(e);process.exitCode=1;});
