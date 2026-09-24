const {server,token}=require('./fixtures/desktop.cjs');
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const browser=await(process.env.WEBKIT?webkit:chromium).launch({headless:true});
 try{
  const page=await browser.newPage({viewport:{width:1100,height:850}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.route(origin+'/app.js',r=>r.fulfill({contentType:'text/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../ui/app.js'),'utf8')+'\nexport {state,renderSidebar,updateReactions};'}));
  await page.addInitScript(t=>sessionStorage.setItem('kindred-token',t),token);await page.goto(origin);await page.waitForSelector('#bots .bot-link');
  const result=await page.evaluate(async()=>{
   const {state,renderSidebar,updateReactions}=await import('/app.js');const template=state.bots[0],now=Date.now()/1000;
   state.bots=['Piper','Atlas','Scratch','Hazel'].map((name,i)=>({...template,id:'quiet-'+i,name,profile:{...template.profile,pinned:i===3}}));
   state.chats=state.bots.map(b=>({id:'dm-'+b.id,name:b.name,members:[b.id],last_message:{text:'Previous reply',created:now-100}}));
   state.allRuns=state.bots.map(b=>({id:'run-'+b.id,bot_id:b.id,chat_id:'group-team',status:'running',activity_started:false,output:'',prompt:'Should be in a better place now',error:'',created:now}));
   state.activities=Object.fromEntries(state.bots.map(b=>[b.id,{run_id:'run-'+b.id,status:'running',shape:'think',started_at:now}]));
   const sample=()=>{renderSidebar();updateReactions();return state.bots.map(b=>{const row=document.querySelector('[aria-label="'+b.name+'"].bot-link')||document.querySelector('[aria-label="'+b.name+'"].pinned-bot');return {label:row.querySelector('.sidebar-activity-label')?.textContent||'',action:row.querySelector('.character')._character.action};});};
   const quiet=sample();state.allRuns[0].activity_started=true;const participating=sample();
   state.allRuns[1].status='awaiting_approval';state.allRuns[2].status='awaiting_user';const waiting=sample();
   state.allRuns[0].chat_id='dm-quiet-0';state.allRuns[0].activity_started=false;const dm=sample();
   state.allRuns[0].chat_id='group-team';delete state.allRuns[0].activity_started;const legacy=sample();
   state.allRuns[0].activity_started=false;state.activities['quiet-0'].commands=1;const command=sample();
   state.allRuns[1].status='queued';state.allRuns[2].status='running';delete state.activities['quiet-0'].commands;const restored=sample();
   return {quiet,participating,waiting,dm,legacy,command,restored};
  });
  assert(result.quiet.every(r=>!r.label&&!['working','think'].includes(r.action)),'All four routing runs remain visually quiet, including the pinned bot');
  assert.equal(result.participating[0].label,'working');assert.equal(result.participating[0].action,'working');assert(result.participating.slice(1).every(r=>!r.label));
  assert.equal(result.waiting[1].label,'awaiting approval');assert.equal(result.waiting[2].label,'waiting for you');
  assert.equal(result.dm[0].label,'working');assert.equal(result.legacy[0].label,'working');assert.equal(result.command[0].label,'1 command running');
  assert(result.restored.every(r=>!r.label));assert.deepEqual(errors,[]);
  console.log('PASS: quiet group routing in sidebar text/avatars/pins; participating bot, approvals, human waits, commands, DMs and older servers remain visible');
 }finally{await browser.close();server.closeAllConnections();server.close();}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
