//! Profile-local coordinator preference. This is not an authorization boundary.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use crate::db::Db;

pub fn selected(c: &Connection) -> Result<Option<String>> {
    Ok(c.query_row("SELECT b.id FROM settings s JOIN bots b ON b.id=json_extract(s.value,'$') WHERE s.key='primary_bot' AND COALESCE(json_extract(b.profile,'$.archived'),0)=0", [], |r|r.get(0)).optional()?)
}
pub fn get(db: &Db) -> Result<Option<String>> { selected(&db.0.lock().unwrap()) }
pub fn set(db: &Db, id: Option<&str>) -> Result<()> {
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    if let Some(id)=id {
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND COALESCE(json_extract(profile,'$.archived'),0)=0)",[id],|r|r.get::<_,bool>(0))?,"Choose an active bot in this profile");
    }
    tx.execute("INSERT INTO settings(key,value) VALUES('primary_bot',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![json!(id).to_string()])?;
    tx.commit()?;Ok(())
}
pub fn context(db: &Db, bot: &str, private: bool) -> Result<Value> {
    let (id,name)={
        let c=db.0.lock().unwrap();
        let Some(id)=selected(&c)? else {return Ok(Value::Null)};
        if !private && id!=bot {return Ok(Value::Null)}
        let name:String=c.query_row("SELECT name FROM bots WHERE id=?",[&id],|r|r.get(0))?;
        (id,name)
    };
    Ok(json!({"id":id,"name":name,"is_primary":id==bot,
        "role":"The user selected a primary bot for this profile: their default point of contact and coordinator. When you are primary, handle the request or delegate a concrete scoped assignment using existing collaboration tools. Keep track of your delegated work through real task receipts and supported follow-ups; never claim to monitor work without scheduling or an active continuation. Consolidate verified results and blockers into one useful update with attribution. When another bot delegated work to you, report back through that existing collaboration, avoiding duplicate user summaries. Do not divert direct user requests or create acknowledgement loops. This role grants no extra permissions, access to private conversations, approval authority, or authority to change anyone's role. Do not forward unrelated private conversations. Respect explicit recipients and established task owners. Shared-room membership and audience boundaries always apply."}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_is_single_validated_and_ignores_archived_or_deleted_bots() {
        let db=Db::open(":memory:").unwrap();
        let mut a=crate::tests::bot(&db,"codex");a.id="a".into();db.save_bot(&a).unwrap();
        let mut b=a.clone();b.id="b".into();db.save_bot(&b).unwrap();
        let mut archived=a.clone();archived.id="archived".into();archived.profile.archived=true;db.save_bot(&archived).unwrap();
        assert_eq!(get(&db).unwrap(),None);
        set(&db,Some("a")).unwrap();assert_eq!(get(&db).unwrap().as_deref(),Some("a"));
        assert!(set(&db,Some("foreign")).is_err());assert!(set(&db,Some("archived")).is_err());
        assert_eq!(get(&db).unwrap().as_deref(),Some("a"));
        set(&db,Some("b")).unwrap();assert_eq!(get(&db).unwrap().as_deref(),Some("b"));
        db.0.lock().unwrap().execute("UPDATE bots SET profile='{\"archived\":true}' WHERE id='b'",[]).unwrap();assert_eq!(get(&db).unwrap(),None);
        set(&db,Some("a")).unwrap();db.save_setting("primary_bot",&json!("deleted")).unwrap();assert_eq!(get(&db).unwrap(),None);
        set(&db,None).unwrap();assert_eq!(get(&db).unwrap(),None);
    }
    #[test]
    fn primary_bot_context_and_transfer_preserve_role_without_permissions() {
        let db=Db::open(":memory:").unwrap();let bot=crate::tests::bot(&db,"codex");let other=crate::tests::bot(&db,"codex");
        set(&db,Some(&bot.id)).unwrap();
        assert_eq!(context(&db,&bot.id,true).unwrap()["is_primary"],true);
        assert_eq!(context(&db,&other.id,true).unwrap()["is_primary"],false);
        assert_eq!(context(&db,&other.id,false).unwrap(),Value::Null);
        let package=db.prepare_transfer(&crate::db::id(),"Primary bot test").unwrap();
        let restored=Db::open(":memory:").unwrap();restored.import_transfer(&package).unwrap();
        assert_eq!(get(&restored).unwrap(),Some(bot.id.clone()));
        assert_eq!(restored.bot(&bot.id).unwrap().approval_mode,bot.approval_mode);
        let mut archived=restored.bot(&bot.id).unwrap();archived.profile.archived=true;restored.save_bot(&archived).unwrap();
        archived.profile.archived=false;restored.save_bot(&archived).unwrap();assert_eq!(get(&restored).unwrap(),None);
    }
    #[tokio::test]
    async fn primary_bot_api_requires_auth_and_preserves_general_settings() {
        use std::future::IntoFuture;
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(axum::serve(listener,crate::web::router(app.clone())).into_future());
        let c=reqwest::Client::new();let url=format!("{base}/api/primary-bot");
        assert_eq!(c.put(&url).json(&json!({"bot_id":bot.id})).send().await.unwrap().status(),401);
        let result=c.put(&url).bearer_auth(&app.token).json(&json!({"bot_id":bot.id})).send().await.unwrap();assert_eq!(result.status(),200);
        let prefs:Value=c.get(format!("{base}/api/settings")).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
        assert_eq!(prefs["primary_bot_id"],bot.id);
        let result=c.put(format!("{base}/api/settings")).bearer_auth(&app.token).json(&prefs).send().await.unwrap();assert_eq!(result.status(),200);
        assert_eq!(get(&app.db).unwrap(),Some(bot.id));
        assert!(!c.put(&url).bearer_auth(&app.token).json(&json!({"bot_id":"another-profile"})).send().await.unwrap().status().is_success());
        let result=c.put(&url).bearer_auth(&app.token).json(&json!({"bot_id":null})).send().await.unwrap();assert_eq!(result.status(),200);assert_eq!(get(&app.db).unwrap(),None);
        server.abort();
    }

}
