//! Ownership receipts for disposable Codex transcripts. Only IDs created here
//! may be deleted; auth files and filesystem paths are never cleanup targets.
use crate::{db::{self, Bot, Run}, runtime::App, rpc::Rpc};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const LIMIT: i64 = 128;
const AGE: i64 = 7 * 86400;
const ACTIVE: &str = "'queued','running','awaiting_user','awaiting_approval','cancelling'";

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS harness_native_threads(
        authority TEXT NOT NULL,thread_id TEXT NOT NULL,bot_id TEXT NOT NULL,
        session_id TEXT NOT NULL,run_id TEXT NOT NULL,created INTEGER NOT NULL,
        updated INTEGER NOT NULL,retired INTEGER NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0,retry_after INTEGER NOT NULL DEFAULT 0,
        cleanup_until INTEGER NOT NULL DEFAULT 0,
        reservation INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(authority,thread_id));
        CREATE INDEX IF NOT EXISTS harness_native_owner ON harness_native_threads(authority,bot_id,updated);")?;
    Ok(())
}

pub fn authority(app: &App, identity: &Value) -> String {
    // Model/prompt changes retire threads but do not change the deletion realm.
    // Bind the guest-verified account digest, not the email display name or
    // refresh token. Same-account reconnects can clean their retired threads;
    // the profile's database is the outer ownership scope.
    let encoded=json!({"vm":app.config.vm,"account":identity["context_account"]}).to_string();
    ring::digest::digest(&ring::digest::SHA256,encoded.as_bytes()).as_ref().iter().map(|n|format!("{n:02x}")).collect()
}

pub fn reserve(app: &App, bot: &Bot, run: &Run, authority: &str, session: &str) -> Result<bool> {
    let mut c=app.db.0.lock().unwrap();let tx=c.transaction()?;
    // Global profile cap includes old accounts and uncertain creation attempts;
    // repeated auth resets or concurrent bots cannot bypass the disk bound.
    let count:i64=tx.query_row("SELECT COUNT(*) FROM harness_native_threads",[],|r|r.get(0))?;
    if count>=LIMIT {return Ok(false);}
    tx.execute("INSERT INTO harness_native_threads(authority,thread_id,bot_id,session_id,run_id,created,updated,reservation) VALUES(?,?,?,?,?,?,?,1)",
        params![authority,format!("pending:{session}"),bot.id,session,run.id,db::now(),db::now()])?;
    tx.commit()?;Ok(true)
}

pub fn register(app: &App, bot: &Bot, run: &Run, authority: &str, session: &str, thread: &str) -> Result<()> {
    ensure!(!thread.is_empty() && thread.len()<=256,"Invalid owned thread ID");
    let c=app.db.0.lock().unwrap();
    // Atomic replacement of the preflight reservation, never an upsert of an
    // SDK-supplied ID. A crash at the RPC boundary retains the reserved slot.
    ensure!(c.execute("UPDATE harness_native_threads SET thread_id=?,reservation=0,updated=? WHERE authority=? AND thread_id=? AND bot_id=? AND session_id=? AND run_id=? AND reservation=1",
        params![thread,db::now(),authority,format!("pending:{session}"),bot.id,session,run.id])?==1,"Native thread ownership reservation missing");
    Ok(())
}

pub fn touch(app: &App, bot: &Bot, run: &Run, authority: &str, session: &str, thread: &str) -> Result<bool> {
    Ok(app.db.0.lock().unwrap().execute("UPDATE harness_native_threads SET run_id=?,updated=? WHERE authority=? AND thread_id=? AND bot_id=? AND session_id=? AND retired=0 AND cleanup_until=0 AND reservation=0 AND created>=? AND created<=?",
        params![run.id,db::now(),authority,thread,bot.id,session,db::now()-AGE,db::now()])?==1)
}

fn claim(app: &App, bot: &Bot, authority: &str) -> Result<Option<String>> {
    let mut c=app.db.0.lock().unwrap();let tx=c.transaction()?;let now=db::now();
    let sql=format!("SELECT t.thread_id FROM harness_native_threads t WHERE t.authority=?1 AND t.bot_id=?2 AND t.reservation=0 AND t.retry_after<=?3 AND t.cleanup_until<=?3
        AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.id=t.run_id AND r.status IN ({ACTIVE}))
        AND (t.retired=1 OR t.created<?4 OR t.updated<?4 OR NOT EXISTS(SELECT 1 FROM harness_contexts h WHERE h.bot_id=t.bot_id AND h.session_id=t.session_id AND h.checkpoint IS NOT NULL)
            OR (SELECT COUNT(*) FROM harness_native_threads n WHERE n.authority=t.authority AND n.bot_id=t.bot_id AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.id=n.run_id AND r.status IN ({ACTIVE})))>128)
        ORDER BY t.updated,t.thread_id LIMIT 1");
    let thread:Option<String>=tx.query_row(&sql,params![authority,bot.id,now,now-AGE],|r|r.get(0)).optional()?;
    if let Some(thread)=&thread {
        tx.execute("UPDATE harness_native_threads SET retired=1,cleanup_until=? WHERE authority=? AND thread_id=?",params![now+120,authority,thread])?;
    }
    tx.commit()?;Ok(thread)
}

fn finish(app: &App, authority: &str, thread: &str, deleted: bool) -> Result<()> {
    let c=app.db.0.lock().unwrap();
    if deleted {c.execute("DELETE FROM harness_native_threads WHERE authority=? AND thread_id=? AND retired=1",params![authority,thread])?;}
    else {c.execute("UPDATE harness_native_threads SET failures=min(failures+1,8),retry_after=?+min(21600,60*(1<<min(failures,8))),cleanup_until=0 WHERE authority=? AND thread_id=? AND retired=1",params![db::now(),authority,thread])?;}
    Ok(())
}

fn already_missing(error: &anyhow::Error) -> bool {
    let text=error.to_string().to_ascii_lowercase();
    text.starts_with("codex rejected thread/delete:") && (text.contains("thread not found") || text.contains("no rollout found") || text.contains("unknown thread"))
}

pub async fn drain(app: &App, bot: &Bot, authority: &str, rpc: &mut Rpc) -> Result<()> {
    for _ in 0..8 {
        let Some(thread)=claim(app,bot,authority)? else {break};
        let deleted=match rpc.request_guarded("thread/delete",json!({"threadId":thread})).await {
            Ok(_)=>true,Err(error)=>already_missing(&error),
        };
        finish(app,authority,&thread,deleted)?;
        // Unsupported commands/network failures stay pending with backoff. Do
        // not spend eight request timeouts delaying the user's current task.
        if !deleted {break;}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (crate::runtime::Shared,Bot,Run) {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        app.db.queue(&bot.id,"Synthetic native ownership",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();(app,bot,run)
    }
    fn owned(app: &App, bot: &Bot, run: &Run, authority: &str, session: &str, thread: &str) {
        assert!(reserve(app,bot,run,authority,session).unwrap());register(app,bot,run,authority,session,thread).unwrap();
    }
    #[test]
    fn active_owner_authority_bot_and_lease_protect_native_threads() {
        let (app,bot,run)=setup();owned(&app,&bot,&run,"realm","session","owned");
        assert!(claim(&app,&bot,"realm").unwrap().is_none());
        assert!(!touch(&app,&bot,&run,"other-realm","session","owned").unwrap());
        let peer=crate::tests::bot(&app.db,"codex");assert!(!touch(&app,&peer,&run,"realm","session","owned").unwrap());
        assert!(!touch(&app,&bot,&run,"realm","other-session","owned").unwrap());
        app.db.finish(&run.id,"completed","Done","").unwrap();
        assert!(claim(&app,&peer,"realm").unwrap().is_none());assert!(claim(&app,&bot,"other-realm").unwrap().is_none());
        assert_eq!(claim(&app,&bot,"realm").unwrap(),Some("owned".into()));
        assert!(!touch(&app,&bot,&run,"realm","session","owned").unwrap());
        assert!(claim(&app,&bot,"realm").unwrap().is_none());
        finish(&app,"realm","owned",false).unwrap();assert!(claim(&app,&bot,"realm").unwrap().is_none());
        app.db.0.lock().unwrap().execute("UPDATE harness_native_threads SET retry_after=0",[]).unwrap();
        assert_eq!(claim(&app,&bot,"realm").unwrap(),Some("owned".into()));finish(&app,"realm","owned",true).unwrap();
        assert!(claim(&app,&bot,"realm").unwrap().is_none());
    }
    #[test]
    fn retained_checkpoint_and_pending_human_are_not_cleanup_candidates() {
        let (app,bot,run)=setup();let p=crate::harness_context::prepare(&app,&bot,&run,&[],None,"fixture",&json!({})).unwrap();
        owned(&app,&bot,&run,"realm",&p.session_id,"owned");
        assert!(crate::harness_context::complete(&app,&bot,&run,&p,&json!({"schema":1,"estimated_tokens":1})).unwrap());
        app.db.finish(&run.id,"completed","Done","").unwrap();assert!(claim(&app,&bot,"realm").unwrap().is_none());
        app.db.0.lock().unwrap().execute("UPDATE harness_native_threads SET updated=?",[db::now()-8*86400]).unwrap();
        app.db.0.lock().unwrap().execute("UPDATE runs SET status='awaiting_user' WHERE id=?",[&run.id]).unwrap();
        assert!(claim(&app,&bot,"realm").unwrap().is_none());
        app.db.0.lock().unwrap().execute("UPDATE runs SET status='completed' WHERE id=?",[&run.id]).unwrap();
        assert_eq!(claim(&app,&bot,"realm").unwrap(),Some("owned".into()));
    }
    #[test]
    fn unavailable_cleanup_caps_creation_and_unknown_ids_cannot_be_touched() {
        let (app,bot,run)=setup();
        assert!(!touch(&app,&bot,&run,"realm","session","arbitrary-sdk-id").unwrap());
        for i in 0..128 {owned(&app,&bot,&run,"realm",&format!("session-{i}"),&format!("owned-{i}"));}
        assert!(!reserve(&app,&bot,&run,"realm","overflow").unwrap());assert!(!reserve(&app,&bot,&run,"other-realm","overflow").unwrap());
        assert!(register(&app,&bot,&run,"realm","new-session","owned-0").is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn native_cleanup_protocol_is_bounded_idempotent_and_has_no_model_turns() {
        for mode in ["success","missing","unsupported"] {
            let (app,bot,run)=setup();
            for i in 0..10 {owned(&app,&bot,&run,"realm",&format!("session-{i}"),&format!("owned-{i}"));}
            app.db.finish(&run.id,"completed","Done","").unwrap();
            let script=r#"
import json,sys
mode=sys.argv[1];deleted=0
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='thread/delete':
  deleted+=1;assert deleted<=8 and v['params']['threadId'].startswith('owned-')
  if mode!='success':
   print(json.dumps({'id':v['id'],'error':{'code':-32601,'message':'unknown thread' if mode=='missing' else 'Method not found'}}),flush=True);continue
  result={}
 else:raise AssertionError('Cleanup must not invoke model/account work: '+str(v))
 print(json.dumps({'id':v['id'],'result':result}),flush=True)
"#;
            let mut command=tokio::process::Command::new("python3");command.args(["-u","-c",script,mode]);let mut rpc=Rpc::spawn(command).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5),drain(&app,&bot,"realm",&mut rpc)).await.unwrap().unwrap();drop(rpc);
            let (count,failures):(i64,i64)=app.db.0.lock().unwrap().query_row("SELECT COUNT(*),COALESCE(SUM(failures),0) FROM harness_native_threads",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!((count,failures),if mode=="unsupported" {(10,1)} else {(2,0)},"{mode}");
        }
    }
}
