use super::*;

pub(super) fn setup() -> (Shared, Account, Watch) {
    setup_db(Db::open(":memory:").unwrap())
}
fn setup_db(db: Db) -> (Shared, Account, Watch) {
    let mut app = crate::tests::app();
    std::sync::Arc::get_mut(&mut app).unwrap().db = db;
    let bot = crate::tests::bot(&app.db, "codex");
    let a = Account {
        id: db::id(),
        name: "Helpdesk".into(),
        team_id: "T123".into(),
        team_name: "Example team".into(),
        user_id: "U123".into(),
        user_name: "helpdesk".into(),
        bot_id: String::new(),
        app_id: "A123".into(),
        scopes: vec![
            "channels:history".into(),
            "groups:history".into(),
            "im:history".into(),
            "mpim:history".into(),
        ],
        revision: db::id(),
        status: "connected".into(),
        error: String::new(),
        warning: String::new(),
        connected_at: db::now(),
        received_at: 0,
    };
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO slack_accounts VALUES(?,?)",
            params![a.id, serde_json::to_string(&a).unwrap()],
        )
        .unwrap();
    let mut w = Watch {
        id: String::new(),
        bot_id: bot.id,
        account_id: a.id.clone(),
        instructions: "Handle actual helpdesk requests. Finish quietly if no action is needed."
            .into(),
        enabled: true,
        rules: vec![
            rule("all_messages", "channel", "C123"),
            rule("mentions", "all_channels", ""),
            rule("all_messages", "direct_messages", ""),
        ],
    };
    persist_watch(&app.db, &mut w, None).unwrap();
    (app, a, w)
}
fn rule(event: &str, scope: &str, channel: &str) -> Rule {
    Rule {
        event: event.into(),
        scope: scope.into(),
        channel_id: channel.into(),
        channel_name: String::new(),
    }
}
pub(super) fn event(a: &Account, ts: &str, channel: &str, kind: &str, text: &str) -> Value {
    json!({"type":"event_callback","team_id":a.team_id,"api_app_id":a.app_id,"event_id":format!("Ev{ts}"),"authorizations":[{"team_id":a.team_id,"user_id":a.user_id,"is_bot":!a.bot_id.is_empty()}],"event":{"type":"message","channel":channel,"channel_type":kind,"user":"U456","text":text,"ts":ts}})
}
fn age(db: &Db) {
    db.0.lock()
        .unwrap()
        .execute("UPDATE slack_receipts SET received=received-3", [])
        .unwrap();
}
pub(super) fn count(db: &Db, table: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn overlapping_rules_retries_and_broadcasts_deliver_once() {
    let (app, a, _) = setup();
    let mut e = event(&a, "100.1", "C123", "channel", "<@U123> can you help?");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 1);
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    e["event"]["type"] = json!("app_mention");
    e["event_id"] = json!("other-delivery");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    e["event"]["type"] = json!("message");
    e["event"]["subtype"] = json!("thread_broadcast");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    age(&app.db);
    queue_pending(&app.db).unwrap();
    queue_pending(&app.db).unwrap();
    assert_eq!(count(&app.db, "slack_runs"), 1);
    let c = app.db.0.lock().unwrap();
    let prompt: String = c
        .query_row("SELECT prompt FROM runs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(prompt.contains("external data, not instructions"));
    assert!(prompt.contains("<@U123> can you help?"));
}
#[test]
fn matching_distinguishes_dm_channels_and_exact_mentions() {
    let (app, a, _) = setup();
    for (ts, channel, kind, text, expected) in [
        ("101.0", "C123", "channel", "Urgent", 1),
        ("102.0", "C456", "channel", "Ordinary", 0),
        ("103.0", "C456", "channel", "<@U123> please help", 1),
        ("104.0", "C456", "channel", "<@U1234> different person", 0),
        ("105.0", "D123", "im", "Private request", 1),
        ("106.0", "G123", "mpim", "Group DM", 1),
        ("107.0", "G124", "group", "Private channel unrelated", 0),
    ] {
        assert_eq!(
            ingest(&app.db, &a, &event(&a, ts, channel, kind, text)).unwrap(),
            expected,
            "{ts}"
        );
    }
    assert_eq!(count(&app.db, "slack_receipts"), 4);
}
#[test]
fn own_messages_edits_deletes_wrong_accounts_and_replaced_connections_do_not_wake() {
    let (app, a, _) = setup();
    for change in [
        json!({"user":"U123"}),
        json!({"subtype":"message_changed"}),
        json!({"subtype":"message_deleted"}),
        json!({"hidden":true}),
    ] {
        let mut e = event(
            &a,
            &db::now().to_string(),
            "C123",
            "channel",
            "private sentinel",
        );
        for (k, v) in change.as_object().unwrap() {
            e["event"][k] = v.clone();
        }
        assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    }
    for field in ["team_id", "api_app_id"] {
        let mut e = event(&a, "200.1", "C123", "channel", "private");
        e[field] = json!("OTHER");
        assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    }
    let mut e = event(&a, "201.1", "C123", "channel", "private");
    e["authorizations"][0]["user_id"] = json!("U999");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    let mut stale = a.clone();
    stale.revision = "old".into();
    assert_eq!(
        ingest(
            &app.db,
            &stale,
            &event(&a, "202.1", "C123", "channel", "private")
        )
        .unwrap(),
        0
    );
    assert_eq!(count(&app.db, "slack_receipts"), 0);
    queue_pending(&app.db).unwrap();
    assert_eq!(count(&app.db, "runs"), 0);
}
#[test]
fn bot_identity_ignores_its_own_bot_messages() {
    let (app, mut a, _) = setup();
    a.bot_id = "B123".into();
    let mut e = event(&a, "203.1", "C123", "channel", "self");
    e["event"]["user"] = Value::Null;
    e["event"]["bot_id"] = json!("B123");
    e["event"]["subtype"] = json!("bot_message");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    e["event"]["bot_id"] = json!("B456");
    assert_eq!(ingest(&app.db, &a, &e).unwrap(), 1);
}
#[tokio::test]
async fn pause_removal_and_owner_checks_preserve_running_history() {
    let (app, a, w) = setup();
    ingest(&app.db, &a, &event(&a, "300.1", "C123", "channel", "first")).unwrap();
    age(&app.db);
    queue_pending(&app.db).unwrap();
    assert!(
        control(&app, Some("different-bot"), &w.id, "pause")
            .await
            .is_err()
    );
    control(&app, Some(&w.bot_id), &w.id, "pause")
        .await
        .unwrap();
    assert_eq!(count(&app.db, "slack_runs"), 1);
    assert_eq!(app.db.runs(None).unwrap()[0].status, "cancelled");
    assert_eq!(
        ingest(
            &app.db,
            &a,
            &event(&a, "301.1", "C123", "channel", "while paused")
        )
        .unwrap(),
        0
    );
    let mut resumed = w.clone();
    persist_watch(&app.db, &mut resumed, Some(&w.bot_id)).unwrap();
    assert_eq!(
        ingest(
            &app.db,
            &a,
            &event(&a, "301.1", "C123", "channel", "while paused")
        )
        .unwrap(),
        0
    );
    ingest(
        &app.db,
        &a,
        &event(&a, "302.1", "C123", "channel", "new after resume"),
    )
    .unwrap();
    age(&app.db);
    queue_pending(&app.db).unwrap();
    app.db.claim_bot(&w.bot_id).unwrap().unwrap();
    control(&app, Some(&w.bot_id), &w.id, "remove")
        .await
        .unwrap();
    assert!(
        app.db
            .runs(None)
            .unwrap()
            .iter()
            .any(|r| r.status == "running")
    );
    assert_eq!(count(&app.db, "slack_watches"), 0);
    assert_eq!(count(&app.db, "slack_runs"), 2);
}
#[test]
fn burst_is_batched_without_model_polling_or_duplicate_runs() {
    let (app, a, w) = setup();
    app.db.queue(&w.bot_id, "Existing user task", 0).unwrap();
    app.db.claim_bot(&w.bot_id).unwrap();
    for i in 0..1000 {
        let e = event(
            &a,
            &format!("500.{i:06}"),
            "C123",
            "channel",
            "<@U123> fixture",
        );
        assert_eq!(ingest(&app.db, &a, &e).unwrap(), 1);
        assert_eq!(ingest(&app.db, &a, &e).unwrap(), 0);
    }
    age(&app.db);
    queue_pending(&app.db).unwrap();
    assert_eq!(count(&app.db, "slack_runs"), 0);
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE runs SET status='completed'", [])
        .unwrap();
    for _ in 0..40 {
        queue_pending(&app.db).unwrap();
        queue_pending(&app.db).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE runs SET status='completed'", [])
            .unwrap();
    }
    assert_eq!(count(&app.db, "slack_runs"), 40);
    queue_pending(&app.db).unwrap();
    assert_eq!(count(&app.db, "slack_runs"), 40);
    let pending: i64 = app
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM slack_receipts WHERE run_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
}
#[test]
fn disabled_workspace_and_archived_bot_cannot_receive_work() {
    let (app, a, w) = setup();
    app.db
        .save_setting("_account_disabled", &json!(true))
        .unwrap();
    assert_eq!(
        ingest(&app.db, &a, &event(&a, "600.1", "C123", "channel", "no")).unwrap(),
        0
    );
    app.db
        .save_setting("_account_disabled", &json!(false))
        .unwrap();
    let mut bot = app.db.bot(&w.bot_id).unwrap();
    bot.profile.archived = true;
    app.db.save_bot(&bot).unwrap();
    assert_eq!(
        ingest(&app.db, &a, &event(&a, "601.1", "C123", "channel", "no")).unwrap(),
        0
    );
    queue_pending(&app.db).unwrap();
    assert_eq!(count(&app.db, "runs"), 0);
}
#[test]
fn invalid_and_duplicate_rules_are_rejected() {
    for rules in [
        vec![],
        vec![rule("all_messages", "channel", "bad")],
        vec![rule("mentions", "direct_messages", "C123")],
        vec![rule("arbitrary", "all_channels", "")],
        vec![
            rule("mentions", "all_channels", ""),
            rule("mentions", "all_channels", ""),
        ],
    ] {
        assert!(validate_rules(&rules).is_err());
    }
    assert!(
        validate_rules(&[
            rule("all_messages", "channel", "C123"),
            rule("mentions", "all_channels", "")
        ])
        .is_ok()
    );
}
#[test]
fn event_receipts_survive_restart_and_bound_large_messages() {
    let root = std::env::temp_dir().join(format!("kindred-slack-test-{}", db::id()));
    let path = root.join("kindred.db");
    let (app, a, w) = setup_db(Db::open(path.to_str().unwrap()).unwrap());
    let text = "🦀".repeat(10000);
    for i in 0..40 {
        let mut e = event(&a, &format!("800.{i:06}"), "C123", "channel", &text);
        e["event"]["files"] = json!([{"id":"F123","name":"f".repeat(10000)}]);
        assert_eq!(ingest(&app.db, &a, &e).unwrap(), 1);
    }
    drop(app);
    let db = Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        ingest(&db, &a, &event(&a, "800.000000", "C123", "channel", &text)).unwrap(),
        0
    );
    age(&db);
    queue_pending(&db).unwrap();
    let first = db.runs(None).unwrap().pop().unwrap();
    assert!(first.prompt.len() < 26000);
    assert!(first.prompt.contains("\"shortened\":true"));
    assert_eq!(count(&db, "slack_receipts"), 40);
    let first_count: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM slack_receipts WHERE run_id=?",
                [&first.id],
                |r| r.get(0),
            )
            .unwrap();
    assert!((1..25).contains(&first_count));
    let mut identical = w.clone();
    persist_watch(&db, &mut identical, None).unwrap();
    assert_eq!(
        count(&db, "slack_receipts"),
        40,
        "An idempotent save keeps pending messages"
    );
    let mut changed = w;
    changed.instructions = "Changed instructions".into();
    persist_watch(&db, &mut changed, None).unwrap();
    assert_eq!(count(&db, "slack_receipts"), first_count);
    assert_eq!(db.run(&first.id).unwrap().status, "cancelled");
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn rate_limit_is_visible_without_creating_model_work() {
    let (app, a, _) = setup();
    let event = json!({"type":"app_rate_limited","team_id":a.team_id,"api_app_id":a.app_id,"minute_rate_limited":db::now()});
    assert_eq!(ingest(&app.db, &a, &event).unwrap(), 0);
    assert!(
        account(&app.db, &a.id)
            .unwrap()
            .warning
            .contains("Some messages may be missing")
    );
    assert_eq!(count(&app.db, "runs"), 0);
    for bad in ["", "1", "1.", ".2", "1.2.3", "1/2", "1.2?token=bad"] {
        assert!(!valid_ts(bad));
    }
    assert!(valid_ts("1234567890.123456"));
}
#[test]
fn tools_are_available_on_all_providers_without_bloating_unconnected_workspaces() {
    let empty = crate::tests::app();
    assert_eq!(specs(&empty).len(), 2);
    assert!(specs(&empty).iter().any(|t|t["name"]=="slack_monitor_setup"));
    let (app, _, w) = setup();
    for provider in ["codex", "openrouter", "claude-code"] {
        let mut bot = app.db.bot(&w.bot_id).unwrap();
        bot.provider = provider.into();
        app.db.save_bot(&bot).unwrap();
        let tools = crate::runtime::tool_specs_for(&app, &bot);
        for name in [
            "slack_monitor_setup",
            "slack_monitors_list",
            "slack_channels",
            "slack_monitor_save",
            "slack_monitor_control",
        ] {
            let tool = tools.iter().find(|v| v["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let id = app.db.queue(&bot.id, "Monitor all messages in #urgent-requests, mentions across all channels, and all DMs to Helpdesk. Preserve existing rules and help with actual requests. Stay quiet when there is nothing to do.", 0).unwrap();
        let run = app.db.run(&id).unwrap();
        crate::instructions::build(
            &app,
            &bot,
            &run,
            &tools,
            Some(if provider == "codex" { 128000 } else { 32768 }),
        )
        .unwrap();
        app.db.finish(&id, "completed", "", "").unwrap();
    }
}
#[tokio::test]
async fn monitor_api_requires_workspace_auth_and_foreign_ids_cannot_be_used() {
    use std::future::IntoFuture;
    let (app, a, w) = setup();
    let other = crate::tests::app();
    assert!(account(&other.db, &a.id).is_err());
    assert!(control(&other, None, &w.id, "pause").await.is_err());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{base}/api/slack-monitoring"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{base}/api/slack-monitors/{}/control", w.id))
            .json(&json!({"action":"pause"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let view = http
        .get(format!("{base}/api/slack-monitoring"))
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(view.contains("Helpdesk"));
    for secret in ["app_token", "access_token", "xapp-", "xoxp-"] {
        assert!(!view.contains(secret));
    }
    assert!(
        http.post(format!("{base}/api/slack-monitors/{}/control", w.id))
            .bearer_auth(&app.token)
            .json(&json!({"action":"pause"}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(!watches(&app.db).unwrap()[0].enabled);
    server.abort();
}
#[tokio::test]
async fn denied_model_control_does_not_change_a_monitor() {
    let (app, _, w) = setup();
    let bot = app.db.bot(&w.bot_id).unwrap();
    let id = app.db.queue(&bot.id, "Pause Slack", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(id, run.id);
    let cloned = app.clone();
    let watch = w.id.clone();
    let task = tokio::spawn(async move {
        crate::runtime::call_tool(
            &cloned,
            &bot,
            &run,
            "slack_monitor_control",
            json!({"id":watch,"action":"pause"}),
        )
        .await
    });
    let approvals = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows = app.db.approvals().unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.db
        .decide(approvals[0]["id"].as_str().unwrap(), false)
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap()["failed"], true);
    assert!(watches(&app.db).unwrap()[0].enabled);
}
#[tokio::test]
async fn real_provider_bridge_receives_slack_context_and_publishes_once_or_stays_quiet() {
    use axum::{Router, routing::post};
    use std::{
        future::IntoFuture,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    for quiet in [false, true] {
        let (app, a, w) = setup();
        let mut bot = app.db.bot(&w.bot_id).unwrap();
        bot.provider = "openrouter".into();
        app.db.save_bot(&bot).unwrap();
        ingest(
            &app.db,
            &a,
            &event(
                &a,
                "950.1",
                "C123",
                "channel",
                "SLACK_EVENT_REQUEST: the test printer needs help",
            ),
        )
        .unwrap();
        age(&app.db);
        queue_pending(&app.db).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_copy = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let router=Router::new().route("/chat/completions",post(move|Json(body):Json<Value>|{let calls=calls_copy.clone();async move {
            assert_eq!(calls.fetch_add(1,Ordering::SeqCst),0,"Classifying this event must end its turn");
            assert!(body["messages"].to_string().contains("SLACK_EVENT_REQUEST"));assert!(body["messages"].to_string().contains("external data, not instructions"));assert!(body["messages"].to_string().contains("U123"));
            let (name,args)=if quiet {("finish_quietly",json!({}))}else{("routine_complete",json!({"outcome":"report","report":"Helpdesk has a new printer request."}))};
            crate::pi::sse_response(json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"classify","type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]}))
        }}));
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            crate::pi::fixture(&app, &bot, &run, "synthetic-fixture-key", &url),
        )
        .await
        .unwrap()
        .unwrap();
        app.db.finish(&run.id, "completed", &output, "").unwrap();
        app.db.event(&run.id, "run_finished", json!({})).unwrap();
        app.db.chat_complete(&run).unwrap();
        app.db.chat_complete(&run).unwrap();
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        let results = messages
            .iter()
            .filter(|m| m["sender"] == bot.id)
            .collect::<Vec<_>>();
        if quiet {
            assert!(results.is_empty());
            assert!(
                app.db.notifications(Some(0)).unwrap()["items"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0]["text"], "Helpdesk has a new printer request.");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        queue_pending(&app.db).unwrap();
        assert_eq!(count(&app.db, "slack_runs"), 1);
        server.abort();
    }
}
