//! Pending completion delivery must not starve healthy runs behind failures.

use crate::db::Db;

fn count(db: &Db, table: &str, run: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(
            &format!(
                "SELECT count(*) FROM {table} WHERE run_id=? AND {}",
                if table == "chat_messages" {
                    "kind='result'"
                } else {
                    "1=1"
                }
            ),
            [run],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn healthy_pending_completion_is_reached_after_one_hundred_persistent_failures() {
    let db = Db::open(":memory:").unwrap();
    let bot = crate::tests::bot(&db, "codex");
    let mut failing = Vec::with_capacity(100);
    for index in 0..100 {
        let run = db
            .queue(&bot.id, &format!("Persistent failure {index}"), 0)
            .unwrap();
        db.finish(&run, "completed", &format!("Failure result {index}"), "")
            .unwrap();
        failing.push(run);
    }
    let healthy = db
        .queue(&bot.id, "Healthy result after failures", 0)
        .unwrap();
    db.finish(&healthy, "completed", "Healthy result", "")
        .unwrap();

    let values = failing
        .iter()
        .map(|run| format!("'{}'", run.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    db.0.lock()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER block_first_hundred_receipts BEFORE INSERT ON chat_completion_receipts
             WHEN NEW.run_id IN ({values})
             BEGIN SELECT RAISE(ABORT, 'blocked persistent completion'); END;"
        ))
        .unwrap();

    // The first bounded drain only examines the failing page. Failed rows are
    // rotated behind the untouched healthy row, which the second drain gets.
    assert!(db.deliver_pending_completions().is_err());
    assert_eq!(count(&db, "chat_completion_pending", &healthy), 1);
    assert_eq!(count(&db, "chat_messages", &healthy), 0);
    assert!(db.deliver_pending_completions().is_err());
    assert_eq!(count(&db, "chat_completion_pending", &healthy), 0);
    assert_eq!(count(&db, "chat_messages", &healthy), 1);
    for run in &failing {
        assert_eq!(count(&db, "chat_completion_pending", run), 1);
        assert_eq!(count(&db, "chat_messages", run), 0);
    }

    db.0.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER block_first_hundred_receipts")
        .unwrap();
    assert!(db.deliver_pending_completions().is_ok());
    assert!(db.deliver_pending_completions().is_ok());
    for run in &failing {
        assert_eq!(count(&db, "chat_completion_pending", run), 0);
        assert_eq!(count(&db, "chat_messages", run), 1);
    }
}
