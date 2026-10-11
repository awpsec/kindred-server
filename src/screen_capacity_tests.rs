use serde_json::Value;
use std::future::IntoFuture;

#[tokio::test]
async fn chat_status_does_not_reserve_a_computer_or_fail_at_capacity() {
    let app = crate::tests::app();
    let current = crate::tests::bot(&app.db, "codex");
    for slot in 1..=32 {
        let mut historical = crate::tests::bot(&app.db, "codex");
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO screens(bot_id,slot) VALUES(?,?)",
                rusqlite::params![historical.id, slot],
            )
            .unwrap();
        historical.profile.archived = true;
        app.db.save_bot_preferences(&historical, false).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    for _ in 0..3 {
        let response = client
            .get(format!("{base}/api/status?bot_id={}", current.id))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            200,
            "chat status must survive computer capacity exhaustion"
        );
        let status: Value = response.json().await.unwrap();
        assert_eq!(status["takeover"], false);
        assert_eq!(status["computer_busy"], false);
        assert_eq!(status["computer_recovering_seconds"], 0);
    }
    assert_eq!(app.db.screen_ids().unwrap().len(), 32);
    let response = client
        .get(format!("{base}/api/status?bot_id={}", crate::db::id()))
        .bearer_auth(&app.token)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "foreign or unknown bot must still be rejected"
    );
    server.abort();
}

#[tokio::test]
async fn ordinary_status_poll_does_not_create_first_screen() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    for suffix in [String::new(), format!("?bot_id={}", bot.id)] {
        let response = client
            .get(format!("{base}/api/status{suffix}"))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    assert!(app.db.screen_ids().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn queued_chat_runs_without_reserving_a_screen_at_capacity() {
    let app = crate::tests::app();
    for _ in 0..32 {
        let bot = crate::tests::bot(&app.db, "codex");
        app.db.screen(&bot.id).unwrap();
    }
    let bot = crate::tests::bot(&app.db, "codex");
    // A deterministic unavailable provider terminates after scheduler admission, without SSH/provider access.
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE bots SET provider='test-unavailable' WHERE id=?",
            [&bot.id],
        )
        .unwrap();
    let run = app.db.queue(&bot.id, "synthetic conversation", 0).unwrap();
    let worker = tokio::spawn(crate::runtime::scheduler(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if app.db.run(&run).unwrap().status == "failed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        app.db
            .events(&run)
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "run_started")
    );
    assert!(app.db.existing_screen(&bot.id).unwrap().is_none());
    assert_eq!(app.db.screen_ids().unwrap().len(), 32);
    worker.abort();
}

#[test]
fn reclaimed_empty_reservation_is_atomic_and_fences_restore_delete_and_stale_identity() {
    let app = crate::tests::app();
    let old = crate::tests::bot(&app.db, "codex");
    let next = crate::tests::bot(&app.db, "codex");
    // Allocate slot2 using a real preceding reservation.
    app.db.screen(&next.id).unwrap();
    let slot = app.db.screen(&old.id).unwrap();
    let old_identity = app.db.screen_identity(&old.id, slot).unwrap();
    let mut archived = old.clone();
    archived.profile.archived = true;
    app.db.save_bot_preferences(&archived, false).unwrap();
    let target = crate::tests::bot(&app.db, "codex");
    let transition = app.db.begin_empty_reassignment(&target.id, slot).unwrap();
    assert!(app.db.screen_identity(&old.id, slot).is_err());
    assert!(app.db.save_bot_preferences(&old, false).is_err());
    assert!(app.db.delete_archived_bot(&old.id, &old.name).is_err());
    assert!(app.db.begin_empty_reassignment(&target.id, slot).is_err());
    assert_eq!(
        app.db.pending_screen(&target.id).unwrap().unwrap().next,
        transition.next
    );
    app.db.finish_empty_reassignment(&transition).unwrap();
    assert!(app.db.finish_empty_reassignment(&transition).is_err());
    assert!(app.db.screen_identity(&old.id, slot).is_err());
    let new_identity = app.db.screen_identity(&target.id, slot).unwrap();
    assert_ne!(new_identity.generation, old_identity.generation);
    app.db.save_bot_preferences(&old, false).unwrap();
    assert!(app.db.existing_screen(&old.id).unwrap().is_none());
    assert_ne!(app.db.screen(&old.id).unwrap(), slot);
}

#[test]
fn deletion_retains_slot_tombstone_and_existing_profiles_keep_restore_identity() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let slot = app.db.screen(&bot.id).unwrap();
    let identity = app.db.screen_identity(&bot.id, slot).unwrap();
    let mut archived = bot.clone();
    archived.profile.archived = true;
    app.db.save_bot_preferences(&archived, false).unwrap();
    app.db.save_bot_preferences(&bot, false).unwrap();
    assert_eq!(app.db.screen_identity(&bot.id, slot).unwrap(), identity);
    app.db.save_bot_preferences(&archived, false).unwrap();
    app.db.delete_archived_bot(&bot.id, &bot.name).unwrap();
    let fresh = crate::tests::bot(&app.db, "codex");
    assert_ne!(
        app.db.screen(&fresh.id).unwrap(),
        slot,
        "SQL deletion cannot expose a previous browser profile"
    );
}

#[tokio::test]
async fn full_active_capacity_refuses_computer_allocation_without_eviction() {
    let app = crate::tests::app();
    for _ in 0..32 {
        let bot = crate::tests::bot(&app.db, "codex");
        app.db.screen(&bot.id).unwrap();
    }
    let next = crate::tests::bot(&app.db, "codex");
    assert!(app.db.empty_screen_candidates().unwrap().is_empty());
    assert!(
        crate::screen_capacity::reserve(&app, &next.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("Chats still work")
    );
    assert_eq!(app.db.screen_ids().unwrap().len(), 32);
}

#[test]
fn pending_recovery_refuses_changed_previous_owner() {
    let app = crate::tests::app();
    let first = crate::tests::bot(&app.db, "codex");
    app.db.screen(&first.id).unwrap();
    let mut old = crate::tests::bot(&app.db, "codex");
    let slot = app.db.screen(&old.id).unwrap();
    old.profile.archived = true;
    app.db.save_bot_preferences(&old, false).unwrap();
    let next = crate::tests::bot(&app.db, "codex");
    let transition = app.db.begin_empty_reassignment(&next.id, slot).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE screen_ownership SET generation=? WHERE slot=?",
            rusqlite::params![crate::db::id(), slot],
        )
        .unwrap();
    assert!(app.db.pending_screen(&next.id).is_err());
    assert!(app.db.finish_empty_reassignment(&transition).is_err());
    assert_eq!(app.db.existing_screen(&old.id).unwrap(), Some(slot));
    assert_eq!(app.db.existing_screen(&next.id).unwrap(), None);
}

#[test]
fn competing_claims_have_one_winner_and_preserve_losing_bot() {
    let app = crate::tests::app();
    let first = crate::tests::bot(&app.db, "codex");
    app.db.screen(&first.id).unwrap();
    let mut old = crate::tests::bot(&app.db, "codex");
    let slot = app.db.screen(&old.id).unwrap();
    old.profile.archived = true;
    app.db.save_bot_preferences(&old, false).unwrap();
    let a = crate::tests::bot(&app.db, "codex");
    let b = crate::tests::bot(&app.db, "codex");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers = [a.id.clone(), b.id.clone()]
        .into_iter()
        .map(|bot| {
            let app = app.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                app.db.begin_empty_reassignment(&bot, slot)
            })
        })
        .collect::<Vec<_>>();
    let mut results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = results
        .swap_remove(results.iter().position(|r| r.is_ok()).unwrap())
        .unwrap();
    app.db.finish_empty_reassignment(&winner).unwrap();
    assert_eq!(
        app.db.screen_identity(&winner.next.owner, slot).unwrap(),
        winner.next
    );
    let loser = if winner.next.owner == a.id {
        b.id
    } else {
        a.id
    };
    assert_eq!(app.db.existing_screen(&loser).unwrap(), None);
    assert_eq!(app.db.existing_screen(&old.id).unwrap(), None);
}

#[test]
fn archived_slot_with_takeover_quiet_or_pending_run_cannot_be_claimed() {
    let app = crate::tests::app();
    let first = crate::tests::bot(&app.db, "codex");
    app.db.screen(&first.id).unwrap();
    let mut old = crate::tests::bot(&app.db, "codex");
    let slot = app.db.screen(&old.id).unwrap();
    let run = app.db.queue(&old.id, "synthetic pending work", 0).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE runs SET status='completed' WHERE id=?", [&run])
        .unwrap();
    old.profile.archived = true;
    app.db.save_bot_preferences(&old, false).unwrap();
    let next = crate::tests::bot(&app.db, "codex");
    app.db.screen_set_takeover(slot, true).unwrap();
    assert!(!app.db.empty_screen_candidates().unwrap().contains(&slot));
    assert!(app.db.begin_empty_reassignment(&next.id, slot).is_err());
    app.db.screen_set_takeover(slot, false).unwrap();
    app.db
        .screen_set_quiet(slot, crate::db::now() + 65)
        .unwrap();
    assert!(app.db.begin_empty_reassignment(&next.id, slot).is_err());
    app.db.screen_set_quiet(slot, 0).unwrap();
    assert!(app.db.empty_screen_candidates().unwrap().contains(&slot));
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE runs SET status='queued' WHERE id=?", [&run])
        .unwrap();
    assert!(app.db.begin_empty_reassignment(&next.id, slot).is_err());
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE runs SET status='completed' WHERE id=?", [&run])
        .unwrap();
    assert!(app.db.empty_screen_candidates().unwrap().contains(&slot));
}

#[test]
fn runtime_protocol_does_not_authorize_missing_or_stale_launchers() {
    let hashes = crate::vm_maintenance::managed_launcher_hashes();
    assert!(
        crate::screen_capacity::validate_guest_support(
            &serde_json::json!({"protocol":1,"launcher_hashes":hashes})
        )
        .is_err()
    );
    assert!(
        crate::screen_capacity::validate_guest_support(&serde_json::json!({"protocol":2})).is_err()
    );
    for name in [
        "browser-launch.py",
        "desktop-launch",
        "start-desktop.sh",
        "ensure-screen",
        "screen-ownership.py",
    ] {
        let mut stale = hashes.clone();
        stale[name] = serde_json::json!("old helper");
        assert!(
            crate::screen_capacity::validate_guest_support(
                &serde_json::json!({"protocol":2,"launcher_hashes":stale})
            )
            .is_err()
        );
    }
    crate::screen_capacity::validate_guest_support(
        &serde_json::json!({"protocol":2,"launcher_hashes":hashes}),
    )
    .unwrap();
}

#[test]
fn separate_workspace_cannot_resolve_or_finish_foreign_ownership() {
    let one = crate::tests::app();
    let two = crate::tests::app();
    let first = crate::tests::bot(&one.db, "codex");
    one.db.screen(&first.id).unwrap();
    let mut old = crate::tests::bot(&one.db, "codex");
    let slot = one.db.screen(&old.id).unwrap();
    old.profile.archived = true;
    one.db.save_bot_preferences(&old, false).unwrap();
    let next = crate::tests::bot(&one.db, "codex");
    let claim = one.db.begin_empty_reassignment(&next.id, slot).unwrap();
    assert!(two.db.existing_screen(&old.id).is_err());
    assert!(two.db.screen_identity(&old.id, slot).is_err());
    assert!(two.db.finish_empty_reassignment(&claim).is_err());
    assert!(two.db.screen_ids().unwrap().is_empty());
    assert_eq!(one.db.existing_screen(&old.id).unwrap(), Some(slot));
}

#[test]
fn missing_or_mismatched_ledger_is_not_a_free_slot() {
    let app = crate::tests::app();
    let old = crate::tests::bot(&app.db, "codex");
    app.db
        .0
        .lock()
        .unwrap()
        .execute("INSERT INTO screens(bot_id,slot) VALUES(?,2)", [&old.id])
        .unwrap();
    let next = crate::tests::bot(&app.db, "codex");
    assert!(!app.db.empty_screen_candidates().unwrap().contains(&2));
    assert!(app.db.begin_empty_reassignment(&next.id, 2).is_err());
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO screen_ownership VALUES(2,?,?,1)",
            rusqlite::params![crate::db::id(), crate::db::id()],
        )
        .unwrap();
    assert!(app.db.begin_empty_reassignment(&next.id, 2).is_err());
    assert_eq!(app.db.existing_screen(&old.id).unwrap(), Some(2));
}

#[test]
fn changed_pending_previous_identity_cannot_be_committed() {
    let app = crate::tests::app();
    let first = crate::tests::bot(&app.db, "codex");
    app.db.screen(&first.id).unwrap();
    let mut old = crate::tests::bot(&app.db, "codex");
    let slot = app.db.screen(&old.id).unwrap();
    old.profile.archived = true;
    app.db.save_bot_preferences(&old, false).unwrap();
    let next = crate::tests::bot(&app.db, "codex");
    let transition = app.db.begin_empty_reassignment(&next.id, slot).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE screen_reassignments SET old_generation=? WHERE slot=?",
            rusqlite::params![crate::db::id(), slot],
        )
        .unwrap();
    assert!(app.db.finish_empty_reassignment(&transition).is_err());
    assert_eq!(app.db.existing_screen(&old.id).unwrap(), Some(slot));
    assert_eq!(app.db.existing_screen(&next.id).unwrap(), None);
}
