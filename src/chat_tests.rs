use crate::{
    chats::Chat,
    db::{self, Db},
    tests::{app, bot},
};
use serde_json::json;

fn team(db: &Db) -> Chat {
    let a = bot(db, "codex");
    let b = bot(db, "codex");
    let c = Chat {
        bot_only: false,
        description: String::new(),
        id: db::id(),
        name: "Finance team".into(),
        members: vec![a.id, b.id],
        archived: false,
        pinned: false,
        last_message: None,
    };
    db.save_chat(&c).unwrap();
    c
}

#[test]
fn repeated_pair_handoffs_reuse_only_their_active_collaboration_chat() {
    let db = Db::open(":memory:").unwrap();
    let a = bot(&db, "codex");
    let b = bot(&db, "codex");
    let custom = Chat {
        bot_only: false,
        description: String::new(),
        id: db::id(),
        name: "User's own group".into(),
        members: vec![a.id.clone(), b.id.clone()],
        archived: false,
        pinned: false,
        last_message: None,
    };
    db.save_chat(&custom).unwrap();
    let first = db.queue(&a.id, "Ask the teammate", 0).unwrap();
    let child = db
        .chat_handoff(&db.run(&first).unwrap(), &b.id, "First request")
        .unwrap();
    let shared = db.run(&child).unwrap().chat_id;
    assert_ne!(shared, custom.id);
    let second = db.queue(&b.id, "Ask back", 0).unwrap();
    let next = db
        .chat_handoff(&db.run(&second).unwrap(), &a.id, "Second request")
        .unwrap();
    assert_eq!(db.run(&next).unwrap().chat_id, shared);
    assert_eq!(
        db.chats()
            .unwrap()
            .iter()
            .filter(|c| c.id.starts_with("team-"))
            .count(),
        1
    );
    // An archived conversation is never resurrected by a later handoff.
    db.0.lock()
        .unwrap()
        .execute("UPDATE chats SET archived=1 WHERE id=?", [&shared])
        .unwrap();
    let third = db.queue(&a.id, "A fresh request", 0).unwrap();
    let fresh = db
        .chat_handoff(&db.run(&third).unwrap(), &b.id, "New collaboration")
        .unwrap();
    assert_ne!(db.run(&fresh).unwrap().chat_id, shared);
    assert!(db.chat(&shared).unwrap().archived);
    assert_eq!(db.chat(&custom.id).unwrap().name, custom.name);
}

#[tokio::test]
async fn recipient_memory_is_its_own_and_reaches_a_separate_dm() {
    let app = app();
    let sender = bot(&app.db, "codex");
    let recipient = bot(&app.db, "codex");
    app.db
        .save_bot_text(&sender.id, "memory", "I coordinate the team.", None)
        .unwrap();
    app.db
        .save_bot_text(
            &recipient.id,
            "memory",
            "The user prefers concise replies.",
            None,
        )
        .unwrap();
    let root = app
        .db
        .queue(&sender.id, "Tell the teammate their role", 0)
        .unwrap();
    let child = app
        .db
        .chat_handoff(
            &app.db.run(&root).unwrap(),
            &recipient.id,
            "The user assigned you to organize and manage their inbox and email.",
        )
        .unwrap();
    let run = app.db.run(&child).unwrap();
    assert!(run.prompt.contains("YOUR durable memory before replying"));
    let saved = "My ongoing role is to organize and manage the user's inbox and email. The user prefers concise replies.";
    let result =
        crate::runtime::call_tool(&app, &recipient, &run, "remember", json!({"text":saved}))
            .await
            .unwrap();
    assert_ne!(result["failed"], true);
    app.db
        .finish(
            &child,
            "completed",
            "I saved my inbox responsibilities.",
            "",
        )
        .unwrap();
    let dm = app
        .db
        .queue(&recipient.id, "What is your role?", 0)
        .unwrap();
    let current = app.db.bot(&recipient.id).unwrap();
    let context = crate::runtime::instructions(&app, &current, &app.db.run(&dm).unwrap()).unwrap();
    assert_eq!(current.memory, saved);
    assert!(context.contains(saved));
    assert_eq!(
        app.db.bot(&sender.id).unwrap().memory,
        "I coordinate the team."
    );
    assert!(!context.contains("Tell the teammate their role"));
}
#[test]
fn teammates_exchange_results_and_continue_across_multiple_turns() {
    let db = Db::open(":memory:").unwrap();
    let chat = team(&db);
    let first = db
        .chat_send(&chat.id, "Review the invoice", &[chat.members[0].clone()])
        .unwrap()
        .remove(0);
    let a = db.run(&first).unwrap();
    let child = db
        .chat_handoff(&a, &chat.members[1], "Calculate the subtotal")
        .unwrap();
    db.finish(&a.id, "completed", "I asked for the subtotal", "")
        .unwrap();
    db.chat_complete(&a).unwrap();
    let b = db.run(&child).unwrap();
    db.finish(&b.id, "completed", "Subtotal is 42", "").unwrap();
    db.chat_complete(&b).unwrap();
    db.chat_complete(&b).unwrap();
    let resumed = db
        .runs(Some(&chat.members[0]))
        .unwrap()
        .into_iter()
        .find(|r| r.id != first)
        .unwrap();
    assert!(resumed.prompt.contains("Subtotal is 42"));
    assert!(resumed.reply_to.is_empty());
    assert_eq!(resumed.round_id, a.round_id);
    let second = db
        .chat_handoff(&resumed, &chat.members[1], "Check tax too")
        .unwrap();
    db.finish(&resumed.id, "completed", "Waiting for the tax check", "")
        .unwrap();
    db.chat_complete(&resumed).unwrap();
    let b = db.run(&second).unwrap();
    db.finish(&b.id, "completed", "Tax is 4.20", "").unwrap();
    db.chat_complete(&b).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 5);
    assert_eq!(
        db.chat_messages(&chat.id)
            .unwrap()
            .iter()
            .filter(|m| m["text"] == "Subtotal is 42")
            .count(),
        1
    );
}
#[test]
fn mentions_are_atomic_membership_bound_and_deduplicated() {
    let db = Db::open(":memory:").unwrap();
    let chat = team(&db);
    let outsider = bot(&db, "codex");
    assert!(
        db.chat_send(&chat.id, "hello", &[chat.members[0].clone(), outsider.id])
            .is_err()
    );
    assert!(db.runs(None).unwrap().is_empty());
    assert!(db.chat_messages(&chat.id).unwrap().is_empty());
    let runs = db
        .chat_send(
            &chat.id,
            "both",
            &[
                chat.members[0].clone(),
                chat.members[1].clone(),
                chat.members[0].clone(),
            ],
        )
        .unwrap();
    assert_eq!(runs.len(), 2);
    db.save_chat(&chat).unwrap();
    let mut changed=chat.clone();changed.archived=true;
    assert!(db.save_chat(&changed).is_err());
}
#[test]
fn dm_mentions_stay_with_the_selected_bot_until_a_real_handoff() {
    let db = Db::open(":memory:").unwrap();
    let mut iz = bot(&db, "codex");
    iz.name = "Izabella".into();
    db.save_bot(&iz).unwrap();
    let mut piper = bot(&db, "codex");
    piper.name = "Piper".into();
    db.save_bot(&piper).unwrap();
    let dm = format!("dm-{}", iz.id);
    db.save_chat(&Chat {
        bot_only: false,
        description: String::new(),
        id: dm.clone(),
        name: iz.name.clone(),
        members: vec![iz.id.clone()],
        archived: false,
        pinned: false,
        last_message: None,
    })
    .unwrap();
    let id = db
        .chat_send(
            &dm,
            "Can you ask @Piper for your role?",
            &[piper.id.clone()],
        )
        .unwrap()
        .remove(0);
    assert_eq!(db.run(&id).unwrap().bot_id, iz.id);
    assert!(db.chats().unwrap().iter().all(|c| c.id.starts_with("dm-")));
    let requester = db.run(&id).unwrap();
    let child_id = db
        .chat_handoff(
            &requester,
            &piper.id,
            "What role did the user assign to me, Izabella?",
        )
        .unwrap();
    let child = db.run(&child_id).unwrap();
    assert_ne!(child.chat_id, dm);
    assert_eq!(db.chat(&dm).unwrap().members, vec![iz.id.clone()]);
    assert_eq!(
        db.chat(&child.chat_id).unwrap().members,
        vec![iz.id.clone(), piper.id.clone()]
    );
    assert_eq!(db.run(&id).unwrap().chat_id, dm);
    let dm_messages = db.chat_messages(&dm).unwrap();
    assert_eq!(
        dm_messages
            .iter()
            .filter(|m| m["kind"] == "message")
            .count(),
        1
    );
    assert_eq!(
        dm_messages
            .iter()
            .find(|m| m["kind"] == "collaboration")
            .unwrap()["linked_chat_id"],
        child.chat_id
    );
    assert!(
        !db.chat_messages(&child.chat_id)
            .unwrap()
            .iter()
            .any(|m| m["sender"] == "user")
    );
    assert!(child.prompt.contains("addressed to Piper"));
    assert!(child.prompt.contains("Request from Izabella"));
    db.finish(&requester.id, "completed", "I'll ask Piper.", "")
        .unwrap();
    db.chat_complete(&requester).unwrap();
    db.finish(
        &child.id,
        "completed",
        "Izabella will handle inbox work.",
        "",
    )
    .unwrap();
    db.chat_complete(&child).unwrap();
    db.chat_complete(&child).unwrap();
    let resumed = db
        .runs(Some(&iz.id))
        .unwrap()
        .into_iter()
        .find(|r| r.id != id)
        .unwrap();
    assert_eq!(resumed.chat_id, requester.chat_id);
    assert!(resumed.prompt.contains("remember"));
    assert_eq!(db.runs(None).unwrap().len(), 3);
    assert_eq!(
        db.chat(&child.chat_id).unwrap().last_message.unwrap()["text"],
        "Izabella will handle inbox work."
    );
}
#[test]
fn failed_dm_handoff_rolls_back_the_group_and_link() {
    let db = Db::open(":memory:").unwrap();
    let a = bot(&db, "codex");
    let mut b = bot(&db, "codex");
    b.profile.archived = true;
    db.save_bot(&b).unwrap();
    let id = db.queue(&a.id, "Ask a teammate", 0).unwrap();
    let run = db.run(&id).unwrap();
    let before = db.chats().unwrap().len();
    assert!(db.chat_handoff(&run, &b.id, "Help").is_err());
    assert_eq!(db.chats().unwrap().len(), before);
    assert_eq!(db.chat_messages(&run.chat_id).unwrap().len(), 1);
    assert_eq!(db.runs(None).unwrap().len(), 1);
}
#[test]
fn pinning_is_durable_and_does_not_overwrite_memory_or_active_chat_state() {
    let path = std::env::temp_dir().join(format!("kindred-pin-{}", db::id()));
    let (bot_id, chat_id);
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let chat = team(&db);
        bot_id = chat.members[0].clone();
        chat_id = chat.id.clone();
        let mut b = db.bot(&bot_id).unwrap();
        b.memory = "Keep this lasting fact".into();
        db.save_bot(&b).unwrap();
        db.chat_send(&chat.id, "An active request", &[]).unwrap();
        db.pin_bot(&bot_id, true).unwrap();
        db.pin_chat(&chat_id, true).unwrap();
        assert!(db.bot(&bot_id).unwrap().profile.pinned);
        assert_eq!(db.bot(&bot_id).unwrap().memory, "Keep this lasting fact");
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert!(db.bot(&bot_id).unwrap().profile.pinned);
        assert!(db.chat(&chat_id).unwrap().pinned);
        assert_eq!(
            db.chat(&chat_id).unwrap().last_message.unwrap()["text"],
            "An active request"
        );
        db.pin_bot(&bot_id, false).unwrap();
        db.pin_chat(&chat_id, false).unwrap();
        assert!(!db.bot(&bot_id).unwrap().profile.pinned);
        assert!(!db.chat(&chat_id).unwrap().pinned);
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
#[test]
fn collaboration_budget_and_cancelled_results_do_not_create_infinite_loops() {
    let db = Db::open(":memory:").unwrap();
    let chat = team(&db);
    let id = db
        .chat_send(&chat.id, "Work", &[chat.members[0].clone()])
        .unwrap()
        .remove(0);
    let run = db.run(&id).unwrap();
    for _ in 0..23 {
        db.chat_handoff(&run, &chat.members[1], "Bounded work")
            .unwrap();
    }
    assert!(db.chat_handoff(&run, &chat.members[1], "Too much").is_err());
    let last = db.runs(Some(&chat.members[1])).unwrap().remove(0);
    db.finish(&last.id, "completed", "Done", "").unwrap();
    db.chat_complete(&last).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 24);
    // All helpers and the requester must release their turns before a wake-up.
    for helper in db.runs(Some(&chat.members[1])).unwrap() {
        db.finish(&helper.id, "completed", "Done", "").unwrap();
        db.chat_complete(&helper).unwrap();
    }
    db.finish(&run.id, "completed", "Checks assigned", "")
        .unwrap();
    db.chat_complete(&run).unwrap();
    assert!(
        db.chat_messages(&chat.id)
            .unwrap()
            .iter()
            .any(|m| m["kind"] == "notice")
    );
    let count = db.runs(None).unwrap().len();
    db.finish(&last.id, "cancelled", "", "Stopped").unwrap();
    db.chat_complete(&last).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), count);
}
#[test]
fn bots_can_invite_teammates_and_context_stays_in_the_selected_chat() {
    let app = app();
    let a = bot(&app.db, "codex");
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&a.id, "Invoice project", 0).unwrap();
    let run = app.db.run(&id).unwrap();
    let private = Chat {
        bot_only: false,
        description: String::new(),
        id: db::id(),
        name: "Separate chat".into(),
        members: vec![a.id.clone()],
        archived: false,
        pinned: false,
        last_message: None,
    };
    app.db.save_chat(&private).unwrap();
    let other = app
        .db
        .chat_send(&private.id, "unrelated-private-data", &[])
        .unwrap()
        .remove(0);
    app.db
        .finish(&other, "completed", "secret-other-result", "")
        .unwrap();
    let child = app.db.chat_handoff(&run, &b.id, "Calculate total").unwrap();
    let r = app.db.run(&child).unwrap();
    let context = crate::runtime::instructions(&app, &b, &r).unwrap();
    assert!(!context.contains("Invoice project"));
    assert!(context.contains("Calculate total"));
    assert!(!context.contains("unrelated-private-data"));
    assert!(!context.contains("secret-other-result"));
    assert_eq!(app.db.chat(&r.chat_id).unwrap().members.len(), 2);
    let mut archived = bot(&app.db, "codex");
    archived.profile.archived = true;
    app.db.save_bot(&archived).unwrap();
    assert!(app.db.chat_handoff(&run, &archived.id, "No").is_err());
}

#[test]
fn stopping_a_turn_also_stops_its_queued_collaborators() {
    let db = Db::open(":memory:").unwrap();
    let chat = team(&db);
    let id = db
        .chat_send(&chat.id, "Work", &[chat.members[0].clone()])
        .unwrap()
        .remove(0);
    let parent = db.claim().unwrap().unwrap();
    let child = db
        .chat_handoff(&parent, &chat.members[1], "Wait for work")
        .unwrap();
    db.cancel(&id).unwrap();
    assert_eq!(db.run(&id).unwrap().status, "cancelling");
    assert_eq!(db.run(&child).unwrap().status, "cancelled");
    assert!(db.claim().unwrap().is_none());
    assert!(
        db.chat_messages(&chat.id)
            .unwrap()
            .iter()
            .any(|m| m["run_id"] == child && m["kind"] == "result")
    );
}
#[test]
fn legacy_migration_is_repeatable_and_preserves_run_and_event_identity() {
    let path = std::env::temp_dir().join(format!("kindred-chat-migration-{}.db", db::id()));
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE bots(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,memory TEXT NOT NULL,auto_approve INTEGER NOT NULL);INSERT INTO bots VALUES('old','Piper','','codex','','',0);CREATE TABLE runs(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL,prompt TEXT NOT NULL,status TEXT NOT NULL,output TEXT NOT NULL,error TEXT NOT NULL,created INTEGER NOT NULL,depth INTEGER NOT NULL DEFAULT 0);INSERT INTO runs VALUES('run-old','old','Remember this','completed','Preserved answer','',1,0);").unwrap();
    }
    for _ in 0..2 {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let r = db.run("run-old").unwrap();
        assert_eq!(r.chat_id, "dm-old");
        assert_eq!(r.output, "Preserved answer");
        let messages = db.chat_messages("dm-old").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["text"], "Remember this");
        assert_eq!(messages[1]["run_id"], "run-old");
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn failed_migration_rolls_back_schema_and_partial_history() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE bots(id TEXT PRIMARY KEY,name TEXT);CREATE TABLE runs(id TEXT PRIMARY KEY,bot_id TEXT,prompt TEXT,status TEXT,output TEXT,error TEXT,created INTEGER,depth INTEGER);INSERT INTO runs VALUES('orphan','missing','hello','completed','result','',1,0);").unwrap();
    assert!(crate::chats::migrate(&c).is_err());
    let columns = c
        .prepare("PRAGMA table_info(runs)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(!columns.contains(&"chat_id".to_string()));
    let tables: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='chat_messages'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}
#[tokio::test]
async fn chat_routes_require_auth_and_preserve_structured_mentions() {
    let app = app();
    let chat = team(&app.db);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let c = reqwest::Client::new();
    assert_eq!(
        c.get(format!("{base}/api/chats"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = c
        .post(format!("{base}/api/chats/{}/messages", chat.id))
        .bearer_auth(&app.token)
        .json(&json!({"prompt":"@Teammate calculate","mentions":[chat.members[1]]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let runs = app.db.runs(None).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].bot_id, chat.members[1]);
    for (kind, id) in [("bots", &chat.members[0]), ("chats", &chat.id)] {
        let url = format!("{base}/api/{kind}/{id}/pin");
        assert_eq!(
            c.put(&url)
                .json(&json!({"pinned":true}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            c.put(&url)
                .bearer_auth(&app.token)
                .json(&json!({"pinned":true}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert!(app.db.bot(&chat.members[0]).unwrap().profile.pinned);
    assert!(app.db.chat(&chat.id).unwrap().pinned);
    server.abort();
}
