const {server,token}=require('./fixtures/desktop.cjs');
const {webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const browser=await webkit.launch();let probes=0;
 try{
 const page=await browser.newPage({viewport:{width:1100,height:900}});
 await page.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../ui/app.js'),'utf8')+'\nexport {openSettings};'}));
 await page.route(origin+'/api/composio',r=>r.fulfill({json:{configured:true,apps:[{id:'gmail',name:'Gmail',accounts:[{id:'household',name:'Household',status:'ACTIVE'}]},{id:'slack',name:'Slack',accounts:[{id:'team',name:'Team',status:'EXPIRED'}]}]}}));
 await page.route(origin+'/api/composio/gmail/test',r=>{probes++;assert.equal(r.request().postDataJSON().account_id,'household');return r.fulfill({json:{ok:true,email:'household@example.test'}});});
 await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);await page.goto(origin);
 await page.evaluate(async()=>{await(await import('/app.js')).openSettings('connections');});
 const row=page.locator('.composio-service').first(),summary=row.locator('summary');await summary.waitFor();
 assert.equal(await row.evaluate(n=>n.open),false);assert.equal(probes,0);
 assert.match(await summary.innerText(),/Gmail.*1 connected account/s);assert((await summary.boundingBox()).height<55);
 assert.equal(await summary.locator('.connector-logo svg').count(),1);
 await summary.click();await row.getByText('(household@example.test)',{exact:true}).waitFor();
 await summary.click();assert.equal(await row.getByText('Household',{exact:true}).isVisible(),false);
 await summary.click();assert.equal(probes,1);
 assert.equal(await page.locator('.composio-key-editor').evaluate(n=>n.open),false);
 await page.locator('.composio-key-editor summary').click();
 assert.equal(await page.getByRole('link',{name:'Open Composio dashboard'}).locator('svg').count(),1);
 assert.equal(await page.getByRole('link',{name:'Open Composio dashboard'}).innerText(),'');
 const remove=page.getByRole('button',{name:'Remove Composio key'}),save=page.getByRole('button',{name:'Save project key'});
 assert((await remove.boundingBox()).x>(await save.boundingBox()).x+200);
 fs.mkdirSync('test-results/composio-settings',{recursive:true});
 for(const width of [1100,390])for(const theme of ['dark','light']){
 await page.setViewportSize({width,height:900});await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);
 await row.scrollIntoViewIfNeeded();assert(await row.evaluate(n=>n.getBoundingClientRect().right<=innerWidth));
 await page.locator('#settings-content').screenshot({path:`test-results/composio-settings/${width}-${theme}.png`});
 }
 console.log('Compact service rows, disclosure, verified identity, icon link, key actions, mobile and themes passed');
 }finally{await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);server.close();process.exitCode=1;});
