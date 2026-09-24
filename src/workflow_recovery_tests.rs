//! Cross-module workflow regressions with no user account, VM or external action.
use crate::{
    db::{self, Db},
    runtime, tests,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::IntoFuture,
    sync::{Arc, Mutex},
    time::Duration,
};

#[test]
fn a_late_provider_result_cannot_change_a_stopped_task_to_completed() {
    let db = Db::open(":memory:").unwrap();
    let bot = tests::bot(&db, "openrouter");
    let id = db
        .queue(&bot.id, "Stop before completion is saved", 0)
        .unwrap();
    db.claim_bot(&bot.id).unwrap().unwrap();
    let approval = db
        .request_approval(&id, "guest_exec", &serde_json::json!({}))
        .unwrap();
    db.cancel(&id).unwrap();
    // The provider may already have produced its reply when Stop reaches SQLite.
    db.finish(&id, "completed", "Late provider reply", "")
        .unwrap();
    let saved = db.run(&id).unwrap();
    assert_eq!(saved.status, "cancelled");
    assert!(!saved.error.is_empty());
    assert_eq!(db.approval(&approval).unwrap(), "expired");
    db.chat_complete(&saved).unwrap();
    db.chat_complete(&saved).unwrap();
    let messages = db.chat_messages(&saved.chat_id).unwrap();
    let results: Vec<_> = messages.iter().filter(|m| m["kind"] == "result").collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status_notice"]["label"], "Task stopped");
    assert!(db.queued_bots().unwrap().is_empty());
}

#[tokio::test]
async fn a_stop_recorded_during_the_final_provider_poll_wins_over_its_reply() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "openrouter");
    app.db
        .queue(&bot.id, "Stop racing with the final response", 0)
        .unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let slot = app.db.screen(&bot.id).unwrap();
    let mut lease = Some(app.screen_lock(slot).lock_owned().await);
    let work = async {
        app.db.cancel(&run.id)?;
        Ok("Provider response became ready after Stop".to_owned())
    };
    let (result, _) = runtime::drive_run(&app, &run, slot, &mut lease, work)
        .await
        .unwrap();
    assert!(
        result.is_err(),
        "a ready provider future must not bypass Stop"
    );
    assert!(app.db.cancelled(&run.id));
}

struct ProviderScript {
    replies: Mutex<VecDeque<Option<Value>>>,
    requests: Mutex<Vec<Value>>,
}

fn answer(text: &str) -> Option<Value> {
    Some(
        json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]}),
    )
}

fn calls(items: &[(&str, &str, Value)]) -> Option<Value> {
    Some(
        json!({"choices":[{"message":{"role":"assistant","tool_calls":items.iter().map(|(id,name,args)|json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>()},"finish_reason":"tool_calls"}]}),
    )
}

fn file_app(path: &std::path::Path) -> runtime::Shared {
    let mut app = tests::app();
    Arc::get_mut(&mut app).unwrap().db = Db::open(path.to_str().unwrap()).unwrap();
    app
}

async fn run_next(app: &runtime::App, bot: &str, endpoint: &str) -> db::Run {
    let bot = app.db.bot(bot).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let slot = app.db.screen(&bot.id).unwrap();
    let mut lease = Some(app.screen_lock(slot).lock_owned().await);
    let work = crate::pi::fixture(app, &bot, &run, "local-fixture-only", endpoint);
    let (outcome, abrupt) = tokio::time::timeout(
        Duration::from_secs(20),
        runtime::drive_run(app, &run, slot, &mut lease, work),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!abrupt);
    match outcome {
        Ok(output) => app.db.finish(&run.id, "completed", &output, "").unwrap(),
        Err(error) => app
            .db
            .finish(&run.id, "failed", "", &error.to_string())
            .unwrap(),
    }
    app.db.chat_complete(&run).unwrap();
    app.db.run(&run.id).unwrap()
}

#[tokio::test]
async fn real_pi_fanout_keeps_a_user_decision_and_failed_helper_across_restart() {
    use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
    async fn provider(
        State(script): State<Arc<ProviderScript>>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        script.requests.lock().unwrap().push(body);
        match script
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected extra provider call")
        {
            Some(reply) => crate::pi::sse_response(reply),
            None => (
                axum::http::StatusCode::NOT_FOUND,
                "private upstream diagnostic must not be relayed",
            )
                .into_response(),
        }
    }
    let root = std::env::temp_dir().join(format!("kindred-workflow-{}", db::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("workflow.db");
    let app = file_app(&path);
    let mut a = tests::bot(&app.db, "openrouter");
    a.name = "Piper".into();
    app.db.save_bot(&a).unwrap();
    let mut b = tests::bot(&app.db, "openrouter");
    b.name = "Mara".into();
    b.memory = "I review optional changes.".into();
    app.db.save_bot(&b).unwrap();
    let mut c = tests::bot(&app.db, "openrouter");
    c.name = "Ellis".into();
    app.db.save_bot(&c).unwrap();
    let memory = "I review optional changes. The user prefers to defer this optional review until asked again.";
    let script = Arc::new(ProviderScript {
        replies: Mutex::new(VecDeque::from([
            calls(&[
                (
                    "ask-mara",
                    "send_to_bot",
                    json!({"bot_id":b.id,"message":"Ask the user whether to proceed with the optional review; preserve their choice."}),
                ),
                (
                    "ask-ellis",
                    "send_to_bot",
                    json!({"bot_id":c.id,"message":"Check availability and report the actual outcome."}),
                ),
            ]),
            answer("I asked Mara and Ellis for their checks."),
            calls(&[(
                "user-choice",
                "ask_question",
                json!({"topic_key":"optional-review","question":"Run the optional review?","context":"No optional action has started.","options":["Proceed","Defer"]}),
            )]),
            None,
            calls(&[("save-choice", "remember", json!({"text":memory}))]),
            answer("Optional review deferred as the user chose; preference saved."),
            answer(
                "Mara deferred the optional review. Ellis could not complete the availability check (404).",
            ),
        ])),
        requests: Mutex::new(vec![]),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/chat/completions", post(provider))
                .with_state(script.clone()),
        )
        .into_future(),
    );
    let original = app
        .db
        .queue(
            &a.id,
            "Coordinate the two checks, honoring any user choice",
            0,
        )
        .unwrap();
    let parent = run_next(&app, &a.id, &endpoint).await;
    assert_eq!(parent.id, original);
    assert_eq!(
        app.db.collaboration_waits(&parent.chat_id).unwrap().len(),
        2
    );
    let waiting = run_next(&app, &b.id, &endpoint).await;
    let questions = app
        .db
        .decisions_for_run(&waiting, Some("optional-review"))
        .unwrap();
    assert_eq!(questions.len(), 1);
    let question = questions[0]["id"].as_str().unwrap().to_owned();
    let failed = run_next(&app, &c.id, &endpoint).await;
    assert_eq!(failed.status, "failed");
    assert!(failed.error.contains("404"));
    assert!(failed.error.contains("no alternate provider"));
    assert!(!failed.error.contains("private upstream"));
    assert!(
        app.db.claim_bot(&a.id).unwrap().is_none(),
        "requester resumed before the user decided"
    );
    drop(app);

    let app = file_app(&path);
    assert_eq!(app.db.question(&question).unwrap().status, "pending");
    assert!(app.db.claim_bot(&a.id).unwrap().is_none());
    let selected = app
        .db
        .answer_question(
            &question,
            crate::questions::Answer {
                selected: Some(1),
                custom: None,
            },
        )
        .unwrap();
    let repeated = app
        .db
        .answer_question(
            &question,
            crate::questions::Answer {
                selected: Some(1),
                custom: None,
            },
        )
        .unwrap();
    assert_eq!(selected.continuation_run_id, repeated.continuation_run_id);
    let resumed = run_next(&app, &b.id, &endpoint).await;
    assert_eq!(resumed.id, selected.continuation_run_id);
    assert_eq!(app.db.bot(&b.id).unwrap().memory, memory);
    let combined = run_next(&app, &a.id, &endpoint).await;
    assert_eq!(combined.chat_id, parent.chat_id);
    assert!(combined.prompt.contains("Defer") && combined.prompt.contains("404"));
    assert!(combined.output.contains("could not complete"));
    assert!(app.db.queued_bots().unwrap().is_empty());
    assert!(
        app.db
            .collaboration_waits(&parent.chat_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(app.db.runs(None).unwrap().len(), 5);
    assert_eq!(script.requests.lock().unwrap().len(), 7);
    assert!(script.replies.lock().unwrap().is_empty());
    for run in [&parent, &waiting, &failed, &resumed, &combined] {
        app.db.chat_complete(run).unwrap();
    }
    assert!(app.db.queued_bots().unwrap().is_empty());
    drop(app);
    let app = file_app(&path);
    assert!(app.db.queued_bots().unwrap().is_empty());
    assert_eq!(app.db.bot(&b.id).unwrap().memory, memory);
    drop(app);
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
