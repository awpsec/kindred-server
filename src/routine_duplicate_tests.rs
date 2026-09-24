//! Routine scheduling and delivery must be idempotent across ticks and restarts.

use crate::db::{self, Db, Routine};
use rusqlite::params;
use std::sync::Arc;

fn routine(db: &Db, bot: &db::Bot, run_at: Option<i64>) -> Routine {
    let next_run = run_at.unwrap_or(100);
    let routine = Routine {
        id: db::id(),
        bot_id: bot.id.clone(),
        name: "Duplicate guard routine".into(),
        prompt: "Run the check once".into(),
        interval_seconds: 60,
        next_run,
        enabled: true,
        schedule: None,
        run_at,
    };
    db.save_routine(&routine).unwrap();
    routine
}

fn routine_run_count(db: &Db, id: &str) -> i64 {
    db.0.lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM routine_runs WHERE routine_id=?",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn concurrent_due_ticks_create_one_run_for_one_occurrence() {
    let db = Arc::new(Db::open(":memory:").unwrap());
    let bot = crate::tests::bot(&db, "codex");
    let saved = routine(&db, &bot, None);
    let workers = (0..8)
        .map(|_| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || db.tick(100).unwrap())
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(routine_run_count(&db, &saved.id), 1);
    assert_eq!(
        db.runs(Some(&bot.id))
            .unwrap()
            .iter()
            .filter(|run| run.status == "queued")
            .count(),
        1
    );
}

#[test]
fn one_time_routine_stays_consumed_after_restart_and_repeated_tick() {
    let path = std::env::temp_dir().join(format!("kindred-routine-duplicate-{}", db::id()));
    let (bot_id, routine_id) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let saved = routine(&db, &bot, Some(100));
        db.tick(100).unwrap();
        db.tick(100).unwrap();
        assert_eq!(routine_run_count(&db, &saved.id), 1);
        (bot.id, saved.id)
    };
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        db.tick(100).unwrap();
        db.tick(101).unwrap();
        assert_eq!(routine_run_count(&db, &routine_id), 1);
        assert_eq!(
            db.runs(Some(&bot_id))
                .unwrap()
                .iter()
                .filter(|run| run.status == "queued")
                .count(),
            1
        );
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn routine_result_duplicates_remain_physical_but_only_one_is_visible_per_run() {
    check_legacy_results(true);
    check_legacy_results(false);
}

fn check_legacy_results(with_events: bool) {
    let path = std::env::temp_dir().join(format!("kindred-routine-result-{}", db::id()));
    let (first, second, chat, duplicate_seq) = {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let saved = routine(&db, &bot, None);
        let first = db.run_routine_now(&saved.id).unwrap();
        let text = "Routine final report";
        if with_events {
            db.event(&first, "assistant", serde_json::json!({"text": text}))
                .unwrap();
        }
        db.finish(&first, "completed", text, "").unwrap();
        db.chat_complete(&db.run(&first).unwrap()).unwrap();
        let second = db.run_routine_now(&saved.id).unwrap();
        if with_events {
            db.event(&second, "assistant", serde_json::json!({"text": text}))
                .unwrap();
        }
        db.finish(&second, "completed", text, "").unwrap();
        db.chat_complete(&db.run(&second).unwrap()).unwrap();
        let chat = format!("dm-{}", bot.id);
        db.0
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created,source_event_seq) SELECT chat_id,sender,body,'result',run_id,created,NULL FROM chat_messages WHERE run_id=? AND kind='result' LIMIT 1",
                params![first],
            )
            .unwrap();
        let c = db.0.lock().unwrap();
        let duplicate_seq = c.last_insert_rowid();
        c.execute(
            "INSERT INTO user_message_reactions(message_seq,emoji) VALUES(?,'ok')",
            [duplicate_seq],
        )
        .unwrap();
        (first, second, chat, duplicate_seq)
    };

    for _ in 0..2 {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let visible = db
            .chat_messages(&chat)
            .unwrap()
            .into_iter()
            .filter(|message| message["kind"] == "result")
            .collect::<Vec<_>>();
        assert_eq!(
            visible
                .iter()
                .filter(|message| message["run_id"] == first)
                .count(),
            1
        );
        assert_eq!(
            visible
                .iter()
                .filter(|message| message["run_id"] == second)
                .count(),
            1
        );
        assert_eq!(
            visible
                .iter()
                .filter(|message| message["text"] == "Routine final report")
                .count(),
            2
        );
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM chat_messages WHERE run_id=? AND kind='result'",
                    [&first],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let c = db.0.lock().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT emoji FROM user_message_reactions WHERE message_seq=?",
                [duplicate_seq],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "ok"
        );
        assert_eq!(
            crate::message_actions::quoted_message(&c, &chat, duplicate_seq).unwrap()["text"],
            "Routine final report"
        );
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
