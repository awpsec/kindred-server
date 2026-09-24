const {server,token}=require('./fixtures/desktop.cjs');
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const artifacts=process.env.KINDRED_TEST_ARTIFACTS||path.resolve(__dirname,'../../test-results');
fs.mkdirSync(artifacts,{recursive:true});
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const browser=await(process.env.WEBKIT?webkit:chromium).launch({headless:true});
 try{
  const page=await browser.newPage({viewport:{width:1120,height:620}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
  const bots=['Scratch','Piper'].map((name,i)=>({id:'b'+i,name,provider:'codex',profile:{shape:i?'round':'capsule',color:i?'#ffc800':'#8055ff',animated:true}}));
  const chat={id:'team',name:'Team General',members:bots.map(b=>b.id),archived:false,pinned:true};
  const now=Math.floor(Date.now()/1000),messages=[{seq:1,sender:'user',text:"Well that was already sent over and they checked on it so I don’t think it needs to be remade again. What we needed was classification—what did Casey ask for again @Scratch",kind:'message',created:now},{seq:2,sender:'b0',text:'I’ll check Casey’s request and the updates in #team-general. @Piper, I’ll keep the answer focused on classification.',kind:'handoff',created:now+1}];
  await page.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../ui/app.js'),'utf8')+'\nexport {state,mentionBadge};'}));
  await page.route(origin+'/api/**',async route=>{
   const name=new URL(route.request().url()).pathname.slice(4),send=json=>route.fulfill({json});
   if(name==='/settings')return send({name:'Alex',theme:'light',reduced_motion:true});
   if(name==='/bots')return send(bots);if(name==='/chats')return send([chat,...bots.map(b=>({id:'dm-'+b.id,name:b.name,members:[b.id]}))]);
   if(name==='/runs')return send([]);if(name==='/activity')return send({});
   if(name.startsWith('/chats/'))return send({chat,messages});
   return route.continue();
  });
  await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);await page.goto(origin);
  await page.getByRole('button',{name:'#team-general',exact:true}).first().click();
  await page.locator('.message-row.user .mention-badge').waitFor();
  await page.evaluate(async()=>{const {state,mentionBadge}=await import('/app.js');document.querySelector('#prompt').replaceChildren(mentionBadge(state.bots[0]));});
  for(const theme of ['light','dark']){
   await page.evaluate(async()=>{const {state,mentionBadge}=await import('/app.js');document.querySelector('#prompt').replaceChildren(mentionBadge(state.bots[0]));});
   await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);
   for(const hover of [false,true]){
    if(hover)await page.locator('.message-row.user .mention-badge').hover();else await page.mouse.move(1,1);
    const colors=await page.locator('.mention-badge').evaluateAll(nodes=>nodes.map(n=>{
     const s=getComputedStyle(n),label=getComputedStyle(n.lastElementChild);
     const luminance=color=>color.match(/[\d.]+/g).slice(0,3).map(Number).map(v=>{v/=255;return v<=.04045?v/12.92:((v+.055)/1.055)**2.4;}).reduce((a,v,i)=>a+v*[.2126,.7152,.0722][i],0);
     const a=luminance(label.color),b=luminance(s.backgroundColor);
     return {text:label.color,background:s.backgroundColor,contrast:(Math.max(a,b)+.05)/(Math.min(a,b)+.05)};
    }));
    assert(colors.length>=4,JSON.stringify({theme,hover,colors,badges:await page.locator('.mention-badge').allTextContents()}));assert(colors.every(c=>c.contrast>=4.5),JSON.stringify({theme,hover,colors}));
    assert.equal(colors[0].text,theme==='light'?'rgb(32, 32, 32)':'rgb(237, 237, 237)');
   }
   await page.mouse.move(1,1);await page.screenshot({path:path.join(artifacts,'mention-'+theme+'.png')});
  }
  assert.deepEqual(errors,[]);console.log('PASS: user/assistant/channel/composer mentions meet 4.5:1 contrast in light/dark and hover states');
 }finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
