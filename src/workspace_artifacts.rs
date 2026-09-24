//! Private, persistent documents; no per-artifact process or public content route.
use crate::{db::{self,Db,Run},runtime::Shared};
use anyhow::{Result,ensure,Context};
use rusqlite::{Connection,OptionalExtension,params};
use serde_json::{Value,json};
use axum::{Json,extract::{State,Path}};
const IDLE:i64=14*24*60*60;
pub fn migrate(c:&Connection)->Result<()> {
 c.execute_batch("CREATE INDEX IF NOT EXISTS chat_workspace_artifact_events ON chat_messages(body,seq) WHERE kind='workspace_artifact';")?;
 c.execute_batch("CREATE TABLE IF NOT EXISTS workspace_artifacts(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id),bot_id TEXT NOT NULL REFERENCES bots(id),artifact_key TEXT NOT NULL,body TEXT NOT NULL,revision INTEGER NOT NULL,updated INTEGER NOT NULL,archived INTEGER NOT NULL DEFAULT 0,UNIQUE(chat_id,artifact_key)); CREATE TABLE IF NOT EXISTS workspace_artifact_versions(artifact_id TEXT NOT NULL REFERENCES workspace_artifacts(id) ON DELETE CASCADE,revision INTEGER NOT NULL,body TEXT NOT NULL,updated INTEGER NOT NULL,PRIMARY KEY(artifact_id,revision));")?;
 c.execute_batch("CREATE TABLE IF NOT EXISTS workspace_artifact_folders(name TEXT PRIMARY KEY,position INTEGER NOT NULL);")?;
 let mut query=c.prepare("SELECT json_extract(body,'$.folder') FROM workspace_artifacts WHERE COALESCE(json_extract(body,'$.folder'),'')!='' GROUP BY json_extract(body,'$.folder') ORDER BY MAX(updated) DESC,1")?;
 let names=query.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
 for name in names { register_folder(c,&name)?; }
 Ok(())
}
fn register_folder(c:&Connection,name:&str)->Result<()> {
 if !name.is_empty(){c.execute("INSERT OR IGNORE INTO workspace_artifact_folders(name,position) VALUES(?,(SELECT COALESCE(MAX(position),-1)+1 FROM workspace_artifact_folders))",[name])?;}Ok(())
}
fn folders(c:&Connection)->Result<Value>{
 let mut q=c.prepare("SELECT name FROM workspace_artifact_folders ORDER BY position,name")?;
 Ok(json!(q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?))
}
fn validate(v:&Value)->Result<()> {
 ensure!(v["title"].as_str().is_some_and(|s|!s.trim().is_empty()&&s.len()<=200),"Title must contain 1–200 bytes");
 ensure!(matches!(v["language"].as_str(),Some("markdown"|"html"|"jsx")),"Use markdown, html or jsx");
 ensure!(v["source"].as_str().is_some_and(|s|s.len()<=64000),"Source must be text up to 64 KB");
 ensure!(v.get("kind").is_none()||matches!(v["kind"].as_str(),Some("document"|"slides"|"sheet"|"app")),"Use document, slides, sheet or app");
 ensure!(v.get("folder").is_none()||v["folder"].as_str().is_some_and(|s|s.len()<=120&&!s.chars().any(char::is_control)),"Folder must be text up to 120 bytes");
 ensure!(v["state"].to_string().len()<=32000,"Shared state exceeds 32 KB");Ok(())
}
fn record(c:&Connection,id:&str)->Result<Value> {
 let (body,chat,bot,revision,updated,archived):(String,String,String,i64,i64,bool)=c.query_row("SELECT body,chat_id,bot_id,revision,updated,archived FROM workspace_artifacts WHERE id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).context("Artifact not found in this workspace")?;
 let mut v:Value=serde_json::from_str(&body)?;
 let created:Option<i64>=c.query_row("SELECT MIN(updated) FROM workspace_artifact_versions WHERE artifact_id=?",[id],|r|r.get(0))?;
 v["created"]=json!(created);
 v["creator_name"]=match v["created_by"].as_str(){Some("user")=>json!("Workspace user"),Some("bot")=>json!(c.query_row("SELECT name FROM bots WHERE id=?",[&bot],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||"Archived bot".into())),_=>json!("Not recorded")};
 for(k,x)in [("id",json!(id)),("chat_id",json!(chat)),("bot_id",json!(bot)),("revision",json!(revision)),("updated",json!(updated)),("archived",json!(archived||updated<=db::now()-IDLE)),("path",json!(format!("/artifacts/{id}")))]{v[k]=x;}Ok(v)
}
fn writable(c:&Connection,chat:&str)->Result<()> {
 ensure!(!crate::workspace_transfer::frozen(c)?,"Workspace transfer is in progress");
 let archived:bool=c.query_row("SELECT archived FROM chats WHERE id=?",[chat],|r|r.get(0))?;ensure!(!archived,"Restore the conversation before editing");Ok(())
}
fn publish(c:&Connection,v:&Value)->Result<()> {
 let id=v["id"].as_str().unwrap();
 // Keep the original creation card and publish subsequent edits as events.
 c.execute("UPDATE chat_messages SET suppressed=0 WHERE seq=(SELECT MIN(seq) FROM chat_messages WHERE kind='workspace_artifact' AND body=?)",[id])?;
 c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'workspace_artifact','',?)",params![v["chat_id"].as_str(),v["bot_id"].as_str(),id,db::now()])?;Ok(())
}
impl Db {
 pub fn workspace_artifact_folders(&self)->Result<Value>{folders(&self.0.lock().unwrap())}
 pub fn workspace_artifact_folder_create(&self,name:&str)->Result<Value>{
  let name=name.trim();ensure!(!name.is_empty()&&name.len()<=120&&!name.chars().any(char::is_control),"Folder name must contain 1–120 bytes");
  ensure!(!name.eq_ignore_ascii_case("All"),"Choose a folder name other than All");
  let mut c=self.0.lock().unwrap();let tx=c.transaction()?;ensure!(!crate::workspace_transfer::frozen(&tx)?,"Workspace transfer is in progress");
  ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM workspace_artifact_folders WHERE name=?)",[name],|r|r.get::<_,bool>(0))?,"A folder with this name already exists");
  register_folder(&tx,name)?;let result=folders(&tx)?;tx.commit()?;Ok(result)
 }
 pub fn workspace_artifact_folder_move(&self,name:&str,before:Option<&str>)->Result<Value>{
  let mut c=self.0.lock().unwrap();let tx=c.transaction()?;ensure!(!crate::workspace_transfer::frozen(&tx)?,"Workspace transfer is in progress");
  let mut names:Vec<String>=serde_json::from_value(folders(&tx)?)?;
  ensure!(names.iter().any(|n|n==name),"Folder not found");
  ensure!(before.is_none_or(|b|names.iter().any(|n|n==b)),"Destination folder not found");
  if before!=Some(name){names.retain(|n|n!=name);let index=before.and_then(|b|names.iter().position(|n|n==b)).unwrap_or(names.len());names.insert(index,name.to_owned());
   for (i,n) in names.iter().enumerate(){tx.execute("UPDATE workspace_artifact_folders SET position=? WHERE name=?",params![i as i64,n])?;}}
  tx.commit()?;Ok(json!(names))
 }

 pub fn workspace_artifact_read(&self,id:&str)->Result<Value>{record(&self.0.lock().unwrap(),id)}
 pub fn workspace_artifact_list(&self)->Result<Value>{
  let c=self.0.lock().unwrap();let cutoff=db::now()-IDLE;
  let mut query=c.prepare("SELECT id,chat_id,bot_id,revision,updated,archived,json_extract(body,'$.title'),json_extract(body,'$.language'),json_extract(body,'$.kind'),json_extract(body,'$.folder') FROM workspace_artifacts ORDER BY updated DESC,id")?;
  let values=query.query_map([],|r|{
   let id:String=r.get(0)?;let language:String=r.get(7)?;let updated:i64=r.get(4)?;let archived:bool=r.get(5)?;let kind:Option<String>=r.get(8)?;
   Ok(json!({"id":id,"chat_id":r.get::<_,String>(1)?,"bot_id":r.get::<_,String>(2)?,"revision":r.get::<_,i64>(3)?,"updated":updated,"archived":archived||updated<=cutoff,"title":r.get::<_,String>(6)?,"kind":kind.unwrap_or_else(||if language=="markdown"{"document"}else{"app"}.into()),"language":language,"folder":r.get::<_,Option<String>>(9)?.unwrap_or_default(),"path":format!("/artifacts/{id}")}))
  })?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(json!(values))
 }
 pub fn workspace_artifact_create(&self,run:&Run,args:&Value)->Result<Value>{self.workspace_artifact_create_in_chat(&run.chat_id,&run.bot_id,args)}
 pub fn workspace_artifact_create_in_chat(&self,chat:&str,bot:&str,args:&Value)->Result<Value>{self.workspace_artifact_create_with_actor(chat,bot,args,"bot")}
 pub fn workspace_artifact_create_with_actor(&self,chat:&str,bot:&str,args:&Value,actor:&str)->Result<Value>{
  let key=args["key"].as_str().context("A stable artifact key is required")?;ensure!(!key.is_empty()&&key.len()<=100,"Invalid artifact key");
  let mut c=self.0.lock().unwrap();let tx=c.transaction()?;writable(&tx,chat)?;
  if let Some(id)=tx.query_row("SELECT id FROM workspace_artifacts WHERE chat_id=? AND artifact_key=?",params![chat,key],|r|r.get::<_,String>(0)).optional()?{return record(&tx,&id);}
  let body=json!({"created_by":actor,"folder":args.get("folder").cloned().unwrap_or(json!("")),"title":args["title"],"language":args["language"],"source":args["source"],"state":args.get("state").cloned().unwrap_or(json!({})),"kind":args.get("kind").cloned().unwrap_or(json!(if args["language"]=="markdown" {"document"}else{"app"}))});validate(&body)?;register_folder(&tx,body["folder"].as_str().unwrap_or(""))?;let id=db::id();let now=db::now();
  tx.execute("INSERT INTO workspace_artifacts VALUES(?,?,?,?,?,1,?,0)",params![id,chat,bot,key,body.to_string(),now])?;
  tx.execute("INSERT INTO workspace_artifact_versions VALUES(?,1,?,?)",params![id,body.to_string(),now])?;
  let v=record(&tx,&id)?;publish(&tx,&v)?;tx.commit()?;Ok(v)
 }
 pub fn workspace_artifact_update(&self,id:&str,patch:&Value)->Result<Value>{
  let mut c=self.0.lock().unwrap();let tx=c.transaction()?;let mut v=record(&tx,id)?;writable(&tx,v["chat_id"].as_str().unwrap())?;
  let revision=v["revision"].as_i64().unwrap();ensure!(patch["expected_revision"].as_i64()==Some(revision),"Artifact changed. Reload and merge your edits before saving");
  ensure!(v["archived"]!=true||patch["reopen"]==true,"Artifact is archived. Reopen it before editing");
  for k in ["title","language","source","state","kind","folder"]{if let Some(x)=patch.get(k){v[k]=x.clone();}}validate(&v)?;register_folder(&tx,v["folder"].as_str().unwrap_or(""))?;
  let body=json!({"created_by":v.get("created_by").cloned().unwrap_or(Value::Null),"folder":v.get("folder").cloned().unwrap_or(json!("")),"title":v["title"],"language":v["language"],"source":v["source"],"state":v["state"],"kind":v.get("kind").cloned().unwrap_or(json!(if v["language"]=="markdown" {"document"}else{"app"}))});let now=db::now();
  tx.execute("UPDATE workspace_artifacts SET body=?,revision=revision+1,updated=?,archived=? WHERE id=?",params![body.to_string(),now,patch["archive"]==true,id])?;
  tx.execute("INSERT INTO workspace_artifact_versions VALUES(?,?,?,?)",params![id,revision+1,body.to_string(),now])?;
  let v=record(&tx,id)?;publish(&tx,&v)?;tx.commit()?;Ok(v)
 }
}
pub async fn folder_list(State(app):State<Shared>)->Result<Json<Value>,crate::web::Error>{Ok(Json(app.db.workspace_artifact_folders()?))}
pub async fn folder_create(State(app):State<Shared>,Json(args):Json<Value>)->Result<Json<Value>,crate::web::Error>{Ok(Json(app.db.workspace_artifact_folder_create(args["name"].as_str().context("Folder name is required")?)?))}
pub async fn folder_move(State(app):State<Shared>,Json(args):Json<Value>)->Result<Json<Value>,crate::web::Error>{
 if !args.get("before").is_some_and(|v|v.is_null()||v.is_string()){return Err(anyhow::anyhow!("Destination must be a folder name or null").into());}
 Ok(Json(app.db.workspace_artifact_folder_move(args["name"].as_str().context("Folder name is required")?,args["before"].as_str())?))
}
pub async fn list(State(app):State<Shared>)->Result<Json<Value>,crate::web::Error>{Ok(Json(app.db.workspace_artifact_list()?))}
pub async fn create(State(app):State<Shared>,Json(args):Json<Value>)->Result<Json<Value>,crate::web::Error>{
 let chat=app.db.chat(args["chat_id"].as_str().context("Choose a conversation for this artifact")?)?;
 let bot=chat.members.first().context("This conversation has no bot")?;
 Ok(Json(app.db.workspace_artifact_create_with_actor(&chat.id,bot,&args,"user")?))
}
pub async fn get(State(app):State<Shared>,Path(id):Path<String>)->Result<Json<Value>,crate::web::Error>{Ok(Json(app.db.workspace_artifact_read(&id)?))}
pub async fn update(State(app):State<Shared>,Path(id):Path<String>,Json(patch):Json<Value>)->Result<Json<Value>,crate::web::Error>{Ok(Json(app.db.workspace_artifact_update(&id,&patch)?))}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn folders_survive_reopen_migrate_and_reorder_without_losing_new_folders(){
  let dir=std::env::temp_dir().join(format!("kindred-folder-test-{}",db::id()));std::fs::create_dir_all(&dir).unwrap();let path=dir.join("workspace.db");let db=Db::open(path.to_str().unwrap()).unwrap();
  assert_eq!(db.workspace_artifact_folder_create(" Empty ").unwrap(),json!(["Empty"]));
  assert!(db.workspace_artifact_folder_create("Empty").is_err());assert!(db.workspace_artifact_folder_create(" ").is_err());assert!(db.workspace_artifact_folder_create("Bad\nName").is_err());
  db.workspace_artifact_folder_create("Reports").unwrap();db.workspace_artifact_folder_create("New on another device").unwrap();
  assert_eq!(db.workspace_artifact_folder_move("Reports",Some("Empty")).unwrap(),json!(["Reports","Empty","New on another device"]));
  assert!(db.workspace_artifact_folder_move("Reports",Some("Missing")).is_err());
  assert_eq!(db.workspace_artifact_folder_move("Reports",None).unwrap(),json!(["Empty","New on another device","Reports"]));
  let bot=crate::tests::bot(&db,"codex");let rid=db.queue(&bot.id,"report",0).unwrap();let run=db.run(&rid).unwrap();
  let artifact=db.workspace_artifact_create(&run,&json!({"key":"doc","title":"Doc","language":"markdown","source":"Hi","folder":"Bot folder"})).unwrap();
  db.workspace_artifact_update(artifact["id"].as_str().unwrap(),&json!({"expected_revision":1,"folder":"Reports"})).unwrap();
  let expected=json!(["Empty","New on another device","Reports","Bot folder"]);assert_eq!(db.workspace_artifact_folders().unwrap(),expected);
  drop(db);let db=Db::open(path.to_str().unwrap()).unwrap();assert_eq!(db.workspace_artifact_folders().unwrap(),expected);
  db.0.lock().unwrap().execute("DELETE FROM workspace_artifact_folders WHERE name='Reports'",[]).unwrap();drop(db);
  let db=Db::open(path.to_str().unwrap()).unwrap();assert_eq!(db.workspace_artifact_folders().unwrap(),json!(["Empty","New on another device","Bot folder","Reports"]));
  assert_eq!(Db::open(":memory:").unwrap().workspace_artifact_folders().unwrap(),json!([]));drop(db);std::fs::remove_dir_all(dir).unwrap();
 }
 #[test]fn persistence_conflicts_expiry_and_workspace_isolation(){
  let db=Db::open(":memory:").unwrap();let bot=crate::tests::bot(&db,"codex");let rid=db.queue(&bot.id,"create brief",0).unwrap();let run=db.run(&rid).unwrap();
  let args=json!({"key":"brief","title":"Daily brief","language":"html","source":"<h1>Brief</h1>","state":{"done":false}});
  let a=db.workspace_artifact_create(&run,&args).unwrap();let id=a["id"].as_str().unwrap();assert_eq!(db.workspace_artifact_create(&run,&args).unwrap()["id"],a["id"]);
  let b=db.workspace_artifact_update(id,&json!({"expected_revision":1,"state":{"done":true}})).unwrap();assert_eq!(b["path"],a["path"]);assert_eq!(b["state"]["done"],true);
  assert!(db.workspace_artifact_update(id,&json!({"expected_revision":1,"source":"stale"})).is_err());
  db.0.lock().unwrap().execute("UPDATE workspace_artifacts SET updated=? WHERE id=?",params![db::now()-IDLE,id]).unwrap();assert_eq!(db.workspace_artifact_read(id).unwrap()["archived"],true);
  assert!(db.workspace_artifact_update(id,&json!({"expected_revision":2,"source":"oops"})).is_err());
  let reopened=db.workspace_artifact_update(id,&json!({"expected_revision":2,"reopen":true})).unwrap();assert_eq!(reopened["archived"],false);assert_eq!(reopened["source"],a["source"]);
  assert!(Db::open(":memory:").unwrap().workspace_artifact_read(id).is_err());
  let messages=db.chat_messages(&run.chat_id).unwrap();let events:Vec<_>=messages.iter().filter(|m|m["kind"]=="workspace_artifact").collect();
  assert_eq!(events.len(),3);assert_eq!(events[0]["artifact_action"],"created");assert!(events[1..].iter().all(|m|m["artifact_action"]=="updated"));
 }
 #[tokio::test]
 async fn folder_and_export_keep_latest_human_edits(){
  use base64::Engine;use std::io::{Cursor,Read};
  let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let rid=app.db.queue(&bot.id,"report",0).unwrap();let run=app.db.run(&rid).unwrap();
  let a=app.db.workspace_artifact_create(&run,&json!({"key":"report","title":"Report","folder":"Client Reports","language":"markdown","source":"# Original"})).unwrap();let id=a["id"].as_str().unwrap();
  app.db.workspace_artifact_update(id,&json!({"expected_revision":1,"source":"# Human correction"})).unwrap();assert_eq!(app.db.workspace_artifact_list().unwrap()[0]["folder"],"Client Reports");
  let result=crate::runtime::call_tool(&app,&bot,&run,"artifact_export",json!({"id":id,"include_content":true})).await.unwrap();let exported:Value=serde_json::from_str(result["text"].as_str().unwrap()).unwrap();assert_eq!(exported["revision"],2);assert!(exported["file"].is_object());
  let bytes=base64::engine::general_purpose::STANDARD.decode(exported["data_base64"].as_str().unwrap()).unwrap();let mut archive=zip::ZipArchive::new(Cursor::new(bytes)).unwrap();let mut xml=String::new();archive.by_name("word/document.xml").unwrap().read_to_string(&mut xml).unwrap();assert!(xml.contains("Human correction"));assert!(!xml.contains("Original"));
  app.db.workspace_artifact_update(id,&json!({"expected_revision":2,"folder":"Reviewed"})).unwrap();assert_eq!(app.db.workspace_artifact_list().unwrap()[0]["folder"],"Reviewed");
 }
 #[tokio::test]
 async fn private_routes_require_workspace_auth_and_transfer_preserves_content(){
  use std::future::IntoFuture;
  let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let rid=app.db.queue(&bot.id,"brief",0).unwrap();let run=app.db.run(&rid).unwrap();
  let a=app.db.workspace_artifact_create(&run,&json!({"key":"brief","title":"Brief","language":"markdown","source":"Private brief"})).unwrap();let id=a["id"].as_str().unwrap();
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/api/workspace-artifacts/{id}",listener.local_addr().unwrap());
  let server=tokio::spawn(axum::serve(listener,crate::web::router(app.clone())).into_future());let client=reqwest::Client::new();
  assert_eq!(client.get(&url).send().await.unwrap().status(),401);
  assert_eq!(client.get(format!("{url}/export")).send().await.unwrap().status(),401);
  let exported=client.get(format!("{url}/export")).bearer_auth(&app.token).send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(exported["filename"],"Brief.docx");
  assert_eq!(client.patch(&url).json(&json!({"expected_revision":1,"source":"bad"})).send().await.unwrap().status(),401);
  assert_eq!(client.get(&url).bearer_auth(&app.token).send().await.unwrap().json::<Value>().await.unwrap()["source"],"Private brief");
  let library=url.trim_end_matches(&format!("/{id}"));
  assert_eq!(client.get(library).send().await.unwrap().status(),401);
  let draft=json!({"chat_id":run.chat_id,"key":"human-sheet","title":"Project sheet","kind":"sheet","language":"html","source":"<table></table>","state":{"rows":[["Original"]]}});
  assert_eq!(client.post(library).json(&draft).send().await.unwrap().status(),401);
  let created=client.post(library).bearer_auth(&app.token).json(&draft).send().await.unwrap().json::<Value>().await.unwrap();
  assert_eq!(created["creator_name"],"Workspace user");assert!(created["created"].as_i64().is_some());assert_eq!(created["kind"],"sheet");let human_id=created["id"].as_str().unwrap();
  let saved=client.patch(format!("{library}/{human_id}")).bearer_auth(&app.token).json(&json!({"expected_revision":1,"folder":"Reports","state":{"rows":[["Human edit"]]}})).send().await.unwrap();assert!(saved.status().is_success());
  let latest=app.db.workspace_artifact_read(human_id).unwrap();assert_eq!(latest["state"]["rows"][0][0],"Human edit");
  let bot_edit=app.db.workspace_artifact_update(human_id,&json!({"expected_revision":latest["revision"],"source":"<h1>Improved layout</h1><table></table>"})).unwrap();assert_eq!(bot_edit["state"],latest["state"]);assert_eq!(bot_edit["path"],created["path"]);
  let index=client.get(library).bearer_auth(&app.token).send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(index.as_array().unwrap().len(),2);assert!(index.as_array().unwrap().iter().all(|v|v.get("source").is_none()&&v.get("state").is_none()));
  for route in ["/artifacts".to_owned(),created["path"].as_str().unwrap().to_owned()] {let page=client.get(format!("http://{}{}",url.split('/').nth(2).unwrap(),route)).send().await.unwrap();assert!(page.status().is_success());assert!(page.text().await.unwrap().contains("/app.js"));}
  let folder_url=library.replace("workspace-artifacts","workspace-artifact-folders");
  assert_eq!(client.get(&folder_url).send().await.unwrap().status(),401);
  assert_eq!(client.post(&folder_url).json(&json!({"name":"Empty"})).send().await.unwrap().status(),401);
  assert_eq!(client.patch(&folder_url).json(&json!({"name":"Reports","before":null})).send().await.unwrap().status(),401);
  let names=client.post(&folder_url).bearer_auth(&app.token).json(&json!({"name":"Empty"})).send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(names,json!(["Reports","Empty"]));
  let names=client.patch(&folder_url).bearer_auth(&app.token).json(&json!({"name":"Empty","before":"Reports"})).send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(names,json!(["Empty","Reports"]));
  server.abort();
  app.db.finish(&run.id,"completed","Brief saved","").unwrap();
  let package=app.db.prepare_transfer(&db::id(),"Test workspace").unwrap();let restored=Db::open(":memory:").unwrap();restored.import_transfer(&package).unwrap();assert_eq!(restored.workspace_artifact_read(id).unwrap()["source"],"Private brief");
  assert_eq!(restored.workspace_artifact_folders().unwrap(),json!(["Empty","Reports"]));assert!(app.db.workspace_artifact_folder_create("While frozen").is_err());
  let mut old=package.clone();old["tables"].as_object_mut().unwrap().remove("workspace_artifact_folders");let restored_old=Db::open(":memory:").unwrap();restored_old.import_transfer(&old).unwrap();assert_eq!(restored_old.workspace_artifact_folders().unwrap(),json!(["Reports"]));
 }

 #[tokio::test(flavor="multi_thread",worker_threads=4)]
 async fn two_bots_merge_repeated_conflicts_without_losing_human_edits(){
  let app=crate::tests::app();let author=crate::tests::bot(&app.db,"codex");let reviewer=crate::tests::bot(&app.db,"claude-code");
  let first=app.db.queue(&author.id,"write shared report",0).unwrap();let second=app.db.queue(&reviewer.id,"review shared report",0).unwrap();
  let author_run=app.db.run(&first).unwrap();let reviewer_run=app.db.run(&second).unwrap();
  let initial=app.db.workspace_artifact_create(&author_run,&json!({"key":"joint-report","title":"Joint report","folder":"Client Reports","language":"markdown","source":"# Human approved introduction\n","state":{"approved_by_human":true}})).unwrap();
  let id=initial["id"].as_str().unwrap().to_owned();
  for round in 0..16 {
   let baseline=app.db.workspace_artifact_read(&id).unwrap();let gate=std::sync::Arc::new(tokio::sync::Barrier::new(2));let mut tasks=tokio::task::JoinSet::new();
   for (actor,run,label) in [(author.clone(),author_run.clone(),"author"),(reviewer.clone(),reviewer_run.clone(),"reviewer")] {
    let app=app.clone();let gate=gate.clone();let id=id.clone();let baseline=baseline.clone();
    tasks.spawn(async move {
     let addition=format!("{label} contribution {round}\n");gate.wait().await;
     let result=crate::runtime::call_tool(&app,&actor,&run,"artifact_update",json!({"id":id,"expected_revision":baseline["revision"],"source":format!("{}{}",baseline["source"].as_str().unwrap(),addition)})).await.unwrap();
     (actor,run,addition,result)
    });
   }
   let mut accepted=0;let mut rejected=None;
   while let Some(result)=tasks.join_next().await {let(actor,run,addition,value)=result.unwrap();if value["failed"]==true{rejected=Some((actor,run,addition));}else{accepted+=1;}}
   assert_eq!(accepted,1,"exactly one stale-baseline writer can commit");let(actor,run,addition)=rejected.unwrap();
   let read=crate::runtime::call_tool(&app,&actor,&run,"artifact_read",json!({"id":id})).await.unwrap();let read:Value=serde_json::from_str(read["text"].as_str().unwrap()).unwrap();
   let merged=crate::runtime::call_tool(&app,&actor,&run,"artifact_update",json!({"id":id,"expected_revision":read["revision"],"source":format!("{}{}",read["source"].as_str().unwrap(),addition)})).await.unwrap();assert_ne!(merged["failed"],true);
  }
  let latest=app.db.workspace_artifact_read(&id).unwrap();assert_eq!(latest["revision"],33);assert_eq!(latest["state"],initial["state"]);assert_eq!(latest["folder"],"Client Reports");assert_eq!(latest["path"],initial["path"]);
  let source=latest["source"].as_str().unwrap();assert!(source.starts_with("# Human approved introduction\n"));for round in 0..16{for label in ["author","reviewer"]{assert_eq!(source.matches(&format!("{label} contribution {round}\n")).count(),1);}}
  assert_eq!(app.db.chat_messages(&author_run.chat_id).unwrap().iter().filter(|m|m["kind"]=="workspace_artifact"&&m["artifact_action"]=="created").count(),1);
  assert_eq!(app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM workspace_artifact_versions WHERE artifact_id=?",[&id],|r|r.get::<_,i64>(0)).unwrap(),33);
 }

 #[tokio::test(flavor="multi_thread",worker_threads=4)]
 async fn stress_http_and_model_tools_preserve_revisions_and_content(){
  use std::future::IntoFuture;
  let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let rid=app.db.queue(&bot.id,"artifact stress",0).unwrap();let run=app.db.run(&rid).unwrap();
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let base=format!("http://{}/api/workspace-artifacts",listener.local_addr().unwrap());
  let server=tokio::spawn(axum::serve(listener,crate::web::router(app.clone())).into_future());let client=reqwest::Client::new();
  // Exercise the exact tool dispatcher used by models, across all supported kinds.
  let mut ids=Vec::new();
  for i in 0..128 {
   let (kind,language,source)=match i%4{0=>("document","markdown","# Brief"),1=>("sheet","html","<table><tr><td>Data</td></tr></table>"),2=>("slides","html","<section>Slide</section>"),_=>("app","jsx","export default function App(){return <h1>App</h1>}")};
   let args=json!({"key":format!("stress-{i}"),"title":format!("Artifact {i}"),"kind":kind,"language":language,"source":source});
   let result=crate::runtime::call_tool(&app,&bot,&run,"artifact_create",args.clone()).await.unwrap();
   let value:Value=serde_json::from_str(result["text"].as_str().unwrap()).unwrap();let id=value["id"].as_str().unwrap().to_owned();
   assert_eq!(value["kind"],kind);assert_eq!(app.db.workspace_artifact_create(&run,&args).unwrap()["id"],id);ids.push(id);
  }
  let index=client.get(&base).bearer_auth(&app.token).send().await.unwrap().json::<Value>().await.unwrap();
  assert_eq!(index.as_array().unwrap().len(),128);assert!(index.as_array().unwrap().iter().all(|v|v.get("source").is_none()));
  let url=format!("{base}/{}",ids[0]);let mut requests=tokio::task::JoinSet::new();
  // All writers begin at the same revision: exactly one may commit.
  for i in 0..32 {let c=client.clone();let u=url.clone();let token=app.token.clone();requests.spawn(async move{c.patch(u).bearer_auth(token).json(&json!({"expected_revision":1,"state":{"writer":i}})).send().await.unwrap().status().is_success()});}
  let mut won=0;while let Some(result)=requests.join_next().await{won+=usize::from(result.unwrap());}assert_eq!(won,1);
  let read=crate::runtime::call_tool(&app,&bot,&run,"artifact_read",json!({"id":ids[0]})).await.unwrap();let read:Value=serde_json::from_str(read["text"].as_str().unwrap()).unwrap();assert_eq!(read["revision"],2);
  crate::runtime::call_tool(&app,&bot,&run,"artifact_update",json!({"id":ids[0],"expected_revision":2,"source":"# Updated by bot"})).await.unwrap();
  let after=client.get(&url).bearer_auth(&app.token).send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(after["state"],read["state"]);assert_eq!(after["revision"],3);
  // Invalid and unauthorized writes must not create revisions or destroy content.
  for patch in [json!({"expected_revision":3,"source":"x".repeat(64001)}),json!({"expected_revision":3,"state":{"large":"x".repeat(32001)}}),json!({"expected_revision":3,"kind":"unknown"}),json!({"source":"missing revision"})]{
   assert!(!client.patch(&url).bearer_auth(&app.token).json(&patch).send().await.unwrap().status().is_success());
  }
  assert_eq!(client.get(&url).bearer_auth("wrong-token").send().await.unwrap().status(),401);
  assert_eq!(app.db.workspace_artifact_read(&ids[0]).unwrap(),after);
  let tool_list=crate::runtime::call_tool(&app,&bot,&run,"artifact_list",json!({})).await.unwrap();let tool_list:Value=serde_json::from_str(tool_list["text"].as_str().unwrap()).unwrap();assert_eq!(tool_list.as_array().unwrap().len(),128);
  assert_eq!(app.db.chat_messages(&run.chat_id).unwrap().iter().filter(|m|m["kind"]=="workspace_artifact"&&m["workspace_artifact"]["id"]==ids[0]&&m["artifact_action"]=="created").count(),1);
  // A different bot can discover, review and edit the same workspace artifact.
  let reviewer=crate::tests::bot(&app.db,"claude-code");let review_id=app.db.queue(&reviewer.id,"review existing artifact",0).unwrap();let review_run=app.db.run(&review_id).unwrap();
  for provider in ["claude-code","codex","openrouter"] {let mut actor=reviewer.clone();actor.provider=provider.into();let specs=crate::runtime::tool_specs_for(&app,&actor);for name in ["artifact_list","artifact_read","artifact_create","artifact_update","artifact_export"]{assert!(specs.iter().any(|v|v["name"]==name));}}
  let review=crate::runtime::call_tool(&app,&reviewer,&review_run,"artifact_read",json!({"id":ids[0]})).await.unwrap();
  let review:Value=serde_json::from_str(review["text"].as_str().unwrap()).unwrap();for field in ["id","source","state","revision","path"]{assert_eq!(review[field],after[field]);}assert_eq!(review["url"],format!("{}{}",app.config.public_url,after["path"].as_str().unwrap()));
  let stale=crate::runtime::call_tool(&app,&reviewer,&review_run,"artifact_update",json!({"id":ids[0],"expected_revision":1,"source":"stale reviewer"})).await.unwrap();assert_eq!(stale["failed"],true);assert_eq!(app.db.workspace_artifact_read(&ids[0]).unwrap(),after);
  let amended=crate::runtime::call_tool(&app,&reviewer,&review_run,"artifact_update",json!({"id":ids[0],"expected_revision":3,"source":"# Reviewed and corrected"})).await.unwrap();
  assert_ne!(amended["failed"],true);let amended:Value=serde_json::from_str(amended["text"].as_str().unwrap()).unwrap();assert_eq!(amended["state"],after["state"]);assert_eq!(amended["path"],after["path"]);
  let original_view=crate::runtime::call_tool(&app,&bot,&run,"artifact_read",json!({"id":ids[0]})).await.unwrap();assert_eq!(serde_json::from_str::<Value>(original_view["text"].as_str().unwrap()).unwrap(),amended);
  assert_eq!(app.db.chat_messages(&run.chat_id).unwrap().iter().filter(|m|m["kind"]=="workspace_artifact"&&m["workspace_artifact"]["id"]==ids[0]&&m["artifact_action"]=="created").count(),1);
  server.abort();
 }

}

pub async fn export(State(app):State<Shared>,Path(id):Path<String>)->Result<Json<Value>,crate::web::Error>{Ok(Json(crate::artifact_export::export(&app.db.workspace_artifact_read(&id)?)?))}
