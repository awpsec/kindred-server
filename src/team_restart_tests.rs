//! Restart recovery tests for shared-chat result delivery.

use crate::{
    chats::Chat,
    db::{self, Bot, Db, Run},
};

fn shared_chat(db: &Db) -> (Bot, Bot, Chat) {
    let mut a = crate::tests::bot(db, "codex");
    a.name = "A".into();
    db.save_bot(&a).unwrap();
    let mut b = crate::tests::bot(db, "codex");
    b.name = "B".into();
    db.save_bot(&b).unwrap();
    let chat = Chat {
        bot_only: false,
        description: String::new(),
        id: db::id(),
        name: "A and B".into(),
        members: vec![a.id.clone(), b.id.clone()],
        archived: false,
        pinned: false,
        last_message: None,
    };
    db.save_chat(&chat).unwrap();
    (a, b, chat)
}

fn queued_for(db: &Db, bot: &str) -> Vec<Run> {
    db.runs(Some(bot))
        .unwrap()
        .into_iter()
        .filter(|run| run.status == "queued")
        .collect()
}

fn cleanup(path: &std::path::Path) {
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn restart_routes_recovered_shared_result_once_when_crash_precedes_chat_complete() {
    let path = std::env::temp_dir().join(format!("kindred-team-restart-{}", db::id()));
    let (run_id, b_id, chat_id, expected) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let (a, b, chat) = shared_chat(&db);
        let run = db
            .chat_send(&chat.id, "A, start the review", &[])
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let expected = "B, please continue from this verified result.".to_string();
        assert!(
            crate::team_chats::addressed(
                &expected,
                &[
                    (a.id.clone(), a.name.clone()),
                    (b.id.clone(), b.name.clone())
                ],
                true
            )
            .contains(&b.id)
        );
        assert_eq!(db.run(&run).unwrap().bot_id, a.id);
        db.finish(&run, "completed", &expected, "").unwrap();
        // Simulate a process exit after durable completion but before chat_complete.
        (run, b.id, chat.id, expected)
    };

    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let queued = queued_for(&db, &b_id);
        assert_eq!(queued.len(), 1);
        assert!(queued[0].prompt.contains(&expected));
        assert_eq!(queued[0].chat_id, chat_id);
        assert_eq!(
            db.chat_messages(&chat_id)
                .unwrap()
                .iter()
                .filter(|m| m["kind"] == "result" && m["run_id"] == run_id)
                .count(),
            1
        );
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(queued_for(&db, &b_id).len(), 1);
    }
    cleanup(&path);
}

#[test]
fn restart_does_not_duplicate_shared_result_already_delivered_before_exit() {
    let path = std::env::temp_dir().join(format!("kindred-team-restart-delivered-{}", db::id()));
    let (b_id, expected) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let (_, b, chat) = shared_chat(&db);
        let run = db
            .chat_send(&chat.id, "A, start the review", &[])
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let expected = "B, please review this result delivered before restart.".to_string();
        db.finish(&run, "completed", &expected, "").unwrap();
        db.chat_complete(&db.run(&run).unwrap()).unwrap();
        assert_eq!(queued_for(&db, &b.id).len(), 1);
        (b.id, expected)
    };
    let db = Db::open(path.to_str().unwrap()).unwrap();
    let queued = queued_for(&db, &b_id);
    assert_eq!(queued.len(), 1);
    assert!(queued[0].prompt.contains(&expected));
    drop(db);
    cleanup(&path);
}

#[test]
fn a_failed_delivery_keeps_its_pending_record_and_retries_once() {
    let db = Db::open(":memory:").unwrap();
    let (_, b, chat) = shared_chat(&db);
    let id = db.chat_send(&chat.id, "A, start the review", &[]).unwrap()[0].clone();
    db.finish(&id, "completed", "B, please review the saved result.", "")
        .unwrap();
    db.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON chat_messages WHEN NEW.kind='result' BEGIN SELECT RAISE(FAIL,'Injected result write failure'); END;").unwrap();
    assert!(db.chat_complete(&db.run(&id).unwrap()).is_err());
    assert!(queued_for(&db, &b.id).is_empty());
    let pending: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM chat_completion_pending WHERE run_id=?",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(pending, 1);
    db.0.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_result")
        .unwrap();
    db.deliver_pending_completions().unwrap();
    db.deliver_pending_completions().unwrap();
    assert_eq!(queued_for(&db, &b.id).len(), 1);
    let pending: i64 =
        db.0.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM chat_completion_pending", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(pending, 0);
}

#[test]
fn historical_replies_without_pending_delivery_are_not_replayed_on_upgrade() {
    let path = std::env::temp_dir().join(format!("kindred-historical-team-{}", db::id()));
    let (b_id, chat_id) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let (_, b, chat) = shared_chat(&db);
        let id = db.chat_send(&chat.id, "A, old conversation", &[]).unwrap()[0].clone();
        db.finish(
            &id,
            "completed",
            "B, this is historical conversation text.",
            "",
        )
        .unwrap();
        // Model a database written before pending-delivery records existed.
        db.0.lock()
            .unwrap()
            .execute("DELETE FROM chat_completion_pending", [])
            .unwrap();
        (b.id, chat.id)
    };
    let db = Db::open(path.to_str().unwrap()).unwrap();
    assert!(queued_for(&db, &b_id).is_empty());
    assert!(
        db.chat_messages(&chat_id)
            .unwrap()
            .iter()
            .any(|m| m["text"] == "B, this is historical conversation text.")
    );
    drop(db);
    cleanup(&path);
}

#[test]
fn stopping_a_completed_reply_before_delivery_prevents_new_teammate_work() {
    let db = Db::open(":memory:").unwrap();
    let (_, b, chat) = shared_chat(&db);
    let id = db.chat_send(&chat.id, "A, start the review", &[]).unwrap()[0].clone();
    db.finish(&id, "completed", "B, please review this result.", "")
        .unwrap();
    db.cancel(&id).unwrap();
    db.cancel(&id).unwrap();
    db.deliver_pending_completions().unwrap();
    db.chat_complete(&db.run(&id).unwrap()).unwrap();
    assert!(queued_for(&db, &b.id).is_empty());
    assert_eq!(
        db.run(&id).unwrap().status,
        "completed",
        "preserve the result already completed before Stop"
    );
    assert_eq!(
        db.chat_messages(&chat.id)
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "result" && m["run_id"] == id)
            .count(),
        1
    );
}

#[test]
fn pending_delivery_is_frozen_and_moves_with_the_workspace() {
    let path = std::env::temp_dir().join(format!("kindred-transfer-pending-{}", db::id()));
    let source = Db::open(path.to_str().unwrap()).unwrap();
    let (_, b, chat) = shared_chat(&source);
    let id = source
        .chat_send(&chat.id, "A, start the review", &[])
        .unwrap()[0]
        .clone();
    source
        .finish(
            &id,
            "completed",
            "B, please review the transferred result.",
            "",
        )
        .unwrap();
    let transfer = db::id();
    let package = source
        .prepare_transfer(&transfer, "Pending result")
        .unwrap();
    assert_eq!(
        package["tables"]["chat_completion_pending"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    source.deliver_pending_completions().unwrap();
    source.chat_complete(&source.run(&id).unwrap()).unwrap();
    assert!(queued_for(&source, &b.id).is_empty());
    drop(source);
    let source = Db::open(path.to_str().unwrap()).unwrap();
    assert!(
        queued_for(&source, &b.id).is_empty(),
        "reopening cannot bypass transfer freeze"
    );
    let destination = Db::open(":memory:").unwrap();
    destination.import_transfer(&package).unwrap();
    source
        .finish_transfer(&transfer, "https://destination.example")
        .unwrap();
    destination.deliver_pending_completions().unwrap();
    destination.deliver_pending_completions().unwrap();
    assert_eq!(queued_for(&destination, &b.id).len(), 1);
    assert!(queued_for(&source, &b.id).is_empty());
    drop(source);
    cleanup(&path);
}

#[test]
fn cancelling_transfer_releases_pending_delivery_once_on_the_original_workspace() {
    let db = Db::open(":memory:").unwrap();
    let (_, b, chat) = shared_chat(&db);
    let id = db.chat_send(&chat.id, "A, start the review", &[]).unwrap()[0].clone();
    db.finish(
        &id,
        "completed",
        "B, please review the retained result.",
        "",
    )
    .unwrap();
    let transfer = db::id();
    db.prepare_transfer(&transfer, "Retained result").unwrap();
    db.deliver_pending_completions().unwrap();
    assert!(queued_for(&db, &b.id).is_empty());
    db.cancel_transfer(&transfer).unwrap();
    db.deliver_pending_completions().unwrap();
    db.deliver_pending_completions().unwrap();
    assert_eq!(queued_for(&db, &b.id).len(), 1);
}
