const {server,token}=require('./fixtures/desktop.cjs');
const {webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const assert=require('node:assert/strict'),fs=require('node:fs');
(async()=>{await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await webkit.launch();try{
 const page=await browser.newPage({viewport:{width:1100,height:760}}),origin='http://127.0.0.1:'+server.address().port;
 const chat={id:'dm-piper',name:'Piper',members:['piper']},list={id:'daily',chat_id:chat.id,bot_id:'piper',revision:1,title:'Daily client brief',items:[{id:'report',title:'Review the finished report',state:'pending',owner:'user',sources:[]}]};
 let moved=false;const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);
 await page.route(origin+'/api/runs',r=>r.fulfill({json:[]}));
 await page.route(origin+'/api/chats/dm-piper*',r=>{const card={seq:moved?4:1,sender:'piper',kind:'checklist',text:list.title,planning:{...list,revision:moved?2:1},created:moved?4:1};const text={seq:3,sender:'piper',kind:'message',text:'The routine checked the client sources and refreshed your list.',created:3};return r.fulfill({json:{chat,messages:moved?[text,card]:[card,text],page:{has_before:false,has_after:false}}});});
 await page.goto(origin);await page.locator('#content [data-list="daily"]').waitFor();
 moved=true;await page.locator('#content [data-message="4"] [data-list="daily"]').waitFor();
 assert.equal(await page.locator('#content [data-list="daily"]').count(),1);
 assert.equal(await page.locator('#content [data-message="1"]').count(),0);
 const order=await page.locator('#content [data-message]').evaluateAll(ns=>ns.map(n=>n.dataset.message));assert(order.indexOf('4')>order.indexOf('3'));
 const out='/opt/kindred/testing/list-recency';fs.mkdirSync(out,{recursive:true});await page.screenshot({path:out+'/updated-list.png'});assert.deepEqual(errors,[]);console.log('PASS: live refresh removes old checklist placement and shows one updated card after newer messages');
}finally{await browser.close();server.closeAllConnections();server.close();}})().catch(e=>{console.error(e);process.exitCode=1});
