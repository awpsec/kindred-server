//! Synthetic attempts use the real durable recovery/finish path; no providers,
//! credentials, VM, or external side effects are needed.
use super::*;
use crate::{db, routine_completion as completion, routine_recovery as recovery, tests};
use std::{cell::Cell, future::ready};

fn scheduled(app: &App, provider: &str) -> (db::Bot, Run) {
    let bot = tests::bot(&app.db, provider);
    let id = app.db.queue(&bot.id, "Check synthetic state", 0).unwrap();
    { let c = app.db.0.lock().unwrap();
      c.execute("INSERT INTO routine_runs VALUES(?,'synthetic-routine',0)", [&id]).unwrap();
      completion::register(&c,&id,&id).unwrap(); }
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    (bot,run)
}
fn read(app: &App, run: &Run, args: Value, failed: bool, retryable: bool) {
    let call = db::id();
    app.db.event(&run.id,"tool_requested",json!({"tool":"connector_tools","call_id":call,"args":args})).unwrap();
    app.db.event(&run.id,"tool_result",json!({"tool":"connector_tools","call_id":call,"failed":failed,"retryable_read":retryable,"text":if failed {"Temporary read failure"} else {"Verified read"}})).unwrap();
}
fn complete(app: &App, run: &Run, kind: &str) -> Result<Value> {
    completion::complete(&app.db.0.lock().unwrap(),&run.id,&json!({"outcome":kind,"report":if kind=="quiet" {""} else {"The read could not finish."}}))
}
fn finish(app: &App, run: &Run, result: Result<String>) {
    match result { Ok(out) => app.db.finish(&run.id,"completed",&out,"").unwrap(), Err(e) => app.db.finish(&run.id,"failed","",&e.to_string()).unwrap() }
    app.db.event(&run.id,"run_finished",json!({})).unwrap();
    app.db.chat_complete(run).unwrap();
}
fn retries(app: &App, run: &Run) -> usize { recovery::spent(&app.db.0.lock().unwrap(), &run.id).unwrap() }

#[tokio::test]
async fn reported_read_error_recovers_quietly_for_every_provider_without_resetting_progress() {
    for provider in ["codex","claude-code","openrouter","custom-00000000-0000-4000-8000-000000000001"] {
        let app=tests::app(); let (bot,run)=scheduled(&app,provider); let calls=Cell::new(0);
        let result=retry(&app,&run,|| {
            calls.set(calls.get()+1);
            if calls.get()==1 {
                read(&app,&run,json!({"query":"already done"}),false,false);
                read(&app,&run,json!({"query":"unfinished"}),true,true);
                complete(&app,&run,"error").unwrap();
            } else {
                assert!(complete(&app,&run,"quiet").is_err(),"backoff is not proof the read succeeded");
                assert_eq!(app.db.run(&run.id).unwrap().status,"running");
                assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="result"));
                let context=crate::runtime::instructions_for(&app,&bot,&run,&crate::runtime::tool_specs_for_run(&app,&bot,&run),None).unwrap();
                assert!(context.contains("Verified read") && context.contains("Continue only unfinished work"));
                read(&app,&run,json!({"query":"different"}),false,false);
                assert!(complete(&app,&run,"quiet").is_err(),"different read cannot clear the error");
                read(&app,&run,json!({"query":"unfinished"}),false,false);
                complete(&app,&run,"quiet").unwrap();
            }
            ready(Ok(String::new()))
        },&[Duration::ZERO;5]).await;
        finish(&app,&run,result);
        assert_eq!(calls.get(),2); assert_eq!(retries(&app,&run),1);
        assert_eq!(app.db.run(&run.id).unwrap().status,"completed");
        assert!(app.db.run(&run.id).unwrap().output.is_empty());
        assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="result"));
        assert_eq!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="routine_recovery_outcome").count(),1);
        assert_eq!(app.db.runs(None).unwrap().len(),1);
    }
}

#[tokio::test]
async fn mixed_connection_and_reported_read_failure_share_two_attempts_and_one_final_card() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let calls=Cell::new(0);
    let result=retry(&app,&run,|| {
        calls.set(calls.get()+1);
        if calls.get()==1 { return ready(Err(anyhow::anyhow!("Codex request timed out"))); }
        read(&app,&run,json!({"query":"unfinished"}),true,true);
        complete(&app,&run,"error").unwrap();
        ready(Ok(String::new()))
    },&[Duration::ZERO;5]).await;
    assert_eq!(calls.get(),3); assert_eq!(retries(&app,&run),2);
    finish(&app,&run,result); app.db.chat_complete(&run).unwrap();
    assert_eq!(app.db.run(&run.id).unwrap().status,"failed");
    assert_eq!(app.db.chat_messages(&run.chat_id).unwrap().iter().filter(|m|m["kind"]=="result").count(),1);
    assert!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="provider_retry").all(|e|e["body"]["limit"]==2));
}

#[tokio::test]
async fn unclassified_read_failure_recovers_but_unexplained_empty_result_does_not() {
    for recoverable in [true,false] {
        let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let calls=Cell::new(0);
        let result=retry(&app,&run,|| {
            calls.set(calls.get()+1);
            if calls.get()==1 && recoverable {
                read(&app,&run,json!({}),true,true);
                return ready(Err(recovery::Unclassified(crate::providers::routine_empty_error(&app,&run).unwrap().unwrap()).into()));
            }
            if recoverable { read(&app,&run,json!({}),false,false); complete(&app,&run,"quiet").unwrap(); }
            ready(Ok(String::new()))
        },&[Duration::ZERO;5]).await;
        finish(&app,&run,result);
        assert_eq!(calls.get(),if recoverable {2}else{1});
        assert_eq!(app.db.run(&run.id).unwrap().status,if recoverable {"completed"}else{"failed"});
    }
}

#[tokio::test]
async fn intentional_silence_needs_no_recovery() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"claude-code"); let calls=Cell::new(0);
    let result=retry(&app,&run,|| { calls.set(calls.get()+1); complete(&app,&run,"quiet").unwrap(); ready(Ok(String::new())) },&[Duration::ZERO;5]).await;
    finish(&app,&run,result); assert_eq!(calls.get(),1); assert_eq!(retries(&app,&run),0);
    assert_eq!(app.db.run(&run.id).unwrap().status,"completed");
}

#[tokio::test]
async fn permission_quota_unknown_and_external_effects_are_never_replayed() {
    for case in ["permission","quota","unknown","write","uncertain","missing_receipt"] {
        let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let calls=Cell::new(0);
        if matches!(case,"write"|"uncertain"|"missing_receipt") {
            let tool=if case=="write" {"send_to_bot"} else {"connector_tools"};
            app.db.event(&run.id,"tool_requested",json!({"tool":tool,"call_id":"effect","args":{"read_only":true}})).unwrap();
            if case!="missing_receipt" { app.db.event(&run.id,"tool_result",json!({"tool":tool,"call_id":"effect","failed":false,"uncertain_effect":case=="uncertain"})).unwrap(); }
        }
        let result=retry(&app,&run,|| {
            calls.set(calls.get()+1);
            read(&app,&run,json!({}),true,true);
            let error=match case {
                "permission"=>anyhow::Error::new(crate::rpc::CodexTurnError::from_value(&json!({"codexErrorInfo":"unauthorized"}))).context("Codex request timed out"),
                "quota"=>anyhow::Error::new(crate::rpc::CodexTurnError::from_value(&json!({"codexErrorInfo":"usageLimitExceeded"}))),
                "unknown"=>anyhow::anyhow!("Unknown provider failure"),
                _=>anyhow::anyhow!("Codex request timed out"),
            };
            ready(Err(error))
        },&[Duration::ZERO;5]).await;
        assert!(result.is_err()); assert_eq!(calls.get(),1,"{case}"); assert_eq!(retries(&app,&run),0,"{case}");
    }
}

#[tokio::test]
async fn retry_budget_survives_reentry_and_manual_continue_gets_one_fresh_attempt() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let calls=Cell::new(0);
    let result=retry(&app,&run,|| { calls.set(calls.get()+1); ready(Err(anyhow::anyhow!("Codex request timed out"))) },&[Duration::ZERO;5]).await;
    assert_eq!(calls.get(),3); assert_eq!(retries(&app,&run),2);
    assert!(retry(&app,&run,|| { calls.set(calls.get()+1); ready(Ok(String::new())) },&[Duration::ZERO;5]).await.is_err());
    assert_eq!(calls.get(),3,"reentry cannot reset the budget");
    finish(&app,&run,result);
    let next_id=app.db.continue_task(&run.id).unwrap(); let next=app.db.claim_bot(&run.bot_id).unwrap().unwrap(); assert_eq!(next.id,next_id);
    let more=Cell::new(0);
    let result=retry(&app,&next,|| { more.set(more.get()+1); ready(Err(anyhow::anyhow!("Codex request timed out"))) },&[Duration::ZERO;5]).await;
    assert!(result.is_err()); assert_eq!(more.get(),1); assert_eq!(retries(&app,&next),2);
}

#[tokio::test]
async fn cancellation_during_routine_backoff_starts_no_second_call() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let calls=Cell::new(0);
    let work=retry(&app,&run,|| { calls.set(calls.get()+1); ready(Err(anyhow::anyhow!("Codex request timed out"))) },&[Duration::from_millis(20);5]);
    let stop=async { tokio::time::sleep(Duration::from_millis(5)).await; app.db.cancel(&run.id).unwrap(); };
    let (result,())=tokio::join!(work,stop); assert!(result.is_err()); assert_eq!(calls.get(),1);
}

#[test]
fn structured_http_and_codex_quota_denials_override_wrapping_timeouts() {
    for status in [401,403,402,429] {
        let e=anyhow::Error::new(recovery::HttpFailure{status,message:"Denied".into()}).context("Codex request timed out");
        assert!(!recovery::transport_error(&e));
    }
    for code in ["unauthorized","usageLimitExceeded","rateLimitExceeded","sessionBudgetExceeded","cyberPolicy"] {
        let e=anyhow::Error::new(crate::rpc::CodexTurnError::from_value(&json!({"codexErrorInfo":code}))).context("Codex request timed out");
        assert!(!recovery::transport_error(&e));
    }
    for status in [408,502,503,504] { assert!(recovery::transport_error(&anyhow::Error::new(recovery::HttpFailure{status,message:"Temporary".into()}))); }
}

#[test]
fn nonretryable_read_failure_stays_blocked_until_identical_verified_success() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex");
    read(&app,&run,json!({"query":"same"}),true,false); // permission/quota/unknown
    read(&app,&run,json!({"query":"same"}),true,true); // later timeout cannot hide it
    assert!(!recovery::safe_to_resume(&app.db.0.lock().unwrap(),&run.id).unwrap());
    assert!(complete(&app,&run,"quiet").is_err());
    read(&app,&run,json!({"query":"same"}),false,false);
    assert!(recovery::safe_to_resume(&app.db.0.lock().unwrap(),&run.id).unwrap());
    complete(&app,&run,"quiet").unwrap();
}

#[test]
fn malformed_or_forged_read_success_cannot_clear_a_failed_read() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex");
    read(&app,&run,json!({}),true,true);
    for tool in ["connector_tools","send_to_bot"] {
        let call=db::id();
        app.db.event(&run.id,"tool_requested",json!({"tool":tool,"args":{},"call_id":call})).unwrap();
        // Omitted failed flag is not a positive receipt. A mismatched tool name
        // cannot pretend a write is a successful read.
        let mut body=json!({"tool":"connector_tools","call_id":call});
        if tool=="send_to_bot" { body["failed"]=json!(false); }
        app.db.event(&run.id,"tool_result",body).unwrap();
        assert!(complete(&app,&run,"quiet").is_err());
    }
}

#[tokio::test]
async fn explicit_nonrecoverable_routine_error_is_final_without_retry() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"claude-code"); let calls=Cell::new(0);
    let result=retry(&app,&run,|| {
        calls.set(calls.get()+1); read(&app,&run,json!({}),true,false);
        complete(&app,&run,"error").unwrap(); ready(Ok(String::new()))
    },&[Duration::ZERO;5]).await;
    finish(&app,&run,result); assert_eq!(calls.get(),1); assert_eq!(retries(&app,&run),0);
    assert_eq!(app.db.run(&run.id).unwrap().status,"failed");
}

#[test]
fn unfinished_reads_survive_continuation_and_other_routines_cannot_clear_them() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex");
    read(&app,&run,json!({"query":"unfinished"}),true,true);
    finish(&app,&run,Err(anyhow::anyhow!("Synthetic interruption")));
    let next=app.db.continue_task(&run.id).unwrap(); let next=app.db.run(&next).unwrap();
    let (_other_bot,other)=scheduled(&app,"codex");
    read(&app,&other,json!({"query":"unfinished"}),false,false);
    assert!(complete(&app,&next,"quiet").is_err());
    read(&app,&next,json!({"query":"unfinished"}),false,false);
    complete(&app,&next,"quiet").unwrap();
}

#[test]
fn connector_read_proof_comes_from_dispatch_not_model_argument() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex");
    let args=json!({"origin":"codex-account","account_key":"fixture","connector_key":"slack","tool_name":"verified-read","arguments":{},"read_only":true});
    let call=db::id();
    app.db.event(&run.id,"tool_requested",json!({"tool":"codex_connector","call_id":call,"args":args})).unwrap();
    assert!(!recovery::read_call(&app.db.0.lock().unwrap(),&run.id,"codex_connector",&call).unwrap());
    let artifact=crate::connector_artifacts::create(&app.db,&run,&args).unwrap();
    let mut trusted=args.clone(); trusted["artifact_id"]=json!(artifact);
    app.db.event(&run.id,"tool_started",json!({"tool":"codex_connector","args":trusted})).unwrap();
    crate::connector_artifacts::complete(&app.db,&artifact,&json!({"text":"Verified read"}),false).unwrap();
    app.db.event(&run.id,"tool_result",json!({"tool":"codex_connector","call_id":call,"failed":false})).unwrap();
    assert!(recovery::read_call(&app.db.0.lock().unwrap(),&run.id,"codex_connector",&call).unwrap());
    assert!(recovery::safe_to_resume(&app.db.0.lock().unwrap(),&run.id).unwrap());
    let mut write=args; write["read_only"]=json!(false);
    let write_artifact=crate::connector_artifacts::create(&app.db,&run,&write).unwrap();
    crate::connector_artifacts::complete(&app.db,&write_artifact,&json!({"text":"Sent once"}),false).unwrap();
    assert!(!recovery::safe_to_resume(&app.db.0.lock().unwrap(),&run.id).unwrap());
}

#[tokio::test]
async fn real_dispatcher_allows_quiet_after_refused_quiet_and_verified_readback() {
    let app=tests::app(); let (bot,run)=scheduled(&app,"codex");
    read(&app,&run,json!({"query":"finish this"}),true,true);
    let denied=crate::runtime::call_tool(&app,&bot,&run,"finish_quietly",json!({})).await.unwrap();
    assert_eq!(denied["failed"],true);
    assert!(app.db.events(&run.id).unwrap().iter().any(|e|e["kind"]=="tool_result" && e["body"]["tool"]=="finish_quietly" && e["body"]["failed"]==true));
    read(&app,&run,json!({"query":"finish this"}),false,false);
    let result=crate::runtime::call_tool(&app,&bot,&run,"finish_quietly",json!({})).await.unwrap();
    assert_eq!(result["finish_quietly"],true);
    finish(&app,&run,Ok(String::new()));
    assert_eq!(app.db.run(&run.id).unwrap().status,"completed");
    assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="result"));
}

#[test]
fn actual_pi_error_frames_preserve_http_status_and_normal_retry_policy() {
    for provider in ["openrouter","custom-fixture","opencode","opencode-go"] {
        for status in [401,429,503] {
            let error=crate::pi::provider_failure(provider,&json!({"type":"error","code":"provider_http","status":status}));
            assert_eq!(error.downcast_ref::<recovery::HttpFailure>().unwrap().status,status as u16);
            assert_eq!(recovery::transport_error(&error),status==503,"{provider} {status}");
            assert!(error.to_string().contains(&format!("HTTP {status}")));
            if provider=="openrouter" || provider.starts_with("custom-") {
                assert_eq!(super::transient(&error),status==429 || status==503,"ordinary task retry policy is unchanged");
            }
        }
        let redirect=crate::pi::provider_failure(provider,&json!({"type":"error","code":"transport"}));
        assert!(!recovery::transport_error(&redirect),"opaque redirects are not safe recovery evidence");
    }
    let denied=anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).context("Codex request timed out");
    assert!(!recovery::transport_error(&denied));
}

fn connector_card(app: &App, run: &Run, readonly: bool, failed: bool, retryable: bool) -> String {
    let args=json!({"origin":"codex-account","account_key":"fixture","connector_key":"slack","tool_name":"fixture-action","arguments":{},"read_only":readonly});
    let call=db::id();
    app.db.event(&run.id,"tool_requested",json!({"tool":"codex_connector","call_id":call,"args":args})).unwrap();
    let artifact=crate::connector_artifacts::create(&app.db,run,&args).unwrap();
    let mut trusted=args; trusted["artifact_id"]=json!(artifact);
    app.db.event(&run.id,"tool_started",json!({"tool":"codex_connector","args":trusted})).unwrap();
    crate::connector_artifacts::complete(&app.db,&artifact,&json!({"text":"Fixture receipt"}),failed).unwrap();
    app.db.event(&run.id,"tool_result",json!({"tool":"codex_connector","call_id":call,"failed":failed,"retryable_read":retryable})).unwrap();
    artifact
}
fn suppressed(app: &App, id: &str) -> bool {
    app.db.0.lock().unwrap().query_row("SELECT suppressed FROM chat_messages WHERE kind='connector_artifact' AND body=?",[id],|r|r.get(0)).unwrap()
}

#[tokio::test]
async fn exhausted_read_recovery_leaves_one_failure_and_private_artifact_receipts() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex"); let cards=std::cell::RefCell::new(Vec::<String>::new());
    let result=retry(&app,&run,|| {
        for id in cards.borrow().iter() { assert!(suppressed(&app,id)); }
        cards.borrow_mut().push(connector_card(&app,&run,true,true,true));
        complete(&app,&run,"error").unwrap(); ready(Ok(String::new()))
    },&[Duration::ZERO;5]).await;
    assert_eq!(cards.borrow().len(),3);
    for id in cards.borrow().iter() {
        assert!(suppressed(&app,id));
        assert_eq!(crate::connector_artifacts::record(&app.db.0.lock().unwrap(),id).unwrap()["status"],"failed");
    }
    finish(&app,&run,result);
    let messages=app.db.chat_messages(&run.chat_id).unwrap();
    assert_eq!(messages.iter().filter(|m|m["kind"]=="result").count(),1);
    assert!(!messages.iter().any(|m|m["kind"]=="connector_artifact"));
}

#[test]
fn artifact_suppression_never_hides_writes_successes_or_nonrecoverable_failures() {
    let app=tests::app(); let (_bot,run)=scheduled(&app,"codex");
    let eligible=connector_card(&app,&run,true,true,true);
    let denied=connector_card(&app,&run,true,true,false);
    let write=connector_card(&app,&run,false,true,true);
    let success=connector_card(&app,&run,true,false,true);
    recovery::suppress_failed_read_cards(&app.db.0.lock().unwrap(),&run.id).unwrap();
    assert!(suppressed(&app,&eligible));
    for id in [denied,write,success] { assert!(!suppressed(&app,&id)); }
}

#[tokio::test]
async fn sqlite_restart_retains_budget_and_requires_explicit_continuation() {
    let path=std::env::temp_dir().join(format!("kindred-routine-recovery-{}.db",db::id()));
    let mut app=tests::app();
    std::sync::Arc::get_mut(&mut app).unwrap().db=db::Db::open(path.to_str().unwrap()).unwrap();
    let (_bot,run)=scheduled(&app,"codex");
    read(&app,&run,json!({"query":"already read"}),false,false);
    recovery::enter(&app,&run).unwrap();
    assert!(recovery::reserve(&app,&run,Duration::from_secs(5)).unwrap());
    assert_eq!(retries(&app,&run),1);
    drop(app);
    let mut app=tests::app();
    std::sync::Arc::get_mut(&mut app).unwrap().db=db::Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(app.db.run(&run.id).unwrap().status,"interrupted");
    assert_eq!(retries(&app,&run),1);
    assert!(app.db.claim_bot(&run.bot_id).unwrap().is_none());
    let calls=Cell::new(0);
    assert!(retry(&app,&run,|| {calls.set(calls.get()+1);ready(Ok(String::new()))},&[Duration::ZERO;5]).await.is_err());
    assert_eq!(calls.get(),0);
    let next=app.db.continue_task(&run.id).unwrap();let next=app.db.run(&next).unwrap();
    let next=app.db.claim_bot(&next.bot_id).unwrap().unwrap();
    let result=retry(&app,&next,|| {
        calls.set(calls.get()+1);
        if calls.get()==1 {ready(Err(anyhow::anyhow!("Codex request timed out")))}
        else {complete(&app,&next,"quiet").unwrap();ready(Ok(String::new()))}
    },&[Duration::ZERO;5]).await;
    assert_eq!(calls.get(),2);assert_eq!(retries(&app,&next),2);
    finish(&app,&next,result);assert_eq!(app.db.run(&next.id).unwrap().status,"completed");
    assert_eq!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="tool_requested").count(),1);
    drop(app);std::fs::remove_file(path).unwrap();
}
