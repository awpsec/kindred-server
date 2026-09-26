const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server,token}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
// Pane resizing: sidebar drag, fade and avatar rail; right panes that only grow;
// the minimum chat width; persistence, viewport clamps, keyboard separators,
// shielded remote screens/frames, expanded computer, Mac chrome, phones and the
// pinned artifact library. All server data is a local fixture.
const artifacts=process.env.KINDRED_TEST_ARTIFACTS||path.resolve(__dirname,'../../test-results');fs.mkdirSync(artifacts,{recursive:true});
const engine=process.env.WEBKIT?'webkit':'chromium',MIN_CHAT=420,RAIL=76,MAC_RAIL=96;
const vendor=`export * from './vendor.js?real';
export class RFB extends EventTarget {
 constructor(host){super();this.host=host;this.screen=document.createElement('div');this.screen.style.cssText='display:flex;width:100%;height:100%;align-items:center;justify-content:center';
 this.canvas=document.createElement('canvas');this.canvas.width=1280;this.canvas.height=800;this.screen.append(this.canvas);host.append(this.screen);
 for(const type of ['pointermove','pointerdown','pointerup'])this.canvas.addEventListener(type,()=>window.remoteEvents=(window.remoteEvents||0)+1);
 this.canvas.getContext('2d').fillRect(0,0,1280,800);setTimeout(()=>this.dispatchEvent(new Event('connect')),50);}
 set background(v){} set scaleViewport(v){window.refits=(window.refits||0)+1;const b=this.host.getBoundingClientRect(),s=Math.min(b.width/1280,b.height/800)||.2;this.canvas.style.width=1280*s+'px';this.canvas.style.height=800*s+'px';}
 set viewOnly(v){} set resizeSession(v){}
 disconnect(){this.screen.remove();this.dispatchEvent(new Event('disconnect'));}
}`;
const profile=(color,pinned=false)=>({shape:'round',color,eyes:'curious',label:'Helper',description:'Fixture bot',notifications:true,animated:false,pinned});
const bots=[
 {id:'piper',name:'Piper',provider:'codex',model:'test',reasoning_effort:'high',instructions:'Fixture',memory:'',approval_mode:'auto',profile:profile('#2475ff')},
 {id:'juno',name:'Juno',provider:'codex',model:'test',reasoning_effort:'high',instructions:'Fixture',memory:'',approval_mode:'auto',profile:profile('#e0569b',true)},
 {id:'orla',name:'Orla',provider:'codex',model:'test',reasoning_effort:'high',instructions:'Fixture',memory:'',approval_mode:'auto',profile:profile('#27a567')},
];
const chats=[{id:'dm-piper',name:'Piper',members:['piper'],archived:false},{id:'team',name:'Launch team',members:['piper','orla'],archived:false,pinned:false}];
const frames=p=>p.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));
// Keyboard and double-click changes glide briefly; measure once they settle.
const settle=async p=>{await p.waitForFunction(()=>!document.querySelector('#app').classList.contains('pane-settling'));await frames(p);};
const box=(p,selector)=>p.locator(selector).first().evaluate(n=>{const r=n.getBoundingClientRect(),q=v=>Math.round(v);return {left:q(r.left),right:q(r.right),width:q(r.width),top:q(r.top),bottom:q(r.bottom)};});
const saved=p=>p.evaluate(()=>JSON.parse(localStorage.getItem('kindred-pane-sizes')||'{}'));
async function grab(p,handle){const b=await handle.boundingBox();assert(b&&b.width>=6,'Separator has a usable hit area');const x=b.x+b.width/2,y=b.y+Math.min(b.height/2,300);await p.mouse.move(x,y);await p.mouse.down();return {x,y};}
async function open(browser,{width=1280,height=860,platform=null,overlay=false,storage=null,reduced=false}={}){
 const context=await browser.newContext({viewport:{width,height},reducedMotion:reduced?'reduce':'no-preference'}),p=await context.newPage(),errors=[];
 p.on('pageerror',e=>errors.push(e.message));p.setDefaultTimeout(10000);
 await context.route(origin+'/vendor.js',r=>r.fulfill({body:vendor,contentType:'text/javascript'}));
 await context.route(origin+'/vendor.js?real',r=>r.fulfill({body:fs.readFileSync(path.resolve(__dirname,'../../ui/vendor.js')),contentType:'text/javascript'}));
 await context.route(origin+'/api/computer/session',r=>r.fulfill({json:{ticket:'fixture'}}));
 await context.route(origin+'/api/computer/resources',r=>r.fulfill({status:404,json:{error:'none'}}));
 await context.route(origin+'/api/bots',r=>r.request().method()==='GET'?r.fulfill({json:bots}):r.fallback());
 await context.route(origin+'/api/chats',r=>r.request().method()==='GET'?r.fulfill({json:chats}):r.fallback());
 await context.route(origin+'/api/workspace-artifacts',r=>r.fulfill({json:[]}));
 await context.addInitScript(({token,platform,overlay,storage})=>{
  sessionStorage.setItem('kindred-token',token);
  if(storage&&!sessionStorage.getItem('pane-fixture-seeded')){localStorage.setItem('kindred-pane-sizes',JSON.stringify(storage));sessionStorage.setItem('pane-fixture-seeded','1');}
  if(platform){window.__KINDRED_DESKTOP={platform};window.__KINDRED_MAC_OVERLAY=overlay;window.__TAURI__={core:{invoke:async()=>null}};}
 },{token,platform,overlay,storage});
 await p.goto(origin);await p.locator('#app').waitFor({state:'visible'});await p.locator('#bots .bot-link').first().waitFor({state:'attached'});await frames(p);
 return {context,p,errors};
}
let origin;
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));origin='http://127.0.0.1:'+server.address().port;
 const browser=await(process.env.WEBKIT?webkit:chromium).launch();
 try{
  // 1. Defaults, sidebar drag without lag, detail fade, rail snap and persistence.
  {
   const {context,p,errors}=await open(browser);
   assert.equal((await box(p,'.sidebar')).width,280,'Default sidebar width is unchanged');
   const handle=p.getByRole('separator',{name:'Resize sidebar'});
   assert.equal(await handle.getAttribute('aria-orientation'),'vertical');assert.equal(await handle.getAttribute('aria-controls'),'sidebar');
   const {x,y}=await grab(p,handle);
   // Each frame the chat abuts the sidebar and the sidebar tracks the pointer.
   for(const dx of [12,40,90,150]){
    await p.mouse.move(x+dx,y);await frames(p);
    const [sidebar,chat]=[await box(p,'.sidebar'),await box(p,'.conversation')];
    assert(Math.abs(sidebar.width-(280+dx))<=1,`Sidebar follows the pointer (${sidebar.width} vs ${280+dx})`);
    assert(Math.abs(sidebar.right-chat.left)<=1,'No gap or overlap between sidebar and chat');
    assert.equal(await p.locator('.pane-resize-shield').count(),1,'A shield covers frames during the drag');
   }
   await p.mouse.move(x+1000,y);await frames(p);
   assert.equal((await box(p,'.sidebar')).width,480,'Sidebar stops at its maximum');
   await p.mouse.move(x-110,y);await frames(p);
   const fading=await p.evaluate(()=>({detail:parseFloat(getComputedStyle(document.querySelector('#bots .bot-info')).opacity),width:document.querySelector('.sidebar').getBoundingClientRect().width,fading:document.querySelector('#app').classList.contains('sidebar-fading')}));
   assert(fading.fading&&fading.detail>0&&fading.detail<1,'Text fades gradually below the readable minimum: '+JSON.stringify(fading));
   await p.mouse.move(x-200,y);await frames(p);
   assert.equal((await box(p,'.sidebar')).width,RAIL,'Dragging past the threshold snaps to the rail');
   await p.mouse.up();await frames(p);
   assert.equal(await p.locator('.pane-resize-shield').count(),0);
   assert.equal(await handle.evaluate(n=>n===document.activeElement),false,'A pointer drag leaves no lingering focus line');
   assert.deepEqual(await saved(p),{rail:true},'The rail is saved for this device');
   // Rail presentation: large avatars, no chat cards, SVG-only named controls.
   const rail=await p.evaluate(()=>{const s=document.querySelector('.sidebar');return {
    info:[...s.querySelectorAll('.bot-info')].every(n=>!n.getClientRects().length),
    avatars:[...s.querySelectorAll('#bots .bot-link>.character,.pinned-bot>.character')].map(n=>Math.round(n.getBoundingClientRect().width)),
    stack:Math.round(s.querySelector('#bots .participant-stack').getBoundingClientRect().width),
    icons:[...s.querySelectorAll('.sidebar-bottom .sidebar-settings>svg')].map(n=>Math.round(n.getBoundingClientRect().width)),
    labels:[...s.querySelectorAll('.sidebar-bottom .sidebar-settings>span')].map(n=>Math.round(n.getBoundingClientRect().width)),
    inside:[...s.querySelectorAll('button')].filter(n=>n.getClientRects().length).every(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=s.getBoundingClientRect().right+.5;})};});
   assert(rail.info,'Chat cards collapse to avatars');
   assert(rail.avatars.length>=3&&rail.avatars.every(w=>w===50),'Rail avatars are larger than the 44px row avatars: '+rail.avatars);
   assert.equal(rail.stack,50,'Group chats keep a matching avatar stack');
   assert(rail.icons.length===3&&rail.icons.every(w=>w===22)&&rail.labels.every(w=>w<=1),'Bottom controls are larger icons with hidden text');
   assert(rail.inside,'Rail controls stay within the rail');
   for(const name of ['Artifacts','Marketplace','Settings'])assert(await p.getByRole('button',{name,exact:true}).isVisible(),name+' keeps its accessible name');
   assert(await p.getByRole('button',{name:'Juno'}).first().isVisible(),'Pinned bots stay in the rail');
   await p.getByRole('button',{name:'Orla',exact:true}).hover();await p.locator('.rail-tooltip').waitFor();
   assert.match(await p.locator('.rail-tooltip').textContent(),/^Orla/,'Hover shows the bot name beside the rail');
   const tip=await box(p,'.rail-tooltip');assert(tip.left>=RAIL,'The tooltip sits outside the rail');
   await p.getByRole('button',{name:'Settings',exact:true}).focus();await p.keyboard.press('Shift+Tab');await p.keyboard.press('Tab');
   await p.waitForFunction(()=>document.querySelector('.rail-tooltip')?.textContent==='Settings');
   await p.screenshot({path:path.join(artifacts,`pane-rail-${engine}-dark.png`)});
   await p.evaluate(()=>document.documentElement.dataset.theme='light');await p.mouse.move(640,400);await p.waitForTimeout(300);
   await p.screenshot({path:path.join(artifacts,`pane-rail-${engine}-light.png`)});
   await p.evaluate(()=>document.documentElement.dataset.theme='dark');
   // Search from the rail opens the full sidebar over the chat without moving it.
   const chatBefore=await box(p,'.conversation');
   await p.keyboard.press(process.platform==='darwin'?'Meta+k':'Control+k');await p.waitForFunction(()=>document.querySelector('#app').classList.contains('sidebar-peek'));
   await frames(p);
   assert(Math.abs((await box(p,'.sidebar')).width-280)<=1,'Search peek shows the full sidebar');
   assert.deepEqual(await box(p,'.conversation'),chatBefore,'The chat does not reflow under the search peek');
   await p.keyboard.type('orl');await frames(p);
   assert.equal(await p.locator('#bots .bot-link:visible').count(),1,'Search filters while peeking');
   await p.keyboard.press('Escape');await frames(p);
   assert.equal(await p.evaluate(()=>document.querySelector('#app').classList.contains('sidebar-peek')),false);
   assert.equal(await p.locator('#search').inputValue(),'','Leaving the peek clears the hidden query');
   // Reload keeps the rail; the separator restores a readable sidebar from the keyboard.
   await p.reload();await p.locator('#bots .bot-link').first().waitFor();
   assert.equal((await box(p,'.sidebar')).width,RAIL,'The rail survives reload');
   await handle.focus();
   assert.equal(await handle.getAttribute('aria-valuetext'),'Collapsed to avatars');
   await p.keyboard.press('ArrowRight');await settle(p);
   assert.equal((await box(p,'.sidebar')).width,200,'ArrowRight from the rail restores the readable minimum');
   await p.keyboard.press('Shift+ArrowRight');await settle(p);assert.equal((await box(p,'.sidebar')).width,264);
   await p.keyboard.press('End');await settle(p);assert.equal((await box(p,'.sidebar')).width,480);
   assert.equal(await handle.getAttribute('aria-valuenow'),'480');assert.equal(await handle.getAttribute('aria-valuemax'),'480');
   await p.keyboard.press('Enter');await settle(p);assert.equal((await box(p,'.sidebar')).width,RAIL,'Enter collapses to the rail');
   await p.keyboard.press('Enter');await settle(p);assert.equal((await box(p,'.sidebar')).width,480,'Enter restores the previous width');
   await p.keyboard.press('Home');await settle(p);assert.equal((await box(p,'.sidebar')).width,RAIL,'Home collapses to the rail');
   await handle.dblclick();await settle(p);
   assert.equal((await box(p,'.sidebar')).width,280,'Double-click restores the default');assert.deepEqual(await saved(p),{});
   // Releasing between the rail and the minimum settles on the nearer one.
   let at=await grab(p,handle);await p.mouse.move(at.x-95,at.y);await p.mouse.up();await p.waitForTimeout(260);
   assert.equal((await box(p,'.sidebar')).width,200,'A release just below the minimum settles to it');
   assert.deepEqual(errors,[]);await context.close();
  }
  // 2. Right panes grow but never shrink, keep a usable chat, shield the remote screen.
  {
   const {context,p,errors}=await open(browser,{storage:{sidebar:300}});
   await p.locator('#show-computer').click();await p.locator('#desktop canvas').waitFor();await frames(p);
   const panel='#computer-panel',handle=p.getByRole('separator',{name:'Resize computer panel'});
   assert.equal((await box(p,panel)).width,350,'Computer pane opens at its default width');
   let at=await grab(p,handle);await p.mouse.move(at.x+120,at.y);await frames(p);
   assert.equal((await box(p,panel)).width,350,'The computer pane cannot shrink below its default');
   // A frame over the chat would normally receive these moves.
   await p.evaluate(()=>{const f=document.createElement('iframe');f.id='probe-frame';f.srcdoc='<body style="margin:0;height:100vh" onpointermove="parent.frameMoves=(parent.frameMoves||0)+1"></body>';f.style.cssText='position:fixed;left:320px;top:120px;width:520px;height:500px;border:0;z-index:1';document.body.append(f);window.frameMoves=0;window.remoteEvents=0;});
   await p.locator('#probe-frame').evaluate(f=>new Promise(r=>f.contentDocument?.readyState==='complete'?r():f.addEventListener('load',r,{once:true})));
   for(let i=1;i<=12;i++){await p.mouse.move(at.x-i*40,at.y+(i%3)*8);}
   await frames(p);
   const during=await p.evaluate(()=>({frame:window.frameMoves,remote:window.remoteEvents,canvas:getComputedStyle(document.querySelector('#desktop canvas')).pointerEvents,iframe:getComputedStyle(document.querySelector('#probe-frame')).pointerEvents}));
   assert.deepEqual(during,{frame:0,remote:0,canvas:'none',iframe:'none'},'Frames and the remote screen see nothing during a drag');
   const grown=await box(p,panel),chat=await box(p,'.conversation');
   assert.equal(Math.round(chat.width),MIN_CHAT,'Growth stops at the minimum chat width');
   assert.equal(Math.round(grown.width),1280-300-MIN_CHAT);
   await p.mouse.up();await frames(p);
   await p.mouse.move(500,300);await p.mouse.move(520,320);await p.waitForTimeout(50);
   assert(await p.evaluate(()=>window.frameMoves>0),'The probe frame receives moves again after the drag');
   await p.locator('#probe-frame').evaluate(n=>n.remove());
   assert(await p.evaluate(()=>window.refits>0),'The remote screen refits while resizing');
   assert.equal((await saved(p)).computer,560);
   // Keyboard: ArrowLeft grows (the separator moves left), Home restores the default.
   await handle.focus();await p.keyboard.press('Home');await settle(p);assert.equal((await box(p,panel)).width,350);
   await p.keyboard.press('ArrowLeft');await settle(p);assert.equal((await box(p,panel)).width,366);
   assert.equal(await handle.getAttribute('aria-valuemin'),'350');assert.equal(await handle.getAttribute('aria-valuemax'),'560');
   await p.keyboard.press('ArrowRight');await p.keyboard.press('ArrowRight');await settle(p);assert.equal((await box(p,panel)).width,350,'Keyboard cannot shrink below the default');
   await p.keyboard.press('End');await settle(p);assert.equal((await box(p,panel)).width,560);
   // Escape cancels a drag in progress.
   at=await grab(p,handle);await p.mouse.move(at.x+100,at.y);await frames(p);await p.keyboard.press('Escape');await p.mouse.up();await frames(p);
   assert.equal((await box(p,panel)).width,560,'Escape restores the width from before the drag');
   // Viewport changes clamp the pane first, keep the saved preference, and restore on growth.
   await p.setViewportSize({width:1100,height:860});await frames(p);
   assert.equal(Math.round((await box(p,'.conversation')).width),MIN_CHAT,'A narrower window keeps the chat usable');
   assert.equal(Math.round((await box(p,panel)).width),1100-300-MIN_CHAT);
   await p.setViewportSize({width:950,height:860});await frames(p);
   const small=await box(p,panel);assert(small.width>=300-.5,'The pane never goes below its default at smaller widths');
   assert(Math.round((await box(p,'.sidebar')).width)>=200);
   await p.setViewportSize({width:1280,height:860});await frames(p);
   assert.equal((await box(p,panel)).width,560,'Growing the window restores the saved size');
   // Expanded computer ignores pane widths and follows the resized sidebar.
   await p.locator('#computer-expand').click();await p.waitForFunction(()=>document.querySelector('#computer-panel').classList.contains('expanded'));await p.waitForTimeout(420);
   assert.equal((await box(p,panel)).left,300,'Expanded computer starts at the resized sidebar');
   assert.equal(await handle.isVisible(),false,'Expanded computer has no pane separator');
   const sidebarHandle=p.getByRole('separator',{name:'Resize sidebar'});
   at=await grab(p,sidebarHandle);await p.mouse.move(at.x-60,at.y);await frames(p);
   assert.equal((await box(p,panel)).left,240,'Expanded computer tracks the sidebar during a drag');
   await p.mouse.up();
   await p.locator('#computer-expand').click();await p.waitForTimeout(420);
   assert.equal((await box(p,panel)).width,560,'Collapsing restores the resized compact pane');
   await p.screenshot({path:path.join(artifacts,`pane-computer-${engine}.png`)});
   // Details pane: its own size and default.
   await p.locator('#bot-details').click();await p.locator('#details-panel:not([hidden])').waitFor();await frames(p);
   const details=p.getByRole('separator',{name:'Resize details panel'});
   assert.equal((await box(p,'#details-panel')).width,365,'Details pane opens at its default width');
   await details.focus();await p.keyboard.press('Shift+ArrowLeft');await settle(p);
   assert.equal((await box(p,'#details-panel')).width,429);
   const sizes=await saved(p);assert.equal(sizes.details,429);assert.equal(sizes.computer,560);
   await p.reload();await p.locator('#bots .bot-link').first().waitFor();await p.locator('#bot-details:not([disabled])').click();await p.locator('#details-panel:not([hidden])').waitFor();await frames(p);
   assert.equal((await box(p,'#details-panel')).width,429,'Details width survives reload');
   assert.deepEqual(errors,[]);await context.close();
  }
  // 3. Reduced motion: keyboard changes apply at once with no settling transition.
  {
   const {context,p}=await open(browser,{reduced:true});
   const handle=p.getByRole('separator',{name:'Resize sidebar'});await handle.focus();await p.keyboard.press('Home');
   assert.equal(await p.evaluate(()=>document.querySelector('#app').classList.contains('pane-settling')),false);
   assert.equal((await box(p,'.sidebar')).width,RAIL);
   await p.getByRole('button',{name:'Orla',exact:true}).hover();await p.locator('.rail-tooltip').waitFor();
   assert.equal(await p.locator('.rail-tooltip').evaluate(n=>getComputedStyle(n).animationName),'none');
   await context.close();
  }
  // 4. Mac traffic lights stay clear of the rail controls, overlay or HTML.
  for(const overlay of [false,true]){
   const {context,p,errors}=await open(browser,{platform:'macos',overlay,storage:{rail:true}});
   const sidebar=await box(p,'.sidebar'),add=await box(p,'#new-bot');
   assert.equal(sidebar.width,MAC_RAIL,'The Mac rail leaves room for the traffic lights');
   assert(add.top>=57,'New chat sits below the traffic-light row');
   if(!overlay){const lights=await p.locator('.window-control').evaluateAll(n=>n.map(e=>e.getBoundingClientRect().right));assert(lights.every(r=>r<=MAC_RAIL),'HTML traffic lights fit inside the rail');}
   if(!overlay)await p.screenshot({path:path.join(artifacts,`pane-rail-${engine}-macos.png`)});
   assert.deepEqual(errors,[]);await context.close();
  }
  // 5. Phones keep the drawer and full-width overlays; saved desktop sizes are ignored.
  {
   const {context,p,errors}=await open(browser,{width:700,height:820,storage:{rail:true,computer:600}});
   assert.equal(await p.locator('.pane-resizer:visible').count(),0,'No separators on narrow layouts');
   await p.locator('#mobile-menu').click();assert.equal((await box(p,'.sidebar')).width,270,'The phone drawer keeps its width');
   await p.evaluate(()=>document.querySelector('#app').classList.remove('sidebar-open'));
   await p.locator('#show-computer').click();await frames(p);assert(Math.round((await box(p,'#computer-panel')).width)<=370);
   await p.setViewportSize({width:1280,height:820});await frames(p);
   assert.equal((await box(p,'.sidebar')).width,RAIL,'Wide layouts pick the saved rail back up');
   assert.equal((await box(p,'#computer-panel')).width,600);
   assert.deepEqual(errors,[]);await context.close();
  }
  // 6. The pinned artifact library resizes; the unpinned drawer has no separator.
  {
   const {context,p,errors}=await open(browser);
   await p.evaluate(()=>localStorage.setItem('kindred-artifact-library-pinned','true'));
   await p.locator('#artifacts-button').click();await p.locator('.artifact-studio').waitFor();await frames(p);
   const handle=p.getByRole('separator',{name:'Resize artifact library'});
   assert.equal((await box(p,'.artifact-studio-library')).width,252);
   const at=await grab(p,handle);await p.mouse.move(at.x+80,at.y);await frames(p);
   assert.equal(Math.round((await box(p,'.artifact-studio-library')).width),332,'The pinned library follows the pointer');
   assert(Math.abs((await box(p,'.artifact-studio-library')).right-(await box(p,'.artifact-workbench')).left)<=1);
   await p.mouse.up();assert.equal((await saved(p)).library,332);
   await p.getByRole('button',{name:'Unpin artifact library'}).first().click();await frames(p);
   assert.equal(await handle.isVisible(),false,'The unpinned drawer has no separator');
   assert.deepEqual(errors,[]);await context.close();
  }
  console.log(`Pane resizing (${engine}): sidebar drag/fade/rail, search peek, keyboard separators, right-pane limits, minimum chat, shielding, viewport clamps, expanded computer, reduced motion, Mac chrome, phones and artifact library passed`);
 }finally{await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
