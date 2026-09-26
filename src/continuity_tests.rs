//! Offline reconstruction evaluations. These exercise real SQLite, context and
//! tool boundaries; they deliberately do not claim to evaluate a live model.
use crate::{
    continuity::{self, store},
    db::{self, Db, Run},
    tests::{app, bot},
};
use rusqlite::params;
use serde_json::{Value, json};

fn message(db: &Db, run: &Run, text: &str) -> i64 {
    let c = db.0.lock().unwrap();
    c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user',?,'message',?,?)",params![run.chat_id,text,run.id,db::now()]).unwrap();
    c.last_insert_rowid()
}
fn setup(db: &Db) -> (crate::db::Bot, Run) {
    let b = bot(db, "codex");
    let run = db
        .run(&db.queue(&b.id, "Continue my work", 0).unwrap())
        .unwrap();
    (b, run)
}
fn note(db: &Db, run: &Run, seq: i64, text: &str, revision: i64) -> Value {
    continuity::save(db,run,&json!({"topic":"work","summary":text,"status":"active","expected_revision":revision,"kind":"checkpoint","statement_kind":"user_statement","sources":[{"message_seq":seq}]})).unwrap()
}
fn promise(db: &Db, run: &Run, seq: i64, key: &str) -> Value {
    store::obligation_save(db,run,&json!({"key":key,"description":"Return to the draft after the user decides","status":"waiting","expected_revision":0,"sources":[{"message_seq":seq}]})).unwrap()
}
fn packet(text: &str) -> Value {
    serde_json::from_str(text.rsplit_once('\n').unwrap().1).unwrap()
}

#[test]
fn saved_knowledge_is_retrieved_across_allowed_rooms_and_keeps_provenance() {
    let db = Db::open(":memory:").unwrap();
    let (b, dm) = setup(&db);
    let peer = bot(&db, "codex");
    let room = crate::chats::Chat {
        id: db::id(),
        name: "Workshop".into(),
        members: vec![b.id.clone(), peer.id],
        archived: false,
        description: String::new(),
        bot_only: false,
        pinned: false,
        last_message: None,
    };
    db.save_chat(&room).unwrap();
    let id = db
        .chat_send(&room.id, "Save the measured observation", &[b.id.clone()])
        .unwrap()
        .remove(0);
    let run = db.run(&id).unwrap();
    let seq = message(
        &db,
        &run,
        "Verified measurement from the retained worksheet",
    );
    store::start_session(&db, &run, "pi", "model").unwrap();
    continuity::save(&db,&run,&json!({"topic":"calibration","summary":"Cobalt steamer operates at pressure 24. The units still need verification.","status":"active","expected_revision":0,"kind":"knowledge","statement_kind":"observation","sources":[{"message_seq":seq}]})).unwrap();
    let mut recall = dm.clone();
    recall.prompt = "What did we learn about the cobalt steamer?".into();
    let value = continuity::context(&db, &recall).unwrap();
    assert!(
        value["relevant_notes"]
            .to_string()
            .contains("units still need verification")
    );
    assert_eq!(value["relevant_notes"][0]["statement_kind"], "observation");
    assert_eq!(value["relevant_notes"][0]["sources"][0]["message_seq"], seq);
    let mut other = room.clone();
    other.id = db::id();
    other.name = "Unrelated room".into();
    db.save_chat(&other).unwrap();
    recall.chat_id = other.id;
    assert!(
        continuity::context(&db, &recall).unwrap()["relevant_notes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET body='Measurement withdrawn' WHERE seq=?",
            [seq],
        )
        .unwrap();
    recall.chat_id = dm.chat_id;
    assert!(
        continuity::context(&db, &recall).unwrap()["relevant_notes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn restart_without_provider_session_recovers_intentions_and_receipts() {
    let dir = std::env::temp_dir().join(format!("continuity-restart-{}", db::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kindred.db");
    let db = Db::open(path.to_str().unwrap()).unwrap();
    let (b, run) = setup(&db);
    let source = message(
        &db,
        &run,
        "Juniper: draft the invitation, but wait before sending.",
    );
    store::start_session(&db, &run, "codex", "old-model").unwrap();
    promise(&db, &run, source, "invitation");
    db.event(
        &run.id,
        "tool_requested",
        json!({"tool":"send_mail","call_id":"sent-once"}),
    )
    .unwrap();
    db.event(&run.id,"tool_result",json!({"tool":"send_mail","call_id":"sent-once","failed":false,"text":"Verified provider receipt receipt-17"})).unwrap();
    // No checkpoint or provider session file is written. A crash leaves the run
    // interrupted; durable receipts must still survive the subsequent model change.
    db.0.lock()
        .unwrap()
        .execute("UPDATE runs SET status='running' WHERE id=?", [&run.id])
        .unwrap();
    drop(db);
    let db = Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(db.run(&run.id).unwrap().status, "interrupted");
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE bots SET provider='claude-code',model='new-model' WHERE id=?",
            [&b.id],
        )
        .unwrap();
    let next = db
        .run(
            &db.queue(&b.id, "What happened with that invitation?", 0)
                .unwrap(),
        )
        .unwrap();
    store::start_session(&db, &next, "claude-code", "new-model").unwrap();
    let context = continuity::context(&db, &next).unwrap();
    assert_eq!(context["obligations"]["total"], 1);
    assert!(
        context["relevant_history"]
            .to_string()
            .contains("wait before sending")
    );
    let receipts = store::receipts(&db, &next, &run.id, i64::MAX).unwrap();
    assert!(receipts.to_string().contains("receipt-17"));
    assert!(
        receipts["instructions"]
            .as_str()
            .unwrap()
            .contains("not verified success")
    );
    let count: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM events WHERE kind='tool_requested'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "Reconstruction itself must execute no actions");
    assert_eq!(db.bot(&b.id).unwrap().id, b.id);
    assert_eq!(next.chat_id, run.chat_id);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn private_sources_memory_and_tools_never_enter_shared_context() {
    let app = app();
    let (b, private) = setup(&app.db);
    let other = bot(&app.db, "codex");
    message(&app.db, &private, "PRIVATE_HISTORY_SENTINEL at the orchard");
    app.db
        .save_bot_text(&b.id, "memory", "PRIVATE_MEMORY_SENTINEL", None)
        .unwrap();
    app.db
        .save_setting(
            "general",
            &json!({"identity":"PRIVATE_PREFERENCE_SENTINEL"}),
        )
        .unwrap();
    let room = crate::chats::Chat {
        id: db::id(),
        name: "Shared".into(),
        members: vec![b.id.clone(), other.id],
        archived: false,
        description: String::new(),
        bot_only: false,
        pinned: false,
        last_message: None,
    };
    app.db.save_chat(&room).unwrap();
    let mut shared = private.clone();
    shared.chat_id = room.id.clone();
    shared.prompt = "orchard".into();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE runs SET chat_id=? WHERE id=?",
            params![room.id, shared.id],
        )
        .unwrap();
    let text = crate::instructions::build(
        &app,
        &b,
        &shared,
        &crate::runtime::tool_specs(),
        Some(128000),
    )
    .unwrap();
    for sentinel in [
        "PRIVATE_HISTORY_SENTINEL",
        "PRIVATE_MEMORY_SENTINEL",
        "PRIVATE_PREFERENCE_SENTINEL",
    ] {
        assert!(!text.contains(sentinel));
    }
    assert!(continuity::search_for(&app.db, &shared, Some(&private.chat_id), "orchard").is_err());
    for (tool, args) in [
        ("chat_read", json!({"chat_id":private.chat_id})),
        ("memory_read", json!({})),
        (
            "history_search",
            json!({"chat_id":private.chat_id,"query":"orchard"}),
        ),
    ] {
        let result = crate::runtime::call_tool(&app, &b, &shared, tool, args)
            .await
            .unwrap();
        assert_eq!(result["failed"], true, "{tool}");
        assert!(!result.to_string().contains("SENTINEL"));
    }
}

#[test]
fn corrections_invalidate_cross_conversation_checkpoints_and_stale_writers() {
    let db = Db::open(":memory:").unwrap();
    let (b, run) = setup(&db);
    let peer = bot(&db, "codex");
    let room = crate::chats::Chat {
        id: db::id(),
        name: "Shared".into(),
        members: vec![b.id.clone(), peer.id],
        archived: false,
        description: String::new(),
        bot_only: false,
        pinned: false,
        last_message: None,
    };
    db.save_chat(&room).unwrap();
    let mut source_run = run.clone();
    source_run.chat_id = room.id;
    let source = message(&db, &source_run, "The appointment is Thursday");
    let own = message(&db, &run, "Prepare the appointment checklist");
    store::start_session(&db, &run, "codex", "model").unwrap();
    note(&db, &run, own, "Thursday appointment", 0);
    db.event(
        &run.id,
        "context",
        json!({"state":"compacted","summary":"Thursday appointment"}),
    )
    .unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET body='Correction: appointment Friday' WHERE seq=?",
            [source],
        )
        .unwrap();
    let read = continuity::notes(&db, &run, None, i64::MAX).unwrap();
    assert_eq!(read["items"][0]["invalidated"], true);
    assert_eq!(read["items"][0]["summary"], "");
    assert!(continuity::context(&db, &run).unwrap()["last_harness_summary"].is_null());
    assert!(continuity::save(&db,&run,&json!({"topic":"work","summary":"Stale Thursday","status":"active","expected_revision":1})).is_err());
    let next = db.run(&db.queue(&b.id, "Continue", 0).unwrap()).unwrap();
    store::start_session(&db, &next, "pi", "new-model").unwrap();
    note(&db, &next, own, "Check current appointment source", 1);
    assert_eq!(
        continuity::notes(&db, &next, None, i64::MAX).unwrap()["items"][0]["invalidated"],
        false
    );
}

#[test]
fn source_supersession_forgetting_and_index_rebuild_do_not_resurrect_prose() {
    let db = Db::open(":memory:").unwrap();
    let (b, run) = setup(&db);
    let original = message(&db, &run, "Cedar appointment Thursday obsolete-sentinel");
    let correction = message(&db, &run, "Cedar appointment Friday corrected-sentinel");
    note(&db, &run, original, "obsolete-sentinel", 0);
    promise(&db, &run, original, "cedar");
    store::revise_source(&db,original,&json!({"action":"supersede","successor":correction,"expected_text":"Cedar appointment Thursday obsolete-sentinel","expected_revision":0})).unwrap();
    let result = continuity::search_for(&db, &run, None, "Cedar")
        .unwrap()
        .to_string();
    assert!(!result.contains("obsolete-sentinel"));
    assert!(result.contains("corrected-sentinel"));
    assert_eq!(
        store::obligations(&db, &run, "", false).unwrap()["items"][0]["requires_reconciliation"],
        true
    );
    store::revise_source(&db,correction,&json!({"action":"forget","expected_text":"Cedar appointment Friday corrected-sentinel","expected_revision":0})).unwrap();
    while !store::backfill(&db, 2).unwrap() {}
    assert!(
        !continuity::context(&db, &run)
            .unwrap()
            .to_string()
            .contains("sentinel")
    );
    assert!(
        continuity::search_for(&db, &run, None, "sentinel").unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mem = db.bot(&b.id).unwrap().memory;
    assert!(!mem.contains("sentinel"));
    let c = db.0.lock().unwrap();
    let old: i64 = c
        .query_row(
            "SELECT count(*) FROM continuity_revisions WHERE summary LIKE '%sentinel%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old, 0);
}

#[test]
fn dormant_obligations_are_paged_without_search_and_require_real_execution_links() {
    let db = Db::open(":memory:").unwrap();
    let (_, run) = setup(&db);
    let seq = message(&db, &run, "Please follow up after I decide.");
    for i in 0..45 {
        promise(&db, &run, seq, &format!("promise-{i}"));
    }
    for i in 0..200 {
        message(&db, &run, &format!("Unrelated subject {i}"));
    }
    let context = continuity::bounded_context(&db, &run, 4000).unwrap();
    assert_eq!(context["obligations"]["total"], 45);
    let mut cursor = String::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = store::obligations(&db, &run, &cursor, false).unwrap();
        for item in page["items"].as_array().unwrap() {
            assert_eq!(item["execution"]["scheduled"], false);
            seen.insert(item["id"].as_str().unwrap().to_string());
        }
        match page["next_after"].as_str() {
            Some(next) => cursor = next.into(),
            None => break,
        }
    }
    assert_eq!(seen.len(), 45);
    let args = json!({"key":"promise-0","description":"Follow up","expected_revision":1,"status":"open","sources":[{"message_seq":seq}],"link_kind":"routine","link_id":"invented"});
    assert!(store::obligation_save(&db, &run, &args).is_err());
    let mut no_link = args.clone();
    no_link["link_kind"] = json!("");
    no_link["link_id"] = json!("");
    store::obligation_save(&db, &run, &no_link).unwrap();
    assert!(store::obligation_save(&db, &run, &no_link).is_err());
    let count: i64 =
        db.0.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM routines", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn bounded_context_model_budgets_and_diagnostics_do_not_log_content() {
    let app = app();
    let (b, run) = setup(&app.db);
    message(&app.db, &run, "TOP_SECRET_SELECTION_SENTINEL");
    for i in 0..120 {
        message(
            &app.db,
            &run,
            &format!("Historical note {i}: {}", "x".repeat(1000)),
        );
    }
    for window in [32768, 64000, 128000] {
        let tools = crate::runtime::tool_specs();
        let text = crate::instructions::build(&app, &b, &run, &tools, Some(window)).unwrap();
        let parsed = packet(&text);
        assert_eq!(parsed["context_selection"]["window_tokens"], window);
        assert!(
            store::estimated_tokens(&text)
                + store::estimated_tokens(&json!(tools).to_string())
                + store::estimated_tokens(&run.prompt)
                <= window as usize - (window as usize / 10).min(8192) - window as usize / 20
        );
    }
    let logs = app
        .db
        .events(&run.id)
        .unwrap()
        .into_iter()
        .filter(|e| e["kind"] == "context_selection")
        .collect::<Vec<_>>();
    assert!(!json!(logs).to_string().contains("SENTINEL"));
    assert!(
        crate::instructions::build(&app, &b, &run, &crate::runtime::tool_specs(), Some(1024))
            .is_err()
    );
}

#[test]
fn mixed_history_reconstruction_metrics_and_resumable_backfill() {
    let db = Db::open(":memory:").unwrap();
    let (_, run) = setup(&db);
    {
        let c = db.0.lock().unwrap();
        c.execute_batch("DROP TRIGGER continuity_search_insert;")
            .unwrap();
    }
    let baseline: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let start = std::time::Instant::now();
    {
        let mut c = db.0.lock().unwrap();
        let tx = c.transaction().unwrap();
        for i in 0..10000 {
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user',?,'message',?)",params![run.chat_id,format!("Subject item{i:05}: an independent task, preference, decision or observation."),i]).unwrap();
        }
        tx.commit().unwrap();
    }
    store::migrate(&db.0.lock().unwrap()).unwrap();
    let before = continuity::search_for(&db, &run, None, "item00003").unwrap();
    assert_eq!(before["items"].as_array().unwrap().len(), 1);
    assert!(!store::backfill(&db, 127).unwrap());
    let cursor: i64 =
        db.0.lock()
            .unwrap()
            .query_row("SELECT cursor FROM continuity_index_progress", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(cursor, 127);
    store::migrate(&db.0.lock().unwrap()).unwrap();
    assert_eq!(
        db.0.lock()
            .unwrap()
            .query_row("SELECT cursor FROM continuity_index_progress", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        127
    );
    while !store::backfill(&db, 256).unwrap() {}
    let query_start = std::time::Instant::now();
    let mut hits = 0;
    let mut results = 0;
    for i in [3, 127, 500, 999, 2001, 4000, 7007, 8888, 9998, 9999] {
        let target = format!("item{i:05}");
        let result = continuity::search_for(&db, &run, None, &target).unwrap();
        results += result["items"].as_array().unwrap().len();
        hits += usize::from(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["excerpt"].as_str().unwrap().contains(&target)),
        );
    }
    assert_eq!(hits, 10);
    assert_eq!(results, 10);
    // A recent antecedent lets a context-free pronoun recover an old source.
    message(&db, &run, "Let's revisit item00003.");
    let mut indirect = run.clone();
    indirect.id = "new-request".into();
    indirect.prompt = "What did we decide about that?".into();
    assert!(
        continuity::context(&db, &indirect).unwrap()["relevant_history"]
            .to_string()
            .contains("item00003")
    );
    let c = db.0.lock().unwrap();
    let bytes: i64 = c
        .query_row(
            "SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()",
            [],
            |r| r.get(0),
        )
        .unwrap();
    eprintln!(
        "CONTINUITY_METRICS {}",
        json!({"fixture":"mixed-10000","exact_recall_at_8":hits as f64/10.0,"exact_precision":hits as f64/results as f64,"queries":10,"query_total_us":query_start.elapsed().as_micros(),"fixture_total_ms":start.elapsed().as_millis(),"sqlite_bytes":bytes,"sqlite_growth_bytes":bytes-baseline,"bytes_per_added_message":(bytes-baseline)/10000,"live_model":false})
    );
}

#[test]
fn transfer_keeps_continuity_and_execution_receipts_without_provider_sessions() {
    let db = Db::open(":memory:").unwrap();
    let (_, run) = setup(&db);
    let seq = message(&db, &run, "Return to this unfinished work");
    store::start_session(&db, &run, "pi", "model").unwrap();
    note(&db, &run, seq, "Unfinished", 0);
    promise(&db, &run, seq, "later");
    db.finish(
        &run.id,
        "completed",
        "Draft prepared; nothing scheduled.",
        "",
    )
    .unwrap();
    db.chat_complete(&run).unwrap();
    db.0.lock().unwrap().execute("INSERT INTO command_jobs(id,run_id,bot_id,device_id,screen,title,status,receipt,created) VALUES('command-receipt',?,?,'old-device',0,'Build','completed','{\"exit_code\":0}',0)",params![run.id,run.bot_id]).unwrap();
    let package = db
        .prepare_transfer(&db::id(), "Continuity fixture")
        .unwrap();
    let destination = Db::open(":memory:").unwrap();
    destination.import_transfer(&package).unwrap();
    assert_eq!(
        store::obligations(&destination, &run, "", false).unwrap()["total"],
        1
    );
    assert_eq!(
        continuity::notes(&destination, &run, None, i64::MAX).unwrap()["items"][0]["summary"],
        "Unfinished"
    );
    assert!(
        continuity::search_for(&destination, &run, None, "unfinished").unwrap()["items"]
            .as_array()
            .unwrap()
            .len()
            > 0
    );
    let count: i64 = destination
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM command_jobs WHERE id='command-receipt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(package["tables"].get("continuity_search").is_none());
}

#[test]
fn concurrent_checkpoint_writers_have_one_winner_and_failed_save_is_atomic() {
    let db = std::sync::Arc::new(Db::open(":memory:").unwrap());
    let (_, run) = setup(&db);
    let seq = message(&db, &run, "Preserve this decision");
    note(&db, &run, seq, "Original", 0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers=(0..2).map(|i|{let db=db.clone();let run=run.clone();let barrier=barrier.clone();std::thread::spawn(move||{
        barrier.wait();continuity::save(&db,&run,&json!({"topic":"work","summary":format!("Writer {i}"),"status":"active","expected_revision":1})).is_ok()
    })}).collect::<Vec<_>>();
    let wins = workers
        .into_iter()
        .map(|w| usize::from(w.join().unwrap()))
        .sum::<usize>();
    assert_eq!(wins, 1);
    db.0.lock().unwrap().execute_batch("CREATE TEMP TRIGGER fail_checkpoint BEFORE INSERT ON continuity_revisions BEGIN SELECT RAISE(ABORT,'simulated interruption'); END;").unwrap();
    assert!(
        continuity::save(
            &db,
            &run,
            &json!({"topic":"work","summary":"Uncommitted","status":"active","expected_revision":2})
        )
        .is_err()
    );
    let state = continuity::notes(&db, &run, None, i64::MAX).unwrap();
    assert_eq!(state["items"][0]["revision"], 2);
    assert_ne!(state["items"][0]["summary"], "Uncommitted");
}

#[test]
fn migration_preserves_legacy_records_and_rejects_future_schema() {
    let db = Db::open(":memory:").unwrap();
    let (b, run) = setup(&db);
    message(&db, &run, "Legacy retained evidence");
    db.save_bot_text(&b.id, "memory", "Existing memory", None)
        .unwrap();
    {
        let c = db.0.lock().unwrap();
        c.execute_batch("DELETE FROM continuity_schema; DELETE FROM continuity_index_progress;")
            .unwrap();
    }
    for _ in 0..3 {
        continuity::migrate(&db.0.lock().unwrap()).unwrap();
    }
    assert_eq!(db.bot(&b.id).unwrap().memory, "Existing memory");
    assert_eq!(db.run(&run.id).unwrap().chat_id, run.chat_id);
    assert!(
        continuity::search_for(&db, &run, None, "Legacy")
            .unwrap()
            .to_string()
            .contains("Legacy retained evidence")
    );
    let c = db.0.lock().unwrap();
    c.execute("INSERT INTO continuity_schema VALUES(999,0)", [])
        .unwrap();
    assert!(continuity::migrate(&c).is_err());
}

#[tokio::test]
async fn source_lifecycle_endpoint_requires_owner_auth_and_preserves_source_on_stale_revision() {
    use tower::ServiceExt;
    let app = app();
    let (_, run) = setup(&app.db);
    let seq = message(&app.db, &run, "Retained original");
    let router = crate::web::router(app.clone());
    let body = json!({"action":"forget","expected_text":"Retained original","expected_revision":0})
        .to_string();
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/continuity/sources/{seq}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(
        store::revise_source(
            &app.db,
            seq,
            &json!({"action":"forget","expected_text":"Retained original","expected_revision":1})
        )
        .is_err()
    );
    assert!(
        continuity::search_for(&app.db, &run, None, "original")
            .unwrap()
            .to_string()
            .contains("Retained original")
    );
}

#[test]
fn review_new_receipts_remain_readable_after_a_source_correction() {
    let db = Db::open(":memory:").unwrap();
    let (b, old) = setup(&db);
    store::start_session(&db, &old, "pi", "model").unwrap();
    let seq = message(&db, &old, "Old preference");
    db.event(&old.id, "tool_result", json!({"text":"old-receipt"}))
        .unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET body='Corrected preference' WHERE seq=?",
            [seq],
        )
        .unwrap();
    let fresh = db
        .run(&db.queue(&b.id, "Perform the new task", 0).unwrap())
        .unwrap();
    store::start_session(&db, &fresh, "pi", "model").unwrap();
    db.event(
        &fresh.id,
        "tool_result",
        json!({"text":"fresh-confirmed-receipt"}),
    )
    .unwrap();
    db.finish(&fresh.id, "completed", "fresh-confirmed-result", "")
        .unwrap();
    let observer = db
        .run(&db.queue(&b.id, "Review recent progress", 0).unwrap())
        .unwrap();
    let context = continuity::context(&db, &observer).unwrap();
    assert!(
        context["recent_task_journal"]
            .to_string()
            .contains("fresh-confirmed-result")
    );
    assert!(
        store::receipts(&db, &fresh, &fresh.id, i64::MAX)
            .unwrap()
            .to_string()
            .contains("fresh-confirmed-receipt")
    );
    assert!(
        !store::receipts(&db, &fresh, &old.id, i64::MAX)
            .unwrap()
            .to_string()
            .contains("old-receipt")
    );
}

#[test]
fn review_exact_message_reads_cannot_resurrect_invalidated_derivations() {
    let db = Db::open(":memory:").unwrap();
    let (b, old) = setup(&db);
    store::start_session(&db, &old, "pi", "model").unwrap();
    let original = message(&db, &old, "Remove this private sentinel");
    let derived = {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,'Repeated private sentinel','assistant',?,?)", params![old.chat_id,b.id,old.id,db::now()]).unwrap();
        c.last_insert_rowid()
    };
    store::revise_source(&db, original, &json!({"action":"forget","expected_text":"Remove this private sentinel","expected_revision":0})).unwrap();
    assert!(
        db.bot_chat_message(&b.id, &old.chat_id, derived, 0)
            .is_err()
    );
    let fresh = db
        .run(&db.queue(&b.id, "Use current evidence", 0).unwrap())
        .unwrap();
    store::start_session(&db, &fresh, "pi", "model").unwrap();
    assert!(
        store::source_refs(
            &db.0.lock().unwrap(),
            &fresh,
            &json!({"sources":[{"message_seq":derived}]})
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires a disposable database created by the published 0.83.0 binary"]
fn published_database_upgrades_in_place_without_losing_continuity() {
    let source = std::env::var("KINDRED_TEST_LEGACY_CONTINUITY_DB").unwrap();
    let destination = std::env::temp_dir().join(format!("kindred-legacy-upgrade-{}.db", db::id()));
    std::fs::copy(source, &destination).unwrap();
    for _ in 0..2 {
        let db = Db::open(destination.to_str().unwrap()).unwrap();
        assert_eq!(
            db.bot("legacy-bot").unwrap().memory,
            "Keep this stable preference"
        );
        assert_eq!(
            db.bot("legacy-bot").unwrap().instructions,
            "Keep the same role"
        );
        let run = db.run("legacy-run").unwrap();
        assert_eq!(run.output, "Verified result");
        assert_eq!(db.routines().unwrap()[0].id, "legacy-routine");
        assert_eq!(
            continuity::notes(&db, &run, None, i64::MAX).unwrap()["items"][0]["summary"],
            "Preserved checkpoint"
        );
        assert!(
            continuity::search_for(&db, &run, None, "Legacy continuity evidence")
                .unwrap()
                .to_string()
                .contains("Legacy continuity evidence")
        );
        while !store::backfill(&db, 1).unwrap() {}
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
    std::fs::remove_file(&destination).unwrap();
    let _ = std::fs::remove_file(format!("{}.lock", destination.display()));
}

#[test]
fn review_legacy_derived_messages_do_not_survive_source_forgetting() {
    let db = Db::open(":memory:").unwrap();
    let (b, old) = setup(&db);
    // Legacy runs have no dependency/session records.
    let original = message(&db, &old, "Legacy private sentinel");
    let derived = {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,'Legacy private sentinel repeated','assistant',?,?)", params![old.chat_id,b.id,old.id,db::now()]).unwrap();
        c.last_insert_rowid()
    };
    store::revise_source(
        &db,
        original,
        &json!({"action":"forget","expected_text":"Legacy private sentinel","expected_revision":0}),
    )
    .unwrap();
    while !store::backfill(&db, 64).unwrap() {}
    assert!(
        db.bot_chat_message(&b.id, &old.chat_id, derived, 0)
            .is_err()
    );
    assert!(
        !continuity::search_for(&db, &old, None, "sentinel")
            .unwrap()
            .to_string()
            .contains("Legacy private sentinel")
    );
    // Other people's original statements are evidence, not generated summaries.
    let human = {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'human:fixture','Retained human correction','message',?,?)", params![old.chat_id,old.id,db::now()]).unwrap();
        c.last_insert_rowid()
    };
    assert!(
        db.bot_chat_read(&b.id, &old.chat_id, 0, 20)
            .unwrap()
            .to_string()
            .contains("Retained human correction")
    );
    assert!(db.bot_chat_message(&b.id, &old.chat_id, human, 0).is_ok());
}

#[test]
fn review_obligations_cannot_reintroduce_invalidated_derived_evidence() {
    let db = Db::open(":memory:").unwrap();
    let (b, old) = setup(&db);
    store::start_session(&db, &old, "pi", "model").unwrap();
    let original = message(&db, &old, "Forget private detail");
    let derived = {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,'derived-secret','assistant',?,?)", params![old.chat_id,b.id,old.id,db::now()]).unwrap();
        c.last_insert_rowid()
    };
    db.event(&old.id, "tool_result", json!({"text":"derived-secret"}))
        .unwrap();
    let event: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT max(seq) FROM events WHERE run_id=?",
                [&old.id],
                |r| r.get(0),
            )
            .unwrap();
    for (key, source) in [
        ("message", json!({"message_seq":derived})),
        ("event", json!({"event_seq":event})),
    ] {
        store::obligation_save(&db,&old,&json!({"key":key,"description":"Act on derived-secret","expected_revision":0,"sources":[source]})).unwrap();
    }
    // A correction also invalidates derivations, without the explicit forget path's redaction.
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET body='Corrected input' WHERE seq=?",
            [original],
        )
        .unwrap();
    let fresh = db
        .run(&db.queue(&b.id, "Review pending work", 0).unwrap())
        .unwrap();
    store::start_session(&db, &fresh, "pi", "model").unwrap();
    let obligations = store::obligations(&db, &fresh, "", false).unwrap();
    assert_eq!(obligations["total"], 2);
    assert!(!obligations.to_string().contains("derived-secret"));
    assert!(
        obligations["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["requires_reconciliation"] == true)
    );
    assert!(
        store::source_refs(
            &db.0.lock().unwrap(),
            &fresh,
            &json!({"sources":[{"event_seq":event}]})
        )
        .is_err()
    );
}
