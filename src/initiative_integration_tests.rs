//! Real Pi SDK + HTTP provider fixture + production tool dispatcher.
//! No external provider, account, VM or customer data is used.
use crate::{db::{self, Bot, Run}, runtime::{self, Shared}};
use axum::{extract::State, routing::post, Json, Router};
use chrono::{TimeZone, Timelike, Utc};
use serde_json::{json, Value};
use std::{future::IntoFuture, sync::{Arc, Mutex}, time::Duration};

#[derive(Clone)]
struct Script {
    replies: Arc<Vec<Value>>,
    requests: Arc<Mutex<Vec<Value>>>,
    reached: Arc<tokio::sync::Notify>,
    stall: bool,
    fail: bool,
}
async fn reply(State(script): State<Script>, Json(request): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let turn = { let mut requests=script.requests.lock().unwrap(); let turn=requests.len(); requests.push(request); turn };
    script.reached.notify_one();
    if script.stall { std::future::pending::<()>().await; }
    if script.fail { return (axum::http::StatusCode::BAD_REQUEST, "synthetic provider rejection").into_response(); }
    let Some(response)=script.replies.get(turn) else {
        return (axum::http::StatusCode::BAD_REQUEST,"unexpected extra model request").into_response();
    };
    crate::pi::sse_response(response.clone())
}
fn answer(text: &str) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
}
fn action(id: &str, text: &str, name: &str, args: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
}
async fn endpoint(replies: Vec<Value>, stall: bool, fail: bool) -> (String, Script, tokio::task::JoinHandle<()>) {
    let script=Script{replies:Arc::new(replies),requests:Default::default(),reached:Default::default(),stall,fail};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url=format!("http://{}/chat/completions",listener.local_addr().unwrap());
    let router=Router::new().route("/chat/completions",post(reply)).with_state(script.clone());
    let server=tokio::spawn(async move {axum::serve(listener,router).into_future().await.unwrap();});
    (url,script,server)
}
fn setup(provider: &str) -> (Shared,Bot,Run,i64) {
    let app=crate::tests::app();
    app.db.save_setting("general",&json!({"timezone":"UTC"})).unwrap();
    let mut bot=crate::tests::bot(&app.db,provider);
    bot.approval_mode="full".into();bot.profile.initiative=true;
    bot.profile.initiative_instructions="Prepare an internal draft and only report new information.".into();
    app.db.save_bot(&bot).unwrap();
    // Tick against a deterministic daytime clock, then align run age with the
    // real clock used by the provider driver. Scheduler time boundaries have
    // separate virtual-clock stress coverage.
    let time=Utc.timestamp_opt(db::now(),0).single().unwrap().with_hour(12).unwrap().with_minute(0).unwrap().timestamp();
    let chat=format!("dm-{}",bot.id);
    {let c=app.db.0.lock().unwrap();
        c.execute("INSERT OR IGNORE INTO chats(id,name,members) VALUES(?,'Owner DM',?)",rusqlite::params![chat,json!([bot.id]).to_string()]).unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','Please keep an eye on the synthetic project.','message',?)",rusqlite::params![chat,time]).unwrap();
        c.execute("UPDATE initiative_state SET next_check=?1,enabled_after=?1 WHERE bot_id=?2",rusqlite::params![time,bot.id]).unwrap();
    }
    crate::initiative::tick(&app.db,time).unwrap();
    app.db.0.lock().unwrap().execute("UPDATE runs SET created=? WHERE bot_id=?",rusqlite::params![db::now(),bot.id]).unwrap();
    let run=app.db.claim_bot(&bot.id).unwrap().expect("scheduler queued initiative");
    let cursor=app.db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
    (app,bot,run,cursor)
}
async fn execute(app: &Shared, bot: &Bot, run: &Run, url: &str) -> anyhow::Result<String> {
    let mut lease=None;
    let (result,_)=tokio::time::timeout(Duration::from_secs(20),runtime::drive_run(app,run,0,&mut lease,crate::pi::fixture(app,bot,run,"synthetic-key",url))).await??;
    let (status,output,error)=match &result {Ok(s)=>("completed",s.as_str(),String::new()),Err(e)=>(runtime::failure_status(app,run,e),"",e.to_string())};
    app.db.finish(&run.id,status,output,&error).unwrap();
    app.db.event(&run.id,"run_finished",json!({})).unwrap();
    app.db.chat_complete(run).unwrap();
    result
}

#[tokio::test]
async fn initiative_real_sdk_reports_once_and_quiet_checks_stay_quiet_for_api_providers() {
    for provider in ["openrouter","custom-11111111-1111-4111-8111-111111111111"] {
        for quiet in [false,true] {
            let (app,bot,run,cursor)=setup(provider);
            let final_step=if quiet {action("finish","Internal no-change assessment","finish_quietly",json!({}))} else {answer("Your internal draft is ready for review. Nothing was sent.")};
            let (url,script,server)=endpoint(vec![
                action("inspect","Looking through existing work","skills_list",json!({})),
                action("save","Saving internal follow-up state","initiative_checkpoint",json!({"notes":"Synthetic draft v1 prepared; owner notified; avoid duplicating it."})),final_step
            ],false,false).await;
            let output=execute(&app,&bot,&run,&url).await.unwrap();
            app.db.chat_complete(&run).unwrap(); // retry completion after a dropped client response
            let messages=app.db.chat_messages(&run.chat_id).unwrap();
            let results:Vec<_>=messages.iter().filter(|m|m["sender"]==bot.id).collect();
            let notifications=app.db.notifications(Some(cursor)).unwrap();
            if quiet {assert!(output.is_empty());assert!(results.is_empty());assert!(notifications["items"].as_array().unwrap().is_empty());}
            else {assert_eq!(output,"Your internal draft is ready for review. Nothing was sent.");assert_eq!(results.len(),1);assert_eq!(results[0]["text"],output);assert_eq!(notifications["items"].as_array().unwrap().len(),1);}
            assert!(!json!(messages).to_string().contains("Looking through existing work"));
            let requests=script.requests.lock().unwrap();assert_eq!(requests.len(),3);
            assert!(requests[0]["messages"].to_string().contains("optional background initiative check"));
            assert!(requests[0]["tools"].to_string().contains("initiative_checkpoint"));
            assert!(!requests[0]["tools"].to_string().contains("routine_create"));
            assert!(requests[2]["messages"].to_string().contains("next_check_not_before"), "{provider} quiet={quiet}: checkpoint result must reach the model: {}", requests[2]["messages"]);
            assert!(crate::initiative::context(&app.db,&run).unwrap()["notes"].as_str().unwrap().contains("avoid duplicating"));
            server.abort();
        }
    }
}

#[tokio::test]
async fn initiative_real_sdk_forced_approval_returns_actionable_result_without_wait() {
    let (app,bot,run,cursor)=setup("openrouter");
    let (url,script,server)=endpoint(vec![
        action("blocked","Preparing requested instruction revision","bot_instructions_update",json!({"bot_id":"self","expected_instructions":bot.instructions,"instructions":"Changed without asking"})),
        answer("I prepared the suggestion; changing instructions needs your approval.")
    ],false,false).await;
    execute(&app,&bot,&run,&url).await.unwrap();
    assert_eq!(app.db.bot(&bot.id).unwrap().instructions,bot.instructions);
    assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
    assert_eq!(app.db.run(&run.id).unwrap().status,"completed");
    let requests=script.requests.lock().unwrap();assert_eq!(requests.len(),2);
    assert!(requests[1]["messages"].to_string().contains("needs approval"));
    assert_eq!(app.db.notifications(Some(cursor)).unwrap()["items"].as_array().unwrap().len(),1);
    server.abort();
}

#[tokio::test]
async fn initiative_real_sdk_cancellation_drops_stalled_provider_and_preserves_owner_queue() {
    let (app,mut bot,run,_)=setup("custom-11111111-1111-4111-8111-111111111111");
    let (url,script,server)=endpoint(vec![],true,false).await;
    let worker={let app=app.clone();let bot=bot.clone();let run=run.clone();tokio::spawn(async move {execute(&app,&bot,&run,&url).await})};
    tokio::time::timeout(Duration::from_secs(10),script.reached.notified()).await.unwrap();
    let direct=app.db.queue(&bot.id,"An explicit owner request",0).unwrap();
    bot.profile.initiative=false;app.db.save_bot(&bot).unwrap();
    let result=tokio::time::timeout(Duration::from_secs(3),worker).await.unwrap().unwrap();
    assert!(result.is_err());assert_eq!(app.db.run(&run.id).unwrap().status,"cancelled");
    assert_eq!(app.db.run(&direct).unwrap().status,"queued");
    assert_eq!(app.db.claim_bot(&bot.id).unwrap().unwrap().id,direct);
    assert_eq!(script.requests.lock().unwrap().len(),1);server.abort();
}

#[tokio::test]
async fn initiative_real_sdk_background_run_does_not_consume_direct_followup() {
    let (app,bot,run,_)=setup("openrouter");
    let direct=app.db.chat_send(&run.chat_id,"Owner-only followup must remain a new direct task",&[]).unwrap()[0].clone();
    let (url,script,server)=endpoint(vec![action("inspect","Internal work","skills_list",json!({})),answer("Useful background finding.")],false,false).await;
    execute(&app,&bot,&run,&url).await.unwrap();
    assert_eq!(app.db.run(&direct).unwrap().status,"queued");
    let requests=script.requests.lock().unwrap();assert_eq!(requests.len(),2);
    assert!(!requests[1]["messages"].as_array().unwrap().iter().any(|m|m["role"]=="user"&&m["content"].as_str().is_some_and(|s|s=="Owner-only followup must remain a new direct task")));
    server.abort();
}

#[tokio::test]
async fn initiative_real_sdk_provider_failure_reports_once_without_immediate_requeue() {
    let (app,bot,run,cursor)=setup("custom-11111111-1111-4111-8111-111111111111");
    let (url,script,server)=endpoint(vec![],false,true).await;
    assert!(execute(&app,&bot,&run,&url).await.is_err());
    app.db.chat_complete(&run).unwrap();
    assert_eq!(app.db.run(&run.id).unwrap().status,"failed");
    assert_eq!(app.db.notifications(Some(cursor)).unwrap()["items"].as_array().unwrap().len(),1);
    crate::initiative::tick(&app.db,db::now()).unwrap();
    let c=app.db.0.lock().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM initiative_runs WHERE bot_id=?",[&bot.id],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(script.requests.lock().unwrap().len(),1);server.abort();
}

#[tokio::test]
async fn initiative_background_command_launch_caps_actual_desktop_request_and_later_stops_it() {
    for requested in [None,Some(86400),Some(5)] {
        let (app,mut bot,run,_)=setup("openrouter");
        let device=db::id();
        bot.profile.local_access=true;bot.profile.local_device_id=device.clone();app.db.save_bot(&bot).unwrap();
        app.db.save_setting("general",&json!({"timezone":"UTC","local_access":true})).unwrap();
        let mut registration=json!({"id":device,"secret":"a".repeat(72),"name":"Synthetic desktop","mode":"full","command_protocol":1});
        let _ = crate::local_access::poll(State(app.clone()),Json(registration.clone())).await.ok().expect("synthetic desktop poll succeeds");
        app.db.0.lock().unwrap().execute("UPDATE runs SET created=? WHERE id=?",rusqlite::params![db::now()-840,run.id]).unwrap();
        let mut args=json!({"command":"printf synthetic","background":true,"action_scope":"routine_vm","title":"Synthetic work"});
        if let Some(max)=requested {args["max_seconds"]=json!(max);}
        let task={let app=app.clone();let bot=bot.clone();let run=run.clone();tokio::spawn(async move {runtime::call_tool(&app,&bot,&run,"local_exec",args).await})};
        let request=tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let reply=crate::local_access::poll(State(app.clone()),Json(registration.clone())).await.ok().expect("synthetic desktop poll succeeds").0;
                if !reply["request"].is_null(){break reply["request"].clone();}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let max=request["args"]["max_seconds"].as_u64().expect("limit is sent to actual desktop protocol");
        assert!(max>0&&max<=60);if requested==Some(5){assert_eq!(max,5);}
        let command=request["args"]["process_id"].as_str().unwrap().to_owned();
        registration["receipt"]=json!({"id":request["id"],"nonce":request["nonce"],"result":{"status":"running","id":command}});
        let _ = crate::local_access::poll(State(app.clone()),Json(registration.clone())).await.ok().expect("synthetic desktop poll succeeds");
        let result=tokio::time::timeout(Duration::from_secs(3),task).await.unwrap().unwrap().unwrap();
        assert_ne!(result["failed"],true,"{result}");
        app.db.finish(&run.id,"completed","","").unwrap();
        app.db.0.lock().unwrap().execute("UPDATE runs SET created=? WHERE id=?",rusqlite::params![db::now()-901,run.id]).unwrap();
        let controls=crate::local_access::poll(State(app.clone()),Json(registration)).await.ok().expect("synthetic desktop controls poll succeeds").0;
        assert_eq!(controls["commands"].as_array().unwrap().iter().find(|c|c["id"]==command).unwrap()["stop"],true);
    }
}
