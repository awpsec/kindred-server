const {server,token}=require('./fixtures/desktop.cjs'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
(async()=>{
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port,browser=await(process.env.WEBKIT?webkit:chromium).launch({headless:true});
try{
const p=await browser.newPage({viewport:{width:1100,height:800}}),reads=[],errors=[];p.on('pageerror',e=>errors.push(e.message));
const bots=['alpha','beta'].map(id=>({id,name:id,provider:'codex',profile:{shape:'round',color:'#2475ff'}}));
const chats=[{id:'team-local',name:'Local Team',members:['alpha','beta']},{id:'server-team',name:'Shared Team',members:['alpha','beta'],shared:true,participants:[],cursor:2,read_cursor:0,unread:true}];
const messages=[{seq:1,sender:'user',text:'Hello',kind:'message',created:1},{seq:2,sender:'alpha',text:'The work is ready.',kind:'message',created:2}];
await p.addInitScript(t=>{sessionStorage.setItem('kindred-token',t);window.fixtureFocused=false;Object.defineProperty(document,'hasFocus',{value:()=>window.fixtureFocused});},token);
await p.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(process.env.KINDRED_TEST_APP||path.resolve(__dirname,'../../ui/app.js'),'utf8')+String.fromCharCode(10)+'export {chooseChat,refresh,markVisibleConversationRead,state};'}));
await p.route(origin+'/identity/meta',async r=>{const response=await r.fetch();await r.fulfill({json:{...await response.json(),server_chats:true}})});
await p.route(origin+'/api/**',r=>{const req=r.request(),name=new URL(req.url()).pathname.slice(4),send=json=>r.fulfill({json});
if(name==='/bots')return send(bots);if(name==='/runs')return send([]);if(name==='/activity')return send({});
if(name==='/chats')return send([chats[0]]);if(name==='/server-chats')return send([chats[1]]);
if(name==='/attention')return send({bots:{},chats:{'team-local':{cursor:2,read_cursor:0,latest_message_seq:2,unread:true}}});
if(name.endsWith('/read')){reads.push(name);return send({read_cursor:req.postDataJSON().cursor});}
const chat=chats.find(c=>name===(c.shared?'/server-chats/':'/chats/')+c.id);
if(chat)return send({chat,messages,page:{has_before:false,has_after:false},workers:[]});
return r.continue();});
await p.goto(origin);await p.evaluate(async()=>window.fixture=await import('/app.js'));
for(const chat of chats){
await p.evaluate(async c=>{window.fixtureFocused=false;await fixture.chooseChat(c);},chat);
await p.locator('#content [data-message="2"]').waitFor();await p.waitForTimeout(400);
const before=reads.length;
await p.evaluate(()=>fixture.markVisibleConversationRead());assert.equal(reads.length,before,'Unfocused history remains unread');
await p.evaluate(async()=>{window.fixtureFocused=true;await fixture.markVisibleConversationRead();});
assert.equal(reads.length,before+1,'Floating composer must not block read receipt');
assert.equal(reads.at(-1),(chat.shared?'/server-chats/':'/chats/')+chat.id+'/read');
await p.evaluate(()=>fixture.refresh(true));
assert.equal(await p.evaluate(id=>fixture.state.attention.chats[id].unread,chat.id),false,'Stale polling cannot restore the unread dot');
}
assert.deepEqual(errors,[]);console.log('Floating composer read receipts: local/shared groups, focus guard and stale polling passed');
}finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
