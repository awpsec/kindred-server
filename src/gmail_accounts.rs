//! Read-only Gmail event authorization, independent of the bot's model connector.
use crate::{db::{self,Db},runtime::{App,Shared}};
use anyhow::{Result,Context,ensure};
use axum::{Json,extract::{State,Path,Query},response::{Html,IntoResponse,Response}};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use rusqlite::{Connection,params};
use base64::{Engine,engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest::{digest,SHA256};
use std::{path::PathBuf,time::Duration};
const SCOPE:&str="https://www.googleapis.com/auth/gmail.readonly";
const TOKEN:&str="https://oauth2.googleapis.com/token";
const API:&str="https://gmail.googleapis.com/gmail/v1/users/me/";
#[derive(Clone,Serialize,Deserialize)]
pub struct Account {pub id:String,pub name:String,pub email:String,pub status:String}
#[derive(Serialize,Deserialize)]
struct Secret {client_id:String,client_secret:String,refresh_token:String,access_token:String,expires_at:i64}
#[derive(Serialize,Deserialize)]
struct Pending {name:String,client_id:String,client_secret:String,redirect_uri:String,verifier:String}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {name:String,client_id:String,client_secret:String,public_url:String}
#[derive(Deserialize)]
pub struct Callback {pub state:String,#[serde(default)]code:String,#[serde(default)]error:String}
pub fn migrate(c:&Connection)->Result<()> {
 c.execute_batch("CREATE TABLE IF NOT EXISTS gmail_accounts(id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS gmail_authorizations(state TEXT PRIMARY KEY,expires INTEGER NOT NULL);")?;Ok(())
}
fn path(app:&App)->PathBuf {PathBuf::from(format!("{}.credentials",app.config.database))}
fn save_secret(app:&App,key:&str,value:&str)->Result<()> {crate::connections::save_credential_at(path(app),&format!("gmail:{key}"),value)}
fn secret(app:&App,key:&str)->Result<String> {
 let store:Value=serde_json::from_slice(&std::fs::read(path(app)).context("Reconnect Gmail monitoring in Connections → Gmail")?)?;
 store[format!("gmail:{key}")].as_str().filter(|s|!s.is_empty()).map(str::to_owned).context("Reconnect Gmail monitoring in Connections → Gmail")
}
pub fn accounts(db:&Db)->Result<Vec<Account>> {
 let c=db.0.lock().unwrap(); let values=c.prepare("SELECT body FROM gmail_accounts ORDER BY rowid")?.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
 values.iter().map(|s|Ok(serde_json::from_str(s)?)).collect()
}
pub fn account(db:&Db,id:&str)->Result<Account> {accounts(db)?.into_iter().find(|a|a.id==id).context("Gmail monitoring connection not found")}
pub fn owns_state(app:&App,state:&str)->bool {app.db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM gmail_authorizations WHERE state=? AND expires>?)",params![state,db::now()],|r|r.get(0)).unwrap_or(false)}
fn redirect(origin:&str)->Result<String> {
 let url=reqwest::Url::parse(origin).context("Enter the Kindred server address you use on this device")?;
 ensure!((url.scheme()=="https" || (url.scheme()=="http" && matches!(url.host_str(),Some("127.0.0.1"|"localhost"|"[::1]")))) && url.host_str().is_some() && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path()=="/","Use your Kindred HTTPS origin without a path or credentials");
 Ok(format!("{}/oauth/gmail/callback",url.origin().ascii_serialization()))
}
pub async fn start(State(app):State<Shared>,Json(v):Json<Start>)->Result<Json<Value>,crate::web::Error> {Ok(Json(start_inner(&app,v).await?))}
async fn start_inner(app:&App,v:Start)->Result<Value> {
 let _lock=app.mail_lock.lock().await;
 ensure!(!app.account_disabled() && app.db.transfer_status()?.is_null() && app.config.database!=":memory:","Choose an active persistent workspace");
 ensure!(!v.name.trim().is_empty() && v.name.len()<=80,"Give this Gmail connection a short name");
 ensure!(v.client_id.ends_with(".apps.googleusercontent.com") && v.client_id.len()<300 && !v.client_secret.is_empty() && v.client_secret.len()<512,"Enter the Google Web application client ID and secret");
 let redirect_uri=redirect(&v.public_url)?;
 let expired:Vec<String>={let c=app.db.0.lock().unwrap();let rows=c.prepare("SELECT state FROM gmail_authorizations WHERE expires<=?")?.query_map([db::now()],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;rows};
 for state in expired {save_secret(app,&state,"")?;app.db.0.lock().unwrap().execute("DELETE FROM gmail_authorizations WHERE state=?",[state])?;}
 let pending:i64=app.db.0.lock().unwrap().query_row("SELECT count(*) FROM gmail_authorizations",[],|r|r.get(0))?;
 ensure!(pending<5,"Finish an existing Gmail sign-in or wait ten minutes before starting another");
 let state=format!("{}{}",db::id(),db::id());let verifier=format!("{}{}",db::id(),db::id());
 let challenge=URL_SAFE_NO_PAD.encode(digest(&SHA256,verifier.as_bytes()));
 let mut url=reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
 url.query_pairs_mut().extend_pairs([("client_id",v.client_id.as_str()),("redirect_uri",&redirect_uri),("response_type","code"),("scope",SCOPE),("access_type","offline"),("prompt","consent"),("state",&state),("code_challenge",&challenge),("code_challenge_method","S256")]);
 save_secret(app,&state,&serde_json::to_string(&Pending{name:v.name.trim().into(),client_id:v.client_id,client_secret:v.client_secret,redirect_uri:redirect_uri.clone(),verifier})?)?;
 app.db.0.lock().unwrap().execute("INSERT INTO gmail_authorizations VALUES(?,?)",params![state,db::now()+600])?;
 Ok(json!({"url":url.as_str(),"redirect_uri":redirect_uri}))
}
fn endpoint(app:&App,kind:&str)->String {
    #[cfg(test)] if let Ok(Some(base))=app.db.setting("gmail_test_base") {
        let url=reqwest::Url::parse(base.as_str().unwrap()).unwrap();
        assert_eq!(url.scheme(),"http");assert_eq!(url.host_str(),Some("127.0.0.1"));
        return format!("{}/{kind}/",base.as_str().unwrap());
    }
    let _=app;
    if kind=="token"{TOKEN.into()}else{API.into()}
}
fn client()->Result<reqwest::Client>{Ok(reqwest::Client::builder().timeout(Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build()?)}
async fn token(form:&[(&str,&str)],endpoint:&str)->Result<Value> {
 let r=client()?.post(endpoint).form(form).send().await.context("Could not contact Google; try connecting Gmail again")?;
 ensure!(r.status().is_success(),"Google authorization expired or was denied. Reconnect Gmail monitoring.");
 let v:Value=r.json().await.context("Invalid Google authorization response")?;
 ensure!(v["access_token"].as_str().is_some_and(|s|!s.is_empty()) && v["expires_in"].as_i64().is_some_and(|s|s>60),"Google did not return a usable authorization");
 if let Some(scopes)=v["scope"].as_str(){ensure!(scopes.split_whitespace().any(|s|s==SCOPE),"Grant read-only Gmail access to enable monitoring");}
 Ok(v)
}
pub async fn complete(app:&App,v:Callback)->Result<()> {
 let _lock=app.mail_lock.lock().await;
 ensure!(!app.account_disabled() && app.db.transfer_status()?.is_null(),"Workspace unavailable");
 ensure!(v.state.len()<=100 && owns_state(app,&v.state),"Gmail sign-in expired or was already used. Start again in Connections → Gmail.");
 let pending:Pending=serde_json::from_str(&secret(app,&v.state)?)?;
 app.db.0.lock().unwrap().execute("DELETE FROM gmail_authorizations WHERE state=?",[&v.state])?;save_secret(app,&v.state,"")?;
 ensure!(v.error.is_empty() && !v.code.is_empty() && v.code.len()<=4096,"Gmail sign-in was cancelled. No connection was changed.");
 let v=token(&[("grant_type","authorization_code"),("code",&v.code),("client_id",&pending.client_id),("client_secret",&pending.client_secret),("redirect_uri",&pending.redirect_uri),("code_verifier",&pending.verifier)],&endpoint(app,"token")).await?;
 let access=v["access_token"].as_str().unwrap();let refresh=v["refresh_token"].as_str().filter(|s|!s.is_empty()).context("Google did not grant background access. Reconnect with offline consent.")?;
 let profile=read_api(access,reqwest::Method::GET,"profile",vec![],None,&endpoint(app,"api")).await?;
 let email=profile["emailAddress"].as_str().filter(|s|s.contains('@')).context("Google did not identify this mailbox")?.to_ascii_lowercase();
 ensure!(accounts(&app.db)?.len()<20 || accounts(&app.db)?.iter().any(|a|a.email.eq_ignore_ascii_case(&email)),"At most twenty Gmail monitoring accounts per workspace");
 let id=accounts(&app.db)?.iter().find(|a|a.email.eq_ignore_ascii_case(&email)).map(|a|a.id.clone()).unwrap_or_else(||format!("gmail_{}",db::id()));
 save_secret(app,&id,&serde_json::to_string(&Secret{client_id:pending.client_id,client_secret:pending.client_secret,refresh_token:refresh.into(),access_token:access.into(),expires_at:db::now()+v["expires_in"].as_i64().unwrap().min(86400)-60})?)?;
 let account=Account{id,name:pending.name,email,status:"connected".into()};
 app.db.0.lock().unwrap().execute("INSERT INTO gmail_accounts VALUES(?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![account.id,serde_json::to_string(&account)?])?;
 Ok(())
}
pub fn callback_response(result:Result<()>)->Response {
 let (status,message)=if result.is_ok(){(axum::http::StatusCode::OK,"Gmail connected. Return to Kindred to choose this account and finish monitoring setup.")}else{(axum::http::StatusCode::BAD_REQUEST,"Gmail could not connect. Return to Kindred, check your Google app settings and start sign-in again. Existing connections were not changed.")};
 (status,[("cache-control","no-store"),("referrer-policy","no-referrer")],Html(format!("<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'><title>Kindred Gmail</title><main><h1>Kindred</h1><p>{message}</p></main>"))).into_response()
}
pub async fn callback(State(app):State<Shared>,Query(v):Query<Callback>)->Response {callback_response(complete(&app,v).await)}
fn allowed(method:&reqwest::Method,path:&str)->bool {
 (*method==reqwest::Method::GET && (matches!(path,"profile"|"history"|"messages"|"labels") || path.strip_prefix("messages/").is_some_and(|s|!s.is_empty() && !matches!(s,"send"|"import"|"insert"|"batchDelete"|"batchModify") && s.bytes().all(|b|b.is_ascii_alphanumeric())))) || (*method==reqwest::Method::POST && path=="watch")
}
async fn read_api(access:&str,method:reqwest::Method,path:&str,query:Vec<(&str,String)>,body:Option<Value>,base:&str)->Result<Value>{
 ensure!(allowed(&method,path),"Gmail monitoring supports read-only change detection");
 let mut req=client()?.request(method,format!("{base}{path}")).bearer_auth(access).query(&query);if let Some(body)=body{req=req.json(&body);}
 let r=req.send().await.context("Gmail monitoring could not reach Google")?;
 if r.status()==reqwest::StatusCode::NOT_FOUND{anyhow::bail!(if path=="history" {"GMAIL_HISTORY_EXPIRED"} else {"Gmail message unavailable (HTTP 404)"});}
 ensure!(r.status().is_success(),"Gmail monitoring request failed (HTTP {}). Check its connection and Google permissions.",r.status().as_u16());
 Ok(r.json().await.context("Invalid Gmail response")?)
}
pub async fn request(app:&App,id:&str,method:reqwest::Method,path:&str,query:Vec<(&str,String)>,body:Option<Value>)->Result<Value>{
 ensure!(account(&app.db,id)?.status=="connected","Reconnect Gmail monitoring in Connections → Gmail");
 let mut s:Secret=serde_json::from_str(&secret(app,id)?)?;
 if s.expires_at<=db::now(){
  let v=token(&[("grant_type","refresh_token"),("client_id",&s.client_id),("client_secret",&s.client_secret),("refresh_token",&s.refresh_token)],&endpoint(app,"token")).await?;
  s.access_token=v["access_token"].as_str().unwrap().into();s.expires_at=db::now()+v["expires_in"].as_i64().unwrap().min(86400)-60;
  save_secret(app,id,&serde_json::to_string(&s)?)?;
 }
 read_api(&s.access_token,method,path,query,body,&endpoint(app,"api")).await
}
pub async fn disconnect(State(app):State<Shared>,Path(id):Path<String>)->Result<Json<Value>,crate::web::Error>{Ok(Json(disconnect_inner(&app,&id).await?))}
async fn disconnect_inner(app:&App,id:&str)->Result<Value>{
 let _lock=app.mail_lock.lock().await;ensure!(!app.account_disabled() && app.db.transfer_status()?.is_null(),"Workspace unavailable");
 let mut a=account(&app.db,&id)?;
 ensure!(!crate::mail_watch::watches(&app.db)?.iter().any(|w|w.input.account_id==id && w.input.enabled),"Pause this Gmail account's monitors before disconnecting");
 a.status="disconnected".into();save_secret(&app,&id,"")?;
 app.db.0.lock().unwrap().execute("UPDATE gmail_accounts SET body=? WHERE id=?",params![serde_json::to_string(&a)?,id])?;
 Ok(json!({"disconnected":true}))
}
#[cfg(test)] mod integration_tests;
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn monitoring_transport_cannot_send_or_redirect(){
  assert!(allowed(&reqwest::Method::GET,"messages/abc123"));assert!(allowed(&reqwest::Method::POST,"watch"));
  for p in ["messages/send","messages/abc/modify","../profile","https://evil.example/","messages/abc?token=x"]{assert!(!allowed(&reqwest::Method::GET,p));assert!(!allowed(&reqwest::Method::POST,p));}
  assert!(redirect("https://example.com").unwrap().ends_with("/oauth/gmail/callback"));
  for p in ["http://example.com","https://user:pass@example.com","https://example.com/path","https://example.com?token=x"]{assert!(redirect(p).is_err());}
 }
 #[test] fn state_is_expiring_and_workspace_scoped(){let a=crate::tests::app();let b=crate::tests::app();a.db.0.lock().unwrap().execute("INSERT INTO gmail_authorizations VALUES('fixture',?)",[db::now()+30]).unwrap();assert!(owns_state(&a,"fixture"));assert!(!owns_state(&b,"fixture"));assert!(!owns_state(&a,"missing"));a.db.0.lock().unwrap().execute("UPDATE gmail_authorizations SET expires=?",[db::now()-1]).unwrap();assert!(!owns_state(&a,"fixture"));}
}
