//! Adversarial boundaries: no live providers or external side effects.
use crate::{connector_artifacts, db::{self, Bot, Run}, initiative, runtime::{self, Shared}, tests};
use rusqlite::params;
use serde_json::{Value, json};
use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};

fn fixture() -> (Shared, Bot, Run) {
    let app=tests::app();
    let mut bot=tests::bot(&app.db,"codex");
    bot.profile.initiative=true;bot.approval_mode="full".into();app.db.save_bot(&bot).unwrap();
    let id=app.db.queue(&bot.id,"Synthetic background check",0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![id,bot.id]).unwrap();
    let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
    (app,bot,run)
}
fn connector(origin:&str,read_only:bool)->(&'static str,Value) {
    let tool=match origin {"codex-account"=>"codex_connector","claude-account"=>"claude_connector",_=>"connector_execute"};
    (tool,json!({"origin":origin,"account_key":"fixture-account","connector_key":"drive","connection":"Google Drive","tool_name":if read_only{"google_drive.search"}else{"google_drive.create_file"},"arguments":{"name":"isolated synthetic fixture"},"permission_persistence_available":true,"forced":false,"read_only":read_only}))
}
async fn reviewed(app:&Shared,bot:&Bot,run:&Run,tool:&str,mut args:Value)->Value {
    let id=connector_artifacts::create(&app.db,run,&args).unwrap();args["artifact_id"]=json!(id);
    assert!(connector_artifacts::review(app,bot,run,tool,&mut args,false).await.unwrap());args
}

#[tokio::test]
async fn initiative_captured_bot_cannot_preserve_revoked_local_or_inherited_permission() {
    let (app,captured,run)=fixture();
    let args=json!({"action_scope":"external"});
    assert!(runtime::approve(&app,&captured,&run,"guest_exec",&args).await.unwrap());
    let mut live=captured.clone();live.approval_mode="ask".into();app.db.save_bot(&live).unwrap();
    assert!(runtime::approve(&app,&captured,&run,"guest_exec",&args).await.unwrap_err().to_string().contains("needs approval"));
    live.approval_mode="inherit".into();app.db.save_bot(&live).unwrap();
    app.db.save_setting("general",&json!({"approval_mode":"full"})).unwrap();
    assert!(runtime::approve(&app,&captured,&run,"guest_exec",&args).await.unwrap());
    app.db.save_setting("general",&json!({"approval_mode":"ask"})).unwrap();
    assert!(runtime::approve(&app,&captured,&run,"guest_exec",&args).await.is_err());
    assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
    assert_eq!(app.db.run(&run.id).unwrap().status,"running");
}

#[tokio::test]
async fn initiative_every_connector_rechecks_live_read_and_write_permissions_at_dispatch() {
    for origin in ["codex-account","claude-account","kindred"] {for read_only in [true,false] {
        let (app,bot,run)=fixture();let (tool,args)=connector(origin,read_only);
        let args=reviewed(&app,&bot,&run,tool,args).await;
        app.db.0.lock().unwrap().execute("INSERT INTO connector_grants VALUES(?,?,?,'drive','ask')",params![bot.id,origin,"fixture-account"]).unwrap();
        let result=connector_artifacts::dispatch(&app.db,&run,&args);
        assert!(result.is_err(),"{origin} read_only={read_only}: revoked connector dispatched");
        let status:String=app.db.0.lock().unwrap().query_row("SELECT status FROM connector_artifacts WHERE id=?",[args["artifact_id"].as_str().unwrap()],|r|r.get(0)).unwrap();
        assert_ne!(status,"executing");
        assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
    }}
}

#[tokio::test]
async fn initiative_disable_between_connector_review_and_dispatch_preserves_user_work() {
    for origin in ["codex-account","claude-account","kindred"] {
        let (app,mut bot,run)=fixture();let (tool,args)=connector(origin,false);
        let args=reviewed(&app,&bot,&run,tool,args).await;
        let ordinary=app.db.queue(&bot.id,"Independent owner message",0).unwrap();
        bot.profile.initiative=false;app.db.save_bot(&bot).unwrap();
        assert!(connector_artifacts::dispatch(&app.db,&run,&args).is_err());
        assert_eq!(app.db.run(&ordinary).unwrap().status,"queued");
        let c=app.db.0.lock().unwrap();
        assert_eq!(c.query_row("SELECT status FROM connector_artifacts WHERE id=?",[args["artifact_id"].as_str().unwrap()],|r|r.get::<_,String>(0)).unwrap(),"interrupted");
    }
}

#[tokio::test]
async fn initiative_full_access_drafts_do_not_inherit_email_send_permission() {
    for origin in ["codex-account","claude-account","kindred"] {
        let (app,bot,run)=fixture();let(tool,mut draft)=connector(origin,false);
        draft["connection"]=json!("Gmail");draft["connector_key"]=json!("gmail");draft["tool_name"]=json!("gmail.create_draft");
        assert!(runtime::approve(&app,&bot,&run,tool,&draft).await.unwrap());
        let mut send=draft.clone();send["tool_name"]=json!("gmail.send_email");
        assert!(runtime::approve(&app,&bot,&run,tool,&send).await.is_err());
        app.db.0.lock().unwrap().execute("INSERT INTO connector_action_grants VALUES(?,?,?,'gmail','email_send','allow')",params![bot.id,origin,"fixture-account"]).unwrap();
        assert!(runtime::approve(&app,&bot,&run,tool,&send).await.unwrap());
        app.db.0.lock().unwrap().execute("UPDATE connector_action_grants SET permission='ask' WHERE bot_id=?",[&bot.id]).unwrap();
        assert!(runtime::approve(&app,&bot,&run,tool,&send).await.is_err());
        assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
    }
}

#[tokio::test]
async fn initiative_setup_tools_cannot_create_questions_or_change_saved_permissions() {
    let(app,bot,run)=fixture();
    let account="a".repeat(64);
    crate::connector_policy::catalogue_for(&app.db,"codex-account",&account,&json!([{"origin":"codex-account","connector_key":"gmail","app_id":"gmail","name":"codex Gmail","display_name":"Gmail","status":"connected","permission_persistence_available":true,"tools":["gmail.send_email"]}])).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO connector_action_grants VALUES(?,'codex-account',?,'gmail','email_send','allow')",params![bot.id,account]).unwrap();
    let specs=runtime::tool_specs_for_run(&app,&bot,&run);
    for(name,args) in [("show_connector",json!({"toolkit":"gmail","account_name":"Synthetic"})),("connector_configure",json!({"request":"email_sending","source":"provider","connector_key":"gmail","policy":"ask"}))] {
        assert!(!specs.iter().any(|s|s["name"]==name),"{name} must not be advertised to initiative");
        let response=tokio::time::timeout(Duration::from_secs(2),runtime::call_tool(&app,&bot,&run,name,args)).await.unwrap().unwrap();
        assert_eq!(response["failed"],true,"{name} unexpectedly succeeded");
        assert!(response["text"].as_str().unwrap().contains("Initiative"),"{name}: {response}");
    }
    let c=app.db.0.lock().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM questions WHERE run_id=?",[&run.id],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(c.query_row("SELECT permission FROM connector_action_grants WHERE bot_id=?",[&bot.id],|r|r.get::<_,String>(0)).unwrap(),"allow");
    assert_eq!(c.query_row("SELECT status FROM runs WHERE id=?",[&run.id],|r|r.get::<_,String>(0)).unwrap(),"running");
}

#[tokio::test]
async fn initiative_helper_cannot_read_owner_dm_or_write_owner_checkpoints() {
    let(app,owner,run)=fixture();
    let private="PRIVATE_OWNER_SENTINEL_NEVER_DELEGATED";
    app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user',?,'message',?)",params![run.chat_id,private,db::now()]).unwrap();
    initiative::checkpoint(&app.db,&run,&json!({"notes":private})).unwrap();
    let helper=tests::bot(&app.db,"codex");
    let id=app.db.queue(&helper.id,"Find public contract templates only",0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![id,run.id,run.chat_id]).unwrap();
    let child=app.db.claim_bot(&helper.id).unwrap().unwrap();
    let packet=runtime::instructions(&app,&helper,&child).unwrap();
    assert!(!packet.contains(private));
    let read=runtime::call_tool(&app,&helper,&child,"chat_read",json!({"chat_id":run.chat_id,"limit":20})).await.unwrap();
    assert_eq!(read["failed"],true);assert!(!read.to_string().contains(private));
    let save=runtime::call_tool(&app,&helper,&child,"initiative_checkpoint",json!({"notes":"overwrite"})).await.unwrap();
    assert_eq!(save["failed"],true);
    assert_eq!(initiative::context(&app.db,&run).unwrap()["notes"],private);
    assert_eq!(app.db.bot(&owner.id).unwrap().memory,owner.memory);
}

#[tokio::test]
async fn initiative_disable_cancels_pending_provider_future_without_releasing_human_takeover() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {fn drop(&mut self){self.0.store(true,Ordering::SeqCst);}}
    let(app,mut bot,run)=fixture();let slot=app.db.screen(&bot.id).unwrap();
    app.db.screen_set_takeover(slot,true).unwrap();
    let ordinary=app.db.queue(&bot.id,"Independent queued work",0).unwrap();
    let routine=db::Routine{id:"preserved-routine".into(),bot_id:bot.id.clone(),name:"Inbox".into(),prompt:"Existing check".into(),interval_seconds:3600,next_run:db::now()+3600,enabled:true,schedule:None,run_at:None};
    app.db.save_routine(&routine).unwrap();
    let dropped=Arc::new(AtomicBool::new(false));let observed=dropped.clone();
    let work=async move{let _guard=Dropped(observed);std::future::pending::<anyhow::Result<String>>().await};
    let mut lease=None;
    let stop=async {tokio::time::sleep(Duration::from_millis(30)).await;bot.profile.initiative=false;app.db.save_bot(&bot).unwrap();};
    let (outcome,())=tokio::time::timeout(Duration::from_secs(2),async {tokio::join!(runtime::drive_run(&app,&run,slot,&mut lease,work),stop)}).await.unwrap();
    assert!(outcome.unwrap().0.unwrap_err().to_string().contains("turned off"));
    assert!(dropped.load(Ordering::SeqCst));
    assert!(app.db.screen_takeover(slot).unwrap());
    assert_eq!(app.db.run(&ordinary).unwrap().status,"queued");
    assert!(app.db.routines().unwrap().iter().any(|r|r.id==routine.id&&r.enabled&&r.next_run==routine.next_run));
    assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
}

#[test]
fn initiative_managed_command_stops_at_root_deadline_without_stopping_direct_command() {
    let(app,mut bot,run)=fixture();
    bot.profile.local_access=true;bot.profile.local_device_id="11111111-1111-4111-8111-111111111111".into();app.db.save_bot(&bot).unwrap();
    app.db.save_setting("general",&json!({"local_access":true})).unwrap();
    let direct=app.db.queue(&bot.id,"Explicit long-running command",0).unwrap();
    {
        let c=app.db.0.lock().unwrap();
        // The model has yielded via command_wait; no provider watchdog is active.
        c.execute("UPDATE runs SET status='completed',created=? WHERE id=?",params![db::now()-901,run.id]).unwrap();
        for (id,owner) in [("initiative-command",run.id.as_str()),("direct-command",direct.as_str())] {
            c.execute("INSERT INTO command_jobs(id,run_id,bot_id,device_id,screen,title,status,created) VALUES(?,?,?,'11111111-1111-4111-8111-111111111111',0,'Synthetic running job','running',?)",params![id,owner,bot.id,db::now()]).unwrap();
        }
        c.execute("INSERT INTO command_waits(run_id,ids,context) VALUES(?,'[\"initiative-command\"]','Resume after command')",[&run.id]).unwrap();
    }
    let jobs=crate::command_jobs::desktop_poll(&app,"11111111-1111-4111-8111-111111111111",&json!({"command_protocol":1})).unwrap();
    assert_eq!(jobs.iter().find(|j|j["id"]=="initiative-command").unwrap()["stop"],true,"Expired initiative command was left running after provider yielded");
    assert_eq!(jobs.iter().find(|j|j["id"]=="direct-command").unwrap()["stop"],false,"Ordinary command must retain its own runtime limit");
}

#[tokio::test]
async fn initiative_managed_command_launch_clamps_transport_to_remaining_root_budget() {
    use rusqlite::OptionalExtension;
    const DEVICE: &str = "22222222-2222-4222-8222-222222222222";
    for background in [false,true] {
        let(app,mut bot,run)=fixture();
        bot.profile.local_access=true;bot.profile.local_device_id=DEVICE.into();app.db.save_bot(&bot).unwrap();
        app.db.save_setting("general",&json!({"local_access":true})).unwrap();
        {
            let c=app.db.0.lock().unwrap();
            if !background {c.execute("DELETE FROM initiative_runs WHERE run_id=?",[&run.id]).unwrap();}
            c.execute("UPDATE runs SET created=? WHERE id=?",params![db::now()-600,run.id]).unwrap();
            c.execute("INSERT INTO local_devices(id,secret,name,mode,seen) VALUES(?,'synthetic-secret','Fixture desktop','full',?)",params![DEVICE,db::now()]).unwrap();
            c.execute("INSERT INTO local_command_capabilities(device_id,version) VALUES(?,1)",[DEVICE]).unwrap();
        }
        let worker_app=app.clone();let worker_bot=bot.clone();let worker_run=run.clone();
        let worker=tokio::spawn(async move {
            crate::command_jobs::start(&worker_app,&worker_bot,&worker_run,"local_exec",json!({"command":"synthetic fixture: never executed","title":"Budget transport fixture","max_seconds":7200})).await
        });
        let (request,encoded)=tokio::time::timeout(Duration::from_secs(2),async {
            loop {
                let found=app.db.0.lock().unwrap().query_row("SELECT id,args FROM local_requests WHERE run_id=?",[&run.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().unwrap();
                if let Some(found)=found {break found;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let sent:Value=serde_json::from_str(&encoded).unwrap();
        // Simulate only the desktop receipt; the synthetic command never runs.
        app.db.0.lock().unwrap().execute("UPDATE local_requests SET status='done',result=? WHERE id=?",params![json!({"status":"running","text":"Fixture transport acknowledged"}).to_string(),request]).unwrap();
        tokio::time::timeout(Duration::from_secs(2),worker).await.unwrap().unwrap().unwrap();
        let seconds=sent["max_seconds"].as_u64().unwrap();
        if background {assert!((290..=300).contains(&seconds),"Initiative launch requested {seconds}s after spending 600s");}
        else {assert_eq!(seconds,7200,"Direct command must preserve its requested duration");}
    }
}
