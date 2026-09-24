use crate::{
    tests::{app, bot},
    web,
};
use serde_json::{Value, json};

#[tokio::test]
async fn activity_describes_started_searches_and_completed_results() {
    let app = app();
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&b.id, "Find my workflows", 0).unwrap();
    app.db.claim_bot(&b.id).unwrap().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/activity", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    for (tool, args, expected) in [
        ("local_skill_scan", json!({}), "Searching"),
        (
            "guest_exec",
            json!({"command":"find /workspace -name SKILL.md"}),
            "Searching",
        ),
        (
            "local_exec",
            json!({"command":"Get-ChildItem -Recurse"}),
            "Searching",
        ),
        (
            "connector_execute",
            json!({"tool_slug":"GOOGLEDRIVE_FIND_FILE"}),
            "Searching",
        ),
        (
            "local_read",
            json!({"path":"private-file"}),
            "Reading files",
        ),
        (
            "computer_open_url",
            json!({"url":"https://example.com"}),
            "Browsing",
        ),
        (
            "computer_click",
            json!({"x":1,"y":2}),
            "Working at the computer",
        ),
        (
            "guest_exec",
            json!({"command":"printf building-not-a-build"}),
            "Running a command",
        ),
    ] {
        for (kind, failed, label) in [
            ("tool_requested", false, "Preparing an action"),
            ("tool_started", false, expected),
            (
                "tool_result",
                false,
                if expected == "Searching" {
                    "Reviewing search results"
                } else {
                    "Reviewing results"
                },
            ),
            ("tool_result", true, "Action failed · checking the error"),
        ] {
            let body = if kind == "tool_result" {
                json!({"tool":tool,"failed":failed,"text":"private tool result"})
            } else {
                json!({"tool":tool,"args":args})
            };
            app.db.event(&id, kind, body).unwrap();
            let response: Value = client
                .get(&url)
                .bearer_auth(&app.token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response[&b.id]["label"], label, "{tool} {kind}");
            assert_eq!(response[&b.id]["run_id"], id);
            assert!(!response.to_string().contains("private"));
        }
    }
    app.db.finish(&id, "completed", "Done", "").unwrap();
    let response: Value = client
        .get(&url)
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response[&b.id]["label"], "Ready when you are");
    server.abort();
}

#[test]
fn public_replies_persist_during_work_and_finish_once() {
    let app = app();
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&b.id, "Check my accounts", 0).unwrap();
    let run = app.db.run(&id).unwrap();
    let first = "Yes, let me check the available connections.";
    let last = "You can sign in on my computer to continue.";
    app.db
        .event(&id, "assistant", json!({"text":first,"phase":"commentary"}))
        .unwrap();
    let live = app.db.chat_messages(&run.chat_id).unwrap();
    assert_eq!(live.last().unwrap()["text"], first);
    assert!(live.last().unwrap()["source_event_seq"].is_i64());
    app.db
        .event(&id, "reasoning", json!({"text":"private analysis"}))
        .unwrap();
    app.db
        .event(&id, "assistant", json!({"text":"  "}))
        .unwrap();
    app.db
        .event(
            &id,
            "assistant",
            json!({"text":last,"phase":"final_answer"}),
        )
        .unwrap();
    app.db.finish(&id, "completed", last, "").unwrap();
    app.db.chat_complete(&run).unwrap();
    app.db.chat_complete(&run).unwrap();
    let rows = app.db.chat_messages(&run.chat_id).unwrap();
    assert_eq!(rows.iter().filter(|m| m["text"] == first).count(), 1);
    assert_eq!(rows.iter().filter(|m| m["text"] == last).count(), 1);
    assert_eq!(rows.iter().filter(|m| m["kind"] == "result").count(), 1);
    assert!(!rows.iter().any(|m| m["text"] == "private analysis"));
    assert_eq!(rows.last().unwrap()["kind"], "result");
}

#[test]
fn historical_replies_recover_in_order_without_duplicate_results_or_replays() {
    let root = std::env::temp_dir().join(format!("kindred-history-{}", crate::db::id()));
    let path = root.join("kindred.db").to_string_lossy().into_owned();
    let db = crate::db::Db::open(&path).unwrap();
    let b = bot(&db, "codex");
    let id = db.queue(&b.id, "Original request", 0).unwrap();
    let run = db.run(&id).unwrap();
    db.finish(&id, "completed", "Final answer", "").unwrap();
    db.chat_complete(&run).unwrap();
    // Reproduce pre-fix storage: only the final result is in chat; public
    // replies exist solely in events. All share a one-second timestamp.
    {
        let c = db.0.lock().unwrap();
        for (kind, text) in [
            ("assistant", "Checking connections"),
            ("reasoning", "Not public"),
            ("assistant", "Checking the browser"),
            ("assistant", "Final answer"),
        ] {
            c.execute(
                "INSERT INTO events(run_id,kind,body,created) VALUES(?,?,?,1000)",
                rusqlite::params![id, kind, json!({"text":text}).to_string()],
            )
            .unwrap();
        }
        c.execute(
            "UPDATE chat_messages SET created=1000 WHERE chat_id=?",
            [&run.chat_id],
        )
        .unwrap();
    }
    let result_seq = db.chat_messages(&run.chat_id).unwrap().last().unwrap()["seq"].clone();
    drop(db);
    for _ in 0..2 {
        let db = crate::db::Db::open(&path).unwrap();
        let rows = db.chat_messages(&run.chat_id).unwrap();
        assert_eq!(
            rows.iter()
                .map(|m| m["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "Original request",
                "Checking connections",
                "Checking the browser",
                "Final answer"
            ]
        );
        assert_eq!(rows.last().unwrap()["seq"], result_seq);
        let latest = db.chat_message_page(&run.chat_id, None, None, 1).unwrap();
        assert_eq!(latest["messages"][0]["text"], "Final answer");
        let prior = db
            .chat_message_page(&run.chat_id, latest["page"]["first"].as_i64(), None, 2)
            .unwrap();
        assert_eq!(prior["messages"][0]["text"], "Checking connections");
        assert_eq!(prior["messages"][1]["text"], "Checking the browser");
        assert_eq!(db.runs(None).unwrap().len(), 1);
        assert_eq!(db.run(&id).unwrap().status, "completed");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn activity_setting_defaults_off_and_persists_across_partial_updates() {
    let app = app();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/settings", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let read: Value = client
        .get(&url)
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["show_activity"], false);
    let mut update = json!({"name":"You","identity":"","theme":"dark","show_activity":true});
    let saved: Value = client
        .put(&url)
        .bearer_auth(&app.token)
        .json(&update)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["show_activity"], true);
    update.as_object_mut().unwrap().remove("show_activity");
    client
        .put(&url)
        .bearer_auth(&app.token)
        .json(&update)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        app.db.setting("general").unwrap().unwrap()["show_activity"],
        true
    );
    update["show_activity"] = json!("false");
    assert!(
        client
            .put(&url)
            .bearer_auth(&app.token)
            .json(&update)
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    update["show_activity"] = json!(false);
    client
        .put(&url)
        .bearer_auth(&app.token)
        .json(&update)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        app.db.setting("general").unwrap().unwrap()["show_activity"],
        false
    );
    server.abort();
}

#[tokio::test]
async fn message_pages_are_bounded_stable_and_chat_scoped() {
    let app = app();
    let first = bot(&app.db, "codex");
    let second = bot(&app.db, "codex");
    let chat = format!("dm-{}", first.id);
    let other = format!("dm-{}", second.id);
    for b in [&first, &second] {
        app.db
            .save_chat(&crate::chats::Chat {
                bot_only: false,
                description: String::new(),
                id: format!("dm-{}", b.id),
                name: b.name.clone(),
                members: vec![b.id.clone()],
                archived: false,
                pinned: false,
                last_message: None,
            })
            .unwrap();
    }
    let expected;
    {
        let c = app.db.0.lock().unwrap();
        for n in 0..601 {
            c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user',?,'message',?)",rusqlite::params![chat,format!("Message {n}"),1000-n%3]).unwrap();
            c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','Other chat','message',1000)",[&other]).unwrap();
        }
        expected = c
            .prepare("SELECT seq FROM chat_messages WHERE chat_id=? ORDER BY created,seq")
            .unwrap()
            .query_map([&chat], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
    }
    let latest = app.db.chat_message_page(&chat, None, None, 50).unwrap();
    assert_eq!(latest["messages"].as_array().unwrap().len(), 50);
    assert_eq!(latest["page"]["has_before"], true);
    assert_eq!(latest["page"]["has_after"], false);
    let mut found = Vec::new();
    let mut page = latest.clone();
    loop {
        let rows = page["messages"].as_array().unwrap();
        assert!(rows.len() <= 50);
        assert!(rows.iter().all(|m| m["text"] != "Other chat"));
        let mut ids: Vec<i64> = rows.iter().map(|m| m["seq"].as_i64().unwrap()).collect();
        let positions: Vec<(i64, i64)> = rows
            .iter()
            .map(|m| (m["created"].as_i64().unwrap(), m["seq"].as_i64().unwrap()))
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
        ids.extend(found);
        found = ids;
        if page["page"]["has_before"] == false {
            break;
        }
        page = app
            .db
            .chat_message_page(&chat, page["page"]["first"].as_i64(), None, 50)
            .unwrap();
    }
    assert_eq!(
        found, expected,
        "All history, including messages beyond the former 300-message cutoff"
    );
    let forward = app
        .db
        .chat_message_page(&chat, None, Some(expected[49]), 50)
        .unwrap();
    assert_eq!(forward["messages"][0]["seq"], expected[50]);
    assert_eq!(forward["messages"][49]["seq"], expected[99]);
    assert_eq!(forward["page"]["has_after"], true);
    let refresh = app
        .db
        .chat_message_window(&chat, Some(expected[99]), Some(expected[50]), 150, true)
        .unwrap();
    assert_eq!(refresh["messages"], forward["messages"]);
    // New inserts never shift an already issued before/after cursor.
    app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','New arrival','message',2000)",[&chat]).unwrap();
    let newer = app
        .db
        .chat_message_page(&chat, None, latest["page"]["last"].as_i64(), 50)
        .unwrap();
    assert_eq!(newer["messages"].as_array().unwrap().len(), 1);
    assert_eq!(newer["messages"][0]["text"], "New arrival");
    assert_eq!(
        app.db
            .chat_message_window(&chat, Some(expected[99]), Some(expected[50]), 150, true)
            .unwrap()["messages"],
        forward["messages"]
    );
    assert!(app.db.chat_message_page(&chat, None, None, 151).is_err());
    assert!(app.db.chat_message_page(&chat, None, None, 0).is_err());
    assert!(
        app.db
            .chat_message_page(&chat, Some(10), Some(20), 50)
            .is_err()
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/chats/{chat}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    assert_eq!(client.get(&base).send().await.unwrap().status(), 401);
    let response: Value = client
        .get(&base)
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["messages"].as_array().unwrap().len(), 50);
    assert_eq!(response["chat"]["id"], json!(chat));
    for query in [
        "limit=151",
        "limit=0",
        "before=-1",
        "after=-1",
        "after=20&before=10",
    ] {
        assert!(
            client
                .get(format!("{base}?{query}"))
                .bearer_auth(&app.token)
                .send()
                .await
                .unwrap()
                .status()
                .is_client_error()
        );
    }
    server.abort();
}
