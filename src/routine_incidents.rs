//! A recurring schedule has one current interruption, not one recovery action per tick.
//! Presentation never deletes execution receipts or marks unfinished work successful.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS routine_health(routine_id TEXT PRIMARY KEY,reset_after INTEGER NOT NULL DEFAULT 0,auto_paused INTEGER NOT NULL DEFAULT 0); CREATE INDEX IF NOT EXISTS routine_runs_routine ON routine_runs(routine_id);")?;
    Ok(())
}
pub fn reset(c: &Connection, id: &str) -> Result<()> {
    c.execute("INSERT INTO routine_health VALUES(?1,(SELECT COALESCE(MAX(rowid),0) FROM runs),0) ON CONFLICT(routine_id) DO UPDATE SET reset_after=excluded.reset_after,auto_paused=0",[id])?;
    Ok(())
}
// Only the latest leaf of each occurrence counts. A command/question handoff is
// not successful completion and must not clear a previous failed check.
pub fn view(c: &Connection, id: &str) -> Result<Value> {
    let definition: Option<(String, bool)> = c
        .query_row("SELECT name,enabled FROM routines WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((name, enabled)) = definition else {
        return Ok(Value::Null);
    };
    let (reset, auto): (i64, bool) = c
        .query_row(
            "SELECT reset_after,auto_paused FROM routine_health WHERE routine_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, false));
    let mut q=c.prepare("SELECT r.id,r.status,r.error,r.rowid, EXISTS(SELECT 1 FROM routine_completion rc WHERE rc.run_id=r.id AND rc.outcome IN ('quiet','report')) OR (x.run_id IS NULL AND NOT EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind IN ('process_wait','question_wait'))) FROM routine_runs rr JOIN runs r ON r.id=rr.run_id LEFT JOIN routine_execution x ON x.run_id=r.id WHERE rr.routine_id=? AND NOT EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind='task_continued') AND NOT EXISTS(SELECT 1 FROM routine_execution newer WHERE newer.root_run_id=x.root_run_id AND newer.rowid>x.rowid) ORDER BY r.rowid DESC")?;
    let mut failures = 0;
    let mut streak = 0;
    let mut latest = None;
    let mut error = String::new();
    let mut active = false;
    for row in q.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })? {
        let (run, status, reason, order, verified) = row?;
        if status == "completed" && reason.is_empty() && verified {
            break;
        }
        if matches!(
            status.as_str(),
            "queued" | "running" | "awaiting_user" | "awaiting_approval" | "cancelling"
        ) {
            active = true;
        }
        if matches!(status.as_str(), "failed" | "interrupted") {
            failures += 1;
            if order > reset {
                streak += 1;
            }
            if latest.is_none() {
                latest = Some(run);
                error = reason;
            }
        }
    }
    Ok(
        json!({"routine_id":id,"name":name,"enabled":enabled,"auto_paused":auto,"failures":failures,"streak":streak,"run_id":latest,"error":error,"active":active}),
    )
}
pub fn pause_if_needed(c: &Connection, id: &str) -> Result<bool> {
    let v = view(c, id)?;
    if v["enabled"] == true && v["streak"].as_u64().unwrap_or(0) >= 3 {
        c.execute("UPDATE routines SET enabled=0 WHERE id=?", [id])?;
        c.execute("INSERT INTO routine_health(routine_id,auto_paused) VALUES(?,1) ON CONFLICT(routine_id) DO UPDATE SET auto_paused=1",[id])?;
        crate::routine_controls::cancel_queued(
            c,
            id,
            "Routine paused after three consecutive failed checks",
        )?;
        return Ok(true);
    }
    Ok(false)
}
pub fn finished(c: &Connection, run: &str) -> Result<()> {
    let id: Option<String> = c
        .query_row(
            "SELECT routine_id FROM routine_runs WHERE run_id=?",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        pause_if_needed(c, &id)?;
    }
    Ok(())
}
fn decorate(
    c: &Connection,
    row: &mut Value,
    cache: &mut std::collections::HashMap<String, (Value, Option<i64>)>,
) -> Result<()> {
    if row.get("status_notice").is_none() {
        return Ok(());
    }
    let run = row["run_id"].as_str().unwrap_or("");
    if row["status_notice"].get("continued_by").is_some() {
        row["interruption_hidden"] = json!(true);
        return Ok(());
    }
    let id: Option<String> = c
        .query_row(
            "SELECT routine_id FROM routine_runs WHERE run_id=?",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        if !cache.contains_key(&id) {
            let v = view(c, &id)?;
            let representative:Option<i64>=c.query_row("SELECT MAX(seq) FROM chat_messages WHERE run_id=? AND kind='result' AND suppressed=0",[v["run_id"].as_str().unwrap_or("")],|r|r.get(0))?;
            cache.insert(id.clone(), (v, representative));
        }
        let (v, representative) = &cache[&id];
        // Evaluate once per schedule against complete history, not each chat page.
        let visible = !v.is_null()
            && v["active"] != true
            && v["failures"].as_u64().unwrap_or(0) > 0
            && row["seq"].as_i64() == *representative;
        row["interruption_hidden"] = json!(!visible);
        if visible {
            row["routine_incident"] = v.clone();
        }
    }
    Ok(())
}

pub fn decorate_rows(c: &Connection, rows: &mut [Value]) -> Result<()> {
    let mut cache = std::collections::HashMap::new();
    for row in rows {
        decorate(c, row, &mut cache)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, tests};
    fn fixture() -> (crate::runtime::Shared, db::Routine) {
        let app = tests::app();
        let bot = tests::bot(&app.db, "codex");
        let r = db::Routine {
            id: db::id(),
            bot_id: bot.id,
            name: "Inbox check".into(),
            prompt: "Check inbox".into(),
            enabled: true,
            interval_seconds: 3600,
            next_run: db::now() + 3600,
            schedule: None,
            run_at: None,
        };
        app.db.save_routine(&r).unwrap();
        (app, r)
    }
    fn fail(app: &crate::runtime::App, r: &db::Routine) -> String {
        let run = app.db.run_routine_now(&r.id).unwrap();
        app.db
            .finish(&run, "failed", "", "Quota exhausted")
            .unwrap();
        app.db.chat_complete(&app.db.run(&run).unwrap()).unwrap();
        run
    }
    #[test]
    fn repeated_failures_pause_once_and_project_one_action_across_pages() {
        let (app, r) = fixture();
        let mut ids = vec![];
        for _ in 0..3 {
            ids.push(fail(&app, &r));
        }
        assert!(!app.db.routines().unwrap()[0].enabled);
        let before = app.db.runs(None).unwrap().len();
        app.db.tick(db::now() + 86400).unwrap();
        assert_eq!(before, app.db.runs(None).unwrap().len());
        let messages = app.db.chat_messages(&format!("dm-{}", r.bot_id)).unwrap();
        let visible = messages
            .iter()
            .filter(|m| m.get("routine_incident").is_some())
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["routine_incident"]["failures"], 3);
        assert_eq!(visible[0]["routine_incident"]["auto_paused"], true);
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["interruption_hidden"] == true)
                .count(),
            2
        );
        for m in &messages {
            let seq = m["seq"].as_i64().unwrap();
            let page = app
                .db
                .chat_message_window(&format!("dm-{}", r.bot_id), Some(seq), Some(seq), 1, true)
                .unwrap();
            assert_eq!(page["messages"].as_array().unwrap().len(), 1);
            assert_eq!(
                page["messages"][0]["interruption_hidden"],
                m["interruption_hidden"]
            );
        }

        assert_eq!(app.db.run(&ids[0]).unwrap().status, "failed");
        assert!(
            app.db.continue_task(&ids[0]).is_err(),
            "Old failures cannot restart obsolete occurrences"
        );
    }
    #[test]
    fn recovery_is_one_current_check_and_verified_success_clears_old_cards() {
        let (app, r) = fixture();
        for _ in 0..3 {
            fail(&app, &r);
        }
        let next = app.db.recover_routine(&r.id).unwrap();
        assert_eq!(app.db.recover_routine(&r.id).unwrap(), next);
        assert!(app.db.routines().unwrap()[0].enabled);
        assert!(
            app.db
                .chat_messages(&format!("dm-{}", r.bot_id))
                .unwrap()
                .iter()
                .all(|m| m.get("status_notice").is_none() || m["interruption_hidden"] == true)
        );
        crate::routine_completion::complete(
            &app.db.0.lock().unwrap(),
            &next,
            &json!({"outcome":"report","report":"Checked current inbox"}),
        )
        .unwrap();
        app.db
            .finish(&next, "completed", "Checked current inbox", "")
            .unwrap();
        app.db.chat_complete(&app.db.run(&next).unwrap()).unwrap();
        let view = view(&app.db.0.lock().unwrap(), &r.id).unwrap();
        assert_eq!(view["failures"], 0);
        let messages = app.db.chat_messages(&format!("dm-{}", r.bot_id)).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["text"] == "Checked current inbox")
        );
        assert!(
            messages
                .iter()
                .filter(|m| m.get("status_notice").is_some())
                .all(|m| m["interruption_hidden"] == true)
        );
        fail(&app, &r);
        assert!(app.db.routines().unwrap()[0].enabled);
    }
    #[test]
    fn explicit_resume_resets_budget_but_does_not_claim_success() {
        let (app, r) = fixture();
        for _ in 0..3 {
            fail(&app, &r);
        }
        crate::routine_updates::update(
            &app,
            None,
            &r.id,
            crate::routine_updates::Changes {
                enabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let v = view(&app.db.0.lock().unwrap(), &r.id).unwrap();
        assert_eq!(v["streak"], 0);
        assert_eq!(v["failures"], 3);
        fail(&app, &r);
        assert!(app.db.routines().unwrap()[0].enabled);
    }
    #[test]
    fn stopped_recovery_can_be_retried_without_replaying_old_work() {
        let (app, r) = fixture();
        fail(&app, &r);
        let stopped = app.db.recover_routine(&r.id).unwrap();
        app.db.finish(&stopped, "cancelled", "", "Stopped").unwrap();
        let fresh = app.db.recover_routine(&r.id).unwrap();
        assert_ne!(fresh, stopped);
        assert_eq!(app.db.recover_routine(&r.id).unwrap(), fresh);
        assert_eq!(
            view(&app.db.0.lock().unwrap(), &r.id).unwrap()["failures"],
            1
        );
    }
    #[test]
    fn unrelated_success_and_cancel_do_not_clear_failure() {
        let (app, r) = fixture();
        fail(&app, &r);
        let other = app.db.queue(&r.bot_id, "Unrelated work", 0).unwrap();
        app.db.finish(&other, "completed", "Done", "").unwrap();
        let cancel = app.db.run_routine_now(&r.id).unwrap();
        app.db.finish(&cancel, "cancelled", "", "Stopped").unwrap();
        let v = view(&app.db.0.lock().unwrap(), &r.id).unwrap();
        assert_eq!(v["failures"], 1);
        assert_eq!(v["streak"], 1);
    }
    #[test]
    fn continued_task_notice_is_hidden_without_erasing_the_run() {
        let (app, r) = fixture();
        let source = app.db.queue(&r.bot_id, "One task", 0).unwrap();
        app.db
            .finish(&source, "failed", "", "Disconnected")
            .unwrap();
        app.db.chat_complete(&app.db.run(&source).unwrap()).unwrap();
        app.db.continue_task(&source).unwrap();
        let messages = app.db.chat_messages(&format!("dm-{}", r.bot_id)).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["run_id"] == source && m["interruption_hidden"] == true)
        );
        assert_eq!(app.db.run(&source).unwrap().status, "failed");
    }
    #[test]
    fn compact_directory_bounds_errors_but_explicit_details_keep_them() {
        let (app, r) = fixture();
        let run = app.db.run_routine_now(&r.id).unwrap();
        let error = "Provider diagnostic. ".repeat(200);
        app.db.finish(&run, "failed", "", &error).unwrap();
        let list = crate::mail_watch::directory(&app.db, &r.bot_id, None).unwrap();
        assert_eq!(list[0]["incident"]["error_shortened"], true);
        assert!(list[0]["incident"]["error"].as_str().unwrap().len() < 650);
        let detail = crate::mail_watch::directory(&app.db, &r.bot_id, Some(&r.id)).unwrap();
        assert_eq!(detail[0]["incident"]["error"], error);
    }
    #[test]
    fn legacy_backlog_and_pause_survive_reopening() {
        let path = std::env::temp_dir()
            .join(format!("kindred-incidents-{}", db::id()))
            .join("kindred.db");
        let database = db::Db::open(path.to_str().unwrap()).unwrap();
        let bot = tests::bot(&database, "codex");
        let r = db::Routine {
            id: db::id(),
            bot_id: bot.id.clone(),
            name: "Legacy inbox".into(),
            prompt: "Check inbox".into(),
            enabled: true,
            interval_seconds: 60,
            next_run: db::now() - 1,
            schedule: None,
            run_at: None,
        };
        database.save_routine(&r).unwrap();
        for _ in 0..15 {
            let run = database.run_routine_now(&r.id).unwrap();
            database
                .finish(&run, "failed", "", "Quota exhausted")
                .unwrap();
            database
                .chat_complete(&database.run(&run).unwrap())
                .unwrap();
        }
        // Simulate an older installation that never paused the schedule.
        database
            .0
            .lock()
            .unwrap()
            .execute("UPDATE routines SET enabled=1", [])
            .unwrap();
        drop(database);
        let database = db::Db::open(path.to_str().unwrap()).unwrap();
        database.tick(db::now()).unwrap();
        assert!(!database.routines().unwrap()[0].enabled);
        let rows = database.chat_messages(&format!("dm-{}", bot.id)).unwrap();
        assert_eq!(
            rows.iter()
                .filter(|m| m.get("routine_incident").is_some())
                .count(),
            1
        );
        assert_eq!(
            view(&database.0.lock().unwrap(), &r.id).unwrap()["failures"],
            15
        );
        let active = database.recover_routine(&r.id).unwrap();
        drop(database);
        let database = db::Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(database.recover_routine(&r.id).unwrap(), active);
        assert_eq!(database.runs(None).unwrap().len(), 16);
        drop(database);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
