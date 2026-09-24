use super::*;
use axum::{Router, routing::any};
use std::sync::{Arc, Mutex};

struct Fixture {
    history: Mutex<Value>,
    requests: Mutex<Vec<Value>>,
    fail: Mutex<bool>,
    expired: Mutex<bool>,
}
async fn provider(State(s): State<Arc<Fixture>>, req: axum::extract::Request) -> Json<Value> {
    let path = req.uri().path().to_owned();
    if path.starts_with("/connected_accounts/") {
        return Json(
            json!({"id":"ca_mail","user_id":"owner","toolkit":{"slug":"gmail"},"auth_config":{"id":"ac_mail"},"status":"ACTIVE"}),
        );
    }
    assert_eq!(path, "/tools/execute/proxy");
    let body = axum::body::to_bytes(req.into_body(), 65536).await.unwrap();
    let args: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(args["connected_account_id"], "ca_mail");
    s.requests.lock().unwrap().push(args.clone());
    if *s.fail.lock().unwrap() {
        return Json(json!({"status":401,"data":{"error":"expired"}}));
    }
    let endpoint = args["endpoint"].as_str().unwrap();
    let value = if endpoint.ends_with("/profile") {
        json!({"emailAddress":"work@example.test","historyId":"100"})
    } else if endpoint.ends_with("/watch") {
        json!({"expiration":((db::now()+7*86400)*1000).to_string(),"historyId":"100"})
    } else if endpoint.ends_with("/messages") {
        json!({"messages":[{"id":"recovered","threadId":"recovered-thread"}]})
    } else {
        assert!(endpoint.ends_with("/history"));
        if *s.expired.lock().unwrap() {
            return Json(json!({"status":404,"data":{}}));
        }
        s.history.lock().unwrap().clone()
    };
    Json(json!({"status":200,"data":value}))
}
async fn setup() -> (
    Shared,
    crate::db::Bot,
    Arc<Fixture>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let s = Arc::new(Fixture {
        history: Mutex::new(json!({"historyId":"100"})),
        requests: Mutex::new(vec![]),
        fail: Mutex::new(false),
        expired: Mutex::new(false),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.db
        .save_setting(
            "composio_test_base",
            &json!(format!("http://{}", listener.local_addr().unwrap())),
        )
        .unwrap();
    app.db.save_setting("composio",&json!({"user_id":"owner","accounts":{"mail":{"id":"ca_mail","name":"Work","auth_config_id":"ac_mail","toolkit":"gmail","permission":"read","status":"ACTIVE","checked_at":0,"scopes":["https://www.googleapis.com/auth/gmail.readonly"],"last_test":null}},"configs":{}})).unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(any(provider)).with_state(s.clone()),
        )
        .into_future(),
    );
    (app, bot, s, server)
}
fn input(bot: &crate::db::Bot) -> WatchInput {
    serde_json::from_value(json!({"bot_id":bot.id,"account_id":"ca_mail","name":"Work inbox","instructions":"Alert me when mail needs attention. Otherwise finish_quietly."})).unwrap()
}
fn mail(ids: &[&str], cursor: &str) -> Value {
    json!({"historyId":cursor,"history":[{"messagesAdded":ids.iter().map(|id|json!({"message":{"id":id,"threadId":format!("thread-{id}"),"labelIds":["INBOX"]}})).collect::<Vec<_>>()}]})
}

#[tokio::test]
async fn bot_activity_controls_preserve_pause_and_do_not_replay_mail() {
    let (app, mut bot, s, server) = setup().await;
    bot.approval_mode = "full".into();
    app.db.save_bot(&bot).unwrap();
    let saved = save_watch(&app, input(&bot)).await.unwrap();
    let id = saved["id"].as_str().unwrap();
    let paused = control(&app, Some(&bot.id), id, "pause").await.unwrap();
    assert_eq!(paused["monitor"]["enabled"], false);
    let requests = s.requests.lock().unwrap().len();
    let edited=save_for_bot(&app,&bot,&json!({"id":id,"account_id":"ca_mail","name":"Renamed QA inbox","instructions":"Only alert about actual requests"})).await.unwrap();
    assert_eq!(edited["enabled"], false);
    assert_eq!(edited["status"], "paused");
    assert_eq!(edited["history_id"], paused["monitor"]["history_id"]);
    assert_eq!(s.requests.lock().unwrap().len(), requests);
    assert!(
        control(&app, Some("another-bot"), id, "resume")
            .await
            .is_err()
    );
    assert!(control(&app, Some(&bot.id), id, "run_now").await.is_err());
    let resumed = control(&app, Some(&bot.id), id, "resume").await.unwrap();
    assert_eq!(resumed["monitor"]["enabled"], true);
    let requests = s.requests.lock().unwrap().len();
    let repeated = control(&app, Some(&bot.id), id, "resume").await.unwrap();
    assert_eq!(
        repeated["monitor"]["history_id"],
        resumed["monitor"]["history_id"]
    );
    assert_eq!(s.requests.lock().unwrap().len(), requests);
    let edited=save_for_bot(&app,&bot,&json!({"id":id,"account_id":"ca_mail","name":"Renamed QA inbox","instructions":"Revised criteria"})).await.unwrap();
    assert_eq!(edited["next_check"], resumed["monitor"]["next_check"]);
    assert_eq!(s.requests.lock().unwrap().len(), requests);
    let run_id = app
        .db
        .queue(&bot.id, "Remove my test inbox monitor", 0)
        .unwrap();
    let run = app.db.run(&run_id).unwrap();
    let removed = crate::runtime::call_tool(
        &app,
        &bot,
        &run,
        "routine_control",
        json!({"id":id,"action":"remove"}),
    )
    .await
    .unwrap();
    assert_ne!(removed["failed"], true);
    assert!(watches(&app.db).unwrap().is_empty());
    assert!(app.db.run(&run_id).is_ok());
    server.abort();
}

#[tokio::test]
async fn setup_choice_creates_one_constant_routine_and_unifies_catalogue() {
    let (app, mut bot, s, server) = setup().await;
    bot.approval_mode = "full".into();
    app.db.save_bot(&bot).unwrap();
    app.db
        .queue(&bot.id, "Can you monitor my Gmail inbox?", 0)
        .unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let args = json!({"topic_key":"work-inbox"});
    let result = crate::runtime::call_tool(&app, &bot, &run, "inbox_monitor_setup", args.clone())
        .await
        .unwrap();
    assert_eq!(result["deferred_question"], true);
    let result: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["question"]["options"][0], "Monitor activity");
    assert!(
        result["question"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "Set a custom schedule")
    );
    assert_eq!(result["inbox_setup"]["account_id"], "ca_mail");
    assert!(watches(&app.db).unwrap().is_empty());
    assert!(s.requests.lock().unwrap().is_empty());
    assert_eq!(crate::runtime::call_tool(&app,&bot,&run,"routine_create",json!({"trigger":"activity","account_id":"ca_mail","prompt":"Alert me about client replies."})).await.unwrap()["failed"],true);
    let repeated = super::setup(&app, &bot, &run, &args).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(repeated["text"].as_str().unwrap()).unwrap()["question"]["id"],
        result["question"]["id"]
    );
    app.db.finish(&run.id, "completed", "", "").unwrap();
    let answer = app
        .db
        .answer_question(
            result["question"]["id"].as_str().unwrap(),
            crate::questions::Answer {
                selected: Some(0),
                custom: None,
            },
        )
        .unwrap();
    let next = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(next.id, answer.continuation_run_id);
    let create = json!({"trigger":"activity","account_id":"ca_mail","prompt":"Alert me about client replies; stay quiet otherwise."});
    for _ in 0..2 {
        crate::runtime::call_tool(&app, &bot, &next, "routine_create", create.clone())
            .await
            .unwrap();
    }
    assert_eq!(watches(&app.db).unwrap().len(), 1);
    assert!(app.db.routines().unwrap().is_empty());
    let catalogue = routines(&app.db).unwrap();
    assert_eq!(catalogue.len(), 1);
    assert_eq!(catalogue[0]["name"], "Monitor work@example.test inbox");
    assert_eq!(catalogue[0]["frequency"], "Constant");
    assert_eq!(catalogue[0]["monitor"]["status"], "fast");
    let listed = crate::runtime::call_tool(&app, &bot, &next, "routines_list", json!({}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(listed["text"].as_str().unwrap()).unwrap(),
        json!(catalogue)
    );
    let mut invalid = create.clone();
    invalid["interval_seconds"] = json!(60);
    assert_eq!(
        crate::runtime::call_tool(&app, &bot, &next, "routine_create", invalid)
            .await
            .unwrap()["failed"],
        true
    );
    crate::runtime::call_tool(
        &app,
        &bot,
        &next,
        "routine_create",
        json!({"name":"Morning review","prompt":"Review inbox","interval_seconds":86400}),
    )
    .await
    .unwrap();
    let catalogue = routines(&app.db).unwrap();
    assert_eq!(catalogue.len(), 2);
    assert_eq!(catalogue[0]["trigger"], "schedule");
    assert_eq!(catalogue[1]["trigger"], "activity");
    use tower::ServiceExt;
    let response = crate::web::router(app.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/routines")
                .header("authorization", format!("Bearer {}", app.token))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!(catalogue)
    );
    server.abort();
}

#[tokio::test]
async fn setup_connection_and_account_choices_do_not_start_monitoring() {
    let (app, bot, s, server) = setup().await;
    app.db.queue(&bot.id, "Watch my inbox", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let original = app.db.setting("composio").unwrap().unwrap();
    app.db
        .save_setting(
            "composio",
            &json!({"user_id":"owner","accounts":{},"configs":{}}),
        )
        .unwrap();
    let result = super::setup(&app, &bot, &run, &json!({"topic_key":"missing"})).unwrap();
    let v: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["toolkit"], "gmail");
    assert!(v["question_id"].is_string());
    assert_eq!(app.db.question(v["question_id"].as_str().unwrap()).unwrap().options[1], "Not now");
    let mut multiple = original.clone();
    multiple["accounts"]["personal"] = multiple["accounts"]["mail"].clone();
    multiple["accounts"]["personal"]["id"] = json!("ca_personal");
    multiple["accounts"]["personal"]["name"] = json!("Personal");
    app.db.save_setting("composio", &multiple).unwrap();
    let result = super::setup(&app, &bot, &run, &json!({"topic_key":"multiple"})).unwrap();
    let v: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["inbox_setup"]["phase"], "account");
    assert_eq!(v["question"]["options"].as_array().unwrap().len(), 2);
    assert!(
        super::setup(
            &app,
            &bot,
            &run,
            &json!({"topic_key":"invalid","account_id":"another_workspace"})
        )
        .is_err()
    );
    assert!(watches(&app.db).unwrap().is_empty());
    assert!(app.db.routines().unwrap().is_empty());
    assert!(s.requests.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn custom_setup_answer_preserves_real_weekly_schedule() {
    let (app, mut bot, _, server) = setup().await;
    bot.approval_mode = "full".into();
    app.db.save_bot(&bot).unwrap();
    app.db.queue(&bot.id, "Watch my work mail", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let result = crate::runtime::call_tool(
        &app,
        &bot,
        &run,
        "inbox_monitor_setup",
        json!({"topic_key":"custom-inbox"}),
    )
    .await
    .unwrap();
    let v: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    app.db.finish(&run.id, "completed", "", "").unwrap();
    app.db
        .answer_question(
            v["question"]["id"].as_str().unwrap(),
            crate::questions::Answer {
                selected: None,
                custom: Some("Weekdays hourly from 09:00 through 17:00 America/New_York".into()),
            },
        )
        .unwrap();
    let next = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert!(next.prompt.contains("America/New_York"));
    let schedule = json!({"timezone":"America/New_York","days":[1,2,3,4,5],"start":"09:00","end":"17:00","every_minutes":60});
    crate::runtime::call_tool(&app,&bot,&next,"routine_create",json!({"name":"Work inbox review","prompt":"Review Gmail account ca_mail. Alert on new client requests, finish_quietly otherwise.","schedule":schedule})).await.unwrap();
    let rows = routines(&app.db).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["schedule"], schedule);
    assert_eq!(rows[0]["trigger"], "schedule");
    assert!(watches(&app.db).unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn empty_checks_do_not_run_ai_new_mail_deduplicates_and_busy_bot_batches() {
    let (app, bot, s, server) = setup().await;
    let v = save_watch(&app, input(&bot)).await.unwrap();
    assert_eq!(v["status"], "fast");
    let mut w = watches(&app.db).unwrap().remove(0);
    queue_pending(&app.db).unwrap();
    assert!(app.db.runs(None).unwrap().is_empty());
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert!(app.db.runs(None).unwrap().is_empty());
    *s.history.lock().unwrap() = mail(&["a", "b"], "105");
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert!(run.prompt.contains("\"message_id\":\"a\""));
    assert!(run.prompt.contains("ca_mail"));
    assert!(
        crate::runtime::instructions(&app, &bot, &run)
            .unwrap()
            .contains("new Gmail inbox messages")
    );
    assert!(
        app.db
            .chat_messages(&format!("dm-{}", bot.id))
            .unwrap()
            .is_empty()
    );
    *s.history.lock().unwrap() = mail(&["b", "c"], "110");
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    crate::runtime::call_tool(&app, &bot, &run, "finish_quietly", json!({}))
        .await
        .unwrap();
    app.db
        .finish(&run.id, "completed", "Nothing needs attention", "")
        .unwrap();
    app.db.event(&run.id, "run_finished", json!({})).unwrap();
    app.db.chat_complete(&run).unwrap();
    assert!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    queue_pending(&app.db).unwrap();
    let next = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert!(next.prompt.contains("\"message_id\":\"c\""));
    assert!(!next.prompt.contains("\"message_id\":\"b\""));
    app.db
        .finish(&next.id, "completed", "A client needs your reply", "")
        .unwrap();
    app.db.event(&next.id, "run_finished", json!({})).unwrap();
    app.db.chat_complete(&next).unwrap();
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let notification = app.db.notifications(Some(0)).unwrap()["items"][0].clone();
    assert_eq!(
        notification["title"],
        app.db.bot(&next.bot_id).unwrap().name
    );
    assert_eq!(notification["body"], "A client needs your reply");
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 2);
    assert_eq!(
        s.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] != "GET")
            .count(),
        0
    );
    server.abort();
}
#[tokio::test]
async fn expiry_recovers_cursor_without_backlog_replay_and_failures_notify_once() {
    let (app, bot, s, server) = setup().await;
    save_watch(&app, input(&bot)).await.unwrap();
    let mut w = watches(&app.db).unwrap().remove(0);
    *s.expired.lock().unwrap() = true;
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    *s.fail.lock().unwrap() = true;
    for _ in 0..2 {
        let e = sync(&app, &mut w).await.unwrap_err();
        set_error(&app, &mut w, &e.to_string()).unwrap();
    }
    assert_eq!(w.status, "error");
    assert_eq!(
        app.db
            .runs(None)
            .unwrap()
            .iter()
            .filter(|r| r.status == "failed")
            .count(),
        1
    );
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    *s.fail.lock().unwrap() = false;
    sync(&app, &mut w).await.unwrap();
    assert_eq!(w.status, "fast");
    assert!(w.error.is_empty());
    server.abort();
}
#[tokio::test]
async fn account_binding_pause_and_push_mode_are_explicit() {
    let (app, bot, s, server) = setup().await;
    let mut bad = input(&bot);
    bad.account_id = "ca_other".into();
    assert!(save_watch(&app, bad).await.is_err());
    assert!(s.requests.lock().unwrap().is_empty());
    let mut push = input(&bot);
    push.mode = "push".into();
    assert!(save_watch(&app, push.clone()).await.is_err());
    push.topic = "projects/my-project/topics/kindred-mail".into();
    push.service_account = "push@my-project.iam.gserviceaccount.com".into();
    push.public_url = "https://kindred.example.test".into();
    let v = save_watch(&app, push).await.unwrap();
    assert_eq!(v["status"], "waiting_for_push");
    assert!(v["expires_at"].as_i64().unwrap() > db::now());
    let id = v["id"].as_str().unwrap();
    assert!(receive(&app, id, &Default::default(), b"{}").await.is_err());
    *s.history.lock().unwrap() = mail(&["a"], "110");
    let mut w = watches(&app.db).unwrap().remove(0);
    sync(&app, &mut w).await.unwrap();
    queue_pending(&app.db).unwrap();
    assert!(pause(State(app.clone()), Path(id.into())).await.is_ok());
    assert!(!watches(&app.db).unwrap()[0].input.enabled);
    assert!(app.db.claim_bot(&bot.id).unwrap().is_none());
    assert_eq!(
        s.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == "POST")
            .count(),
        1
    );
    server.abort();
}
#[tokio::test]
async fn push_receipts_bind_mailbox_deduplicate_and_preserve_concurrent_delivery() {
    use base64::Engine;
    let (app, bot, _, server) = setup().await;
    let mut p = input(&bot);
    p.mode = "push".into();
    p.topic = "projects/my-project/topics/inbox".into();
    p.service_account = "push@my-project.iam.gserviceaccount.com".into();
    p.public_url = "https://kindred.example.test".into();
    save_watch(&app, p).await.unwrap();
    let stale = watches(&app.db).unwrap().remove(0);
    let payload = |mail: &str, id: &str| {
        serde_json::to_vec(&json!({"message":{"messageId":id,"data":base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"emailAddress":mail,"historyId":"110"})).unwrap())}})).unwrap()
    };
    assert!(receive_payload(&app, &stale, &payload("wrong@example.test", "d1")).is_err());
    assert_eq!(
        receive_payload(&app, &stale, &payload("work@example.test", "d1")).unwrap(),
        StatusCode::NO_CONTENT
    );
    receive_payload(&app, &stale, &payload("work@example.test", "d1")).unwrap();
    let fresh = watches(&app.db).unwrap().remove(0);
    assert_eq!(fresh.push_seq, 1);
    assert!(fresh.next_check <= db::now());
    write(&app.db.0.lock().unwrap(), &stale).unwrap();
    let mut merged = watches(&app.db).unwrap().remove(0);
    assert_eq!(merged.push_seq, 1);
    sync(&app, &mut merged).await.unwrap();
    assert_eq!(merged.status, "push");
    assert!(merged.next_check >= db::now() + 295);
    app.db
        .save_setting("_account_disabled", &json!(true))
        .unwrap();
    receive_payload(&app, &merged, &payload("work@example.test", "d2")).unwrap();
    assert_eq!(watches(&app.db).unwrap()[0].push_seq, 1);
    server.abort();
}
#[test]
fn queued_mail_and_deduplication_survive_database_reopen() {
    let root = std::env::temp_dir().join(format!("kindred-mail-restart-{}", db::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("mail.db");
    let first = Db::open(path.to_str().unwrap()).unwrap();
    let bot = crate::tests::bot(&first, "codex");
    let mut input = input(&bot);
    input.id = db::id();
    let watch = Watch {
        input,
        mailbox: "work@example.test".into(),
        history_id: "12345678901234567890".into(),
        status: "fast".into(),
        error: String::new(),
        checked_at: 1,
        push_at: 0,
        push_seq: 0,
        next_check: 15,
        expires_at: 0,
        renew_after: 0,
        created: 1,
        since: 1,
        failures: 0,
        notice_error: false,
    };
    write(&first.0.lock().unwrap(), &watch).unwrap();
    first
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO mail_receipts VALUES(?,'new',?,1,NULL)",
            params![
                watch.input.id,
                json!({"message_id":"new","account_id":"ca_mail"}).to_string()
            ],
        )
        .unwrap();
    drop(first);
    let reopened = Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        watches(&reopened).unwrap()[0].history_id,
        "12345678901234567890"
    );
    queue_pending(&reopened).unwrap();
    queue_pending(&reopened).unwrap();
    assert_eq!(reopened.runs(None).unwrap().len(), 1);
    drop(reopened);
    let again = Db::open(path.to_str().unwrap()).unwrap();
    queue_pending(&again).unwrap();
    assert_eq!(again.runs(None).unwrap().len(), 1);
    drop(again);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn monitor_controls_need_auth_and_public_push_cannot_queue_unsigned_mail() {
    use tower::ServiceExt;
    let (app, bot, _, server) = setup().await;
    let mut p = input(&bot);
    p.mode = "push".into();
    p.topic = "projects/my-project/topics/inbox".into();
    p.service_account = "push@my-project.iam.gserviceaccount.com".into();
    p.public_url = "https://kindred.example.test".into();
    let value = save_watch(&app, p).await.unwrap();
    let id = value["id"].as_str().unwrap();
    let root = std::env::temp_dir().join(format!("kindred-mail-portal-{}", db::id()));
    let mut config = app.config.clone();
    config.profiles.enabled = true;
    config.profiles.directory = root.to_string_lossy().into();
    let portal = crate::profiles::Profiles::open(config, Some(app.clone()), false).unwrap();
    let router = crate::profiles::router(portal);
    for (method, path, expected) in [
        ("GET", "/api/inbox-monitors".to_owned(), 401),
        ("POST", format!("/hooks/gmail/{id}"), 401),
        ("GET", format!("/hooks/gmail/{id}"), 405),
    ] {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(req).await.unwrap().status().as_u16(),
            expected
        );
    }
    assert!(app.db.runs(None).unwrap().is_empty());
    assert_eq!(watches(&app.db).unwrap()[0].push_seq, 0);
    drop(router);
    std::fs::remove_dir_all(root).unwrap();
    server.abort();
}
#[tokio::test]
async fn persisted_push_wakes_the_background_worker_without_a_routine() {
    use base64::Engine;
    let (app, bot, s, server) = setup().await;
    let mut p = input(&bot);
    p.mode = "push".into();
    p.topic = "projects/my-project/topics/inbox".into();
    p.service_account = "push@my-project.iam.gserviceaccount.com".into();
    p.public_url = "https://kindred.example.test".into();
    save_watch(&app, p).await.unwrap();
    let w = watches(&app.db).unwrap().remove(0);
    *s.history.lock().unwrap() = mail(&["arrived"], "115");
    let body=serde_json::to_vec(&json!({"message":{"messageId":"delivery","data":base64::engine::general_purpose::STANDARD.encode(br#"{"emailAddress":"work@example.test","historyId":"115"}"#)}})).unwrap();
    receive_payload(&app, &w, &body).unwrap();
    let task = tokio::spawn(worker(app.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !app.db.runs(None).unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(app.db.routines().unwrap().is_empty());
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    assert!(app.db.runs(None).unwrap()[0].prompt.contains("arrived"));
    server.abort();
}
