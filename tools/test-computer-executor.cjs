// Actual guest executor on disposable X11 + local headed Chromium. No model/site/account.
// Start ignored guest::browser_tests::local_executor_fixture with ADDRESS and SCREEN.
const assert=require('node:assert/strict'),net=require('node:net'),http=require('node:http');
const fs=require('node:fs'),path=require('node:path'),os=require('node:os');
const {chromium}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const mode=process.env.KINDRED_FIXTURE_MODE||'candidate';
const out=process.env.KINDRED_TEST_ARTIFACTS; fs.mkdirSync(out,{recursive:true});
const [host,port]=process.env.KINDRED_EXECUTOR_FIXTURE_ADDRESS.split(':');
const calls=[];let fixtureBrowser,fixtureProfile;const completionTimer=setTimeout(()=>{console.error('Fixture did not complete');process.exit(1)},115000);
async function wireRpc(tool,args={}) { const start=performance.now();const result=await new Promise((resolve,reject)=>{
 const socket=net.connect(+port,host,()=>socket.write(JSON.stringify({tool,args})+'\n'));let data='';socket.on('data',b=>{data+=b;if(data.includes('\n')){socket.end();resolve(JSON.parse(data));}});socket.on('end',()=>{if(!data){if(tool==='fixture_stop')resolve({stopped:true});else reject(Error('Executor disconnected without receipt'));}});socket.on('error',reject);socket.setTimeout(15000,()=>{socket.destroy();reject(Error('fixture timeout'));});
 });const record={tool,args,ms:performance.now()-start,failed:result.failed===true,bytes:result.image?.length||0,text:result.text};
 if(result.image){const png=Buffer.from(result.image.split(',')[1],'base64');record.dimensions=[png.readUInt32BE(16),png.readUInt32BE(20)];record.png_bytes=png.length;fs.writeFileSync(path.join(out,`${calls.length}-${tool}.png`),png);}
 calls.push(record);return result;
}
// The real Pi SDK transports every production result. A deterministic local
// provider emits the fixture's next tool, never interprets pixels as a model.
let commandWaiter, queued, providerRequests=0, deliveredImages=0, finishProvider;
const command = () => queued ? Promise.resolve(queued).then(v=>{queued=null;return v}) : new Promise(r=>commandWaiter=r);
const enqueue = v => { if(commandWaiter){const resolve=commandWaiter;commandWaiter=null;resolve(v);}else queued=v; };
let sdkWork;
async function startTransport(){
 const {runSession}=await import('../harness/pi/session.mjs');
 const tools=['computer_screenshot','computer_click','computer_type','computer_key','computer_scroll'].map(name=>({name,description:'Disposable executor fixture',inputSchema:{type:'object',properties:{x:{type:'integer'},y:{type:'integer'},button:{type:'integer'},text:{type:'string'},key:{type:'string'},direction:{type:'string'},clicks:{type:'integer'},observe:{type:'boolean'}},required:[],additionalProperties:false}}));
 let current, serial=0;
 sdkWork=runSession({protocol:1,provider:'custom-11111111-1111-4111-8111-111111111111',model:'fixture-model',model_info:{id:'fixture-model',vision:true,context_window:128000,max_tokens:4096},endpoint:'http://127.0.0.1:8000/v1/chat/completions',api_key:'fixture-key',instructions:'Disposable executor transport fixture',prompt:'Select jamier1,12.34,Fixture note; verify confirmation; never repeat uncertain submission.',tools,max_steps:100,reasoning_effort:''},async(id,name,args)=>{
  assert.equal(id,current.id);assert.equal(name,current.tool);assert.deepEqual(args,current.args);
  const result=await wireRpc(name,args);if(result.image){deliveredImages++;current.image=result.image;}current.resolve(result);return result;
 },()=>{},undefined,{fetch:async req=>{
  await new Promise(r=>setTimeout(r,20));
  const body=await req.json();assert.equal(body.model,'fixture-model');providerRequests++;
  if(current){const previous=body.messages.findLast(m=>m.role==='tool');assert.equal(previous.tool_call_id,current.id);if(current.image)assert(JSON.stringify(body.messages).includes(current.image));}
  const next=await command();let delta,finish;
  if(next){current={...next,id:'fixture-'+(++serial)};delta={role:'assistant',tool_calls:[{index:0,id:current.id,type:'function',function:{name:next.tool,arguments:JSON.stringify(next.args)}}]};finish='tool_calls';}
  else {delta={role:'assistant',content:'Fixture complete'};finish='stop';}
  const chunk=d=>({id:'fixture',object:'chat.completion.chunk',created:1,model:'fixture-model',choices:[{index:0,delta:d,finish_reason:null}]});const end=chunk({});end.choices[0].finish_reason=finish;
  return new Response('data: '+JSON.stringify(chunk(delta))+'\n\ndata: '+JSON.stringify(end)+'\n\ndata: [DONE]\n\n',{headers:{'Content-Type':'text/event-stream'}});
 }});
 // Every intermediate tool response resolves its waiting driver. Propagate SDK failures.
 sdkWork.catch(error=>{console.error(error);process.exit(1)});
}
async function rpc(tool,args={}) {
 if(tool==='fixture_stop'){enqueue(null);await sdkWork;return wireRpc(tool,args);}
 return new Promise((resolve,reject)=>enqueue({tool,args,resolve,reject}));
}
const PAGE=`<!doctype html><meta charset=utf-8><title>Disposable request fixture</title><style>
body{font:20px sans-serif;margin:0;background:#f3f5f7}main{position:absolute;left:720px;top:340px;width:650px}button,input{font:20px sans-serif;height:40px;margin:8px}input{width:230px}#choices button{display:block;width:260px}#modal{background:white;border:3px solid #456;padding:15px}#bottom{margin-top:500px}#login{position:absolute;left:850px;top:70px}
</style><button id=login>Sign in (fixture)</button><main><label>Recipient<input id=recipient autocomplete=off></label><div id=choices hidden><button id=wrong>Jamie Rivera · @jamier2</button><button id=right>Jamie Rivera · @jamier1</button></div><p id=selected></p><div id=fields hidden><label>Amount<input id=amount></label><label>Note<input id=note></label><button id=review>Review</button><div id=modal hidden><p id=summary></p><button id=submit>Confirm request</button></div><p id=status></p><div id=bottom><button id=bottomButton>Scrolled target</button></div></div></main><script>
window.receipts=[];window.loggedIn=false;window.ambiguous=false;window.selected='';
login.onclick=()=>{window.loggedIn=true;login.textContent='Signed in';};
recipient.oninput=()=>{setTimeout(()=>choices.hidden=false,180)};
for(const [id,name]of [['wrong','jamier2'],['right','jamier1']])document.getElementById(id).onclick=()=>{window.selected=name;document.getElementById('selected').textContent=name;choices.hidden=true;fields.hidden=false;};
review.onclick=()=>setTimeout(()=>{summary.textContent=window.selected+' | '+amount.value+' | '+note.value;modal.hidden=false;},180);
submit.onclick=()=>{window.receipts.push({recipient:window.selected,amount:amount.value,note:note.value});submit.disabled=true;setTimeout(()=>{if(!window.ambiguous)document.getElementById('status').textContent='Request confirmed R42';},250);};bottomButton.onclick=()=>bottomButton.textContent='Reached';
</script>`;
(async()=>{const server=http.createServer((req,res)=>{res.setHeader('Content-Type','text/html');res.end(PAGE)});await new Promise(r=>server.listen(0,'127.0.0.1',r));
const origin=`http://127.0.0.1:${server.address().port}`;const profile=fs.mkdtempSync(path.join(os.tmpdir(),'kindred-executor-browser-'));
fixtureProfile=profile;const browser=await chromium.launchPersistentContext(profile,{executablePath:process.env.KINDRED_TEST_CHROME,headless:false,viewport:null,args:['--no-sandbox','--kiosk',`--app=${origin}`,'--window-position=0,0','--window-size=1440,900']});fixtureBrowser=browser;const page=browser.pages()[0];await page.goto(origin);await page.waitForFunction(()=>innerWidth>1200&&innerHeight>700);
const geometry=await page.evaluate(()=>({x:screenX+(outerWidth-innerWidth)/2,y:screenY+outerHeight-innerHeight,width:innerWidth,height:innerHeight})); console.log({geometry});
await page.waitForTimeout(200);await startTransport();const workflowStart=performance.now();let fresh=await rpc('computer_screenshot');assert.deepEqual(calls.at(-1).dimensions,mode==='baseline'?[1280,800]:[1440,900]);
async function observe(){return rpc('computer_screenshot');}
async function act(tool,args){const r=await rpc(tool,{...args,observe:mode!=='legacy-observation'});assert(!r.failed,r.text);if(mode!=='candidate')await observe();else assert(r.image,'post-action observation');return r;}
async function click(id){const b=await page.locator('#'+id).boundingBox();assert(b);const scale=mode==='baseline'?8/9:1;await act('computer_click',{x:Math.round((b.x+b.width/2+geometry.x)*scale),y:Math.round((b.y+b.height/2+geometry.y)*scale)});}
await click('recipient');await act('computer_type',{text:'Jamie'});await page.waitForTimeout(220);
if(mode==='baseline'){const result={mode,focused:await page.evaluate(()=>document.activeElement.id),recipient:await page.locator('#recipient').inputValue(),completion:0,incorrect_target:await page.locator('#recipient').inputValue()!=='Jamie'?1:0,duplicate:0,provider_requests:providerRequests,provider_delay_ms:20,elapsed_ms:performance.now()-workflowStart,delivered_images:deliveredImages,calls};fs.writeFileSync(path.join(out,'metrics.json'),JSON.stringify(result,null,2));assert.equal(result.incorrect_target,1);console.log(JSON.stringify(result));}
else {
 assert.equal(await page.locator('#recipient').inputValue(),'Jamie');await observe();await click('right');assert.equal(await page.evaluate(()=>window.selected),'jamier1');
 await click('amount');await act('computer_type',{text:'12.34'});await act('computer_key',{key:'Tab'});await act('computer_type',{text:'Fixture note'});
 await click('review');await page.waitForTimeout(220);await observe();assert.equal(await page.locator('#summary').innerText(),'jamier1 | 12.34 | Fixture note');await click('submit');await page.waitForTimeout(300);await observe();assert.equal(await page.locator('#status').innerText(),'Request confirmed R42');assert.equal(await page.evaluate(()=>receipts.length),1);
 await act('computer_scroll',{direction:'down',clicks:8});await page.waitForTimeout(100);await observe();await click('bottomButton');assert.equal(await page.locator('#bottomButton').innerText(),'Reached');
 // Same profile, new local document: uncertain confirmation never authorizes a second submit.
 await page.goto(origin);await page.evaluate(()=>window.ambiguous=true);await observe();await click('recipient');await act('computer_type',{text:'Jamie'});await page.waitForTimeout(220);await observe();await click('right');await click('amount');await act('computer_type',{text:'12.34'});await act('computer_key',{key:'Tab'});await act('computer_type',{text:'Fixture note'});await click('review');await page.waitForTimeout(220);await observe();await click('submit');await page.waitForTimeout(300);await observe();assert.equal(await page.locator('#status').innerText(),'');assert.equal(await page.evaluate(()=>receipts.length),1);assert(await page.locator('#submit').isDisabled());
 // Human completes fixture login; resumed executor observes before subsequent input.
 await page.goto(origin);await page.locator('#login').click();await observe();assert(await page.evaluate(()=>window.loggedIn));await click('recipient');await act('computer_type',{text:'After login'});assert.equal(await page.locator('#recipient').inputValue(),'After login');await act('computer_key',{key:'ctrl+a'});await act('computer_type',{text:'Replacement'});await act('computer_key',{key:'Return'});assert.equal(await page.locator('#recipient').inputValue(),'Replacement');
 // Native edges are reachable; out-of-range requests fail before input.
 await act('computer_click',{x:1439,y:899});const pointer=require('node:child_process').execFileSync('xdotool',['getmouselocation','--shell'],{encoding:'utf8'});assert(pointer.includes('X=1439')&&pointer.includes('Y=899'));let invalid=await rpc('computer_click',{x:1440,y:899,observe:true});assert(invalid.failed);assert(!invalid.uncertain_effect);
 await page.goto(origin);await observe();fs.writeFileSync(process.env.KINDRED_EXECUTOR_CAPTURE_FAULT,'fixture');const b=await page.locator('#recipient').boundingBox();const missing=await rpc('computer_click',{x:Math.round(b.x+b.width/2+geometry.x),y:Math.round(b.y+b.height/2+geometry.y),observe:true});fs.unlinkSync(process.env.KINDRED_EXECUTOR_CAPTURE_FAULT);assert.equal(missing.action_applied,true);assert.equal(missing.observation_error,true);assert(!missing.failed);assert.equal(await page.evaluate(()=>document.activeElement.id),'recipient');await observe();
 // Simulated process failure AFTER real xdotool typing: receipt must say uncertain, no replay.
 fs.writeFileSync(process.env.KINDRED_EXECUTOR_INPUT_FAULT,'fixture');const partial=await rpc('computer_type',{text:'Once',observe:true});fs.unlinkSync(process.env.KINDRED_EXECUTOR_INPUT_FAULT);assert.equal(partial.failed,true);assert.equal(partial.uncertain_effect,true);assert.equal(await page.locator('#recipient').inputValue(),'Once');await observe();
 const result={mode,completion:1,incorrect_target:0,duplicate:0,ambiguous_submissions:1,login_continued:true,provider_requests:providerRequests,provider_delay_ms:20,elapsed_ms:performance.now()-workflowStart,delivered_images:deliveredImages,calls};fs.writeFileSync(path.join(out,'metrics.json'),JSON.stringify(result,null,2));console.log(JSON.stringify({...result,calls: result.calls.length}));
}
await browser.close();await new Promise(r=>server.close(r));fs.rmSync(profile,{recursive:true,force:true});await rpc('fixture_stop');
const metrics=JSON.parse(fs.readFileSync(path.join(out,'metrics.json')));metrics.provider_requests=providerRequests;metrics.provider_final_included=true;fs.writeFileSync(path.join(out,'metrics.json'),JSON.stringify(metrics,null,2));clearTimeout(completionTimer);
})().catch(async e=>{clearTimeout(completionTimer);console.error(e);if(fixtureBrowser)await fixtureBrowser.close().catch(()=>{});if(fixtureProfile)fs.rmSync(fixtureProfile,{recursive:true,force:true});process.exit(1)});
