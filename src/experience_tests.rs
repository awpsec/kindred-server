use crate::{
    db, runtime,
    tests::{app, bot},
    web,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;

fn proposal() -> serde_json::Value {
    json!({"name":"Ada","role":"Invoicing","description":"Prepare and track invoices.","instructions":"Draft invoices for review. Keep track of due dates. Ask before sending.","shape":"pebble","color":"teal"})
}
#[tokio::test]
async fn teammate_drafts_require_user_creation_and_retries_create_only_once() {
    let app = app();
    let mut actor = bot(&app.db, "codex");
    actor.memory = "Private source memory".into();
    actor.approval_mode = "full".into();
    app.db.save_bot(&actor).unwrap();
    app.db
        .queue(&actor.id, "Draft an invoicing teammate", 0)
        .unwrap();
    let run = app.db.claim().unwrap().unwrap();
    let result = runtime::call_tool(&app, &actor, &run, "draft_bot", proposal())
        .await
        .unwrap();
    assert_ne!(result["failed"], true);
    assert_eq!(app.db.bots().unwrap().len(), 1);
    let messages = app.db.chat_messages(&run.chat_id).unwrap();
    let draft = &messages.last().unwrap()["draft"];
    assert_eq!(draft["bot"]["memory"], "");
    assert_eq!(draft["bot"]["profile"]["color"], "#14bfc7");
    assert_eq!(draft["bot"]["approval_mode"], "inherit");
    assert_eq!(draft["bot"]["profile"]["label"], "Invoicing");
    let id = draft["id"].as_str().unwrap().to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let url = format!("{base}/api/bot-drafts/{id}/create");
    assert_eq!(
        client
            .post(&url)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut edited: db::Bot = serde_json::from_value(draft["bot"].clone()).unwrap();
    edited.name = "Ada Ledger".into();
    edited.instructions = "Updated from Details".into();
    edited.profile.color = "#2ec767".into();
    let created: db::Bot = client
        .post(&url)
        .bearer_auth(&app.token)
        .json(&json!({"bot":edited}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.name, "Ada Ledger");
    assert_eq!(created.instructions, "Updated from Details");
    let (one, two) = tokio::join!(
        client
            .post(&url)
            .bearer_auth(&app.token)
            .json(&json!({}))
            .send(),
        client
            .post(&url)
            .bearer_auth(&app.token)
            .json(&json!({}))
            .send()
    );
    for response in [one, two] {
        assert_eq!(
            response.unwrap().json::<db::Bot>().await.unwrap().id,
            created.id
        );
    }
    assert_eq!(app.db.bots().unwrap().len(), 2);
    assert!(app.db.chat(&format!("dm-{}", created.id)).is_ok());
    assert_eq!(app.db.bot_draft(&id).unwrap()["created_bot_id"], created.id);
    let mut bad = proposal();
    bad["instructions"] = json!("");
    assert!(app.db.draft_bot(&run, &bad).is_err());
    app.db.finish(&run.id, "cancelled", "", "").unwrap();
    assert!(app.db.draft_bot(&run, &proposal()).is_err());
    server.abort();
}
#[test]
fn text_editors_preserve_new_memory_and_reject_conflicting_saves() {
    let app = app();
    let mut b = bot(&app.db, "codex");
    app.db
        .save_bot_text(&b.id, "memory", "Fresh model memory", None)
        .unwrap();
    b.name = "Renamed".into();
    app.db.save_bot_preferences(&b, true).unwrap();
    assert_eq!(app.db.bot(&b.id).unwrap().memory, "Fresh model memory");
    assert!(
        app.db
            .save_bot_text(&b.id, "memory", "Stale editor", Some(""))
            .is_err()
    );
    app.db
        .save_bot_text(
            &b.id,
            "instructions",
            "Long term role",
            Some("Use the tools."),
        )
        .unwrap();
    assert_eq!(app.db.bot(&b.id).unwrap().memory, "Fresh model memory");
    assert!(
        app.db
            .save_bot_text(&b.id, "memory", "🦀".repeat(crate::db::BOT_MEMORY_MAX_BYTES / 4 + 1).as_str(), None)
            .is_err()
    );
    assert!(app.db.save_bot_text(&b.id, "name", "Bad", None).is_err());
}
#[tokio::test]
async fn file_uploads_are_authenticated_chat_bound_and_atomically_attached() {
    let app = app();
    let b = bot(&app.db, "codex");
    app.db.queue(&b.id, "Initial", 0).unwrap();
    let chat = format!("dm-{}", b.id);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let payload =
        json!({"chat_id":chat,"name":"invoice.csv","data":STANDARD.encode(vec![b'x';200_000])});
    assert_eq!(
        client
            .post(format!("{base}/api/uploads"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(format!("{base}/api/uploads"))
        .bearer_auth(&app.token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "Route must allow attachments larger than normal JSON requests"
    );
    let file: serde_json::Value = response.json().await.unwrap();
    let id = file["id"].as_str().unwrap().to_string();
    let before = app.db.runs(None).unwrap().len();
    assert!(
        app.db
            .chat_send_files(&chat, "Duplicate", &[], &[id.clone(), id.clone()])
            .is_err()
    );
    assert_eq!(app.db.runs(None).unwrap().len(), before);
    let other = bot(&app.db, "codex");
    app.db.queue(&other.id, "Other", 0).unwrap();
    let other_chat = format!("dm-{}", other.id);
    assert!(
        app.db
            .chat_send_files(&other_chat, "Wrong chat", &[], &[id.clone()])
            .is_err()
    );
    let runs = app
        .db
        .chat_send_files(&chat, "Read attached CSV", &[], &[id.clone()])
        .unwrap();
    let run = app.db.run(&runs[0]).unwrap();
    assert_eq!(app.db.run_upload(&run, &id).unwrap().1.len(), 200_000);
    let other_run = app.db.runs(Some(&other.id)).unwrap().remove(0);
    assert!(app.db.run_upload(&other_run, &id).is_err());
    assert_eq!(
        app.db.chat_messages(&chat).unwrap().last().unwrap()["files"][0]["name"],
        "invoice.csv"
    );
    app.db.remove_upload(&id).unwrap();
    assert!(app.db.upload_bytes(&id).is_ok());
    assert_eq!(
        client
            .get(format!("{base}/api/uploads/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .get(format!("{base}/api/uploads/{id}"))
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.bytes().await.unwrap().len(), 200_000);
    assert!(app.db.upload_file(&chat, "../escape", "").is_err());
    assert!(
        app.db
            .upload_file(
                &chat,
                "huge.bin",
                &STANDARD.encode(vec![0; 8 * 1024 * 1024 + 1])
            )
            .is_err()
    );
    server.abort();
}
#[test]
fn global_notification_frequency_filters_completions_without_backfill() {
    let app = app();
    let b = bot(&app.db, "codex");
    let run = app.db.queue(&b.id, "Complete", 0).unwrap();
    app.db.finish(&run, "completed", "Done", "").unwrap();
    app.db.event(&run, "run_finished", json!({})).unwrap();
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.db
        .save_setting("general", &json!({"notifications":"input_needed"}))
        .unwrap();
    let filtered = app.db.notifications(Some(0)).unwrap();
    assert!(filtered["items"].as_array().unwrap().is_empty());
    let failed = app.db.queue(&b.id, "Needs help", 0).unwrap();
    app.db
        .finish(&failed, "failed", "", "Needs user help")
        .unwrap();
    app.db.event(&failed, "run_finished", json!({})).unwrap();
    assert_eq!(
        app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.db
        .save_setting("general", &json!({"notifications":"none"}))
        .unwrap();
    let muted = app.db.notifications(Some(0)).unwrap();
    assert!(muted["items"].as_array().unwrap().is_empty());
    app.db
        .save_setting("general", &json!({"notifications":"all"}))
        .unwrap();
    assert!(
        app.db.notifications(muted["cursor"].as_i64()).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn notification_mutes_are_scoped_expire_and_unmute_without_backfill() {
    let app = app();
    let b = bot(&app.db, "codex");
    let run = app.db.queue(&b.id, "Complete", 0).unwrap();
    app.db.finish(&run, "completed", "Done", "").unwrap();
    app.db.event(&run, "run_finished", json!({})).unwrap();
    for seconds in [3600, 86400, -1] {
        let saved = app.db.mute_notifications("bot", &b.id, seconds).unwrap();
        assert!(crate::notifications::muted(&saved, &b.id, "elsewhere"));
        assert!(!crate::notifications::muted(&saved, "other", "elsewhere"));
        let feed = app.db.notifications(Some(0)).unwrap();
        assert!(feed["items"].as_array().unwrap().is_empty());
        let saved = app.db.mute_notifications("bot", &b.id, 0).unwrap();
        assert!(!crate::notifications::muted(&saved, &b.id, ""));
        assert!(app.db.notifications(feed["cursor"].as_i64()).unwrap()["items"].as_array().unwrap().is_empty());
    }
    let chat = app.db.run(&run).unwrap().chat_id;
    let saved = app.db.mute_notifications("chat", &chat, -1).unwrap();
    assert!(crate::notifications::muted(&saved, &b.id, &chat));
    assert!(!crate::notifications::muted(&saved, &b.id, "other-chat"));
    assert!(app.db.notifications(Some(0)).unwrap()["items"].as_array().unwrap().is_empty());
    app.db.mute_notifications("chat", &chat, 0).unwrap();
    let expired = json!({format!("bot:{}",b.id):crate::db::now()-1});
    app.db.save_setting("notification_mutes", &expired).unwrap();
    assert_eq!(app.db.notifications(Some(0)).unwrap()["items"].as_array().unwrap().len(), 1);
    assert!(app.db.mute_notifications("bot", &b.id, 12).is_err());
    assert!(app.db.mute_notifications("unknown", &b.id, 3600).is_err());
}
