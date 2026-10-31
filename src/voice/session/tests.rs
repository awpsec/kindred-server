use super::*;

fn setup(provider: &str) -> (Shared,Bot,Ticket,Run) {
    let app=crate::tests::app();let bot=test_bot(&app.db,provider);
    let ticket=Ticket{id:db::id(),chat:format!("dm-{}",bot.id),voice:"marin".into(),expires:db::now()+60};
    let run=begin(&app.db,&ticket).unwrap();(app,bot,ticket,run)
}
fn utterance(app:&Shared,ticket:&Ticket) {
    let mut t=Transcript::default();
    t.append(&json!({"type":"session.input_transcript.delta","event_id":"spoken","delta":"Find the nearest bicycle shop.","start_ms":0,"end_ms":10})).unwrap();
    checkpoint(&app.db,&ticket.id,&t,42).unwrap();
}
fn function(id:&str,name:&str,args:Value)->Value {
    json!({"id":format!("item-{id}"),"type":"function_call","call_id":id,"name":name,"arguments":args.to_string()})
}

#[test]
fn call_owns_work_for_every_bot_provider_without_chat_or_queue_activity() {
    for provider in ["codex","claude-code","openrouter"] {
        let (app,bot,ticket,run)=setup(provider);
        let config=configuration(&app,&bot,&run).unwrap();
        assert_eq!(config["type"],"responses");assert_eq!(config["responses"]["model"],MODEL);
        assert!(config["responses"]["tools"].as_array().unwrap().iter().any(|t|t["type"]=="web_search"));
        assert!(config["responses"]["tools"].as_array().unwrap().iter().any(|t|t["name"]=="remember"));
        assert!(!config["responses"]["tools"].as_array().unwrap().iter().any(|t|t["name"]=="ask_question"));
        app.db.event(&run.id,"assistant",json!({"text":"Private intermediate finding"})).unwrap();
        app.db.event(&run.id,"tool_started",json!({"tool":"computer_screenshot"})).unwrap();
        assert!(app.db.chat_messages(&ticket.chat).unwrap().is_empty());
        assert!(app.db.queued_bots().unwrap().is_empty());assert!(app.db.runs(None).unwrap().is_empty());
        let ordinary=app.db.chat_send(&ticket.chat,"Separate typed task",&[]).unwrap();
        assert!(app.db.push_targets().unwrap().iter().all(|(_,target)|target!=&run.id));
        assert!(app.db.request_steering(&ordinary[0],&run.id).is_err());
        assert!(!app.db.pushed_followups_pending(&run.id).unwrap());
        assert_eq!(app.db.claim_bot(&bot.id).unwrap().unwrap().id,ordinary[0]);
        assert_ne!(ordinary[0],run.id);
    }
}

#[tokio::test]
async fn a_duplicate_tool_event_cannot_repeat_a_memory_write() {
    let (app,bot,ticket,run)=setup("codex");
    let item=function("remember-once","remember",json!({"mode":"append","text":"Prefer the north entrance for bicycle deliveries."}));
    let tool=accept_tool(&app.db,&ticket.id,"response-one",&item).unwrap().unwrap();
    let (_,result)=execute(app.clone(),bot.clone(),run.clone(),ticket.id.clone(),tool).await;
    assert_ne!(result["failed"],true,"{result}");
    assert!(accept_tool(&app.db,&ticket.id,"response-one",&item).unwrap().is_none());
    let memory=app.db.bot(&bot.id).unwrap().memory;
    assert_eq!(memory.matches("north entrance").count(),1);
    assert!(accept_tool(&app.db,&ticket.id,"response-one",&function("remember-once","remember",json!({"text":"Changed action","mode":"replace"}))).is_err());
    assert!(app.db.chat_messages(&ticket.chat).unwrap().is_empty());
}

#[test]
fn hangup_publishes_one_receipt_and_recap_without_replaying_remaining_speech() {
    let (app,_,ticket,run)=setup("openrouter");utterance(&app,&ticket);
    let item=json!({"type":"message","id":"answer","content":[{"type":"output_text","text":"The shop opens at nine. https://example.com/bikes","annotations":[]}]});
    record_item(&app.db,&ticket.id,"r1",&item).unwrap();record_item(&app.db,&ticket.id,"r1",&item).unwrap();
    assert!(app.db.chat_messages(&ticket.chat).unwrap().is_empty());
    super::super::finish(&app.db,&ticket.id,true).unwrap();
    super::super::finish(&app.db,&ticket.id,true).unwrap();
    let summary=summary_input(&app.db,&ticket.id).unwrap();
    assert_eq!(summary["backend_messages"].as_array().unwrap().len(),1);
    assert!(summary.to_string().contains("https://example.com/bikes"));
    save_recap(&app.db,&ticket.id,"From our call: the shop opens at nine. [Details](https://example.com/bikes)").unwrap();
    save_recap(&app.db,&ticket.id,"Duplicate").unwrap();
    app.db.chat_complete(&run).unwrap();
    let messages=app.db.chat_messages(&ticket.chat).unwrap();
    assert_eq!(messages.len(),2);assert_eq!(messages[0]["kind"],"voice_call");assert_eq!(messages[1]["kind"],"voice_recap");
    assert!(app.db.queued_bots().unwrap().is_empty());assert!(messages[1]["status_notice"].is_null());
}

#[test]
fn unfinished_actions_are_recorded_without_becoming_chat_tasks_on_recovery() {
    let (app,_,ticket,run)=setup("codex");utterance(&app,&ticket);
    accept_tool(&app.db,&ticket.id,"r",&function("unknown-outcome","remember",json!({"text":"example","mode":"append"}))).unwrap();
    app.db.0.lock().unwrap().execute("UPDATE voice_tool_calls SET state='started' WHERE call_id=?",[&ticket.id]).unwrap();
    super::super::recover(&app.db).unwrap();super::super::recover(&app.db).unwrap();
    assert_eq!(app.db.run(&run.id).unwrap().status,"interrupted");
    assert_eq!(summary_input(&app.db,&ticket.id).unwrap()["unfinished_or_failed_actions"],1);
    assert!(app.db.queued_bots().unwrap().is_empty());
    assert_eq!(app.db.chat_messages(&ticket.chat).unwrap().len(),1);
}

#[test]
fn approval_stays_visible_while_nonessential_cards_wait_for_hangup() {
    let (app,_,ticket,run)=setup("codex");
    let approval=app.db.request_approval(&run.id,"guest_exec",&json!({"command":"pwd"})).unwrap();
    {
        let c=app.db.0.lock().unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,'Private progress','message',?,?)",params![ticket.chat,run.bot_id,run.id,db::now()]).unwrap();
    }
    let visible=app.db.chat_messages(&ticket.chat).unwrap();
    assert_eq!(visible.len(),1);assert_eq!(visible[0]["kind"],"approval");
    assert_eq!(app.db.approval(&approval).unwrap(),"pending");
    super::super::finish(&app.db,&ticket.id,true).unwrap();
    assert_eq!(app.db.approval(&approval).unwrap(),"expired");
    assert!(!app.db.chat_messages(&ticket.chat).unwrap().iter().any(|m|m["text"]=="Private progress"));
}

#[test]
fn result_resume_waits_for_all_tools_and_does_not_resume_twice() {
    let mut state=ResponseWork{pending:2,has_tools:true,..Default::default()};
    assert!(!state.ready());state.pending=1;state.completed=true;assert!(!state.ready());
    state.pending=0;assert!(state.ready());state.continued=true;assert!(!state.ready());
    assert!(!ResponseWork{completed:true,..Default::default()}.ready());
}

#[test]
fn tool_output_preserves_structured_errors_and_screenshot_data() {
    let output=tool_output(&json!({"failed":true,"uncertain_effect":true,"text":"Stopped","image":"AA=="}));
    assert!(output[0]["text"].as_str().unwrap().contains("uncertain_effect"));
    assert_eq!(output[1]["image_url"],"data:image/png;base64,AA==");
}

fn decision()->crate::questions::QuestionInput {
    crate::questions::QuestionInput{topic_key:"voice-confirm-source".into(),question:"Use this account?".into(),context:"This saves the source selection, not an external action.".into(),options:vec!["Use it".into(),"Keep current".into()]}
}

#[tokio::test]
async fn decisions_resume_the_call_without_queuing_an_ordinary_turn() {
    let (app,_,ticket,run)=setup("codex");
    let result=app.db.ask_question(&run,decision()).unwrap();
    let q=app.db.pending_questions().unwrap().remove(0);
    assert_eq!(app.db.run(&run.id).unwrap().status,"awaiting_user");
    assert!(app.db.turn_deferred(&run.id).unwrap());
    assert!(app.db.chat_messages(&ticket.chat).unwrap().iter().any(|m|m["kind"]=="question"));
    let wait=tokio::spawn({let app=app.clone();let run=run.clone();async move {resolve_question(&app,&run,result).await}});
    let answer=app.db.answer_question(&q.id,crate::questions::Answer{selected:Some(0),custom:None}).unwrap();
    assert_eq!(answer.continuation_run_id,run.id);
    assert_eq!(app.db.run(&run.id).unwrap().status,"running");
    assert!(!app.db.turn_deferred(&run.id).unwrap());
    let returned=tokio::time::timeout(Duration::from_secs(2),wait).await.unwrap().unwrap().unwrap();
    assert_eq!(returned["question"]["answer"],"Use it");
    assert!(app.db.queued_bots().unwrap().is_empty());
}

#[tokio::test]
async fn dismissed_and_hung_up_decisions_never_start_late_work() {
    for hangup in [false,true] {
        let (app,_,ticket,run)=setup("claude-code");
        let result=app.db.ask_question(&run,decision()).unwrap();
        let q=app.db.pending_questions().unwrap().remove(0);
        if hangup {super::super::finish(&app.db,&ticket.id,true).unwrap();}
        else {app.db.dismiss_question(&q.id).unwrap();}
        let returned=resolve_question(&app,&run,result).await.unwrap();
        assert_eq!(returned["question"]["status"],"dismissed");
        assert_eq!(returned["question"]["answer"],"");
        assert!(app.db.answer_question(&q.id,crate::questions::Answer{selected:Some(0),custom:None}).is_err());
        assert!(app.db.queued_bots().unwrap().is_empty());
    }
}

#[tokio::test]
async fn explicit_teammate_work_returns_to_call_and_never_wakes_its_chat_model() {
    for hangup_first in [false,true] {
        let (app,_,ticket,run)=setup("codex");
        let helper=test_bot(&app.db,"openrouter");
        let child=app.db.chat_handoff(&run,&helper.id,"Check the delivery entrance").unwrap();
        if hangup_first {super::super::finish(&app.db,&ticket.id,true).unwrap();}
        app.db.finish(&child,"completed","Use the north entrance.","").unwrap();
        app.db.chat_complete(&app.db.run(&child).unwrap()).unwrap();
        crate::collaboration::recover(&app.db.0.lock().unwrap()).unwrap();
        assert!(app.db.queued_bots().unwrap().is_empty());
        if !hangup_first {
            let returned=resolve_handoff(&app,&run).await.unwrap();
            assert_eq!(returned["teammate_results"][0]["reported_result"],"Use the north entrance.");
            super::super::finish(&app.db,&ticket.id,true).unwrap();
            crate::collaboration::recover(&app.db.0.lock().unwrap()).unwrap();
            assert!(app.db.queued_bots().unwrap().is_empty());
        }
    }
}

#[test]
fn restart_keeps_private_work_hidden_and_publishes_attachments_only_once() {
    let root=std::env::temp_dir().join(format!("kindred-call-reopen-{}",db::id()));
    let path=root.join("state.db");
    let db=Db::open(path.to_str().unwrap()).unwrap();let bot=test_bot(&db,"codex");
    let ticket=Ticket{id:db::id(),chat:format!("dm-{}",bot.id),voice:"marin".into(),expires:db::now()+60};
    let run=begin(&db,&ticket).unwrap();
    db.event(&run.id,"assistant",json!({"text":"Private call progress"})).unwrap();
    db.attach_file(&run,"/workspace/route.md",b"North entrance").unwrap();
    let mut t=Transcript::default();t.append(&json!({"type":"session.input_transcript.delta","event_id":"a","delta":"Save the route","start_ms":0,"end_ms":10})).unwrap();
    checkpoint(&db,&ticket.id,&t,42).unwrap();drop(db);
    let db=Db::open(path.to_str().unwrap()).unwrap();super::super::recover(&db).unwrap();
    save_recap(&db,&ticket.id,"The route was saved.").unwrap();drop(db);
    let db=Db::open(path.to_str().unwrap()).unwrap();super::super::recover(&db).unwrap();
    let messages=db.chat_messages(&ticket.chat).unwrap();
    assert_eq!(messages.iter().filter(|m|m["kind"]=="voice_recap").count(),1);
    let recap=messages.iter().find(|m|m["kind"]=="voice_recap").unwrap();
    assert_eq!(recap["attachments"].as_array().unwrap().len(),1);
    assert!(!messages.iter().any(|m|m["kind"]=="result" || m["text"]=="Private call progress"));
    assert!(db.queued_bots().unwrap().is_empty());drop(db);std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn recap_uses_no_tools_and_rejects_incomplete_or_empty_provider_output() {
    use std::future::IntoFuture;
    for (status,text,success) in [("completed","We chose the [north entrance](https://example.com/route).",true),("incomplete","Partial",false),("completed","",false)] {
        let router=axum::Router::new().route("/",axum::routing::post(move |axum::Json(body):axum::Json<Value>|async move {
            assert_eq!(body["tools"],json!([]));assert_eq!(body["store"],false);
            assert_eq!(body["model"],MODEL);assert!(body["input"].as_str().unwrap().contains("north"));
            axum::Json(json!({"status":status,"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]}))
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(axum::serve(listener,router).into_future());
        let result=generate_recap("fixture-only",json!({"call_transcript":"Use the north entrance"}),&url).await;
        assert_eq!(result.is_ok(),success);
        if success {assert!(result.unwrap().contains("https://example.com/route"));}
        server.abort();
    }
}

#[tokio::test]
async fn approval_and_takeover_feeds_expose_controls_without_exposing_private_runs() {
    use std::future::IntoFuture;
    let (app,_,_,run)=setup("codex");
    let approval=app.db.request_approval(&run.id,"guest_exec",&json!({"command":"pwd"})).unwrap();
    let router=crate::web::router(app.clone());
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let base=format!("http://{}",listener.local_addr().unwrap());
    let server=tokio::spawn(axum::serve(listener,router).into_future());let client=reqwest::Client::new();
    let rows:Value=client.get(format!("{base}/api/approvals")).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(rows[0]["voice_run"]["id"],run.id);assert_eq!(rows[0]["voice_run"]["status"],"awaiting_approval");
    assert!(rows[0]["voice_run"]["prompt"].is_null());
    app.db.decide(&approval,false).unwrap();
    let task=app.db.request_user_task(&run,"Sign in","Use the website directly.").unwrap();
    let rows:Value=client.get(format!("{base}/api/user-tasks")).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(rows[0]["id"],task.id);assert_eq!(rows[0]["voice_run"]["status"],"awaiting_user");
    let runs:Value=client.get(format!("{base}/api/runs")).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(runs,json!([]));server.abort();
}

#[test]
fn voice_records_transfer_and_older_workspaces_still_import() {
    for legacy in [false,true] {
        let (app,_,ticket,_)=setup("codex");utterance(&app,&ticket);
        super::super::finish(&app.db,&ticket.id,true).unwrap();save_recap(&app.db,&ticket.id,"We discussed the route.").unwrap();
        let mut package=app.db.prepare_transfer(&db::id(),"Call workspace").unwrap();
        if legacy {for table in ["voice_sessions","voice_session_items","voice_tool_calls","voice_deferred_messages"] {package["tables"].as_object_mut().unwrap().remove(table);}}
        let target=Db::open(":memory:").unwrap();target.import_transfer(&package).unwrap();
        assert_eq!(target.chat_messages(&ticket.chat).unwrap().iter().filter(|m|m["kind"]=="voice_recap").count(),1);
        assert!(target.queued_bots().unwrap().is_empty());
    }
}

// Opt-in contract probe: synthetic context and silence only, no user recording,
// production workspace, or tool execution. Never run automatically in CI.
#[tokio::test]
#[ignore = "Requires an explicitly supplied OpenAI credential file and makes two bounded provider requests"]
async fn provider_accepts_call_configuration_and_a_tool_free_recap() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use base64::Engine;
    let path=std::env::var("KINDRED_VOICE_CHECK_CREDENTIAL_FILE").expect("Explicit check credential file required");
    let credentials:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let key=credentials["decisions"].as_str().filter(|s|!s.is_empty()).expect("No OpenAI credential configured");
    let (app,bot,ticket,run)=setup("codex");
    let mut request=ENDPOINT.into_client_request().unwrap();request.headers_mut().insert("Authorization",format!("Bearer {key}").parse().unwrap());
    let (mut provider,_)=tokio::time::timeout(Duration::from_secs(15),tokio_tungstenite::connect_async(request)).await.unwrap().expect("Provider transport unavailable");
    send_upstream(&mut provider,json!({"type":"session.start","session":{"model":"gpt-live-1","store":false,"instructions":"This is a synthetic connection check. Remain silent and wait for the user. Do not delegate or execute any actions.","audio":{"format":{"type":"audio/pcm","rate":24000},"output":{"voice":"marin"}},"delegation":configuration(&app,&bot,&run).unwrap()}})).await.unwrap();
    let mut started=false;let mut audio=tokio::time::interval(Duration::from_millis(100));
    let silence=base64::engine::general_purpose::STANDARD.encode(vec![0u8;4800]);
    let end=tokio::time::Instant::now()+Duration::from_secs(12);
    loop {
        tokio::select! {
            _=tokio::time::sleep_until(end)=>break,
            _=audio.tick(),if started=>send_upstream(&mut provider,json!({"type":"session.input_audio.append","audio":silence})).await.unwrap(),
            incoming=provider.next()=>{
                let incoming=incoming.expect("Provider closed early").expect("Provider transport failed");
                if !incoming.is_text(){continue;}
                let event:Value=serde_json::from_str(incoming.to_text().unwrap()).unwrap();
                assert_ne!(event["type"],"error","Provider rejected call configuration (code: {})",event["error"]["code"].as_str().unwrap_or("unspecified"));
                if event["type"]=="session.started" {started=true;}
            }
        }
    }
    assert!(started);send_upstream(&mut provider,json!({"type":"session.close"})).await.unwrap();
    let closed=tokio::time::timeout(Duration::from_secs(5),async {
        while let Some(Ok(incoming))=provider.next().await {
            if incoming.is_text() && serde_json::from_str::<Value>(incoming.to_text().unwrap()).unwrap()["type"]=="session.closed" {return true}
        }
        false
    }).await.unwrap_or(false);
    assert!(closed,"Provider did not confirm closing the synthetic session");
    let recap=generate_recap(key,json!({"call_transcript":"User: Use the north entrance. AI: Agreed.","backend_messages":[{"text":"Route notes: https://example.com/route"}],"unfinished_or_failed_actions":0}),RECAP_ENDPOINT).await.expect("Provider recap request failed");
    assert!(recap.contains("https://example.com/route"));
    assert!(app.db.queued_bots().unwrap().is_empty());
    assert!(app.db.chat_messages(&ticket.chat).unwrap().is_empty());
    println!("Provider contract passed: session started, no configuration error, clean close, tool-free recap preserved its source link. No production messages or tools were used.");
}

#[test]
fn overlapping_delegations_wait_for_all_results_before_one_resume() {
    let mut responses=HashMap::new();
    responses.insert("lookup".into(),ResponseWork{pending:0,has_tools:true,completed:true,continued:false});
    responses.insert("correction".into(),ResponseWork{pending:1,has_tools:true,completed:true,continued:false});
    assert!(!ready_to_resume(&responses));
    responses.get_mut("correction").unwrap().pending=0;
    assert!(ready_to_resume(&responses));
    for state in responses.values_mut().filter(|s|s.ready()) {state.continued=true;}
    assert!(!ready_to_resume(&responses));
}
