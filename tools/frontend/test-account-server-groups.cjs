const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server}=require('./fixtures/desktop.cjs');const assert=require('node:assert/strict');
(async()=>{await new Promise(r=>server.listen(0,'127.0.0.1',r));try{for(const engine of [chromium,webkit]){
 const browser=await engine.launch();try{const p=await browser.newPage({viewport:{width:560,height:780}});
 await p.addInitScript(()=>{window.calls=[];window.__TAURI__={core:{invoke:async(name,args)=>{
 calls.push({name,args});if(name==='profile_home_state')return {platform:'linux',theme:'dark',last:'a',entries:[
 {key:'a',server:'https://work.example',username:'alex',name:'Work',session_available:true},
 {key:'b',server:'https://home.example',username:'sam',name:'Personal',session_available:false},
 {key:'c',server:'https://work.example',username:'taylor',name:'Team',session_available:true}
 ]};if(name==='standalone_status')return {status:'idle'};if(name==='profile_activity')return {};return null;
 }}}});
 await p.goto('http://127.0.0.1:'+server.address().port+'/profile-home.html?section=accounts');
 await p.locator('.account-server-group').first().waitFor();
 assert.equal(await p.locator('.account-server-group').count(),2);
 assert.equal(await p.locator('.account-server-group').first().locator('.saved-profile').count(),2);
 if(process.env.PREVIEW_PATH&&engine===chromium)await p.screenshot({path:process.env.PREVIEW_PATH});
 await p.locator('#add-account').click();await p.locator('#known-server').selectOption('https://home.example');
 assert.equal(await p.locator('#address').inputValue(),'https://home.example');assert(await p.locator('#address').isHidden());
 await p.locator('#server-form button[type=submit]').click();
 assert(await p.evaluate(()=>calls.some(c=>c.name==='connect_profile_server'&&c.args.address==='https://home.example')));
 await p.locator('#known-server').selectOption('new');assert(await p.locator('#address').isVisible());assert.equal(await p.locator('#address').inputValue(),'');
 await p.locator('.select[data-key=b]').click();assert(await p.evaluate(()=>calls.some(c=>c.name==='switch_native_profile'&&c.args.key==='b')));
 }finally{await browser.close()}
}console.log('PASS server grouping, backend selection, new address, saved account selection in Chromium and WebKit');
}finally{server.close()}})().catch(e=>{console.error(e);process.exitCode=1});
