use crate::{
    db::{self, Db, Run},
    instructions, plans, runtime, tests,
};
use serde_json::{Value, json};
fn run(db: &Db) -> Run {
    let b = tests::bot(db, "codex");
    let id = db
        .queue(
            &b.id,
            "Build today's list and remind me at noon for the client in my objectives",
            0,
        )
        .unwrap();
    db.run(&id).unwrap()
}
fn list() -> Value {
    json!({"key":"today-2099-09-10","title":"Today's objectives","local_date":"2099-09-10","items":[{"title":"Prepare Northwind scan scope","owner":"bot","sources":[{"title":"Northwind objective","url":"https://monday.example/boards/10/items/20"}]},{"title":"Start Northwind scans","owner":"user"},{"title":"Review evidence","owner":"bot"}]})
}
fn reminder() -> Value {
    json!({"key":"northwind-scans-noon-2099-09-10","message":"Start scans for Northwind","local_time":"2099-09-10T12:00","timezone":"America/New_York","sources":[{"title":"Northwind objective","url":"https://monday.example/boards/10/items/20"}]})
}
fn saved(db: &Db, table: &str, id: &Value) -> Value {
    plans::record(&db.0.lock().unwrap(), table, id.as_str().unwrap()).unwrap()
}
fn count(db: &Db, table: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn list_focus_completion_revision_conflicts_and_editing_preserve_user_state() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.checklist_create(&r, list()).unwrap();
    assert_eq!(a["items"][0]["state"], "current");
    assert_eq!(a["items"][1]["owner"], "user");
    assert_eq!(a, db.checklist_create(&r, list()).unwrap());
    assert_eq!(count(&db, "checklists"), 1);
    let id = a["id"].as_str().unwrap();
    let second = a["items"][1]["id"].clone();
    let b = db
        .checklist_update(
            &r.chat_id,
            None,
            id,
            json!({"expected_revision":1,"item_id":second,"state":"current"}),
        )
        .unwrap();
    assert_eq!(b["items"][0]["state"], "pending");
    assert_eq!(b["items"][1]["state"], "current");
    assert!(
        db.checklist_update(
            &r.chat_id,
            Some(&r.bot_id),
            id,
            json!({"expected_revision":1,"items":a["items"]})
        )
        .is_err()
    );
    let c = db
        .checklist_update(
            &r.chat_id,
            None,
            id,
            json!({"expected_revision":2,"item_id":second,"state":"done"}),
        )
        .unwrap();
    assert_eq!(c["items"][0]["state"], "current");
    assert_eq!(c["items"][1]["state"], "done");
    assert_eq!(
        count(&db, "runs"),
        1,
        "Checkbox must not execute another task"
    );
    let mut revised = c["items"].as_array().unwrap().clone();
    revised.swap(0, 2);
    revised[0]["title"] = json!("Review the Northwind report");
    revised.push(json!({"title":"Send status update"}));
    let d = db
        .checklist_update(
            &r.chat_id,
            Some(&r.bot_id),
            id,
            json!({"expected_revision":3,"title":"Northwind today","items":revised}),
        )
        .unwrap();
    assert_eq!(d["items"][1]["id"], second);
    assert_eq!(d["items"][1]["state"], "done");
    assert!(d["items"][3]["id"].is_string());
    let chat = db.chat_messages(&r.chat_id).unwrap();
    let card = chat.iter().find(|m| m["kind"] == "checklist").unwrap();
    assert_eq!(card["planning"], d);
    assert_eq!(card["text"], "Northwind today");
    assert_eq!(card["run_id"], "");
    assert_eq!(
        db.chats()
            .unwrap()
            .iter()
            .find(|c| c.id == r.chat_id)
            .unwrap()
            .last_message
            .as_ref()
            .unwrap()["text"],
        "Northwind today"
    );
    db.checklist_update(
        &r.chat_id,
        None,
        id,
        json!({"expected_revision":4,"archived":true}),
    )
    .unwrap();
    assert_eq!(saved(&db, "checklists", &a["id"])["archived"], true);
}

#[test]
fn conversation_scope_and_source_validation_fail_closed() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let other = run(&db);
    let a = db.checklist_create(&r, list()).unwrap();
    let remind = db.reminder_set(&r, reminder()).unwrap();
    assert!(db.planning(&r.chat_id, Some(&other.bot_id)).is_err());
    assert!(
        db.planning(&other.chat_id, Some(&other.bot_id)).unwrap()["checklists"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        db.checklist_update(
            &other.chat_id,
            Some(&other.bot_id),
            a["id"].as_str().unwrap(),
            json!({"expected_revision":1,"archived":true})
        )
        .is_err()
    );
    assert!(
        db.reminder_update(
            &other.chat_id,
            Some(&other.bot_id),
            remind["id"].as_str().unwrap(),
            json!({"expected_revision":1,"cancel":true})
        )
        .is_err()
    );
    for url in [
        "javascript:alert(1)",
        "file:///secret",
        "https://user:pass@example.com/",
    ] {
        let mut bad = list();
        bad["key"] = json!(url);
        bad["items"][0]["sources"][0]["url"] = json!(url);
        assert!(db.checklist_create(&r, bad).is_err());
    }
    let mut bad = list();
    bad["key"] = json!("two-current");
    bad["items"][0]["state"] = json!("current");
    bad["items"][1]["state"] = json!("current");
    assert!(db.checklist_create(&r, bad).is_err());
    assert_eq!(count(&db, "checklists"), 1);
    assert_eq!(count(&db, "reminders"), 1);
}

#[test]
fn reminder_delivers_once_while_bot_busy_without_queue_or_vm_and_notifies() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    db.claim_bot(&r.bot_id).unwrap().unwrap();
    let a = db.reminder_set(&r, reminder()).unwrap();
    let cursor = db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
    let at = a["run_at"].as_i64().unwrap();
    db.tick(at - 1).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "pending");
    db.tick(at).unwrap();
    db.tick(at + 1).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "delivered");
    assert_eq!(count(&db, "runs"), 1);
    assert_eq!(db.run(&r.id).unwrap().status, "running");
    let msgs = db.chat_messages(&r.chat_id).unwrap();
    let delivered: Vec<_> = msgs.iter().filter(|m| m["kind"] == "reminder").collect();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0]["text"], "Start scans for Northwind");
    assert_eq!(delivered[0]["planning"]["delivered_at"], at);
    let feed = db.notifications(Some(cursor)).unwrap();
    assert_eq!(feed["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        feed["items"][0]["body"],
        "Reminder: Start scans for Northwind"
    );
    assert_eq!(feed["items"][0]["chat_id"], r.chat_id);
    assert!(
        db.notifications(Some(feed["cursor"].as_i64().unwrap()))
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        db.reminder_update(
            &r.chat_id,
            None,
            a["id"].as_str().unwrap(),
            json!({"expected_revision":2,"cancel":true})
        )
        .is_err()
    );
}

#[test]
fn reminder_cancellation_reschedule_duplicate_key_and_notification_preferences() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.reminder_set(&r, reminder()).unwrap();
    let id = a["id"].as_str().unwrap();
    let at = a["run_at"].as_i64().unwrap();
    assert_eq!(a, db.reminder_set(&r, reminder()).unwrap());
    let b = db
        .reminder_update(
            &r.chat_id,
            None,
            id,
            json!({"expected_revision":1,"cancel":true}),
        )
        .unwrap();
    assert_eq!(b["status"], "cancelled");
    db.tick(at).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "cancelled");
    assert!(
        db.reminder_update(
            &r.chat_id,
            None,
            id,
            json!({"expected_revision":1,"local_time":"2099-09-10T13:00"})
        )
        .is_err()
    );
    let c=db.reminder_update(&r.chat_id,None,id,json!({"expected_revision":2,"local_time":"2099-09-10T13:00","message":"Start approved Northwind scans"})).unwrap();
    assert_eq!(c["sources"], a["sources"]);
    assert_eq!(c["run_at"], at + 3600);
    db.tick(at + 3600).unwrap();
    assert_eq!(count(&db, "reminders"), 1);
    db.save_setting("general", &json!({"notifications":"none"}))
        .unwrap();
    assert!(
        db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    db.save_setting("general", &json!({"notifications":"input_needed"}))
        .unwrap();
    assert_eq!(
        db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut bot = db.bot(&r.bot_id).unwrap();
    bot.profile.notifications = false;
    db.save_bot(&bot).unwrap();
    assert!(
        db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn local_noon_dst_and_past_times_are_unambiguous() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.reminder_set(&r, reminder()).unwrap();
    assert_eq!(
        chrono::DateTime::from_timestamp(a["run_at"].as_i64().unwrap(), 0)
            .unwrap()
            .to_rfc3339(),
        "2099-09-10T16:00:00+00:00"
    );
    for local in [
        "2020-09-10T12:00",
        "2099-03-08T02:30",
        "2099-11-01T01:30",
        "2099-09-10",
        "not a date",
    ] {
        let mut v = reminder();
        v["key"] = json!(local);
        v["local_time"] = json!(local);
        assert!(db.reminder_set(&r, v).is_err(), "{local}");
    }
    let mut v = reminder();
    v["key"] = json!("unknown-zone");
    v["timezone"] = json!("Mars/Olympus");
    assert!(db.reminder_set(&r, v).is_err());
}

#[test]
fn delivery_rollback_is_atomic_and_retries_without_duplicates() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.reminder_set(&r, reminder()).unwrap();
    let at = a["run_at"].as_i64().unwrap();
    db.0.lock().unwrap().execute_batch("CREATE TRIGGER fixture_delivery_failure BEFORE INSERT ON events WHEN NEW.kind='reminder' BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(db.tick(at).is_err());
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "pending");
    assert!(
        !db.chat_messages(&r.chat_id)
            .unwrap()
            .iter()
            .any(|m| m["kind"] == "reminder")
    );
    db.0.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER fixture_delivery_failure;")
        .unwrap();
    db.tick(at + 50).unwrap();
    db.tick(at + 100).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["delivered_at"], at + 50);
    assert_eq!(
        db.chat_messages(&r.chat_id)
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "reminder")
            .count(),
        1
    );
}

#[test]
fn restart_preserves_list_and_delivers_overdue_reminder_once() {
    let root = std::env::temp_dir().join(format!("kindred-plans-{}", db::id()));
    let path = root.join("data.db");
    let db = Db::open(path.to_str().unwrap()).unwrap();
    let r = run(&db);
    let l = db.checklist_create(&r, list()).unwrap();
    let a = db.reminder_set(&r, reminder()).unwrap();
    let at = a["run_at"].as_i64().unwrap();
    db.finish(&r.id, "completed", "Plan saved", "").unwrap();
    drop(db);
    for _ in 0..2 {
        let reopened = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(saved(&reopened, "checklists", &l["id"]), l);
        reopened.tick(at + 600).unwrap();
        assert_eq!(
            reopened
                .chat_messages(&r.chat_id)
                .unwrap()
                .iter()
                .filter(|m| m["kind"] == "reminder")
                .count(),
            1
        );
        assert_eq!(
            saved(&reopened, "reminders", &a["id"])["delivered_at"],
            at + 600
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn archived_and_transferred_workspaces_pause_delivery_until_explicit_resumption() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let l = db.checklist_create(&r, list()).unwrap();
    let a = db.reminder_set(&r, reminder()).unwrap();
    let at = a["run_at"].as_i64().unwrap();
    db.finish(&r.id, "completed", "Saved", "").unwrap();
    db.0.lock()
        .unwrap()
        .execute("UPDATE chats SET archived=1 WHERE id=?", [&r.chat_id])
        .unwrap();
    db.tick(at).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "pending");
    db.0.lock()
        .unwrap()
        .execute("UPDATE chats SET archived=0 WHERE id=?", [&r.chat_id])
        .unwrap();
    let transfer = db::id();
    let package = db.prepare_transfer(&transfer, "My profile").unwrap();
    db.tick(at).unwrap();
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "pending");
    assert!(
        db.checklist_update(
            &r.chat_id,
            None,
            l["id"].as_str().unwrap(),
            json!({"expected_revision":1,"archived":true})
        )
        .is_err()
    );
    let destination = Db::open(":memory:").unwrap();
    destination.import_transfer(&package).unwrap();
    assert_eq!(saved(&destination, "checklists", &l["id"]), l);
    assert_eq!(
        saved(&destination, "reminders", &a["id"])["status"],
        "paused"
    );
    destination.tick(at + 50).unwrap();
    assert_eq!(
        saved(&destination, "reminders", &a["id"])["status"],
        "paused"
    );
    destination
        .reminder_update(
            &r.chat_id,
            None,
            a["id"].as_str().unwrap(),
            json!({"expected_revision":2,"local_time":"2099-09-10T13:00"}),
        )
        .unwrap();
    destination.tick(at + 3600).unwrap();
    assert_eq!(
        saved(&destination, "reminders", &a["id"])["status"],
        "delivered"
    );
}

#[tokio::test]
async fn runtime_contract_has_contextual_tools_and_live_user_revisions() {
    let app = tests::app();
    let r = run(&app.db);
    let b = app.db.bot(&r.bot_id).unwrap();
    let tools = runtime::tool_specs();
    let prompt = instructions::build(&app, &b, &r, &tools, Some(32000)).unwrap();
    assert!(prompt.contains("Resolve client names"));
    assert!(prompt.contains("reminder_set"));
    assert!(prompt.contains("checkbox"));
    let result = runtime::call_tool(&app, &b, &r, "checklist_create", list())
        .await
        .unwrap();
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .contains("planning_revisions")
    );
    let state = app.db.planning(&r.chat_id, Some(&b.id)).unwrap();
    let l = &state["checklists"][0];
    app.db
        .checklist_update(
            &r.chat_id,
            None,
            l["id"].as_str().unwrap(),
            json!({"expected_revision":1,"item_id":l["items"][0]["id"],"state":"done"}),
        )
        .unwrap();
    let read = runtime::call_tool(&app, &b, &r, "planning_list", json!({}))
        .await
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("Northwind"));
    let wrapped: Value = serde_json::from_str(read["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        wrapped["kindred_live_context"]["planning_revisions"][0]["revision"],
        2
    );
    let fresh = instructions::build(&app, &b, &r, &tools, Some(32000)).unwrap();
    assert!(fresh.contains("conversation_checklists"));
    assert!(fresh.contains("monday.example"));
}

#[tokio::test]
async fn planning_http_requires_auth_and_rejects_other_chat_mutation() {
    let app = tests::app();
    let r = run(&app.db);
    let other = run(&app.db);
    let l = app.db.checklist_create(&r, list()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let path = format!("{base}/api/chats/{}/planning", r.chat_id);
    assert_eq!(client.get(&path).send().await.unwrap().status(), 401);
    let data: Value = client
        .get(&path)
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(data["checklists"][0], l);
    let path = format!(
        "{base}/api/chats/{}/checklists/{}",
        other.chat_id,
        l["id"].as_str().unwrap()
    );
    assert_eq!(
        client
            .patch(&path)
            .bearer_auth(&app.token)
            .json(&json!({"expected_revision":1,"archived":true}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    server.abort();
}

#[test]
fn failed_executable_routine_does_not_rollback_notification_delivery() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.reminder_set(&r, reminder()).unwrap();
    let at = a["run_at"].as_i64().unwrap();
    db.0.lock().unwrap().execute("INSERT INTO routines(id,bot_id,name,prompt,interval_seconds,next_run,enabled,schedule) VALUES('broken',?,'Fixture','Check',60,0,1,'broken-json')",[&r.bot_id]).unwrap();
    assert!(db.tick(at).is_err());
    assert_eq!(saved(&db, "reminders", &a["id"])["status"], "delivered");
    assert!(db.tick(at + 1).is_err());
    assert_eq!(
        db.chat_messages(&r.chat_id)
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "reminder")
            .count(),
        1
    );
}

#[test]
fn planning_enrichment_cannot_hide_a_completed_action_after_membership_changes() {
    let app = tests::app();
    let r = run(&app.db);
    app.db.checklist_create(&r, list()).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE chats SET members='[]' WHERE id=?", [&r.chat_id])
        .unwrap();
    let original = json!({"text":"The authorized action completed","exit_code":0});
    assert_eq!(
        crate::conversation_updates::with_live_context(&app.db, &r, original.clone()).unwrap(),
        original
    );
}

#[tokio::test]
async fn reminder_tools_save_and_cancel_under_the_configured_approval_policy() {
    let app = tests::app();
    let r = run(&app.db);
    let mut b = app.db.bot(&r.bot_id).unwrap();
    assert!(runtime::needs_approval(&app, &b, "reminder_set", &reminder()).unwrap());
    b.approval_mode = "full".into();
    app.db.save_bot(&b).unwrap();
    let response = runtime::call_tool(&app, &b, &r, "reminder_set", reminder())
        .await
        .unwrap();
    assert_ne!(response["failed"], true);
    let plan = app.db.planning(&r.chat_id, Some(&b.id)).unwrap();
    let a = &plan["reminders"][0];
    let response = runtime::call_tool(
        &app,
        &b,
        &r,
        "reminder_update",
        json!({"id":a["id"],"expected_revision":a["revision"],"cancel":true}),
    )
    .await
    .unwrap();
    assert_ne!(response["failed"], true);
    assert_eq!(saved(&app.db, "reminders", &a["id"])["status"], "cancelled");
    assert_eq!(count(&app.db, "approvals"), 0);
}

#[test]
fn edited_list_moves_to_latest_position_without_a_second_visible_card() {
    let db = Db::open(":memory:").unwrap();
    let r = run(&db);
    let a = db.checklist_create(&r, list()).unwrap();
    let old = db.chat_messages(&r.chat_id).unwrap().into_iter().find(|m|m["kind"]=="checklist").unwrap();
    db.event(&r.id,"assistant",json!({"text":"An intervening update"})).unwrap();
    let id = a["id"].as_str().unwrap();
    db.checklist_update(&r.chat_id,Some(&r.bot_id),id,json!({"expected_revision":1,"title":"Refreshed objectives"})).unwrap();
    let messages = db.chat_messages(&r.chat_id).unwrap();
    assert_eq!(messages.iter().filter(|m|m["kind"]=="checklist").count(),1);
    let latest=messages.last().unwrap();
    assert_eq!(latest["planning"]["id"],a["id"]);
    assert!(latest["seq"].as_i64().unwrap()>old["seq"].as_i64().unwrap());
    assert_eq!(latest["text"],"Refreshed objectives");
}
