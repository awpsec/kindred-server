// Deterministic recording/decoder races in both Chromium and WebKit. PCM and
// native replies are fixtures; the composer and dictation state machine are real.
const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');const assert=require('node:assert/strict');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const engine=process.env.WEBKIT?'webkit':'chromium',browser=await({chromium,webkit}[engine]).launch({headless:true});
 try{
  const p=await browser.newPage({viewport:{width:900,height:750}}),errors=[];p.on('pageerror',e=>errors.push(e.message));
  await p.addInitScript(token=>{
   sessionStorage.setItem('kindred-token',token);localStorage.setItem('kindred-dictation-v1',JSON.stringify({enabled:true,model:'local:base'}));
   window.__KINDRED_DESKTOP={platform:'linux'};window.__KINDRED_DICTATION_MODELS=true;
   window.decodes=[];window.tracks=[];window.closeWaiters=[];window.deferDecode=false;window.stallClose=false;window.deferCancel=false;window.cancelWaiters=[];window.cancelCalls=0;
   const state={supported:true,enabled:true,phase:'ready',model:'base',models:[{id:'base',name:'Base',downloaded:true,loaded:true}]};
   window.__TAURI__={core:{invoke:async(command,args)=>{
    if(command==='cancel_dictation'){window.cancelCalls++;if(window.deferCancel)return new Promise(r=>cancelWaiters.push(r));}
    if(command==='transcribe_dictation'){
     const bytes=Uint8Array.from(atob(args.audio),c=>c.charCodeAt(0)),data=new DataView(bytes.buffer);
     const job={seconds:(bytes.length-44)/32000,lastSample:data.getInt16(bytes.length-2,true)};decodes.push(job);
     return window.deferDecode?new Promise((resolve,reject)=>{job.resolve=text=>resolve({text});job.reject=reject;}):{text:'These words are visible while I speak.'};
    }
    return state;
   }}};
   Object.defineProperty(navigator,'mediaDevices',{value:{getUserMedia:async()=>{const track={stopped:false,stop(){this.stopped=true;}};tracks.push(track);return {getTracks:()=>[track]};}}});
   const node=()=>({connect(){},disconnect(){},gain:{value:1}});
   window.AudioContext=class{constructor(){this.state='running';this.sampleRate=16000;this.destination={};}async resume(){}async close(){this.state='closed';if(window.stallClose)await new Promise(r=>closeWaiters.push(r));}createMediaStreamSource(){return node();}createGain(){return node();}createScriptProcessor(){window.processor=node();return window.processor;}};
   window.feed=(seconds,amplitude=.1)=>{for(let at=0;at<seconds*16000;at+=320){const pcm=new Float32Array(Math.min(320,Math.round(seconds*16000-at)));for(let i=0;i<pcm.length;i++)pcm[i]=amplitude*Math.sin((at+i)*2*Math.PI*220/16000);processor.onaudioprocess?.({inputBuffer:{getChannelData:()=>pcm}});}};
  },token);
  await p.goto(origin);await p.locator('#app').waitFor({state:'visible'});await p.waitForFunction(()=>document.querySelector('#heading').textContent==='Piper');
  const mic=p.getByRole('button',{name:'Dictate',exact:true}),stop=p.getByRole('button',{name:'Stop dictating',exact:true});
  await mic.click();await stop.waitFor();await p.evaluate(()=>feed(2,0));await p.waitForTimeout(1800);assert.equal(await p.evaluate(()=>decodes.length),0,'Silence should not start inference');
  await p.evaluate(()=>feed(2));await p.waitForFunction(()=>document.querySelector('#prompt').value.includes('visible while I speak'));assert(await stop.isEnabled(),'Words must appear before Stop');
  const firstCount=await p.evaluate(()=>decodes.length);await p.evaluate(()=>feed(20,0));await p.waitForTimeout(1800);assert.equal(await p.evaluate(()=>decodes.length),firstCount,'A pause must not repeatedly decode the same speech');
  assert(await p.evaluate(()=>{window.stallClose=true;document.querySelector('.dictation-button').click();return document.querySelector('.dictation-button').getAttribute('aria-label')==='Dictate'&&!document.querySelector('#send').disabled;}),'Finished text must become sendable in the same click despite a stalled AudioContext close');
  assert.equal(await p.locator('.dictation-status').textContent(),'');assert.equal(await p.evaluate(()=>decodes.length),firstCount,'Stop after a pause must reuse the visible transcript');assert(await p.evaluate(()=>tracks.every(t=>t.stopped)));
  assert.equal(await p.locator('#prompt .dictation-transcript').count(),0);assert.equal(await p.locator('#prompt').textContent(),'These words are visible while I speak.');
  await p.evaluate(()=>{closeWaiters.splice(0).forEach(r=>r());window.stallClose=false;window.deferDecode=true;});
  // An unfinished decode must not hold Send, even if native cancellation and
  // WebAudio shutdown do not answer. Exercise the actual form submission gate.
  await p.evaluate(()=>{
   window.submissions=[];document.querySelector('#composer').addEventListener('submit',event=>{
    event.preventDefault();event.stopImmediatePropagation();const editor=document.querySelector('#prompt');submissions.push(editor.value);editor.value='';editor.dispatchEvent(new Event('input',{bubbles:true}));
   },true);
  });
  async function nextDecode(seconds=2,amplitude=.1){
   const before=await p.evaluate(()=>decodes.length);await p.evaluate(({seconds,amplitude})=>feed(seconds,amplitude),{seconds,amplitude});await p.waitForFunction(n=>decodes.length===n+1,before);
  }
  for(const platform of ['linux','macos','windows']){
   await p.evaluate(platform=>{window.__KINDRED_DESKTOP.platform=platform;},platform);
   await mic.click();await stop.waitFor();await nextDecode();
   await p.evaluate(()=>decodes.at(-1).resolve('The visible words.'));
   await p.waitForFunction(()=>document.querySelector('#prompt .dictation-transcript')?.textContent.includes('The visible words.'));
   const before=await p.evaluate(()=>decodes.length);
   await p.evaluate(()=>{feed(1,.001);feed(20,0);});await p.waitForFunction(n=>decodes.length===n+1,before);
   const seconds=await p.evaluate(()=>decodes.at(-1).seconds);assert(seconds>=3&&seconds<=3.31,'Live decode retains quiet speech while trimming the silent tail');
   const committed=await p.locator('#prompt').textContent();
   const stopped=await p.evaluate(()=>{
    window.stallClose=true;window.deferCancel=true;const requests=decodes.length,cancels=cancelCalls;
    document.querySelector('.dictation-button').click();
    return {sendDisabled:document.querySelector('#send').disabled,sendHidden:document.querySelector('#send').hidden,status:document.querySelector('.dictation-status').textContent,statusHidden:document.querySelector('.dictation-status').hidden,spans:document.querySelectorAll('.dictation-transcript').length,tracksStopped:tracks.every(t=>t.stopped),extraDecodes:decodes.length-requests,cancels:cancelCalls-cancels};
   });
   assert.deepEqual(stopped,{sendDisabled:false,sendHidden:false,status:'',statusHidden:true,spans:0,tracksStopped:true,extraDecodes:0,cancels:1},platform+': Stop must release the composer in the same event turn');
   await p.locator('#send').click();assert.equal(await p.evaluate(()=>submissions.at(-1)),committed,platform+': Send must work before decoder/cleanup replies');
   await p.locator('#prompt').fill('My next typed draft.');
   await p.evaluate(()=>decodes.at(-1).resolve('Late output must not touch the sent message or next draft.'));await p.waitForTimeout(100);
   assert.equal(await p.locator('#prompt').textContent(),'My next typed draft.');assert.equal(await p.locator('.dictation-status').textContent(),'');
   await p.evaluate(()=>{closeWaiters.splice(0).forEach(r=>r());cancelWaiters.splice(0).forEach(r=>r());window.stallClose=false;window.deferCancel=false;});
  }
  // Stop before the first preview, restart immediately, then receive an old
  // decode error. Neither the new recording nor its existing draft may change.
  const draft=await p.locator('#prompt').textContent();await mic.click();await stop.waitFor();await nextDecode();await stop.click();assert(await mic.isEnabled());
  await mic.click();await stop.waitFor();await p.evaluate(()=>decodes.at(-1).reject(new Error('Old stopped decode failed')));await p.waitForTimeout(100);
  assert.equal(await p.locator('#prompt').textContent(),draft);assert(await stop.isEnabled());assert.equal(await p.locator('#notice.error:not([hidden])').count(),0);
  await p.getByRole('button',{name:'Cancel dictation',exact:true}).click();assert.equal(await p.locator('#prompt').textContent(),draft);
  // Explicit Cancel still discards only the current live preview.
  await mic.click();await stop.waitFor();await nextDecode();await p.evaluate(()=>decodes.at(-1).resolve('Discard this preview.'));
  await p.waitForFunction(()=>document.querySelector('.dictation-transcript')?.textContent.includes('Discard this preview.'));
  await p.getByRole('button',{name:'Cancel dictation',exact:true}).click();assert.equal(await p.locator('#prompt').textContent(),draft);
  // Long live text stays in the scrollable composer at narrow widths.
  await p.setViewportSize({width:390,height:750});await mic.click();await stop.waitFor();await nextDecode();
  await p.evaluate(()=>decodes.at(-1).resolve('A long spoken sentence with several words. '.repeat(30)));
  await p.waitForFunction(()=>document.querySelector('#prompt').scrollTop>0);assert(await stop.isEnabled());
  const visible=await p.locator('#prompt').evaluate(editor=>{const range=document.createRange();range.selectNodeContents(editor.querySelector('.dictation-transcript').firstChild);range.collapse(false);return range.getBoundingClientRect().bottom<=editor.getBoundingClientRect().bottom+2;});assert(visible,'Newest dictated words must remain visible');
  await stop.click();await mic.waitFor();assert.equal(await p.locator('.dictation-status').textContent(),'');
  assert.equal(await p.locator('.message-row.user').count(),1,'Dictation must never send automatically');assert.deepEqual(errors,[]);
  console.log(JSON.stringify({passed:true,engine,liveBeforeStop:true,silenceSkipped:true,stopReusesPreview:true,quietLiveSpeechRetained:true,stopSynchronous:true,sendWhileCleanupPending:true,platforms:['linux','macos','windows'],lateReplyDiscarded:true,cleanupDoesNotBlock:true,longPreviewVisible:true}));
 }finally{await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
