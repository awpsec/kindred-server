const {chromium,webkit}=require(process.env.KINDRED_PLAYWRIGHT_MODULE||'playwright');
const {server}=require('./fixtures/desktop.cjs');
const assert=require('node:assert/strict');
const fs=require('node:fs');
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
 const browser=await(process.env.WEBKIT?webkit:chromium).launch();
 try{
  const page=await browser.newPage({viewport:{width:1200,height:850}});const calls=[];
  let users=[{id:'owner',username:'Admin',admin:true,profile_count:2},...Array.from({length:18},(_,i)=>({id:'member-'+i,username:'Member '+(i+1),admin:false,profile_count:2}))];
  await page.route('**/identity/**',async route=>{const path=new URL(route.request().url()).pathname,body=route.request().method()==='POST'?route.request().postDataJSON():null;calls.push({path,body});
   if(path.endsWith('/meta'))return route.fulfill({json:{profiles:true,registration:false}});
   if(path.endsWith('/profiles'))return route.fulfill({json:{active:'personal',account_id:'owner',username:'Admin',admin:true,profiles:[{id:'personal',name:'Personal',active:true},{id:'work',name:'Work',active:false}]}});
   if(path.endsWith('/computer-settings'))return route.fulfill({json:{supported:true,provisioned:true,state:'shut off',resources:{cpus:4,memory_mb:8192,disk_gb:75}}});
   if(path.endsWith('/admin/remove')){users=users.filter(u=>u.id!==body.user_id);return route.fulfill({json:{removed:true}});}
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
  assert.equal(await page.locator('.server-user-row').count(),20);
  const size=await page.locator('.server-admin-dialog').boundingBox();assert(size.width>=850&&size.height>=600);
  assert(await page.locator('.server-user-table').evaluate(e=>e.scrollHeight>e.clientHeight));
  await page.getByRole('button',{name:'Create invitation link'}).click();assert.equal(await page.getByLabel('One-use invitation link').inputValue(),origin+'/#invite=fixture-one-use-token');
  await page.getByRole('button',{name:'Computers',exact:true}).click();await page.getByText('Bot computer resources',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Users',exact:true}).click();
  await page.getByRole('button',{name:'Remove Member 1',exact:true}).click();
  await page.getByRole('button',{name:'Cancel',exact:true}).click();assert(!calls.some(c=>c.path.endsWith('/admin/remove')));
  await page.getByRole('button',{name:'Remove Member 1',exact:true}).click();await page.getByRole('button',{name:'Remove user',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.server-user-row').length===19);assert.deepEqual(calls.find(c=>c.path.endsWith('/admin/remove')).body,{user_id:'member-0',confirm:'Member 1'});
  if(process.env.PREVIEW_DIR){fs.mkdirSync(process.env.PREVIEW_DIR,{recursive:true});for(const theme of ['dark','light']){await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);await page.locator('.server-admin-dialog').screenshot({path:process.env.PREVIEW_DIR+'/server-admin-'+theme+'.png'});}}
  await page.setViewportSize({width:390,height:844});assert(await page.locator('.server-admin-dialog').evaluate(e=>e.scrollWidth<=e.clientWidth+1));
  await page.goto(origin+'/admin-fixture#invite=fixture-one-use-token');
  await page.evaluate(async()=>{const {createProfileUI}=await import('/profiles.js');window.ui=createProfileUI({getToken:()=>'',setToken:()=>{},connect:async()=>{},notice:()=>{}});await ui.init();});
  await page.getByRole('button',{name:'Create account',exact:true}).waitFor();
  assert.equal(await page.getByLabel('Invitation code').inputValue(),'fixture-one-use-token');assert.equal(new URL(page.url()).hash,'');
  console.log('PASS administration layout, scrollable users, profile switch entry, invitation link, confirmation and removal');
 }finally{await browser.close();server.close();}
})().catch(e=>{console.error(e);server.close();process.exitCode=1;});
