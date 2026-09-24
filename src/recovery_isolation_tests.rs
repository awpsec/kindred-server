//! Pending completion delivery must isolate one bad result from other runs.

use crate::db::Db;

fn result_count(db: &Db, run: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM chat_messages WHERE run_id=? AND kind='result'",
            [run],
            |row| row.get(0),
        )
        .unwrap()
}

fn pending_count(db: &Db, run: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM chat_completion_pending WHERE run_id=?",
            [run],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn one_run_delivery_error_does_not_starve_an_independent_pending_completion() {
    let db = Db::open(":memory:").unwrap();
    let bot = crate::tests::bot(&db, "codex");
    let first = db.queue(&bot.id, "First independent task", 0).unwrap();
    let second = db.queue(&bot.id, "Second independent task", 0).unwrap();
    db.finish(&first, "completed", "First result", "").unwrap();
    db.finish(&second, "completed", "Second result", "")
        .unwrap();

    // A run-specific persistent fault models a malformed/blocked result write;
    // the connection and the second run remain usable.
    db.0.lock()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER block_first_completion BEFORE INSERT ON chat_messages
             WHEN NEW.run_id='{}' AND NEW.kind='result'
             BEGIN SELECT RAISE(ABORT, 'blocked first completion'); END;",
            first
        ))
        .unwrap();

    let delivery = db.deliver_pending_completions();
    assert!(
        delivery.is_err(),
        "the run-specific delivery error must remain observable"
    );
    assert_eq!(
        pending_count(&db, &first),
        1,
        "failed delivery remains retryable"
    );
    assert_eq!(result_count(&db, &first), 0);
    assert_eq!(
        pending_count(&db, &second),
        0,
        "independent delivery must be drained"
    );
    assert_eq!(result_count(&db, &second), 1);

    // Repeated recovery keeps retrying only the failed run and does not create
    // duplicate results for the already-delivered independent run.
    assert!(db.deliver_pending_completions().is_err());
    assert_eq!(pending_count(&db, &first), 1);
    assert_eq!(result_count(&db, &second), 1);
}

#[test]
fn startup_delivery_error_does_not_prevent_later_pending_runs_or_retry() {
    let path = std::env::temp_dir().join(format!("kindred-recovery-isolation-{}", crate::db::id()));
    let (first, second) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let first = db.queue(&bot.id, "First startup recovery", 0).unwrap();
        let second = db.queue(&bot.id, "Second startup recovery", 0).unwrap();
        db.finish(&first, "completed", "First startup result", "")
            .unwrap();
        db.finish(&second, "completed", "Second startup result", "")
            .unwrap();
        db.0
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER block_first_startup_receipt BEFORE INSERT ON chat_completion_receipts
                 WHEN NEW.run_id='{}'
                 BEGIN SELECT RAISE(ABORT, 'blocked first startup receipt'); END;",
                first
            ))
            .unwrap();
        (first, second)
    };

    // Db::open repairs the historical result projection, then attempts both
    // pending deliveries. The first fault must not make startup fail or starve
    // the independent second run.
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(pending_count(&db, &first), 1);
        assert_eq!(pending_count(&db, &second), 0);
        assert_eq!(result_count(&db, &first), 1);
        assert_eq!(result_count(&db, &second), 1);
        db.0.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER block_first_startup_receipt")
            .unwrap();
        db.deliver_pending_completions().unwrap();
        assert_eq!(pending_count(&db, &first), 0);
        assert_eq!(result_count(&db, &first), 1);
    }
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(pending_count(&db, &first), 0);
        assert_eq!(result_count(&db, &first), 1);
        assert_eq!(result_count(&db, &second), 1);
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
