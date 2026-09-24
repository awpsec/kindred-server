use crate::{
    db::{self, Db, Routine},
    questions::{Answer, QuestionInput},
    runtime,
    schedules::WeeklySchedule,
    tests::{app, bot},
    web,
};
use chrono::{DateTime, TimeZone};
use serde_json::json;

fn time(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).unwrap().timestamp()
}
fn schedule() -> WeeklySchedule {
    WeeklySchedule {
        timezone: "America/New_York".into(),
        days: vec![1, 2, 3, 4, 5],
        start: "08:33".into(),
        end: "19:33".into(),
        every_minutes: 60,
    }
}
fn question() -> QuestionInput {
    QuestionInput {
        topic_key: "jira-trial-2026-09-09".into(),
        question: "Want to keep Jira Premium?".into(),
        context:
            "A synthetic email says the trial ends Wednesday. Billing: https://example.com/billing"
                .into(),
        options: vec![
            "Add a card to Jira".into(),
            "Let it lapse".into(),
            "I'll do it myself".into(),
        ],
    }
}
fn option(selected: usize) -> Answer {
    Answer {
        selected: Some(selected),
        custom: None,
    }
}

#[test]
fn weekly_schedule_keeps_exact_hours_weekdays_and_timezone_across_dst() {
    let s = schedule();
    let mut t = time("2026-09-07T08:32:59-04:00");
    for hour in 8..=19 {
        t = s.next_after(t).unwrap();
        let local = chrono_tz::America::New_York.timestamp_opt(t, 0).unwrap();
        assert_eq!(local.format("%H:%M").to_string(), format!("{hour:02}:33"));
    }
    assert_eq!(s.next_after(t).unwrap(), time("2026-09-08T08:33:00-04:00"));
    assert_eq!(
        s.next_after(time("2026-09-11T19:33:00-04:00")).unwrap(),
        time("2026-09-14T08:33:00-04:00")
    );
    assert_eq!(
        s.next_after(time("2026-10-30T19:33:00-04:00")).unwrap(),
        time("2026-11-02T08:33:00-05:00")
    );
    assert_eq!(
        s.next_after(time("2026-03-06T19:33:00-05:00")).unwrap(),
        time("2026-03-09T08:33:00-04:00")
    );
    assert!(!s.inside_window(time("2026-09-07T08:32:59-04:00")).unwrap());
    assert!(s.inside_window(time("2026-09-07T19:33:59-04:00")).unwrap());
    assert!(!s.inside_window(time("2026-09-07T19:34:00-04:00")).unwrap());
    assert!(!s.inside_window(time("2026-09-12T10:33:00-04:00")).unwrap());
    let spring = WeeklySchedule {
        days: vec![7],
        start: "02:30".into(),
        end: "02:30".into(),
        ..s.clone()
    };
    assert_eq!(
        spring
            .next_after(time("2026-03-08T00:00:00-05:00"))
            .unwrap(),
        time("2026-03-15T02:30:00-04:00")
    );
    let fall = WeeklySchedule {
        days: vec![7],
        start: "01:30".into(),
        end: "01:30".into(),
        ..s
    };
    assert_eq!(
        fall.next_after(time("2026-11-01T01:30:00-04:00")).unwrap(),
        time("2026-11-08T01:30:00-05:00")
    );
}
#[test]
fn weekly_schedule_rejects_invalid_or_ambiguous_configuration() {
    for value in [
        json!({"timezone":"Mars/Test"}),
        json!({"days":[]}),
        json!({"days":[1,1]}),
        json!({"days":[0]}),
        json!({"every_minutes":0}),
        json!({"start":"8:33"}),
        json!({"start":"23:00","end":"01:00"}),
    ] {
        let mut s = serde_json::to_value(schedule()).unwrap();
        for (k, v) in value.as_object().unwrap() {
            s[k] = v.clone();
        }
        assert!(
            serde_json::from_value::<WeeklySchedule>(s)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
#[test]
fn scheduled_checks_skip_off_hours_coalesce_busy_ticks_and_keep_the_minute() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let s = schedule();
    let r = Routine {
        id: db::id(),
        bot_id: b.id.clone(),
        name: "Inbox".into(),
        prompt: "Check inbox".into(),
        interval_seconds: 3600,
        next_run: time("2026-09-11T19:33:00-04:00"),
        enabled: true,
        run_at: None,
        schedule: Some(s),
    };
    db.save_routine(&r).unwrap();
    db.tick(time("2026-09-12T10:00:00-04:00")).unwrap();
    assert!(db.runs(None).unwrap().is_empty());
    assert_eq!(
        db.routines().unwrap()[0].next_run,
        time("2026-09-14T08:33:00-04:00")
    );
    let delayed = time("2026-09-14T08:39:00-04:00");
    db.tick(delayed).unwrap();
    db.tick(delayed).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 1);
    assert_eq!(
        db.routines().unwrap()[0].next_run,
        time("2026-09-14T09:33:00-04:00")
    );
    db.tick(time("2026-09-14T09:40:00-04:00")).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 1);
    assert_eq!(
        db.routines().unwrap()[0].next_run,
        time("2026-09-14T10:33:00-04:00")
    );
    let run = db.runs(None).unwrap()[0].clone();
    assert_eq!(run.chat_id, format!("dm-{}", b.id));
    assert!(db.chat_messages(&run.chat_id).unwrap().is_empty());
    let routine: String =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT routine_id FROM routine_runs WHERE run_id=?",
                [run.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(routine, r.id);
}

#[tokio::test]
async fn question_cards_persist_and_two_clients_queue_only_one_continuation() {
    let path = std::env::temp_dir().join(format!("kindred-questions-{}.db", db::id()));
    let file = path.to_str().unwrap();
    let db = Db::open(file).unwrap();
    let b = bot(&db, "codex");
    db.queue(&b.id, "Review the synthetic trial-expiry email", 0)
        .unwrap();
    let run = db.claim().unwrap().unwrap();
    db.ask_question(&run, question()).unwrap();
    let card = db.chat_messages(&run.chat_id).unwrap().pop().unwrap()["question"].clone();
    let id = card["id"].as_str().unwrap().to_string();
    // Restart while the provider is at the question boundary: the decision remains answerable.
    drop(db);
    let mut app = app();
    std::sync::Arc::get_mut(&mut app).unwrap().db = Db::open(file).unwrap();
    assert_eq!(app.db.question(&id).unwrap().status, "pending");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let url = format!("{origin}/api/questions/{id}/answer");
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"selected":1}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for bad in [
        json!({"selected":99}),
        json!({"selected":1,"custom":"other"}),
        json!({"custom":"  "}),
        json!({"custom":"x".repeat(4001)}),
        json!({"selected":1,"bot_id":"someone-else"}),
    ] {
        assert!(
            [400, 422].contains(
                &client
                    .post(&url)
                    .bearer_auth(&app.token)
                    .json(&bad)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            )
        );
    }
    let a = client
        .post(&url)
        .bearer_auth(&app.token)
        .json(&json!({"selected":1}))
        .send();
    let c = client
        .post(&url)
        .bearer_auth(&app.token)
        .json(&json!({"selected":1}))
        .send();
    let (a, c) = tokio::join!(a, c);
    let a: serde_json::Value = a.unwrap().json().await.unwrap();
    let c: serde_json::Value = c.unwrap().json().await.unwrap();
    assert_eq!(a["continuation_run_id"], c["continuation_run_id"]);
    assert_eq!(a["answer"], "Let it lapse");
    let continuation = app
        .db
        .run(a["continuation_run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(continuation.bot_id, b.id);
    assert_eq!(continuation.chat_id, run.chat_id);
    assert!(
        continuation
            .prompt
            .contains("USER'S RESPONSE: Let it lapse")
    );
    assert!(continuation.prompt.contains("https://example.com/billing"));
    assert_eq!(app.db.runs(None).unwrap().len(), 2);
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&app.token)
            .json(&json!({"selected":0}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        app.db
            .chat_messages(&run.chat_id)
            .unwrap()
            .into_iter()
            .find(|m| m["kind"] == "question")
            .unwrap()["question"]["selected"],
        1
    );
    server.abort();
    let _ = server.await;
    drop(app);
    let db = Db::open(file).unwrap();
    assert_eq!(db.question(&id).unwrap().answer, "Let it lapse");
    assert_eq!(
        db.decision_context(&b.id, &run.chat_id, Some("jira-trial-2026-09-09"))
            .unwrap()
            .len(),
        1
    );
    drop(db);
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{file}{suffix}"));
    }
}

#[tokio::test]
async fn pending_questions_end_tools_deduplicate_notify_once_and_retain_answers() {
    let app = app();
    let b = bot(&app.db, "codex");
    app.db.queue(&b.id, "Check inbox", 0).unwrap();
    let run = app.db.claim().unwrap().unwrap();
    let result = runtime::call_tool(
        &app,
        &b,
        &run,
        "ask_question",
        serde_json::to_value(question()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(result["deferred_question"], true);
    let q = app.db.decision_context(&b.id, &run.chat_id, None).unwrap()[0].clone();
    let id = q["id"].as_str().unwrap();
    let blocked = runtime::call_tool(
        &app,
        &b,
        &run,
        "remember",
        json!({"text":"must not be written"}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["failed"], true);
    assert_eq!(app.db.bot(&b.id).unwrap().memory, "");
    app.db.finish(&run.id, "completed", "", "").unwrap();
    app.db.chat_complete(&run).unwrap();
    app.db.event(&run.id, "run_finished", json!({})).unwrap();
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.db.queue(&b.id, "Next inbox check", 0).unwrap();
    let later = app.db.claim().unwrap().unwrap();
    app.db.ask_question(&later, question()).unwrap();
    app.db.finish(&later.id, "completed", "", "").unwrap();
    app.db.event(&later.id, "run_finished", json!({})).unwrap();
    assert_eq!(
        app.db
            .decision_context(&b.id, &run.chat_id, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let saved = app.db.answer_question(id, option(2)).unwrap();
    let follow = app.db.claim().unwrap().unwrap();
    assert_eq!(follow.id, saved.continuation_run_id);
    let owned = app.db.decisions_for_run(&follow, None).unwrap();
    assert_eq!(owned[0]["is_current_continuation"], true);
    assert_eq!(owned[0]["continuation_status"], "running");
    assert_eq!(
        app.db.decisions_for_run(&later, None).unwrap()[0]["is_current_continuation"],
        false
    );
    assert!(
        runtime::instructions(&app, &b, &follow)
            .unwrap()
            .contains(&format!("Current task run ID: {}", follow.id))
    );
    assert!(
        follow
            .prompt
            .contains("THIS TASK IS THE ASSIGNED CONTINUATION")
    );
    assert!(follow.prompt.contains("I'll do it myself"));
    let existing = app.db.ask_question(&follow, question()).unwrap();
    assert_eq!(existing["deferred_question"], false);
    assert!(
        existing["text"]
            .as_str()
            .unwrap()
            .contains("I'll do it myself")
    );
    assert_eq!(app.db.runs(None).unwrap().len(), 3);
    assert!(
        runtime::instructions(&app, &b, &follow)
            .unwrap()
            .contains("I'll do it myself")
    );
}

#[test]
fn custom_answers_and_membership_validation_preserve_question_state() {
    let app = app();
    let b = bot(&app.db, "codex");
    app.db.queue(&b.id, "Choose a path", 0).unwrap();
    let run = app.db.claim().unwrap().unwrap();
    app.db.ask_question(&run, question()).unwrap();
    let id = app.db.decision_context(&b.id, &run.chat_id, None).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.db.finish(&run.id, "completed", "", "").unwrap();
    let mut chat = app.db.chat(&run.chat_id).unwrap();
    chat.archived = true;
    app.db.save_chat(&chat).unwrap();
    assert!(app.db.answer_question(&id, option(0)).is_err());
    assert_eq!(app.db.question(&id).unwrap().status, "pending");
    chat.archived = false;
    app.db.save_chat(&chat).unwrap();
    let saved = app
        .db
        .answer_question(
            &id,
            Answer {
                selected: None,
                custom: Some("  Ask me next month instead.  ".into()),
            },
        )
        .unwrap();
    assert_eq!(saved.answer, "Ask me next month instead.");
    assert_eq!(saved.selected, None);
}

#[tokio::test]
async fn routine_editor_roundtrip_manual_run_and_quiet_checks_preserve_notification_policy() {
    let app = app();
    let b = bot(&app.db, "codex");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let payload = json!({"id":"","bot_id":b.id,"name":"Weekday inbox","prompt":"Check only new mail","interval_seconds":3600,"next_run":1,"enabled":true,"schedule":schedule()});
    let response = client
        .post(format!("{origin}/api/routines"))
        .bearer_auth(&app.token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let r: Routine = response.json().await.unwrap();
    assert_eq!(r.schedule, Some(schedule()));
    assert!(r.next_run > db::now());
    let mut edited = r.clone();
    edited.prompt = "Check only important new mail".into();
    edited.next_run = 1;
    let result: Routine = client
        .post(format!("{origin}/api/routines"))
        .bearer_auth(&app.token)
        .json(&edited)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.next_run, r.next_run);
    let url = format!("{origin}/api/routines/{}/run", r.id);
    assert_eq!(client.post(&url).send().await.unwrap().status(), 401);
    let response: serde_json::Value = client
        .post(&url)
        .bearer_auth(&app.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run = app.db.claim().unwrap().unwrap();
    assert_eq!(run.id, response["run_id"]);
    assert_eq!(run.prompt, edited.prompt);
    assert!(app.db.chat_messages(&run.chat_id).unwrap().is_empty());
    assert!(
        runtime::instructions(&app, &b, &run)
            .unwrap()
            .contains("Current task trigger: scheduled routine check")
    );
    let quiet = runtime::call_tool(&app, &b, &run, "finish_quietly", json!({}))
        .await
        .unwrap();
    assert_eq!(quiet["finish_quietly"], true);
    app.db.finish(&run.id, "completed", "", "").unwrap();
    app.db.event(&run.id, "run_finished", json!({})).unwrap();
    assert!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    app.db.queue(&b.id, "Normal chat", 0).unwrap();
    let normal = app.db.claim().unwrap().unwrap();
    assert!(
        runtime::instructions(&app, &b, &normal)
            .unwrap()
            .contains("Current task trigger: conversation or assigned continuation")
    );
    assert_eq!(
        runtime::call_tool(&app, &b, &normal, "finish_quietly", json!({}))
            .await
            .unwrap()["failed"],
        true
    );
    app.db
        .finish(&run.id, "failed", "", "A real error")
        .unwrap();
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn selecting_an_action_still_uses_the_existing_external_approval_policy() {
    let app = app();
    let mut b = bot(&app.db, "codex");
    b.approval_mode = "auto".into();
    app.db.save_bot(&b).unwrap();
    app.db.queue(&b.id, "Review trial", 0).unwrap();
    let run = app.db.claim().unwrap().unwrap();
    app.db.ask_question(&run, question()).unwrap();
    app.db.finish(&run.id, "completed", "", "").unwrap();
    let id = app.db.decision_context(&b.id, &run.chat_id, None).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.db.answer_question(&id, option(0)).unwrap();
    let follow = app.db.claim().unwrap().unwrap();
    let owner = app.clone();
    let work = tokio::spawn(async move {
        runtime::call_tool(
            &owner,
            &b,
            &follow,
            "guest_exec",
            json!({"command":"echo should-not-execute","action_scope":"external"}),
        )
        .await
    });
    let approval = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(a) = app.db.approvals().unwrap().first() {
                break a.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.db
        .decide(approval["id"].as_str().unwrap(), false)
        .unwrap();
    let result = work.await.unwrap().unwrap();
    assert_eq!(result["failed"], true);
    assert!(result["text"].as_str().unwrap().contains("declined"));
}

#[test]
fn group_questions_keep_topic_scope_but_arrive_and_resume_in_private() {
    let app=app();let b=bot(&app.db,"codex");let peer=bot(&app.db,"codex");let mut ids=vec![];
    for name in ["First group","Second group"] {
        let chat=crate::chats::Chat{id:db::id(),name:name.into(),description:String::new(),bot_only:true,members:vec![b.id.clone(),peer.id.clone()],archived:false,pinned:false,last_message:None};
        app.db.save_chat(&chat).unwrap();app.db.queue(&b.id,"Need an owner answer",0).unwrap();
        let mut run=app.db.claim_bot(&b.id).unwrap().unwrap();
        app.db.0.lock().unwrap().execute("UPDATE runs SET chat_id=? WHERE id=?",rusqlite::params![chat.id,run.id]).unwrap();run.chat_id=chat.id.clone();
        let result=app.db.ask_question(&run,question()).unwrap();let result:serde_json::Value=serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
        let id=result["question"]["id"].as_str().unwrap().to_string();
        assert_eq!(app.db.question(&id).unwrap().delivery_chat_id,format!("dm-{}",b.id));
        assert!(app.db.chat_messages(&chat.id).unwrap().is_empty());
        let duplicate=app.db.ask_question(&run,question()).unwrap();assert!(duplicate["text"].as_str().unwrap().contains(&id));
        assert_eq!(app.db.decision_context(&b.id,&chat.id,None).unwrap().len(),1);
        app.db.finish(&run.id,"completed","","").unwrap();ids.push(id);
    }
    assert_ne!(ids[0],ids[1]);
    assert_eq!(app.db.chat_messages(&format!("dm-{}",b.id)).unwrap().iter().filter(|m|m["kind"]=="question").count(),2);
    for id in ids {let q=app.db.answer_question(&id,Answer{selected:Some(0),custom:None}).unwrap();assert_eq!(app.db.run(&q.continuation_run_id).unwrap().chat_id,q.delivery_chat_id);}
}
