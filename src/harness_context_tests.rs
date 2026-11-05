//! Synthetic SQLite/context contract tests. No provider, credentials or VM.
use crate::{db::{self, Bot, Db, Run}, harness_context::{self, Prepared}, runtime::App};
use serde_json::{Value, json};
use std::sync::Arc;

fn start(app: &App, bot: &Bot, prompt: &str) -> Run {
    app.db.queue(&bot.id,prompt,0).unwrap();
    app.db.claim_bot(&bot.id).unwrap().unwrap()
}
fn prepare(app: &App, bot: &Bot, run: &Run) -> Prepared {
    harness_context::prepare(app,bot,run,&[],Some(128_000),"fixture-v1",&json!({"account":"synthetic-account"})).unwrap()
}
fn checkpoint() -> Value {
    json!({"schema":1,"estimated_tokens":20,"thread_id":"synthetic-native-thread","usage_totals":[100,20,80],"messages":[{"role":"assistant","content":"Completed once"}]})
}
fn finish(app: &App, bot: &Bot, run: &Run, prepared: &Prepared) {
    assert!(harness_context::complete(app,bot,run,prepared,&checkpoint()).unwrap());
    app.db.finish(&run.id,"completed","Completed once","").unwrap();
}
fn group(app: &App, members: Vec<String>) -> crate::chats::Chat {
    let chat=crate::chats::Chat {id:db::id(),name:"Synthetic shared destination".into(),members,
        archived:false,description:String::new(),bot_only:false,pinned:false,last_message:None};
    app.db.save_chat(&chat).unwrap();chat
}

#[test]
fn checkpoint_reopens_exact_same_chat_with_stable_instructions_and_fresh_input() {
    let root=std::env::temp_dir().join(format!("kindred-harness-context-{}",db::id()));
    std::fs::create_dir_all(&root).unwrap();let path=root.join("fixture.db");
    let mut app=crate::tests::app();Arc::get_mut(&mut app).unwrap().db=Db::open(path.to_str().unwrap()).unwrap();
    let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"FIRST synthetic task");
    let first=prepare(&app,&bot,&run);assert!(!first.resumed);assert!(first.checkpoint.is_none());
    finish(&app,&bot,&run,&first);drop(app);
    let mut app=crate::tests::app();Arc::get_mut(&mut app).unwrap().db=Db::open(path.to_str().unwrap()).unwrap();
    let next=start(&app,&bot,"SECOND synthetic question");let second=prepare(&app,&bot,&next);
    assert!(second.resumed);assert_eq!(first.session_id,second.session_id);assert_eq!(second.checkpoint,Some(checkpoint()));
    assert_eq!(first.instructions,second.instructions);assert_ne!(first.live_context,second.live_context);
    assert!(!second.instructions.contains("FIRST synthetic task"));assert!(!second.instructions.contains("SECOND synthetic question"));
    assert!(second.input(&next.prompt).ends_with("# Current user input\nSECOND synthetic question"));
    drop(app);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn checkpoint_identity_tool_model_engine_window_and_policy_changes_reset() {
    for change in ["model","auth","tools","engine","window","role","approval","general-policy","profile"] {
        let app=crate::tests::app();let mut bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"First");
        let first=prepare(&app,&bot,&run);finish(&app,&bot,&run,&first);
        match change {
            "model"=>bot.model="another-model".into(),
            "role"=>bot.instructions="A revised role".into(),
            "approval"=>bot.auto_approve=true,
            "profile"=>bot.profile.color="#123456".into(),
            "general-policy"=>app.db.save_setting("general",&json!({"local_access":true})).unwrap(),
            _=>{}
        }
        app.db.save_bot(&bot).unwrap();let next=start(&app,&bot,"Second");
        let tools=if change=="tools" {vec![json!({"name":"new_tool","description":"Synthetic","inputSchema":{"type":"object"}})]} else {vec![]};
        let identity=json!({"account":if change=="auth" {"OTHER_SYNTHETIC_SECRET"} else {"synthetic-account"}});
        let second=harness_context::prepare(&app,&bot,&next,&tools,Some(if change=="window" {64_000} else {128_000}),if change=="engine" {"fixture-v2"} else {"fixture-v1"},&identity).unwrap();
        assert!(!second.resumed,"{change}");assert!(second.checkpoint.is_none(),"{change}");assert_ne!(first.session_id,second.session_id,"{change}");
        let diagnostics=serde_json::to_string(&app.db.events(&next.id).unwrap()).unwrap();assert!(!diagnostics.contains("OTHER_SYNTHETIC_SECRET"));assert!(!diagnostics.contains("synthetic-native-thread"));
    }
}

#[test]
fn checkpoint_bot_chat_database_and_membership_are_isolated() {
    let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let peer=crate::tests::bot(&app.db,"codex");
    let mut room=group(&app,vec![bot.id.clone()]);
    let run=start(&app,&bot,"Private first");let first=prepare(&app,&bot,&run);finish(&app,&bot,&run,&first);
    let peer_run=start(&app,&peer,"Other bot");let other=prepare(&app,&peer,&peer_run);assert!(!other.resumed);assert_ne!(first.session_id,other.session_id);
    app.db.finish(&peer_run.id,"completed","Done","").unwrap();
    app.db.chat_send(&room.id,"Group task",&[bot.id.clone()]).unwrap();let shared=app.db.claim_bot(&bot.id).unwrap().unwrap();
    let group_context=prepare(&app,&bot,&shared);assert!(!group_context.resumed);assert_ne!(first.session_id,group_context.session_id);finish(&app,&bot,&shared,&group_context);
    room.members.push(peer.id.clone());app.db.save_chat(&room).unwrap();
    app.db.chat_send(&room.id,"Changed audience",&[bot.id.clone()]).unwrap();let shared=app.db.claim_bot(&bot.id).unwrap().unwrap();
    let changed=prepare(&app,&bot,&shared);assert!(!changed.resumed);assert_ne!(group_context.session_id,changed.session_id);
    let elsewhere=crate::tests::app();elsewhere.db.save_bot(&bot).unwrap();let run=start(&elsewhere,&bot,"Different database");
    let isolated=prepare(&elsewhere,&bot,&run);assert!(!isolated.resumed);assert_ne!(first.session_id,isolated.session_id);
}

#[test]
fn checkpoint_forgetting_source_invalidates_saved_and_inflight_context() {
    for action in ["forget","supersede"] {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"Original work");
        let seq={let c=app.db.0.lock().unwrap();c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user','PRIVATE_SOURCE_SENTINEL','message',?,?)",rusqlite::params![run.chat_id,run.id,db::now()]).unwrap();c.last_insert_rowid()};
        let first=prepare(&app,&bot,&run);finish(&app,&bot,&run,&first);
        let next=start(&app,&bot,"Continue safely");let inflight=prepare(&app,&bot,&next);assert!(inflight.resumed);
        let mut args=json!({"action":action,"expected_text":"PRIVATE_SOURCE_SENTINEL","expected_revision":0});
        if action=="supersede" {
            let c=app.db.0.lock().unwrap();c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user','Corrected public fact','message',?,?)",rusqlite::params![run.chat_id,run.id,db::now()]).unwrap();
            args["successor"]=json!(c.last_insert_rowid());
        }
        crate::continuity::store::revise_source(&app.db,seq,&args).unwrap();
        assert!(!harness_context::complete(&app,&bot,&next,&inflight,&checkpoint()).unwrap());
        app.db.finish(&next.id,"completed","Done","").unwrap();
        let after=start(&app,&bot,"New read");let cold=prepare(&app,&bot,&after);assert!(!cold.resumed);assert!(cold.checkpoint.is_none());
        if action=="forget" {assert!(!cold.live_context.contains("PRIVATE_SOURCE_SENTINEL"));}
    }
}

#[test]
fn checkpoint_cas_rejects_stale_writer_and_inflight_reuse() {
    let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"Concurrent lease fixture");
    let first=prepare(&app,&bot,&run);let second=prepare(&app,&bot,&run);
    assert_ne!(first.session_id,second.session_id);assert!(!second.resumed);
    assert!(!harness_context::complete(&app,&bot,&run,&first,&checkpoint()).unwrap());
    assert!(harness_context::complete(&app,&bot,&run,&second,&checkpoint()).unwrap());
    let third=prepare(&app,&bot,&run);assert!(!third.resumed);assert!(third.checkpoint.is_none());
    assert!(!harness_context::complete(&app,&bot,&run,&second,&checkpoint()).unwrap());
}

#[test]
fn checkpoint_failed_interrupted_and_cancelled_runs_never_resume() {
    for state in ["failed","interrupted","cancelled"] {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"Potentially partial work");
        let first=prepare(&app,&bot,&run);assert!(harness_context::complete(&app,&bot,&run,&first,&checkpoint()).unwrap());
        app.db.finish(&run.id,state,"","synthetic failure").unwrap();let next=start(&app,&bot,"Inspect current state");
        let second=prepare(&app,&bot,&next);assert!(!second.resumed,"{state}");assert!(second.checkpoint.is_none());
    }
}

#[test]
fn checkpoint_corruption_age_schema_and_budget_fail_closed() {
    for damage in ["corrupt","expired","future","budget","schema","oversize"] {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"First");let first=prepare(&app,&bot,&run);
        let mut saved=checkpoint();
        if damage=="budget" {saved["estimated_tokens"]=json!(128_000);}
        assert!(harness_context::complete(&app,&bot,&run,&first,&saved).unwrap());app.db.finish(&run.id,"completed","Done","").unwrap();
        {
            let c=app.db.0.lock().unwrap();
            match damage {
                "corrupt"=>{c.execute("UPDATE harness_contexts SET checkpoint='corrupt bytes'",[]).unwrap();},
                "expired"=>{c.execute("UPDATE harness_contexts SET updated=?",[db::now()-8*86400]).unwrap();},
                "future"=>{c.execute("UPDATE harness_contexts SET updated=?",[db::now()+86400]).unwrap();},
                "schema"|"oversize"=>{
                    let value=if damage=="schema" {json!({"schema":99,"estimated_tokens":20})} else {json!({"schema":1,"estimated_tokens":20,"data":"x".repeat(4*1024*1024)})};
                    let body=value.to_string();let hash=ring::digest::digest(&ring::digest::SHA256,body.as_bytes()).as_ref().iter().map(|v|format!("{v:02x}")).collect::<String>();
                    c.execute("UPDATE harness_contexts SET checkpoint=?,checkpoint_hash=?",rusqlite::params![body,hash]).unwrap();
                },_=>{}
            }
        }
        let next=start(&app,&bot,"Second");let second=prepare(&app,&bot,&next);assert!(!second.resumed,"{damage}");assert!(second.checkpoint.is_none(),"{damage}");
    }
}

#[test]
fn checkpoint_rejects_oversize_and_changed_policy_on_completion() {
    let app=crate::tests::app();let mut bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"First");let first=prepare(&app,&bot,&run);
    let mut too_big=checkpoint();too_big["data"]=json!("x".repeat(4*1024*1024));
    assert!(!harness_context::complete(&app,&bot,&run,&first,&too_big).unwrap());
    bot.auto_approve=true;app.db.save_bot(&bot).unwrap();
    assert!(!harness_context::complete(&app,&bot,&run,&first,&checkpoint()).unwrap());
}

#[test]
fn checkpoint_export_excludes_native_history_and_provider_identity() {
    let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");let run=start(&app,&bot,"Export fixture");let first=prepare(&app,&bot,&run);
    let mut saved=checkpoint();saved["private_native_history"]=json!("NATIVE_CACHE_SECRET_SENTINEL");
    assert!(harness_context::complete(&app,&bot,&run,&first,&saved).unwrap());app.db.finish(&run.id,"completed","Done","").unwrap();
    let package=app.db.prepare_transfer(&db::id(),"Synthetic export").unwrap();
    assert!(package["tables"].get("harness_contexts").is_none());assert!(!package.to_string().contains("NATIVE_CACHE_SECRET_SENTINEL"));assert!(!package.to_string().contains("synthetic-native-thread"));
}

#[test]
fn checkpoint_memory_clear_or_replace_invalidates_retained_and_inflight_history() {
    for replacement in ["", "Owner corrected the durable memory"] {
        let app=crate::tests::app();let mut bot=crate::tests::bot(&app.db,"codex");
        bot.memory="PRIVATE_FORGOTTEN_MEMORY_SENTINEL".into();app.db.save_bot(&bot).unwrap();
        let run=start(&app,&bot,"Read initial memory");let first=prepare(&app,&bot,&run);
        finish(&app,&bot,&run,&first);
        bot.memory=replacement.into();app.db.save_bot(&bot).unwrap();
        let next=start(&app,&bot,"Use current memory");let cold=prepare(&app,&bot,&next);
        assert!(!cold.resumed);assert_ne!(cold.session_id,first.session_id);
        assert!(!cold.live_context.contains("PRIVATE_FORGOTTEN_MEMORY_SENTINEL"));
        finish(&app,&bot,&next,&cold);
        let current=start(&app,&bot,"Continue before another memory edit");
        let inflight=prepare(&app,&bot,&current);assert!(inflight.resumed);
        bot.memory="Another owner correction during this turn".into();app.db.save_bot(&bot).unwrap();
        assert!(!harness_context::complete(&app,&bot,&current,&inflight,&checkpoint()).unwrap());
        app.db.finish(&current.id,"completed","Done","").unwrap();
        let after=start(&app,&bot,"Read corrected memory again");let reset=prepare(&app,&bot,&after);
        assert!(!reset.resumed);assert_ne!(reset.session_id,inflight.session_id);
        let diagnostics=serde_json::to_string(&app.db.events(&after.id).unwrap()).unwrap();
        assert!(!diagnostics.contains("PRIVATE_FORGOTTEN_MEMORY_SENTINEL"));
        assert!(!diagnostics.contains("Another owner correction"));
    }
}

#[test]
fn checkpoint_standing_rule_revocation_invalidates_saved_and_inflight_policy() {
    for during_turn in [false,true] {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        let run=start(&app,&bot,"For client replies, prepare drafts and report to me. Stop before sending.");
        let seq:i64=app.db.0.lock().unwrap().query_row("SELECT max(seq) FROM chat_messages WHERE sender='user'",[],|r|r.get(0)).unwrap();
        crate::work_rules::save(&app.db,&run,&json!({"key":"client-replies","expected_revision":0,"source_message_seq":seq,"enabled":true,"scope":"owner_workspace","trigger":"A client reply","allowed_actions":"Prepare drafts","stop_before":"Sending email","report_to":"owner_dm"})).unwrap();
        let first=prepare(&app,&bot,&run);finish(&app,&bot,&run,&first);
        let next=start(&app,&bot,"Revoke the client replies scope.");
        let inflight=if during_turn {let p=prepare(&app,&bot,&next);assert!(p.resumed);Some(p)} else {None};
        let seq:i64=app.db.0.lock().unwrap().query_row("SELECT max(seq) FROM chat_messages WHERE sender='user'",[],|r|r.get(0)).unwrap();
        crate::work_rules::save(&app.db,&next,&json!({"key":"client-replies","expected_revision":1,"source_message_seq":seq,"enabled":false})).unwrap();
        if let Some(inflight)=inflight {
            assert!(!harness_context::complete(&app,&bot,&next,&inflight,&checkpoint()).unwrap());
            app.db.finish(&next.id,"completed","Revoked","").unwrap();
            let after=start(&app,&bot,"Read current scope");let cold=prepare(&app,&bot,&after);
            assert!(!cold.resumed);assert_ne!(cold.session_id,inflight.session_id);
        } else {
            let cold=prepare(&app,&bot,&next);assert!(!cold.resumed);assert_ne!(cold.session_id,first.session_id);
        }
    }
}
