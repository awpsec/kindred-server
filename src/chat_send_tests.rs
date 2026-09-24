use crate::{
    db::{self, Db},
    tests, web,
};
use serde_json::{Value, json};
use std::future::IntoFuture;

#[tokio::test]
async fn repeated_http_send_returns_one_durable_run_and_rejects_key_reuse() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let chat = dm(&app.db, &bot);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let url = format!("{origin}/api/chats/{chat}/messages");
    let body = json!({"prompt":"Please check once","request_id":db::id()});
    let send = || client.post(&url).bearer_auth(&app.token).json(&body).send();
    let (a, b) = tokio::join!(send(), send());
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    let a: Value = a.json().await.unwrap();
    let b: Value = b.json().await.unwrap();
    assert_eq!(a, b);
    assert_eq!(app.db.chat_messages(&chat).unwrap().len(), 1);
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    let changed = json!({"prompt":"A different instruction","request_id":body["request_id"]});
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&app.token)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client.post(&url).json(&body).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&app.token)
            .json(&json!({"prompt":"Hello","request_id":"bad"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let next: Value = client
        .post(&url)
        .bearer_auth(&app.token)
        .json(&json!({"prompt":"Please check once","request_id":db::id()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(next, a);
    assert_eq!(app.db.chat_messages(&chat).unwrap().len(), 2);
    server.abort();
}

#[test]
fn send_receipts_survive_restart_and_failed_transactions_do_not_reserve_keys() {
    let root = std::env::temp_dir().join(format!("kindred-send-{}", db::id()));
    let path = root.join("kindred.db").to_string_lossy().into_owned();
    let db = Db::open(&path).unwrap();
    let a = tests::bot(&db, "codex");
    let b = tests::bot(&db, "codex");
    let chat = dm(&db, &a);
    let other = dm(&db, &b);
    let key = db::id();
    assert!(
        db.chat_send_request(
            &chat,
            "Please check",
            &[],
            &["missing-upload".into()],
            None,
            Some(&key)
        )
        .is_err()
    );
    assert_eq!(db.chat_messages(&chat).unwrap().len(), 0);
    assert_eq!(db.runs(None).unwrap().len(), 0);
    let runs = db
        .chat_send_request(&chat, "Please check", &[], &[], None, Some(&key))
        .unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.chat_send_request(&chat, "Please check", &[], &[], None, Some(&key))
            .unwrap(),
        runs
    );
    assert!(
        db.chat_send_request(&other, "Please check", &[], &[], None, Some(&key))
            .is_err()
    );
    assert_eq!(db.chat_messages(&chat).unwrap().len(), 1);
    assert_eq!(db.runs(None).unwrap().len(), 1);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn transferred_workspace_preserves_send_receipts() {
    let source = Db::open(":memory:").unwrap();
    let bot = tests::bot(&source, "codex");
    let chat = dm(&source, &bot);
    let key = db::id();
    let runs = source
        .chat_send_request(&chat, "One task", &[], &[], None, Some(&key))
        .unwrap();
    source.finish(&runs[0], "completed", "Done", "").unwrap();
    let package = source
        .prepare_transfer(&db::id(), "Send receipt fixture")
        .unwrap();
    let target = Db::open(":memory:").unwrap();
    target.import_transfer(&package).unwrap();
    assert_eq!(
        target
            .chat_send_request(&chat, "One task", &[], &[], None, Some(&key))
            .unwrap(),
        runs
    );
    assert_eq!(target.runs(None).unwrap().len(), 1);
    assert_eq!(target.chat_messages(&chat).unwrap().len(), 1);
}

fn dm(db: &Db, bot: &crate::db::Bot) -> String {
    let id = format!("dm-{}", bot.id);
    db.save_chat(&crate::chats::Chat {
        bot_only: false,
        description: String::new(),
        id: id.clone(),
        name: bot.name.clone(),
        members: vec![bot.id.clone()],
        archived: false,
        pinned: false,
        last_message: None,
    })
    .unwrap();
    id
}
#[test]
fn retry_preserves_bound_attachment_and_quote_without_rebinding() {
    let db = Db::open(":memory:").unwrap();
    let bot = tests::bot(&db, "codex");
    let chat = dm(&db, &bot);
    let seq = {
        let c = db.0.lock().unwrap();
        c.execute(
            "INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,?,?,'message',1)",
            rusqlite::params![chat, bot.id, "Send a note"],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    let file = db.upload_file(&chat, "note.txt", "eA==").unwrap();
    let files = vec![file["id"].as_str().unwrap().to_string()];
    let key = db::id();
    let first = db
        .chat_send_request(&chat, "My note", &[], &files, Some(seq), Some(&key))
        .unwrap();
    assert_eq!(
        db.chat_send_request(&chat, "My note", &[], &files, Some(seq), Some(&key))
            .unwrap(),
        first
    );
    assert!(
        db.chat_send_request(&chat, "My note", &[], &[], Some(seq), Some(&key))
            .is_err()
    );
    let rows = db.chat_messages(&chat).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["reply_to"]["seq"], seq);
    assert_eq!(rows[1]["files"][0]["id"], file["id"]);
    assert_eq!(db.runs(None).unwrap().len(), 1);
}
