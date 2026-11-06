use super::*;
use axum::{Router,routing::any};
use std::sync::{Arc,Mutex};
struct Fixture {new_mail:Mutex<bool>,requests:Mutex<Vec<String>>}
async fn google(State(f):State<Arc<Fixture>>,request:axum::extract::Request)->Json<Value>{
 let path=request.uri().path().to_owned();f.requests.lock().unwrap().push(path.clone());
 if path=="/token/" {return Json(json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600,"scope":SCOPE}));}
 assert_eq!(request.headers()["authorization"],"Bearer fixture-access");
 Json(match path.as_str(){
 "/api/profile"=>json!({"emailAddress":"work@example.test","historyId":"100"}),
 "/api/history"=>if *f.new_mail.lock().unwrap(){json!({"historyId":"101","history":[{"messagesAdded":[{"message":{"id":"abc123","threadId":"thread1","labelIds":["INBOX"]}}]}]})}else{json!({"historyId":"100"})},
 "/api/messages/abc123"=>json!({"id":"abc123","threadId":"thread1","payload":{"mimeType":"text/plain","headers":[{"name":"From","value":"colleague@example.test"}],"body":{"data":"UGxlYXNlIHJldmlldyB0aGlzLg"}}}),
 _=>panic!("Unexpected Gmail request: {path}")})
}
#[tokio::test]
async fn oauth_monitoring_and_bound_message_read_work_for_every_bot_provider_without_empty_model_turns(){
 for provider in ["claude-code","codex","openrouter"]{
  let root=std::env::temp_dir().join(format!("kindred-gmail-{}",db::id()));std::fs::create_dir_all(&root).unwrap();
  let mut app=crate::tests::app();let path=root.join("fixture.db");
  {let app=Arc::get_mut(&mut app).unwrap();app.config.database=path.to_string_lossy().into();app.db=Db::open(&app.config.database).unwrap();}
  let f=Arc::new(Fixture{new_mail:Mutex::new(false),requests:Mutex::new(vec![])});
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  app.db.save_setting("gmail_test_base",&json!(format!("http://{}",listener.local_addr().unwrap()))).unwrap();
  let server=tokio::spawn(axum::serve(listener,Router::new().fallback(any(google)).with_state(f.clone())).into_future());
  let started=start_inner(&app,Start{name:"Work".into(),client_id:"fixture.apps.googleusercontent.com".into(),client_secret:"fixture-secret".into(),public_url:"https://kindred.example.test".into()}).await.unwrap();
  let url=reqwest::Url::parse(started["url"].as_str().unwrap()).unwrap();
  let query=url.query_pairs().into_owned().collect::<std::collections::HashMap<_,_>>();assert_eq!(query["scope"],SCOPE);assert_eq!(query["code_challenge_method"],"S256");assert!(!started.to_string().contains("fixture-secret"));
  let state=query["state"].clone();complete(&app,Callback{state:state.clone(),code:"fixture-code".into(),error:String::new()}).await.unwrap();
  assert!(!owns_state(&app,&state));assert!(complete(&app,Callback{state,code:"fixture-code".into(),error:String::new()}).await.is_err());
  let a=accounts(&app.db).unwrap().remove(0);assert_eq!(a.email,"work@example.test");assert!(!serde_json::to_string(&a).unwrap().contains("fixture-access"));
  let bot=crate::tests::bot(&app.db,provider);
  let input=serde_json::from_value(json!({"bot_id":bot.id,"account_id":a.id,"name":"Work inbox","instructions":"Only alert for requests."})).unwrap();
  let saved=crate::mail_watch::save_watch(&app,input).await.unwrap();assert_ne!(saved["status"],"error");
  crate::mail_watch::queue_pending(&app.db).unwrap();assert!(app.db.runs(None).unwrap().is_empty());
  let id=saved["id"].as_str().unwrap();crate::mail_watch::control(&app,None,id,"run_now").await.unwrap();crate::mail_watch::queue_pending(&app.db).unwrap();assert!(app.db.runs(None).unwrap().is_empty());
  *f.new_mail.lock().unwrap()=true;
  for _ in 0..2 {crate::mail_watch::control(&app,None,id,"run_now").await.unwrap();crate::mail_watch::queue_pending(&app.db).unwrap();}
  assert_eq!(app.db.runs(None).unwrap().len(),1,"Duplicate event must not wake another model turn");
  let run=app.db.claim_bot(&bot.id).unwrap().unwrap();let read=crate::mail_watch::read_message(&app,&run,"abc123").await.unwrap();assert!(read.to_string().contains("Please review this."));
  let count=f.requests.lock().unwrap().len();assert!(crate::mail_watch::read_message(&app,&run,"foreign").await.is_err());assert_eq!(count,f.requests.lock().unwrap().len());
  assert!(disconnect_inner(&app,&a.id).await.is_err());crate::mail_watch::control(&app,None,id,"pause").await.unwrap();disconnect_inner(&app,&a.id).await.unwrap();assert!(secret(&app,&a.id).is_err());
  assert_eq!(f.requests.lock().unwrap().iter().filter(|p|p.as_str()=="/token/").count(),1,"Access token is reused during its lifetime");
  app.db.finish(&run.id,"completed","","").unwrap();
  let setup_id=app.db.queue(&bot.id,"Reconnect Gmail monitoring",0).unwrap();let setup_run=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(setup_run.id,setup_id);
  let setup=crate::gmail_onboarding::setup(&app,&setup_run,&json!({"topic_key":"reconnect","account_id":a.id})).unwrap();
  let setup:Value=serde_json::from_str(setup["text"].as_str().unwrap()).unwrap();let question=setup["question_id"].as_str().unwrap().to_owned();
  app.db.finish(&setup_run.id,"completed","","").unwrap();
  let restarted=start_inner(&app,Start{name:"Work reconnected".into(),client_id:"fixture.apps.googleusercontent.com".into(),client_secret:"fixture-secret".into(),public_url:"https://kindred.example.test".into()}).await.unwrap();
  let url=reqwest::Url::parse(restarted["url"].as_str().unwrap()).unwrap();let state=url.query_pairs().find(|(k,_)|k=="state").unwrap().1.into_owned();
  complete(&app,Callback{state,code:"reconnect-code".into(),error:String::new()}).await.unwrap();assert_eq!(accounts(&app.db).unwrap()[0].id,a.id);
  assert!(!crate::mail_watch::watches(&app.db).unwrap()[0].input.enabled,"Reconnect must preserve paused monitoring");
  let wrong=crate::gmail_onboarding::complete(State(app.clone()),Path(question.clone()),Json(serde_json::from_value(json!({"account_id":"foreign"})).unwrap())).await;assert!(wrong.is_err());
  let mut continuation=None;
  for _ in 0..2 {
   let answer=crate::gmail_onboarding::complete(State(app.clone()),Path(question.clone()),Json(serde_json::from_value(json!({"account_id":a.id})).unwrap())).await.ok().unwrap().0;
   if let Some(expected)=&continuation {assert_eq!(expected,&answer["continuation_run_id"]);}else{continuation=Some(answer["continuation_run_id"].clone());}
  }
  assert!(continuation.unwrap().is_string());assert_eq!(accounts(&app.db).unwrap().len(),1);
  #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(super::path(&app)).unwrap().permissions().mode() & 0o777,0o600);}
  server.abort();let _=server.await;drop(app);std::fs::remove_dir_all(root).unwrap();
 }
}
