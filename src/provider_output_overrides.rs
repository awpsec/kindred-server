//! Explicit owner intent is separate from replaceable /models discovery metadata.
use crate::{db, provider_accounts::{CustomModel, CustomProvider}, runtime::{App, Shared}};
use anyhow::{Context, Result, ensure};
use axum::{Json, extract::{Path, State}};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const KEY: &str = "custom_provider_output_overrides";
const MAX_TOKENS: u64 = 10_000_000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Overrides {
    origin: String,
    revision: String,
    models: BTreeMap<String,u64>,
}
pub(crate) type Store = BTreeMap<String,Overrides>;
fn valid_model(model:&str)->bool { !model.trim().is_empty() && model.len()<=200 && !model.chars().any(char::is_control) }
pub(crate) fn validate(value:&Value, providers:&[CustomProvider])->Result<Store> {
    let store:Store=serde_json::from_value(value.clone())?;
    ensure!(store.len()<=20,"Too many provider output overrides");
    for (id,row) in &store {
        ensure!(crate::provider_accounts::valid_id(id) && id.starts_with("custom-"),"Invalid output override provider");
        ensure!(uuid::Uuid::parse_str(&row.revision).is_ok(),"Invalid output override revision");
        ensure!(providers.iter().any(|p|p.id==*id && p.base_url==row.origin),"Output override provider identity changed");
        ensure!(row.models.len()<=4096,"Too many model output overrides");
        ensure!(row.models.iter().all(|(m,n)|valid_model(m) && (1..=MAX_TOKENS).contains(n)),"Invalid model output override");
    }
    Ok(store)
}
fn read(c:&Connection)->Result<Store> {
    let raw:Option<String>=c.query_row("SELECT value FROM settings WHERE key=?",[KEY],|r|r.get(0)).optional()?;
    let value = raw.map(|raw|serde_json::from_str::<Value>(&raw)).transpose()?.unwrap_or(json!({}));
    validate(&value, &providers(c)?)
}
pub(crate) fn export(c:&Connection,providers:&[CustomProvider])->Result<Value> {
    let value=serde_json::to_value(read(c)?)?;validate(&value,providers)?;Ok(value)
}
fn write(c:&Connection,key:&str,value:&Value)->Result<()> {
    c.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value.to_string()])?;Ok(())
}
fn matching<'a>(store:&'a Store,p:&CustomProvider)->Option<&'a Overrides> {store.get(&p.id).filter(|row|row.origin==p.base_url)}
fn providers(c:&Connection)->Result<Vec<CustomProvider>> {
    let raw:Option<String>=c.query_row("SELECT value FROM settings WHERE key='custom_providers'",[],|r|r.get(0)).optional()?;
    Ok(serde_json::from_str(raw.as_deref().unwrap_or("[]"))?)
}
fn provider(c:&Connection,id:&str)->Result<CustomProvider> {
    providers(c)?.into_iter().find(|p|p.id==id).context("Provider not found")
}
/// Caller holds CONFIG_LOCK; reject existing corruption before credential-file writes.
/// persist_config revalidates within its SQLite transaction as well.
pub(crate) fn check_current(app:&App)->Result<()> {
    read(&app.db.0.lock().unwrap())?;
    Ok(())
}
pub fn effective(app:&App,p:&CustomProvider,m:&CustomModel)->Result<Option<u64>> {
    let store=read(&app.db.0.lock().unwrap())?;
    let explicit=matching(&store,p).and_then(|row|row.models.get(&m.model)).copied();
    if let Some(n)=explicit {ensure!((1..=MAX_TOKENS).contains(&n),"Invalid model output override");}
    Ok(explicit.or(m.max_output_tokens).map(|n|n.min(m.context_window)))
}
fn snapshot(p:&CustomProvider,store:&Store)->Value {
    let row=matching(store,p);
    json!({"provider_revision":p.revision,"revision":row.map(|r|r.revision.as_str()).unwrap_or(""),
        "overrides":row.map(|r|r.models.iter().map(|(model,n)| {
            let current=p.models.iter().find(|m|m.model==*model);
            json!({"model":model,"max_output_tokens":n,"available":current.is_some(),"effective_max_output_tokens":current.map(|m|(*n).min(m.context_window))})
        }).collect::<Vec<_>>()).unwrap_or_default(),
        "limits":p.models.iter().map(|m| {
            let explicit=row.and_then(|r|r.models.get(&m.model)).copied();
            json!({"model":m.model,"context_window":m.context_window,"discovered_max_output_tokens":m.max_output_tokens,"override_max_output_tokens":explicit,"effective_max_output_tokens":explicit.or(m.max_output_tokens).map(|n|n.min(m.context_window))})
        }).collect::<Vec<_>>()})
}
pub fn catalogue_response(app:&App,p:&CustomProvider)->Result<Value> {
    let mut value=crate::provider_catalog::response(p);
    value["output_overrides"]=snapshot(p,&read(&app.db.0.lock().unwrap())?);
    Ok(value)
}
pub async fn get(State(app):State<Shared>,Path(id):Path<String>)->Result<Json<Value>,crate::web::Error> {
    let _guard=crate::provider_accounts::CONFIG_LOCK.lock().await;
    let c=app.db.0.lock().unwrap();let p=provider(&c,&id)?;
    Ok(Json(snapshot(&p,&read(&c)?)))
}
pub async fn put(State(app):State<Shared>,Path(id):Path<String>,Json(v):Json<Value>)->Result<Json<Value>,crate::web::Error> {
    let _guard=crate::provider_accounts::CONFIG_LOCK.lock().await;
    Ok(Json(change(&app,&id,&v)?))
}
fn change(app:&App,id:&str,v:&Value)->Result<Value> {
    let object=v.as_object().context("Output override must be an object")?;
    ensure!(object.len()==4 && ["model","max_output_tokens","provider_revision","revision"].iter().all(|k|object.contains_key(*k)),"Specify model, max_output_tokens (integer or null), provider_revision and revision");
    let model=v["model"].as_str().filter(|m|valid_model(m)).context("Invalid model ID")?;
    let tokens=if v["max_output_tokens"].is_null() {None} else {Some(v["max_output_tokens"].as_u64().filter(|n|(1..=MAX_TOKENS).contains(n)).context("Output limit must be an integer from 1 to 10000000, or null to clear")?)};
    let provider_revision=v["provider_revision"].as_str().context("Provider revision is required")?;
    let revision=v["revision"].as_str().context("Output override revision is required")?;
    let mut c=app.db.0.lock().unwrap();let tx=c.transaction()?;
    let p=provider(&tx,&id)?;
    ensure!(p.revision==provider_revision,"Provider changed; reload output limits before saving");
    let mut store=read(&tx)?;let previous=matching(&store,&p);
    ensure!(previous.map(|r|r.revision.as_str()).unwrap_or("")==revision,"Output overrides changed; reload before saving");
    ensure!(tokens.is_none() || p.models.iter().any(|m|m.model==model),"Choose a currently discovered model before setting an output override");
    let mut row=previous.cloned().unwrap_or_else(||Overrides{origin:p.base_url.clone(),revision:String::new(),models:BTreeMap::new()});
    if let Some(n)=tokens {row.models.insert(model.into(),n);} else {row.models.remove(model);}
    row.revision=db::id();store.insert(id.into(),row);
    let providers:Vec<CustomProvider>=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT value FROM settings WHERE key='custom_providers'",[],|r|r.get(0))?)?;
    validate(&serde_json::to_value(&store)?,&providers)?;
    write(&tx,KEY,&serde_json::to_value(&store)?)?;let answer=snapshot(&p,&store);tx.commit()?;
    Ok(answer)
}
/// Caller holds CONFIG_LOCK. Identity reset and provider settings commit together.
pub(crate) fn persist_config(app:&App,providers:&[CustomProvider],reset:Option<&str>)->Result<()> {
    let mut c=app.db.0.lock().unwrap();let tx=c.transaction()?;
    let mut store=read(&tx)?;
    if let Some(id)=reset {store.remove(id);}
    let value=serde_json::to_value(store)?;
    validate(&value,providers)?;
    if reset.is_some() {write(&tx,KEY,&value)?;}
    write(&tx,"custom_providers",&serde_json::to_value(providers)?)?;tx.commit()?;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Shared;
    use std::sync::{Arc,atomic::{AtomicUsize,AtomicBool,Ordering}};
    use tokio::sync::Notify;
    struct Fixture {app:Shared,id:String,root:std::path::PathBuf,mode:Arc<AtomicUsize>,hold:Arc<AtomicBool>,entered:Arc<Notify>,release:Arc<Notify>,server:tokio::task::JoinHandle<()>}
    impl Drop for Fixture {fn drop(&mut self){self.server.abort();let _=std::fs::remove_dir_all(&self.root);}}
    impl Fixture {
        async fn new()->Self {
            use axum::{Router,routing::get,response::IntoResponse};
            let mode=Arc::new(AtomicUsize::new(0));let hold=Arc::new(AtomicBool::new(false));let entered=Arc::new(Notify::new());let release=Arc::new(Notify::new());
            let (m,h,e,r)=(mode.clone(),hold.clone(),entered.clone(),release.clone());
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/v1",listener.local_addr().unwrap());
            let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/v1/models",get(move || {let (m,h,e,r)=(m.clone(),h.clone(),e.clone(),r.clone());async move {
                if h.load(Ordering::SeqCst) {e.notify_one();r.notified().await;}
                match m.load(Ordering::SeqCst) {
                    1=>Json(json!({"data":[{"id":"owned/model","context_length":64000,"max_output_tokens":8192}]})).into_response(),
                    2=>Json(json!({"data":[]})).into_response(),
                    3=>(axum::http::StatusCode::SERVICE_UNAVAILABLE,"owned error").into_response(),
                    4=>Json(json!({"data":[{"id":"owned/model","context_length":32768}]})).into_response(),
                    _=>Json(json!({"data":[{"id":"owned/model","context_length":262144}]})).into_response(),
                }
            }}))).await.unwrap()});
            let root=std::env::temp_dir().join(format!("kindred-output-override-{}",db::id()));std::fs::create_dir(&root).unwrap();
            let mut app=crate::tests::app();let config=&mut Arc::get_mut(&mut app).unwrap().config;config.database=root.join("db").to_string_lossy().into();
            let path=app.config.database.clone();Arc::get_mut(&mut app).unwrap().db=db::Db::open(&path).unwrap();
            let id=format!("custom-{}",db::id());app.db.save_setting("custom_providers",&json!([{"id":id,"name":"Owned","base_url":url,"no_auth":true,"revision":db::id(),"models":[{"model":"owned/model","context_window":262144}],"catalog":{"checked_at":1}}])).unwrap();
            Self{app,id,root,mode,hold,entered,release,server}
        }
        fn p(&self)->CustomProvider {crate::provider_accounts::custom(&self.app).unwrap().remove(0)}
        async fn state(&self)->Value {get(State(self.app.clone()),Path(self.id.clone())).await.ok().unwrap().0}
        async fn set_with(&self,state:&Value,n:Value)->Result<Value,crate::web::Error> {
            put(State(self.app.clone()),Path(self.id.clone()),Json(json!({"model":"owned/model","max_output_tokens":n,"provider_revision":state["provider_revision"],"revision":state["revision"]}))).await.map(|j|j.0)
        }
        async fn set(&self,n:Value)->Value {self.set_with(&self.state().await,n).await.ok().unwrap()}
        fn effective(&self)->Option<u64> {let p=self.p();effective(&self.app,&p,&p.models[0]).unwrap()}
        async fn refresh(&self) {crate::provider_catalog::refresh(&self.app,&self.id).await.unwrap();}
    }
    #[tokio::test]
    async fn explicit_intent_survives_omission_startup_serialization_and_lower_context() {
        let mut f=Fixture::new().await;f.set(json!(262144)).await;
        for _ in 0..2 {f.refresh().await;assert_eq!(f.p().models[0].max_output_tokens,None);assert_eq!(f.effective(),Some(262144));}
        let path=f.app.config.database.clone();drop(std::mem::replace(&mut f.app,crate::tests::app()));
        let mut reopened=crate::tests::app();Arc::get_mut(&mut reopened).unwrap().config.database=path.clone();Arc::get_mut(&mut reopened).unwrap().db=db::Db::open(&path).unwrap();f.app=reopened;
        crate::provider_catalog::startup(f.app.clone()).await;assert_eq!(f.effective(),Some(262144));
        f.mode.store(4,Ordering::SeqCst);f.refresh().await;assert_eq!(f.effective(),Some(32768));assert_eq!(f.state().await["overrides"][0]["max_output_tokens"],262144);
        f.mode.store(0,Ordering::SeqCst);f.refresh().await;assert_eq!(f.effective(),Some(262144));
    }
    #[tokio::test]
    async fn lower_clear_and_discovered_only_limits_remain_independent() {
        let f=Fixture::new().await;f.mode.store(1,Ordering::SeqCst);f.refresh().await;
        assert_eq!(f.effective(),Some(8192));f.set(json!(4096)).await;assert_eq!(f.effective(),Some(4096));
        f.refresh().await;assert_eq!(f.effective(),Some(4096));f.set(Value::Null).await;assert_eq!(f.effective(),Some(8192));
        f.mode.store(0,Ordering::SeqCst);f.refresh().await;assert_eq!(f.effective(),None);
        let other=Fixture::new().await;assert_eq!(other.effective(),None);assert!(other.state().await["overrides"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn stale_cas_missing_clear_invalid_numbers_and_exact_model_identity() {
        let f=Fixture::new().await;let first=f.state().await;f.set_with(&first,json!(262144)).await.ok().unwrap();
        assert!(f.set_with(&first,Value::Null).await.is_err());
        let current=f.state().await;
        for value in [json!(0),json!(-1),json!(1.5),json!("123"),json!(true),json!(10000001),json!(u64::MAX)] {
            assert!(f.set_with(&current,value).await.is_err());assert_eq!(f.effective(),Some(262144));
        }
        let valid=json!({"model":"owned/model","max_output_tokens":null,"provider_revision":current["provider_revision"],"revision":current["revision"]});
        for key in ["model","max_output_tokens","provider_revision","revision"] {
            let mut body=valid.clone();body.as_object_mut().unwrap().remove(key);assert!(put(State(f.app.clone()),Path(f.id.clone()),Json(body)).await.is_err());
        }
        let mut body=valid.clone();body["provider_revision"]=json!("stale");assert!(put(State(f.app.clone()),Path(f.id.clone()),Json(body)).await.is_err());
        let mut body=valid;body["model"]=json!("Owned/model");body["max_output_tokens"]=json!(1);assert!(put(State(f.app.clone()),Path(f.id.clone()),Json(body)).await.is_err());
        f.set(json!(10000000)).await;assert_eq!(f.effective(),Some(262144));
    }
    #[tokio::test]
    async fn dormant_models_do_not_revive_catalogue_and_failed_refresh_keeps_intent() {
        let f=Fixture::new().await;f.set(json!(262144)).await;
        f.mode.store(2,Ordering::SeqCst);f.refresh().await;assert!(f.p().models.is_empty());
        let state=f.state().await;assert_eq!(state["overrides"][0]["available"],false);assert_eq!(state["overrides"][0]["effective_max_output_tokens"],Value::Null);
        assert!(f.set_with(&state,json!(8192)).await.is_err());
        f.mode.store(3,Ordering::SeqCst);f.refresh().await;assert!(f.p().models.is_empty());assert_eq!(f.state().await["overrides"][0]["max_output_tokens"],262144);
        f.mode.store(0,Ordering::SeqCst);f.refresh().await;assert_eq!(f.effective(),Some(262144));
        f.mode.store(2,Ordering::SeqCst);f.refresh().await;f.set(Value::Null).await;assert!(f.state().await["overrides"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn late_refresh_cannot_erase_concurrent_save_clear_or_provider_identity_change() {
        let f=Fixture::new().await;
        for n in [json!(262144),Value::Null] {
            f.hold.store(true,Ordering::SeqCst);let app=f.app.clone();let id=f.id.clone();let task=tokio::spawn(async move{crate::provider_catalog::refresh(&app,&id).await});
            f.entered.notified().await;f.set(n.clone()).await;f.hold.store(false,Ordering::SeqCst);f.release.notify_one();task.await.unwrap().unwrap();assert_eq!(f.effective(),n.as_u64());
        }
        f.set(json!(262144)).await;let before=f.state().await;let mut p=f.p();p.name="Renamed".into();
        let _ = crate::provider_accounts::save(State(f.app.clone()),Json(json!({"provider":p}))).await.ok().unwrap();assert_eq!(f.effective(),Some(262144));
        f.hold.store(true,Ordering::SeqCst);let app=f.app.clone();let id=f.id.clone();let task=tokio::spawn(async move{crate::provider_catalog::refresh(&app,&id).await});f.entered.notified().await;
        let mut p=f.p();p.base_url="https://new-owned.invalid/v1".into();p.no_auth=false;
        let _ = crate::provider_accounts::save(State(f.app.clone()),Json(json!({"provider":p}))).await.ok().unwrap();
        f.hold.store(false,Ordering::SeqCst);f.release.notify_one();task.await.unwrap().unwrap();assert!(f.p().models.is_empty());assert!(f.state().await["overrides"].as_array().unwrap().is_empty());assert!(f.set_with(&before,json!(262144)).await.is_err());
    }
    #[tokio::test]
    async fn deletion_recreation_and_transaction_abort_cannot_leak_intent() {
        let f=Fixture::new().await;f.set(json!(262144)).await;let previous=f.p();let state=f.state().await;
        f.app.db.0.lock().unwrap().execute_batch("CREATE TRIGGER refuse_config BEFORE UPDATE ON settings WHEN NEW.key='custom_providers' BEGIN SELECT RAISE(ABORT,'owned failure');END;").unwrap();
        assert!(crate::provider_accounts::remove(State(f.app.clone()),Path(f.id.clone())).await.is_err());assert_eq!(f.effective(),Some(262144));
        f.app.db.0.lock().unwrap().execute_batch("DROP TRIGGER refuse_config").unwrap();
        f.hold.store(true,Ordering::SeqCst);let app=f.app.clone();let id=f.id.clone();let task=tokio::spawn(async move{crate::provider_catalog::refresh(&app,&id).await});f.entered.notified().await;
        let _ = crate::provider_accounts::remove(State(f.app.clone()),Path(f.id.clone())).await.ok().unwrap();
        assert!(get(State(f.app.clone()),Path(f.id.clone())).await.is_err());
        let mut recreated=previous;recreated.no_auth=false;
        let _ = crate::provider_accounts::save(State(f.app.clone()),Json(json!({"provider":recreated}))).await.ok().unwrap();
        f.hold.store(false,Ordering::SeqCst);f.release.notify_one();assert!(task.await.unwrap().unwrap().models.is_empty());
        assert_ne!(f.state().await["provider_revision"],state["provider_revision"]);assert!(f.set_with(&state,json!(262144)).await.is_err());assert!(f.state().await["overrides"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn authenticated_api_is_explicit_and_catalogue_metadata_stays_raw() {
        let f=Fixture::new().await;let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/api/providers/{}/output-overrides",listener.local_addr().unwrap(),f.id);
        let app=f.app.clone();let server=tokio::spawn(async move{axum::serve(listener,crate::web::router(app)).await.unwrap()});let client=reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(),401);assert_eq!(client.put(&url).json(&json!({})).send().await.unwrap().status(),401);
        let state:Value=client.get(&url).bearer_auth(&f.app.token).send().await.unwrap().json().await.unwrap();
        let response=client.put(&url).bearer_auth(&f.app.token).json(&json!({"model":"owned/model","max_output_tokens":262144,"provider_revision":state["provider_revision"],"revision":state["revision"]})).send().await.unwrap();assert_eq!(response.status(),200);
        let response=catalogue_response(&f.app,&f.p()).unwrap();assert_eq!(response["models"][0]["max_output_tokens"],Value::Null);assert_eq!(response["output_overrides"]["limits"][0]["effective_max_output_tokens"],262144);
        server.abort();
    }
    #[tokio::test]
    async fn legacy_nonnull_metadata_is_not_promoted_to_explicit_owner_intent() {
        let f=Fixture::new().await;let mut p=f.p();p.models[0].max_output_tokens=Some(8192);
        f.app.db.save_setting("custom_providers",&json!([p])).unwrap();
        assert_eq!(f.effective(),Some(8192));assert!(f.state().await["overrides"].as_array().unwrap().is_empty());
        f.refresh().await;assert_eq!(f.effective(),None);assert!(f.state().await["overrides"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn workspace_transfer_preserves_explicit_identity_and_old_packages_clear_destination() {
        let f=Fixture::new().await;f.set(json!(262144)).await;
        let package=f.app.db.prepare_transfer(&db::id(),"Owned override transfer").unwrap();
        assert_eq!(package["settings"][KEY][&f.id]["models"]["owned/model"],262144);
        let target=crate::tests::app();target.db.import_transfer(&package).unwrap();
        let p=crate::provider_accounts::custom(&target).unwrap().remove(0);
        assert_eq!(effective(&target,&p,&p.models[0]).unwrap(),Some(262144));
        let mut legacy=package.clone();legacy["settings"].as_object_mut().unwrap().remove(KEY);
        let target=crate::tests::app();target.db.save_setting(KEY,&package["settings"][KEY]).unwrap();target.db.import_transfer(&legacy).unwrap();
        let p=crate::provider_accounts::custom(&target).unwrap().remove(0);assert_eq!(effective(&target,&p,&p.models[0]).unwrap(),None);
        let mut foreign=package;foreign["settings"][KEY][&f.id]["origin"]=json!("https://foreign.invalid/v1");
        let target=crate::tests::app();assert!(target.db.import_transfer(&foreign).is_err());assert!(target.db.setting("custom_providers").unwrap().is_none());
    }

    #[tokio::test]
    async fn same_account_same_endpoint_other_provider_keeps_its_own_override() {
        let f=Fixture::new().await;f.set(json!(262144)).await;
        let first=f.p();let mut other=first.clone();other.id=format!("custom-{}",db::id());other.revision=db::id();other.models[0].max_output_tokens=Some(8192);
        f.app.db.save_setting("custom_providers",&json!([first,other])).unwrap();
        let state=get(State(f.app.clone()),Path(other.id.clone())).await.ok().unwrap().0;
        put(State(f.app.clone()),Path(other.id.clone()),Json(json!({"model":"owned/model","max_output_tokens":4096,"provider_revision":state["provider_revision"],"revision":state["revision"]}))).await.ok().unwrap();
        let before=get(State(f.app.clone()),Path(other.id.clone())).await.ok().unwrap().0;
        assert_eq!(f.effective(),Some(262144));assert_eq!(effective(&f.app,&other,&other.models[0]).unwrap(),Some(4096));
        f.set(Value::Null).await;let _=crate::provider_accounts::remove(State(f.app.clone()),Path(f.id.clone())).await.ok().unwrap();
        assert_eq!(get(State(f.app.clone()),Path(other.id.clone())).await.ok().unwrap().0,before);
        assert_eq!(effective(&f.app,&other,&other.models[0]).unwrap(),Some(4096));
    }

}

#[cfg(test)]
mod validation_tests {
use serde_json::{json,Value};
use crate::{provider_output_overrides as output,provider_accounts::CustomProvider,runtime::Shared};
use axum::{extract::{State,Path},Json};
fn fixture()->(Shared,CustomProvider){
 let app=crate::tests::app();let id=format!("custom-{}",crate::db::id());
 let p:CustomProvider=serde_json::from_value(json!({"id":id,"name":"Owned review","base_url":"https://owned.invalid/v1","no_auth":false,"revision":crate::db::id(),"catalog":{"checked_at":1},"models":[{"model":"owned/a","context_window":262144},{"model":"owned/b","context_window":64000}]})).unwrap();
 app.db.save_setting("custom_providers",&json!([p])).unwrap();(app,p)
}
fn seed(p:&CustomProvider,revision:&str)->Value{json!({p.id.clone():{"origin":p.base_url,"revision":revision,"models":{"owned/a":262144}}})}
#[tokio::test]
async fn mobile_invalid_stored_revision_must_not_be_consumed_as_explicit_intent(){
 let(app,p)=fixture();let bad=seed(&p,"invalid-not-a-uuid");assert!(output::validate(&bad,&[p.clone()]).is_err());app.db.save_setting(output::KEY,&bad).unwrap();
 let effective=output::effective(&app,&p,&p.models[0]);let api=output::get(State(app.clone()),Path(p.id.clone())).await;
 println!("invalid revision runtime={effective:?}, GET accepted={}",api.is_ok());
 assert!(effective.is_err(),"Runtime must refuse the same malformed store rejected by source validator");assert!(api.is_err());
}
#[test]
fn mobile_malformed_store_rename_must_refuse_without_config_partial_write(){
 let(app,p)=fixture();app.db.save_setting(output::KEY,&Value::Null).unwrap();let before=app.db.setting("custom_providers").unwrap();
 let mut renamed=p;renamed.name="Renamed despite malformed store".into();
 let result=output::persist_config(&app,&[renamed],None);let after=app.db.setting("custom_providers").unwrap();
 println!("malformed-store config save accepted={}, provider unchanged={}",result.is_ok(),before==after);
 assert!(result.is_err(),"Configuration persistence must fail closed on malformed explicit store");assert_eq!(after,before);
}
#[tokio::test]
async fn mobile_cross_model_cas_cannot_clear_newer_intent_and_unrelated_settings_survive(){
 let(app,p)=fixture();app.db.save_setting("owned_unrelated",&json!({"keep":"OWNED_CANARY"})).unwrap();
 let first=output::get(State(app.clone()),Path(p.id.clone())).await.ok().unwrap().0;
 let request=|model:&str,n:Value,rev:&Value|json!({"model":model,"max_output_tokens":n,"provider_revision":first["provider_revision"],"revision":rev});
 let a=output::put(State(app.clone()),Path(p.id.clone()),Json(request("owned/a",json!(262144),&first["revision"]))).await.ok().unwrap().0;
 assert!(output::put(State(app.clone()),Path(p.id.clone()),Json(request("owned/b",json!(4096),&first["revision"]))).await.is_err());
 let b=output::put(State(app.clone()),Path(p.id.clone()),Json(request("owned/b",json!(4096),&a["revision"]))).await.ok().unwrap().0;
 assert!(output::put(State(app.clone()),Path(p.id.clone()),Json(request("owned/a",Value::Null,&a["revision"]))).await.is_err());
 assert_eq!(output::effective(&app,&p,&p.models[0]).unwrap(),Some(262144));assert_eq!(output::effective(&app,&p,&p.models[1]).unwrap(),Some(4096));
 let cleared=output::put(State(app.clone()),Path(p.id.clone()),Json(request("owned/a",Value::Null,&b["revision"]))).await.ok().unwrap().0;assert_ne!(b["revision"],cleared["revision"]);
 assert_eq!(output::effective(&app,&p,&p.models[0]).unwrap(),None);assert_eq!(app.db.setting("owned_unrelated").unwrap(),Some(json!({"keep":"OWNED_CANARY"})));
}
#[tokio::test]
async fn malformed_store_refuses_every_surface_and_preserves_raw_bytes() {
 let(app,p)=fixture();let good=seed(&p,&crate::db::id());let mut cases=vec![Value::Null,json!([])];
 for (field,value) in [("revision",json!("bad")),("origin",json!("https://foreign.invalid/v1")),("extra",json!(true))] {
  let mut bad=good.clone();bad[&p.id][field]=value;cases.push(bad);
 }
 for value in [json!(0),json!(1.25),json!("4096"),json!(10000001)] {let mut bad=good.clone();bad[&p.id]["models"]["owned/a"]=value;cases.push(bad);}
 let foreign=format!("custom-{}",crate::db::id());cases.push(json!({foreign:good[&p.id]}));
 let mut empty_name=good.clone();empty_name[&p.id]["models"]=json!({"":4096});cases.push(empty_name);
 let before=app.db.setting("custom_providers").unwrap();
 for bad in cases {
  app.db.save_setting(output::KEY,&bad).unwrap();
  let raw:String=app.db.0.lock().unwrap().query_row("SELECT value FROM settings WHERE key=?",[output::KEY],|r|r.get(0)).unwrap();
  assert!(output::effective(&app,&p,&p.models[0]).is_err());
  assert!(output::get(State(app.clone()),Path(p.id.clone())).await.is_err());
  assert!(output::catalogue_response(&app,&p).is_err());
  assert!(output::export(&app.db.0.lock().unwrap(),&[p.clone()]).is_err());
  let body=json!({"model":"owned/a","max_output_tokens":null,"provider_revision":p.revision,"revision":""});
  assert!(output::put(State(app.clone()),Path(p.id.clone()),Json(body)).await.is_err());
  assert!(output::persist_config(&app,&[p.clone()],None).is_err());
  assert!(output::persist_config(&app,&[],Some(&p.id)).is_err(),"reset must not normalize corruption");
  assert_eq!(app.db.setting("custom_providers").unwrap(),before);
  let after:String=app.db.0.lock().unwrap().query_row("SELECT value FROM settings WHERE key=?",[output::KEY],|r|r.get(0)).unwrap();assert_eq!(after,raw);
 }
 app.db.0.lock().unwrap().execute("UPDATE settings SET value='{' WHERE key=?",[output::KEY]).unwrap();
 assert!(output::effective(&app,&p,&p.models[0]).is_err());assert!(output::persist_config(&app,&[p],None).is_err());
 let raw:String=app.db.0.lock().unwrap().query_row("SELECT value FROM settings WHERE key=?",[output::KEY],|r|r.get(0)).unwrap();assert_eq!(raw,"{");
}
#[tokio::test]
async fn missing_key_valid_rename_clear_and_dormant_records_remain_supported() {
 let(app,p)=fixture();assert_eq!(output::effective(&app,&p,&p.models[0]).unwrap(),None);
 let first=output::get(State(app.clone()),Path(p.id.clone())).await.ok().unwrap().0;assert_eq!(first["revision"],"");
 let mut renamed=p.clone();renamed.name="New name".into();output::persist_config(&app,&[renamed.clone()],None).unwrap();
 assert_eq!(app.db.setting(output::KEY).unwrap(),None,"missing store need not be materialized by rename");
 let state=output::put(State(app.clone()),Path(p.id.clone()),Json(json!({"model":"owned/a","max_output_tokens":4096,"provider_revision":p.revision,"revision":""}))).await.ok().unwrap().0;
 let saved=app.db.setting(output::KEY).unwrap();renamed.models.clear();output::persist_config(&app,&[renamed.clone()],None).unwrap();
 assert_eq!(app.db.setting(output::KEY).unwrap(),saved);assert_eq!(output::get(State(app.clone()),Path(p.id.clone())).await.ok().unwrap().0["overrides"][0]["available"],false);
 let cleared=output::put(State(app.clone()),Path(p.id.clone()),Json(json!({"model":"owned/a","max_output_tokens":null,"provider_revision":p.revision,"revision":state["revision"]}))).await.ok().unwrap().0;
 assert_ne!(cleared["revision"],state["revision"]);assert!(cleared["overrides"].as_array().unwrap().is_empty());
}
#[test]
fn next_provider_binding_must_validate_before_any_write_and_scoped_reset_preserves_others() {
 let(app,p)=fixture();let mut other=p.clone();other.id=format!("custom-{}",crate::db::id());other.revision=crate::db::id();
 app.db.save_setting("custom_providers",&json!([p,other])).unwrap();let mut store=seed(&p,&crate::db::id());store.as_object_mut().unwrap().extend(seed(&other,&crate::db::id()).as_object().unwrap().clone());app.db.save_setting(output::KEY,&store).unwrap();
 let before=app.db.setting("custom_providers").unwrap();let mut moved=p.clone();moved.base_url="https://new-owned.invalid/v1".into();moved.revision=crate::db::id();
 assert!(output::persist_config(&app,&[moved.clone(),other.clone()],None).is_err());assert_eq!(app.db.setting("custom_providers").unwrap(),before);assert_eq!(app.db.setting(output::KEY).unwrap(),Some(store.clone()));
 output::persist_config(&app,&[moved.clone(),other.clone()],Some(&p.id)).unwrap();let after=app.db.setting(output::KEY).unwrap().unwrap();assert!(after.get(&p.id).is_none());assert_eq!(after[&other.id],store[&other.id]);
 output::persist_config(&app,&[other.clone()],Some(&moved.id)).unwrap();assert_eq!(output::effective(&app,&other,&other.models[0]).unwrap(),Some(262144));
 let mut new=other.clone();new.id=format!("custom-{}",crate::db::id());new.revision=crate::db::id();output::persist_config(&app,&[other,new.clone()],Some(&new.id)).unwrap();
 assert_eq!(output::effective(&app,&new,&new.models[0]).unwrap(),None);
}
#[tokio::test]
async fn malformed_store_precheck_prevents_existing_save_and_remove_credential_writes() {
 let(mut app,p)=fixture();let root=std::env::temp_dir().join(format!("kindred-override-validation-{}",crate::db::id()));std::fs::create_dir(&root).unwrap();
 std::sync::Arc::get_mut(&mut app).unwrap().config.database=root.join("owned.db").to_string_lossy().into();
 let path=std::path::PathBuf::from(format!("{}.credentials",app.config.database));let raw=serde_json::to_vec(&json!({p.id.clone():"OWNED_ORIGINAL_KEY"})).unwrap();std::fs::write(&path,&raw).unwrap();
 app.db.save_setting(output::KEY,&Value::Null).unwrap();let config=app.db.setting("custom_providers").unwrap();let mut renamed=p.clone();renamed.name="Must refuse".into();
 assert!(crate::provider_accounts::save(State(app.clone()),Json(json!({"provider":renamed,"key":"OWNED_REPLACEMENT_KEY"}))).await.is_err());
 assert_eq!(std::fs::read(&path).unwrap(),raw);assert_eq!(app.db.setting("custom_providers").unwrap(),config);
 assert!(crate::provider_accounts::remove(State(app.clone()),Path(p.id.clone())).await.is_err());
 assert_eq!(std::fs::read(&path).unwrap(),raw);assert_eq!(app.db.setting("custom_providers").unwrap(),config);assert_eq!(app.db.setting(output::KEY).unwrap(),Some(Value::Null));
 assert_eq!(std::fs::read_dir(&root).unwrap().count(),1,"no credential temporary file");std::fs::remove_dir_all(root).unwrap();
}

}
