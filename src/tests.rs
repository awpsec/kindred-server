use crate::{
    config::Config,
    db::{self, Bot, Db, Routine},
    runtime::{App, Shared},
    web,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

const SCREENSHOT_FIXTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=";

#[tokio::test]
async fn artifact_frame_has_an_independent_sandbox_policy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app())).into_future());
    let client = reqwest::Client::new();
    for path in ["/", "/artifacts", "/artifacts/example"] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        assert_eq!(response.headers()["content-security-policy"], include_str!("../ui/app-policy.txt").trim());
        assert!(response.headers()["content-security-policy"].to_str().unwrap().split(';').any(|part| part.trim() == "script-src 'self'"));
    }
    let response = client.get(format!("{base}/artifact-frame.html")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-frame-options"], "SAMEORIGIN");
    assert_eq!(response.headers()["content-security-policy"], include_str!("../ui/artifact-frame-policy.txt").trim());
    assert!(response.text().await.unwrap().contains("event.source!==parent"));
    server.abort();
}

#[tokio::test]
async fn screenshot_attachments_survive_reopen_and_require_authentication() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = std::env::temp_dir().join(format!("kindred-attachments-{}", db::id()));
    let path = root.join("kindred.db").to_string_lossy().into_owned();
    let db = Db::open(&path).unwrap();
    let b = bot(&db, "codex");
    let run = db
        .queue(&b.id, "Show the site screenshot in chat", 0)
        .unwrap();
    let id = db
        .attach_screenshot(&run, SCREENSHOT_FIXTURE, "Site preview")
        .unwrap();
    db.finish(&run, "completed", "Here is your screenshot.", "")
        .unwrap();
    db.chat_complete(&db.run(&run).unwrap()).unwrap();
    assert!(
        db.attach_screenshot(&run, "data:image/svg+xml;base64,AAAA", "bad")
            .is_err()
    );
    assert!(
        db.attach_screenshot("missing-run", SCREENSHOT_FIXTURE, "bad")
            .is_err()
    );
    drop(db);
    let mut app = app();
    Arc::get_mut(&mut app).unwrap().db = Db::open(&path).unwrap();
    assert_eq!(app.db.attachments(&run).unwrap()[0]["id"], id);
    let chat = app.db.chat_messages(&format!("dm-{}", b.id)).unwrap();
    assert_eq!(chat.last().unwrap()["attachments"][0]["id"], id);
    assert!(
        !serde_json::to_string(&app.db.events(&run).unwrap())
            .unwrap()
            .contains("base64")
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    for endpoint in [
        format!("/api/attachments/{id}"),
        "/api/notifications".into(),
    ] {
        assert_eq!(
            client
                .get(format!("{base}{endpoint}"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let response = client
        .get(format!("{base}/api/attachments/{id}"))
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        STANDARD
            .decode(SCREENSHOT_FIXTURE.split(',').nth(1).unwrap())
            .unwrap()
    );
    assert_eq!(
        client
            .get(format!("{base}/api/attachments/{}", db::id()))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.abort();
    let _ = server.await;
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn notification_cursors_respect_preferences_and_skip_historical_or_resolved_events() {
    let db = Db::open(":memory:").unwrap();
    let mut b = bot(&db, "codex");
    let old = db.queue(&b.id, "Private old task", 0).unwrap();
    db.finish(&old, "completed", "Private output", "").unwrap();
    db.event(&old, "run_finished", json!({})).unwrap();
    let baseline = db.notifications(None).unwrap();
    assert_eq!(baseline["items"], json!([]));
    let cursor = baseline["cursor"].as_i64().unwrap();
    let run = db.queue(&b.id, "Private new task", 0).unwrap();
    db.finish(&run, "completed", "Private result", "").unwrap();
    db.event(&run, "run_finished", json!({})).unwrap();
    let updates = db.notifications(Some(cursor)).unwrap();
    assert_eq!(updates["items"].as_array().unwrap().len(), 1);
    assert_eq!(updates["items"][0]["run_id"], run);
    assert_eq!(updates["items"][0]["body"], "Private result");
    assert_eq!(updates["items"][0]["title"], b.name);
    assert_eq!(updates["items"][0]["avatar"]["name"], b.name);
    assert_eq!(
        updates["items"][0]["avatar_key"].as_str().unwrap().len(),
        64
    );
    assert!(!updates.to_string().contains("Private new task"));
    let old_avatar_key = updates["items"][0]["avatar_key"].clone();
    b.name = "Oliver".into();
    db.save_bot(&b).unwrap();
    let renamed = db.notifications(Some(cursor)).unwrap();
    assert_eq!(renamed["items"][0]["avatar"]["name"], "Oliver");
    assert_ne!(renamed["items"][0]["avatar_key"], old_avatar_key);
    assert_eq!(
        db.notifications(Some(updates["cursor"].as_i64().unwrap()))
            .unwrap()["items"],
        json!([])
    );
    b.profile.notifications = false;
    db.save_bot(&b).unwrap();
    assert_eq!(db.notifications(Some(cursor)).unwrap()["items"], json!([]));
    b.profile.notifications = true;
    db.save_bot(&b).unwrap();
    let pending = db.queue(&b.id, "Needs consent", 0).unwrap();
    db.claim().unwrap();
    let approval = db
        .request_approval(&pending, "routine_create", &json!({}))
        .unwrap();
    db.event(&pending, "approval", json!({"id":approval}))
        .unwrap();
    let pending_cursor = updates["cursor"].as_i64().unwrap();
    assert_eq!(
        db.notifications(Some(pending_cursor)).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    db.decide(&approval, false).unwrap();
    assert_eq!(
        db.notifications(Some(pending_cursor)).unwrap()["items"],
        json!([])
    );
    assert!(db.notifications(Some(-1)).is_err());
    assert_eq!(
        db.notifications(Some(i64::MAX)).unwrap()["items"],
        json!([])
    );
}

#[test]
fn notification_paging_does_not_drop_completions_or_realert_old_human_subtasks() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    for _ in 0..105 {
        let run = db.queue(&b.id, "Task", 0).unwrap();
        db.finish(&run, "completed", "Done", "").unwrap();
        db.event(&run, "run_finished", json!({})).unwrap();
    }
    let first = db.notifications(Some(0)).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 100);
    let next = db
        .notifications(Some(first["cursor"].as_i64().unwrap()))
        .unwrap();
    assert_eq!(next["items"].as_array().unwrap().len(), 5);
    let cursor = next["cursor"].as_i64().unwrap();
    let id = db.queue(&b.id, "Sign in", 0).unwrap();
    let run = db.claim().unwrap().unwrap();
    assert_eq!(run.id, id);
    let first = db
        .request_user_task(&run, "Sign in", "Use your computer")
        .unwrap();
    let slot = db.screen(&b.id).unwrap();
    db.ready_user_task(&first, slot, "done").unwrap();
    db.resume_user_task(&first, slot).unwrap();
    db.request_user_task(
        &db.run(&id).unwrap(),
        "Another step",
        "Use your computer again",
    )
    .unwrap();
    let pending = db.notifications(Some(cursor)).unwrap();
    assert_eq!(pending["items"].as_array().unwrap().len(), 1);
}

pub fn bot(db: &Db, provider: &str) -> Bot {
    let b = Bot {
        id: db::id(),
        name: "Test teammate".into(),
        instructions: "Use the tools.".into(),
        provider: provider.into(),
        model: "test/model".into(),
        reasoning_effort: String::new(),
        memory: String::new(),
        auto_approve: false,
        approval_mode: "inherit".into(),
        profile: Default::default(),
    };
    db.save_bot(&b).unwrap();
    b
}
pub fn app() -> Shared {
    Arc::new(App {
        desktop_sessions: Default::default(),
        profile_portal: Default::default(),
        mail_lock: Mutex::new(()),
        config: Config::default(),
        db: Db::open(":memory:").unwrap(),
        token: "integration-test-token-32-characters".into(),
        computer: Arc::new(Mutex::new(())),
        screens: Default::default(),

        auth: Mutex::new(None),
        integrations: Arc::new(Mutex::new(())),
        maintenance_leases: Default::default(),
        pairing: Default::default(),
        vnc: crate::vnc::VncState::default(),
        catalogues: Default::default(),
    })
}

#[tokio::test]
async fn release_feed_is_public_but_cannot_read_workspace_files() {
    let root = std::env::temp_dir().join(format!("kindred-releases-{}", db::id()));
    std::fs::create_dir_all(root.join("releases")).unwrap();
    std::fs::write(
        root.join("releases/stable.json"),
        br#"{"payload":"signed-fixture"}"#,
    )
    .unwrap();
    std::fs::write(root.join("kindred.db"), "workspace-private").unwrap();
    let mut app = app();
    Arc::get_mut(&mut app).unwrap().config.database =
        root.join("kindred.db").to_string_lossy().into();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app)).into_future());
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/updates/stable.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.text().await.unwrap().contains("signed-fixture"));
    for name in [
        "client-stable.json",
        "client-linux.json",
        "kindred-linux-x86_64-1.2.3.AppImage",
        "kindred-macos-aarch64-1.2.3.dmg",
        "kindred-macos-x86_64-1.2.3.dmg",
    ] {
        std::fs::write(root.join("releases").join(name), b"public-package").unwrap();
        let response = client
            .get(format!("{base}/updates/{name}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-length"], "14");
        assert_eq!(response.bytes().await.unwrap(), &b"public-package"[..]);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            root.join("kindred.db"),
            root.join("releases/kindred-macos-aarch64-8.8.8.dmg"),
        )
        .unwrap();
        assert_eq!(
            client
                .get(format!("{base}/updates/kindred-macos-aarch64-8.8.8.dmg"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    for name in [
        "kindred.db",
        "kindred-linux-x86_64-1.2.AppImage",
        "kindred-macos-arm64-1.2.3.dmg",
        "..%2Fkindred.db",
        "kindred-windows-1.2.zip",
        "kindred-windows-1.2.3.zip",
    ] {
        let response = client
            .get(format!("{base}/updates/{name}"))
            .send()
            .await
            .unwrap();
        assert_ne!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("workspace-private"));
    }
    assert_eq!(
        client
            .get(format!("{base}/api/bots"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queue_cancel_and_atomic_claim() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let first = db.queue(&b.id, "first", 0).unwrap();
    let second = db.queue(&b.id, "second", 0).unwrap();
    db.cancel(&first).unwrap();
    assert_eq!(db.claim().unwrap().unwrap().id, second);
    assert!(db.claim().unwrap().is_none());
    assert!(db.queue("not-a-bot", "test", 0).is_err());
    assert!(db.queue(&b.id, "test", 4).is_err());
}
#[test]
fn model_and_thinking_survive_reopen_of_existing_database() {
    let path = std::env::temp_dir().join(format!("kindred-thinking-{}.db", db::id()));
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE bots(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,memory TEXT NOT NULL,auto_approve INTEGER NOT NULL,profile TEXT NOT NULL DEFAULT '{}'); INSERT INTO bots VALUES('existing','Piper','keep instructions','codex','gpt-5.6-luna','keep memory',0,'{}');").unwrap();
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let mut b = db.bot("existing").unwrap();
        assert_eq!(b.model, "gpt-5.6-luna");
        assert_eq!(b.reasoning_effort, "");
        b.reasoning_effort = "high".into();
        db.save_bot(&b).unwrap();
        b.reasoning_effort = "bad setting!".into();
        assert!(db.save_bot(&b).is_err());
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let b = db.bot("existing").unwrap();
        assert_eq!(b.reasoning_effort, "high");
        assert_eq!(b.instructions, "keep instructions");
        assert_eq!(b.memory, "keep memory");
    }
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_file(format!("{}.lock", path.display()));
}
#[test]
fn approval_is_single_use_and_bound_to_active_run() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let run = db.queue(&b.id, "test", 0).unwrap();
    db.claim().unwrap();
    let a = db
        .request_approval(&run, "guest_exec", &json!({"command":"true"}))
        .unwrap();
    db.decide(&a, false).unwrap();
    assert_eq!(db.approval(&a).unwrap(), "denied");
    assert!(db.decide(&a, true).is_err());
    let a = db.request_approval(&run, "guest_exec", &json!({})).unwrap();
    db.cancel(&run).unwrap();
    assert!(db.decide(&a, true).is_err());
}
#[test]
fn restart_preserves_queue_and_expires_inflight_work() {
    let path = std::env::temp_dir().join(format!("kindred-test-{}.db", db::id()));
    let path = path.to_str().unwrap();
    let db = Db::open(path).unwrap();
    let b = bot(&db, "codex");
    let active = db.queue(&b.id, "active", 0).unwrap();
    db.claim().unwrap();
    let queued = db.queue(&b.id, "pending", 0).unwrap();
    let approval = db
        .request_approval(&active, "guest_exec", &json!({}))
        .unwrap();
    db.set_takeover(true).unwrap();
    assert!(Db::open(path).is_err());
    drop(db);
    let db = Db::open(path).unwrap();
    assert_eq!(db.run(&active).unwrap().status, "interrupted");
    assert_eq!(db.run(&queued).unwrap().status, "queued");
    assert_eq!(db.approval(&approval).unwrap(), "expired");
    assert!(db.takeover().unwrap());
    assert!(db.quiet_until().unwrap() > db::now());
    drop(db);
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}
#[test]
fn routines_coalesce_missed_ticks_and_skip_busy_bot() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let r = Routine {
        id: db::id(),
        bot_id: b.id.clone(),
        name: "Daily check".into(),
        prompt: "Check".into(),
        interval_seconds: 60,
        next_run: 1,
        enabled: true,
        run_at: None,
        schedule: None,
    };
    db.save_routine(&r).unwrap();
    db.tick(10000).unwrap();
    db.tick(10000).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 1);
    assert_eq!(db.routines().unwrap()[0].next_run, 10060);
    db.tick(20000).unwrap();
    assert_eq!(db.runs(None).unwrap().len(), 1);
}
#[tokio::test]
async fn http_auth_origin_and_takeover_are_enforced() {
    let app = app();
    let b = bot(&app.db, "codex");
    app.db.screen(&b.id).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
    let c = reqwest::Client::new();
    assert_eq!(
        c.get(format!("{base}/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.get(format!("{base}/api/bots"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        c.get(format!("{base}/api/bots"))
            .bearer_auth(&app.token)
            .header("Origin", "https://attacker.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        c.get(format!("{base}/api/bots"))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let r = c
        .post(format!("{base}/api/computer"))
        .bearer_auth(&app.token)
        .json(&json!({"tool":"computer_key","args":{"key":"Return"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let lease = app.computer.lock().await;
    let r = c
        .post(format!("{base}/api/takeover"))
        .bearer_auth(&app.token)
        .json(&json!({"enabled":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    drop(lease);
    let r = c
        .post(format!("{base}/api/takeover"))
        .bearer_auth(&app.token)
        .json(&json!({"enabled":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(app.db.screen_takeover(1).unwrap());
    let reboot = c
        .post(format!("{base}/api/vm/reboot"))
        .bearer_auth(&app.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(reboot.status(), 400);
    assert!(reboot.text().await.unwrap().contains("control"));
    let r = c.get(&base).send().await.unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    server.abort();
}
#[tokio::test]
async fn handoffs_are_queued_with_depth_and_do_not_copy_other_history() {
    let app = app();
    let mut a = bot(&app.db, "codex");
    a.auto_approve = true;
    app.db.save_bot(&a).unwrap();
    let b = bot(&app.db, "openrouter");
    let id = app.db.queue(&a.id, "private original prompt", 0).unwrap();
    let run = app.db.run(&id).unwrap();
    crate::runtime::call_tool(
        &app,
        &a,
        &run,
        "send_to_bot",
        json!({"bot_id":b.id,"message":"Review the shared result"}),
    )
    .await
    .unwrap();
    let recipient = app.db.runs(Some(&b.id)).unwrap().remove(0);
    assert_eq!(recipient.depth, 1);
    assert!(recipient.prompt.contains("Review the shared result"));
    assert!(!recipient.prompt.contains("private original prompt"));
}
#[tokio::test]
async fn denied_guest_action_never_launches_ssh() {
    let app = app();
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&b.id, "run a command", 0).unwrap();
    app.db.claim().unwrap();
    let run = app.db.run(&id).unwrap();
    let a = app.clone();
    let task = tokio::spawn(async move {
        crate::runtime::call_tool(&a, &b, &run, "guest_exec", json!({"command":"echo never"})).await
    });
    let approvals = loop {
        let p = app.db.approvals().unwrap();
        if !p.is_empty() {
            break p;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    app.db
        .decide(approvals[0]["id"].as_str().unwrap(), false)
        .unwrap();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["failed"], true);
    assert!(result["text"].as_str().unwrap().contains("declined"));
}
#[test]
fn dynamic_tool_contract_matches_current_codex_function_shape() {
    for spec in crate::runtime::tool_specs() {
        assert_eq!(spec["type"], "function");
        assert!(spec["inputSchema"]["required"].is_array());
        assert_eq!(spec["inputSchema"]["additionalProperties"], false);
    }
}

#[test]
fn shared_connectors_are_provider_independent_and_claude_bridge_is_not_a_public_tool() {
    let app = app();
    for provider in ["codex", "claude-code", "kimi-code", "openrouter"] {
        let b = bot(&app.db, provider);
        let tools = crate::runtime::tool_specs_for(&app, &b);
        for name in ["connectors_list", "connector_tools", "connector_execute"] {
            assert!(tools.iter().any(|t| t["name"] == name));
        }
        assert!(!tools.iter().any(|t| t["name"] == "claude_connector"));
    }
}

#[tokio::test]
async fn claude_explicit_permission_cannot_be_skipped_by_full_mode() {
    let app = app();
    let mut b = bot(&app.db, "claude-code");
    b.approval_mode = "full".into();
    app.db.save_bot(&b).unwrap();
    let id = app.db.queue(&b.id, "Read a connector", 0).unwrap();
    app.db.claim().unwrap();
    let run = app.db.run(&id).unwrap();
    let a = app.clone();
    let task = tokio::spawn(async move {
        crate::runtime::approve_required(
            &a,
            &b,
            &run,
            "claude_connector",
            &json!({"origin":"claude-account"}),
            true,
        )
        .await
    });
    let approvals = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let rows = app.db.approvals().unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.db
        .decide(approvals[0]["id"].as_str().unwrap(), false)
        .unwrap();
    assert!(!task.await.unwrap().unwrap());
}

#[allow(dead_code)]
fn _json_value(_: Value) {}
#[test]
fn legacy_bot_profile_migration_preserves_identity_and_history() {
    let path = std::env::temp_dir().join(format!("kindred-migrate-{}.db", db::id()));
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE bots(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,memory TEXT NOT NULL,auto_approve INTEGER NOT NULL); INSERT INTO bots VALUES('old','Piper','keep','codex','','remember me',0);").unwrap();
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let b = db.bot("old").unwrap();
        assert_eq!(b.memory, "remember me");
        assert_eq!(b.profile.shape, "round");
        let mut b = b;
        b.profile.shape = "ghost".into();
        b.profile.pinned = true;
        db.save_bot(&b).unwrap();
        assert!(db.bot("old").unwrap().profile.pinned);
    }
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_file(format!("{}.lock", path.display()));
}
#[tokio::test]
async fn desktop_ticket_requires_auth_origin_and_explicit_takeover() {
    let app = app();
    let b = bot(&app.db, "codex");
    app.db.screen(&b.id).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = web::router(app.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let c = reqwest::Client::new();
    assert_eq!(
        c.post(format!("{base}/api/computer/session"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        c.post(format!("{base}/api/computer/session"))
            .bearer_auth(&app.token)
            .header("origin", "https://attacker.invalid")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        c.post(format!("{base}/api/computer/session"))
            .bearer_auth(&app.token)
            .json(&json!({"control":true}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        c.post(format!("{base}/api/computer/session"))
            .bearer_auth(&app.token)
            .json(&json!({"control":false}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.post(format!("{base}/api/takeover"))
            .bearer_auth(&app.token)
            .json(&json!({"enabled":true}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.post(format!("{base}/api/computer/session"))
            .bearer_auth(&app.token)
            .json(&json!({"control":true}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.get(format!("{base}/characters.js"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(c.get(format!("{base}/tributes.js"))
        .send().await.unwrap().text().await.unwrap().contains("createTribute"));
    server.abort();
}

#[tokio::test]
async fn screens_isolate_locks_takeover_and_cancellation_recovery() {
    let app = app();
    let a = bot(&app.db, "codex");
    let b = bot(&app.db, "codex");
    let one = app.db.screen(&a.id).unwrap();
    let two = app.db.screen(&b.id).unwrap();
    assert_ne!(one, two);
    assert_eq!(app.db.screen(&a.id).unwrap(), one);
    let _lease = app.screen_lock(one).try_lock_owned().unwrap();
    assert!(app.screen_lock(one).try_lock_owned().is_err());
    assert!(app.screen_lock(two).try_lock_owned().is_ok());
    app.db.screen_set_takeover(one, true).unwrap();
    assert!(app.db.screen_takeover(one).unwrap());
    assert!(!app.db.screen_takeover(two).unwrap());
    app.db.screen_set_quiet(one, db::now() + 65).unwrap();
    assert!(app.db.screen_quiet(one).unwrap() > db::now());
    assert_eq!(app.db.screen_quiet(two).unwrap(), 0);
    assert!(app.db.screen("unknown-bot").is_err());
}
#[test]
fn parallel_claims_allow_other_bots_but_serialize_each_bot() {
    let db = Db::open(":memory:").unwrap();
    let a = bot(&db, "codex");
    let b = bot(&db, "codex");
    let first = db.queue(&a.id, "one", 0).unwrap();
    let next = db.queue(&a.id, "two", 0).unwrap();
    let other = db.queue(&b.id, "other", 0).unwrap();
    assert_eq!(db.claim_bot(&a.id).unwrap().unwrap().id, first);
    assert!(db.claim_bot(&a.id).unwrap().is_none());
    assert_eq!(db.claim_bot(&b.id).unwrap().unwrap().id, other);
    let approval = db
        .request_approval(&first, "guest_exec", &json!({}))
        .unwrap();
    assert!(db.claim_bot(&a.id).unwrap().is_none());
    db.decide(&approval, false).unwrap();
    db.finish(&first, "completed", "done", "").unwrap();
    assert_eq!(db.claim_bot(&a.id).unwrap().unwrap().id, next);
}
#[tokio::test]
async fn bot_reactions_are_real_message_bound_and_replaceable() {
    let app = app();
    let a = bot(&app.db, "codex");
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&a.id, "Thanks!", 0).unwrap();
    let run = app.db.run(&id).unwrap();
    let seq = app.db.chat_messages(&run.chat_id).unwrap()[0]["seq"]
        .as_i64()
        .unwrap();
    let result = crate::runtime::call_tool(
        &app,
        &a,
        &run,
        "react_to_message",
        json!({"message_seq":seq,"emoji":"👍"}),
    )
    .await
    .unwrap();
    assert_ne!(result["failed"], true);
    assert_eq!(
        app.db.chat_messages(&run.chat_id).unwrap()[0]["reactions"][0]["emoji"],
        "👍"
    );
    app.db.react(&run.chat_id, &a.id, seq, "❤️").unwrap();
    assert_eq!(
        app.db.chat_messages(&run.chat_id).unwrap()[0]["reactions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let other = app.db.queue(&b.id, "Different chat", 0).unwrap();
    let other = app.db.run(&other).unwrap();
    assert!(app.db.react(&other.chat_id, &b.id, seq, "👍").is_err());
    assert!(
        app.db
            .react(&run.chat_id, &a.id, seq, "not an emoji")
            .is_err()
    );
}
#[test]
fn screen_mapping_and_takeover_survive_a_server_restart() {
    let path = std::env::temp_dir().join(format!("kindred-screen-{}.db", db::id()));
    let bot_id;
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        bot_id = bot(&db, "codex").id;
        let slot = db.screen(&bot_id).unwrap();
        db.screen_set_takeover(slot, true).unwrap();
        db.screen_set_quiet(slot, db::now() + 65).unwrap();
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(db.screen(&bot_id).unwrap(), 1);
        assert!(db.screen_takeover(1).unwrap());
        assert!(db.screen_quiet(1).unwrap() > db::now());
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn parallel_capacity_and_vnc_port_blocks_are_validated() {
    let mut c = Config::default();
    c.max_parallel_runs = 0;
    assert!(c.validate().is_err());
    c.max_parallel_runs = 9;
    assert!(c.validate().is_err());
    c.max_parallel_runs = 4;
    c.vm.vnc_view_port = 5902;
    assert!(c.validate().is_err());
    c.vm.vnc_view_port = 5901;
    assert!(c.validate().is_ok());
    c.vm.vnc_port = 65534;
    c.vm.vnc_view_port = 65535;
    assert!(c.validate().is_err());
}
#[test]
fn approval_defaults_overrides_and_live_changes() {
    let app = app();
    let mut b = bot(&app.db, "codex");
    let routine = json!({"action_scope":"routine_vm"});
    let external = json!({"action_scope":"external"});
    assert!(crate::runtime::needs_approval(&app, &b, "guest_exec", &routine).unwrap());
    app.db
        .save_setting("general", &json!({"approval_mode":"auto"}))
        .unwrap();
    assert!(!crate::runtime::needs_approval(&app, &b, "guest_exec", &routine).unwrap());
    for (tool, args) in [
        ("guest_exec", external),
        ("computer_click", json!({})),
        ("connector_execute", routine.clone()),
        ("routine_create", routine.clone()),
    ] {
        assert!(crate::runtime::needs_approval(&app, &b, tool, &args).unwrap());
    }
    b.approval_mode = "ask".into();
    app.db.save_bot(&b).unwrap();
    assert!(crate::runtime::needs_approval(&app, &b, "computer_type", &routine).unwrap());
    b.approval_mode = "full".into();
    app.db.save_bot(&b).unwrap();
    assert!(!crate::runtime::needs_approval(&app, &b, "connector_execute", &json!({})).unwrap());
    let stale = b.clone();
    b.approval_mode = "inherit".into();
    app.db.save_bot(&b).unwrap();
    app.db
        .save_setting("general", &json!({"approval_mode":"ask"}))
        .unwrap();
    assert!(crate::runtime::needs_approval(&app, &stale, "guest_exec", &routine).unwrap());
    b.approval_mode = "typo".into();
    assert!(app.db.save_bot(&b).is_err());
}

#[test]
fn action_scope_is_required_for_effectful_vm_tools() {
    for spec in crate::runtime::tool_specs() {
        if matches!(
            spec["name"].as_str().unwrap(),
            "guest_exec"
                | "computer_click"
                | "computer_type"
                | "computer_key"
                | "computer_open_url"
                | "computer_scroll"
        ) {
            assert!(
                spec["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("action_scope"))
            );
            assert_eq!(
                spec["inputSchema"]["properties"]["action_scope"]["enum"],
                json!(["routine_vm", "external"])
            );
        }
    }
}

#[test]
fn bot_instructions_accept_32000_bytes_without_expanding_memory_limit() {
    let app = app();
    let mut b = bot(&app.db, "codex");
    b.instructions = "é".repeat(16_000);
    app.db.save_bot(&b).unwrap();
    assert_eq!(app.db.bot(&b.id).unwrap().instructions, b.instructions);
    let expected = b.instructions.clone();
    assert!(app.db.save_bot_text(&b.id, "instructions", &(expected.clone()+"x"), Some(&expected)).is_err());
    app.db.save_bot_text(&b.id, "instructions", &"x".repeat(32_000), Some(&expected)).unwrap();
    b.instructions.push('x');
    assert!(app.db.save_bot(&b).is_err());
    assert!(app.db.save_bot_text(&b.id, "memory", &"x".repeat(16_001), None).is_err());
}

#[test]
fn pi_progress_reaches_activity_without_overriding_human_waits() {
    let app = app();
    let b = bot(&app.db, "codex");
    let id = app.db.queue(&b.id, "A task", 0).unwrap();
    for (state, label) in [("waiting", "Waiting for model"), ("thinking", "Thinking it through"), ("preparing", "Preparing an action"), ("writing", "Writing a response")] {
        app.db.event(&id, "model_progress", json!({"state":state})).unwrap();
        let events = app.db.activity_events(&id).unwrap();
        assert_eq!(crate::runtime::activity("running", events.last()).1, label);
        assert_eq!(crate::runtime::activity("awaiting_user", events.last()).1, "Waiting for you");
        assert_eq!(crate::runtime::activity("awaiting_approval", events.last()).1, "Waiting for your okay");
    }
    app.db.event(&id, "tool_started", json!({"tool":"computer_screenshot"})).unwrap();
    let events = app.db.activity_events(&id).unwrap();
    assert_eq!(crate::runtime::activity("running", events.last()).1, "Looking at the screen");
}
