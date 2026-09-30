const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict');
const fs=require('node:fs');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const browser=await(process.env.WEBKIT?webkit:chromium).launch();if(process.env.PREVIEW_DIR)fs.mkdirSync(process.env.PREVIEW_DIR,{recursive:true});
 try{
  const page=await browser.newPage({viewport:{width:1200,height:850}});const calls=[];let resetState='pending';
  let users=[{id:'owner',username:'Admin',admin:true,profile_count:2},...Array.from({length:18},(_,i)=>({id:'member-'+i,username:'Member '+(i+1),admin:false,profile_count:2}))];
  await page.route('**/identity/**',async route=>{const path=new URL(route.request().url()).pathname,body=route.request().method()==='POST'?route.request().postDataJSON():null;calls.push({path,body});
   if(path.endsWith('/admin/password-resets')){if(body)resetState=body.action==='approve'?'approved':'denied';return route.fulfill({json:{requests:[{id:'a1b2c3d4-fixture',username:'Member 2',created:1790798400,ip:'192.0.2.10',location:'Boston, Massachusetts, United States',state:resetState}]}});}
   if(path.endsWith('/password-reset/request'))return route.fulfill({json:{token:'private-browser-proof',id:'a1b2c3d4-fixture',state:'pending'}});
   if(path.endsWith('/password-reset/status'))return route.fulfill({json:{state:resetState}});
   if(path.endsWith('/password-reset/finish'))return route.fulfill({json:{saved:true}});
   if(path.endsWith('/meta'))return route.fulfill({json:{profiles:true,registration:false}});
   if(path.endsWith('/profiles'))return route.fulfill({json:{active:'personal',account_id:'owner',username:'Admin',admin:true,profiles:[{id:'personal',name:'Personal',active:true},{id:'work',name:'Work',active:false}]}});
   if(path.endsWith('/computer-settings'))return route.fulfill({json:{supported:true,provisioned:true,state:'shut off',resources:{cpus:4,memory_mb:8192,disk_gb:75}}});
   if(path.endsWith('/admin/remove')){users=users.filter(u=>u.id!==body.user_id);return route.fulfill({json:{removed:true}});}
   if(path.endsWith('/admin')&&body?.action==='disable')users.find(u=>u.id===body.user_id).disabled=body.disabled;
   if(path.endsWith('/admin'))return route.fulfill({json:body?.action==='invite'?{invite:'fixture-one-use-token'}:{users,max_users:128,registration:false}});
   return route.fulfill({json:{}});
  });
  await page.route('**/admin-fixture',r=>r.fulfill({contentType:'text/html',body:'<!doctype html><html data-theme="dark"><head><link rel="stylesheet" href="/style.css"></head><body><div id="identity-row"></div><input id="remember-device" type="checkbox"><form id="connect-form"><h1>Connect</h1></form></body></html>'}));
  await page.goto(origin+'/admin-fixture');
  await page.evaluate(async()=>{const {createProfileUI}=await import('/profiles.js');window.ui=createProfileUI({getToken:()=> 'fixture-token',setToken:()=>{},connect:async()=>{},beforeSwitch:async()=>{},notice:(m)=>{throw Error(m)}});await ui.init();});
  await page.getByRole('button',{name:'Switch accounts',exact:true}).click();
  await page.getByRole('menuitem',{name:'Switch to Work',exact:true}).waitFor();
  await page.getByRole('menuitem',{name:'Server administration',exact:true}).click();
  await page.getByRole('table',{name:'Server users'}).waitFor();
  assert.equal(await page.locator('.server-user-row').count(),19);
  const size=await page.locator('.server-admin-dialog').boundingBox();assert(size.width>=850&&size.height>=600);
  assert(await page.locator('.server-user-table').evaluate(e=>e.scrollHeight>e.clientHeight));
  await page.getByRole('button',{name:'Create invitation link'}).click();await page.waitForFunction(()=>document.querySelector('.profile-invite')?.value);assert.equal(await page.getByLabel('One-use invitation link').inputValue(),origin+'/#invite=fixture-one-use-token');
  await page.getByRole('button',{name:'Computers',exact:true}).click();await page.getByText('Bot computer resources',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Users',exact:true}).click();
  await page.locator('.server-user-row').filter({hasText:'Member 1'}).first().hover();
  await page.getByRole('button',{name:'Disable Member 1',exact:true}).click();
  if(process.env.PREVIEW_DIR)await page.locator('dialog').last().screenshot({path:process.env.PREVIEW_DIR+'/disable-user-confirmation.png'});
  await page.getByRole('button',{name:'Cancel',exact:true}).click();assert(!calls.some(c=>c.body?.action==='disable'));
  await page.locator('.server-user-row').filter({hasText:'Member 1'}).first().hover();
  await page.getByRole('button',{name:'Disable Member 1',exact:true}).click();await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await page.getByRole('button',{name:'Enable Member 1',exact:true}).waitFor();assert.deepEqual(calls.find(c=>c.body?.action==='disable').body,{action:'disable',user_id:'member-0',disabled:true});
  assert.match(await page.locator('.server-user-row').last().innerText(),/Member 1\b/);
  assert(await page.locator('.server-user-row').last().evaluate(row=>row.classList.contains('is-disabled') && Number(getComputedStyle(row.querySelector('.server-user-copy')).opacity)<.5));
  assert.equal(await page.getByRole('columnheader').count(),0);assert(await page.locator('.server-user-row').evaluateAll(rows=>rows.every(r=>r.getBoundingClientRect().height<=42)));
  if(process.env.PREVIEW_DIR){fs.mkdirSync(process.env.PREVIEW_DIR,{recursive:true});for(const theme of ['dark','light']){await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);for(const tab of ['Users','Password resets','Computers','Updates']){await page.locator('.settings-nav').getByRole('button',{name:tab,exact:true}).click();if(tab==='Users')await page.locator('.server-user-row').filter({hasText:'Member 2'}).hover();await page.locator('.server-admin-dialog').screenshot({path:process.env.PREVIEW_DIR+'/server-admin-'+tab.toLowerCase().replaceAll(' ','-')+'-'+theme+'.png'});}}}
  await page.locator('.settings-nav').getByRole('button',{name:'Password resets',exact:true}).click();await page.getByRole('button',{name:'Approve',exact:true}).click();await page.getByText('Approved · awaiting new password').waitFor();assert.equal(resetState,'approved');
  await page.setViewportSize({width:390,height:844});assert(await page.locator('.server-admin-dialog').evaluate(e=>e.scrollWidth<=e.clientWidth+1));
  await page.goto(origin+'/admin-fixture#invite=fixture-one-use-token');
  await page.evaluate(async()=>{const {createProfileUI}=await import('/profiles.js');window.ui=createProfileUI({getToken:()=>'',setToken:()=>{},connect:async()=>{},notice:()=>{}});await ui.init();});
  await page.getByRole('button',{name:'Create account',exact:true}).waitFor();
  assert.equal(await page.getByLabel('Invitation code').inputValue(),'fixture-one-use-token');assert.equal(new URL(page.url()).hash,'');
  await page.goto(origin+'/admin-fixture');resetState='pending';
  await page.evaluate(async()=>{const {createProfileUI}=await import('/profiles.js');window.ui=createProfileUI({getToken:()=>'',setToken:()=>{},connect:async()=>{},notice:()=>{}});await ui.init();});
  await page.getByRole('button',{name:'Forgot password?',exact:true}).click();await page.locator('dialog').getByLabel('Username').fill('Member 2');await page.getByRole('button',{name:'Request password reset',exact:true}).click();await page.getByText('Waiting for administrator approval').waitFor();
  if(process.env.PREVIEW_DIR)await page.locator('dialog').screenshot({path:process.env.PREVIEW_DIR+'/password-reset-waiting.png'});
  resetState='approved';await page.getByLabel('New password',{exact:true}).waitFor();
  if(process.env.PREVIEW_DIR)await page.locator('dialog').screenshot({path:process.env.PREVIEW_DIR+'/password-reset-approved.png'});
  await page.getByLabel('New password',{exact:true}).fill('new test password');await page.getByLabel('Confirm password',{exact:true}).fill('wrong password');await page.getByRole('button',{name:'Save password',exact:true}).click();await page.getByText('Passwords do not match.',{exact:true}).waitFor();assert(!calls.some(c=>c.path.endsWith('/password-reset/finish')));
  await page.getByLabel('Confirm password',{exact:true}).fill('new test password');await page.getByRole('button',{name:'Save password',exact:true}).click();await page.getByText('Password saved. Sign in with your new password.').waitFor();assert.equal(await page.evaluate(()=>sessionStorage.getItem('kindred-password-reset')),null);
  console.log('PASS administration layout, scrollable users, profile switch entry, invitation link, confirmation and disabling');
 }finally{await browser.close();server.close();}
})().catch(e=>{console.error(e);server.close();process.exitCode=1;});
