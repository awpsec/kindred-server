use crate::{
    db,
    tests::{app, bot},
    web,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::future::IntoFuture;

#[tokio::test]
async fn settings_timezones_and_computer_names_are_authenticated_validated_and_preserved() {
    let app = app();
    let id = db::id();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO local_devices VALUES(?,?,?,?,?)",
            rusqlite::params![
                id,
                "fixture-private-device-secret",
                "Original computer",
                "ask",
                db::now()
            ],
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    for path in [
        "/settings/timezone/initialize",
        "/settings/timezone/resolve",
    ] {
        assert_eq!(
            client
                .post(format!("{base}{path}"))
                .json(&json!({"timezone":"America/New_York","local":"2027-01-01T09:00"}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(
        client
            .put(format!("{base}/local/devices/{id}"))
            .json(&json!({"name":"Renamed"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let settings = json!({"name":"Person","identity":"Keep this","theme":"system","timezone":"America/New_York","timezone_mode":"fixed","local_access":true});
    client
        .put(format!("{base}/settings"))
        .bearer_auth(&app.token)
        .json(&settings)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let resolved: Value = client
        .post(format!("{base}/settings/timezone/resolve"))
        .bearer_auth(&app.token)
        .json(&json!({"local":"2027-01-01T09:00"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resolved["timezone"], "America/New_York");
    assert_eq!(resolved["run_at"], 1798812000i64);
    let mut old_client = settings.clone();
    old_client.as_object_mut().unwrap().remove("timezone");
    old_client.as_object_mut().unwrap().remove("timezone_mode");
    client
        .put(format!("{base}/settings"))
        .bearer_auth(&app.token)
        .json(&old_client)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        app.db.setting("general").unwrap().unwrap()["timezone"],
        "America/New_York"
    );
    for bad in [json!(true), json!("Wrong/Zone")] {
        let mut invalid = settings.clone();
        invalid["timezone"] = bad;
        assert!(
            client
                .put(format!("{base}/settings"))
                .bearer_auth(&app.token)
                .json(&invalid)
                .send()
                .await
                .unwrap()
                .status()
                .is_client_error()
        );
    }
    client
        .put(format!("{base}/local/devices/{id}"))
        .bearer_auth(&app.token)
        .json(&json!({"name":"My laptop"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for value in [
        json!({"name":"","mode":"full"}),
        json!({"name":"Other","mode":"full"}),
    ] {
        assert!(
            client
                .put(format!("{base}/local/devices/{id}"))
                .bearer_auth(&app.token)
                .json(&value)
                .send()
                .await
                .unwrap()
                .status()
                .is_client_error()
        );
    }
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE local_devices SET name='Hostname from heartbeat' WHERE id=?",
            [&id],
        )
        .unwrap();
    let value: Value = client
        .get(format!("{base}/local/devices"))
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["devices"][0]["name"], "My laptop");
    assert_eq!(value["devices"][0]["mode"], "ask");
    assert!(!value.to_string().contains("fixture-private-device-secret"));
    server.abort();
}

#[test]
fn pasted_images_keep_verified_mime_and_conversation_identity_when_sent_and_reopened() {
    let root = std::env::temp_dir().join(format!("kindred-pasted-image-{}", db::id()));
    let path = root.join("kindred.db");
    let db = db::Db::open(path.to_str().unwrap()).unwrap();
    let first = bot(&db, "codex");
    let second = bot(&db, "codex");
    let chat = format!("dm-{}", first.id);
    for b in [&first, &second] {
        db.save_chat(&crate::chats::Chat {
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
    let bytes=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=").unwrap();
    let file = db
        .upload_file(&chat, "Screenshot.png", &STANDARD.encode(&bytes))
        .unwrap();
    assert_eq!(file["mime"], "image/png");
    let id = file["id"].as_str().unwrap().to_string();
    assert!(
        db.chat_send_request(
            &format!("dm-{}", second.id),
            "Look",
            &[],
            std::slice::from_ref(&id),
            None,
            Some(&db::id())
        )
        .is_err()
    );
    db.chat_send_request(
        &chat,
        "Look at this screenshot",
        &[],
        std::slice::from_ref(&id),
        None,
        Some(&db::id()),
    )
    .unwrap();
    drop(db);
    let db = db::Db::open(path.to_str().unwrap()).unwrap();
    let messages = db.chat_messages(&chat).unwrap();
    assert_eq!(messages[0]["files"][0]["id"], id);
    assert_eq!(messages[0]["files"][0]["mime"], "image/png");
    assert_eq!(db.upload_bytes(&id).unwrap().1, bytes);
    let text = db
        .upload_file(
            &chat,
            "pretend.png",
            &STANDARD.encode(b"<svg>not a raster image</svg>"),
        )
        .unwrap();
    assert!(text["mime"].is_null());
}

#[test]
fn model_defaults_are_provider_scoped_and_preserve_explicit_choices() {
    let db = db::Db::open(":memory:").unwrap();
    let mut inherited = bot(&db, "codex");
    inherited.model.clear(); inherited.reasoning_effort.clear();
    let settings=json!({"default_provider":"openrouter","model_defaults":{"codex":{"model":"preferred-model","reasoning_effort":"high"},"openrouter":{"model":"other-model"}}});
    db::apply_model_default(&mut inherited,&settings).unwrap();
    assert_eq!(inherited.provider,"codex");
    assert_eq!(inherited.model,"preferred-model");
    assert_eq!(inherited.reasoning_effort,"high");
    inherited.model="explicit-model".into(); inherited.reasoning_effort.clear();
    db::apply_model_default(&mut inherited,&settings).unwrap();
    assert_eq!(inherited.model,"explicit-model");assert!(inherited.reasoning_effort.is_empty());
    inherited.model.clear();inherited.reasoning_effort="low".into();
    db::apply_model_default(&mut inherited,&settings).unwrap();assert_eq!(inherited.reasoning_effort,"low");
    inherited.model.clear();inherited.provider="opencode".into();
    assert!(db::apply_model_default(&mut inherited,&settings).is_err());
    inherited.provider="codex".into();
    db::apply_model_default(&mut inherited,&json!({})).unwrap();assert!(inherited.model.is_empty());
}

#[tokio::test]
async fn old_settings_clients_preserve_model_defaults_and_invalid_updates_do_not_replace_them() {
    let app = app();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/settings", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let defaults = json!({"codex":{"model":"chosen","reasoning_effort":"high"}});
    let first = json!({"name":"You","theme":"dark","default_provider":"codex","model_defaults":defaults});
    assert!(client.put(&url).bearer_auth(&app.token).json(&first).send().await.unwrap().status().is_success());
    let prior_client = json!({"name":"Renamed","theme":"light"});
    let saved:Value = client.put(&url).bearer_auth(&app.token).json(&prior_client).send().await.unwrap().json().await.unwrap();
    assert_eq!(saved["model_defaults"],defaults);
    assert_eq!(saved["default_provider"],"codex");
    let invalid = json!({"name":"Renamed","theme":"light","model_defaults":{"unknown":{"model":"bad"}}});
    assert!(!client.put(&url).bearer_auth(&app.token).json(&invalid).send().await.unwrap().status().is_success());
    let saved:Value=client.get(&url).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(saved["model_defaults"],defaults);
    server.abort();
}
