//! The same routine lifecycle is available in chat and in the Routines screen.
use crate::{db, runtime::App};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

pub fn cancel_queued(c: &rusqlite::Connection, id: &str, reason: &str) -> Result<usize> {
    Ok(c.execute("UPDATE runs SET status='cancelled',error=? WHERE status='queued' AND id IN (SELECT run_id FROM routine_runs WHERE routine_id=?)", params![reason,id])?)
}

// Run inside the caller's archive/startup transaction. Keep run history, remove future triggers.
pub(crate) fn remove_archived_bot_schedules(c: &rusqlite::Connection) -> Result<()> {
    c.execute("UPDATE runs SET status='cancelled',error='Assigned bot was archived before this check started' WHERE status='queued' AND bot_id IN (SELECT id FROM bots WHERE json_extract(profile,'$.archived')=1) AND (id IN (SELECT run_id FROM routine_runs) OR id IN (SELECT run_id FROM mail_runs))", [])?;
    c.execute("DELETE FROM routines WHERE bot_id IN (SELECT id FROM bots WHERE json_extract(profile,'$.archived')=1)", [])?;
    for table in ["mail_receipts", "mail_push_receipts"] {
        c.execute(&format!("DELETE FROM {table} WHERE watch_id IN (SELECT id FROM mail_watches WHERE bot_id IN (SELECT id FROM bots WHERE json_extract(profile,'$.archived')=1))"), [])?;
    }
    c.execute("DELETE FROM mail_watches WHERE bot_id IN (SELECT id FROM bots WHERE json_extract(profile,'$.archived')=1)", [])?;
    Ok(())
}

pub fn remove_scheduled(app: &App, owner: Option<&str>, id: &str) -> Result<Value> {
    ensure!(!app.account_disabled(), "This workspace is paused");
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        !crate::workspace_transfer::frozen(&tx)?,
        "This workspace is paused"
    );
    let bot: String = tx
        .query_row("SELECT bot_id FROM routines WHERE id=?", [id], |r| r.get(0))
        .context("Routine not found")?;
    ensure!(
        owner.is_none_or(|owner| owner == bot),
        "Routine not found for this bot"
    );
    let cancelled = cancel_queued(&tx, id, "Routine removed before this check started")?;
    tx.execute("DELETE FROM routines WHERE id=?", [id])?;
    tx.commit()?;
    Ok(
        json!({"removed":true,"id":id,"cancelled_queued_checks":cancelled,"history_preserved":true,"running_checks":"An already running check is unchanged; stop that task separately if needed."}),
    )
}

pub async fn for_bot(app: &App, bot: &db::Bot, run: &db::Run, args: &Value) -> Result<Value> {
    let id = crate::runtime::string(args, "id")?;
    let action = crate::runtime::string(args, "action")?;
    ensure!(
        matches!(action, "pause" | "resume" | "run_now" | "remove"),
        "Choose pause, resume, run_now or remove"
    );
    ensure!(
        !app.account_disabled() && app.db.transfer_status()?.is_null(),
        "This workspace is paused"
    );
    if let Some(w) = crate::mail_watch::watches(&app.db)?
        .into_iter()
        .find(|w| w.input.id == id)
    {
        ensure!(w.input.bot_id == bot.id, "Routine not found for this bot");
        return crate::mail_watch::control(app, Some(&bot.id), id, action).await;
    }
    let routine = app
        .db
        .routines()?
        .into_iter()
        .find(|r| r.id == id && r.bot_id == bot.id)
        .context("Routine not found for this bot. List routines before choosing an ID")?;
    match action {
        "remove" => remove_scheduled(app, Some(&bot.id), id),
        "pause" | "resume" => {
            let saved = crate::routine_updates::update(
                app,
                Some(&bot.id),
                id,
                crate::routine_updates::Changes {
                    enabled: Some(action == "resume"),
                    ..Default::default()
                },
            )?;
            Ok(
                json!({"updated":true,"routine":saved,"running_checks":"An already running check is unchanged; pause cancels queued checks."}),
            )
        }
        "run_now" => {
            // A retried call in this same task must not enqueue a second check.
            let key = format!("routine-run:{}:{}", run.id, routine.id);
            let run_id = app.db.run_routine_now_request(id, Some(&key))?;
            Ok(
                json!({"run_id":run_id,"queued":true,"schedule_unchanged":routine.run_at.is_none(),"one_time_consumed":routine.run_at.is_some(),"next":"End this turn. The saved check runs once through this bot's normal task queue; do not poll or recreate it."}),
            )
        }
        _ => unreachable!(),
    }
}

pub fn receipt(c: &rusqlite::Connection, key: Option<&str>) -> Result<Option<String>> {
    Ok(match key {
        Some(key) => c
            .query_row(
                "SELECT run_id FROM routine_run_requests WHERE request_id=?",
                [key],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime,
        tests::{app, bot},
    };
    fn routine(app: &App, bot_id: &str, at: Option<i64>) -> db::Routine {
        let r = db::Routine {
            id: db::id(),
            bot_id: bot_id.into(),
            name: "QA check".into(),
            prompt: "Report the fixture".into(),
            interval_seconds: 60,
            next_run: at.unwrap_or(db::now() + 60),
            enabled: true,
            schedule: None,
            run_at: at,
        };
        app.db.save_routine(&r).unwrap();
        r
    }
    fn text(v: Value) -> Value {
        assert_ne!(v["failed"], true, "{v}");
        serde_json::from_str(v["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn one_time_waits_for_busy_bot_and_claims_exactly_once_across_reopen() {
        let dir = std::env::temp_dir().join(format!("kindred-once-{}", db::id()));
        let path = dir.join("kindred.db").to_string_lossy().into_owned();
        let database = db::Db::open(&path).unwrap();
        let b = bot(&database, "codex");
        let at = db::now() + 60;
        let r = db::Routine {
            id: db::id(),
            bot_id: b.id.clone(),
            name: "Once".into(),
            prompt: "Remind me once".into(),
            interval_seconds: 60,
            next_run: at,
            enabled: true,
            schedule: None,
            run_at: Some(at),
        };
        database.save_routine(&r).unwrap();
        let busy = database.queue(&b.id, "Other work", 0).unwrap();
        database.tick(at + 10).unwrap();
        assert!(database.routines().unwrap()[0].enabled);
        assert_eq!(database.runs(None).unwrap().len(), 1);
        database.finish(&busy, "completed", "Done", "").unwrap();
        database.tick(at + 120).unwrap();
        let saved = database.routines().unwrap().remove(0);
        assert!(!saved.enabled);
        assert_eq!(saved.next_run, at);
        assert_eq!(database.runs(None).unwrap().len(), 2);
        drop(database);
        let database = db::Db::open(&path).unwrap();
        database.tick(at + 1000).unwrap();
        assert_eq!(database.runs(None).unwrap().len(), 2);
        assert!(!database.routines().unwrap()[0].enabled);
        drop(database);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn tool_controls_are_owned_idempotent_and_preserve_history() {
        let app = app();
        let mut b = bot(&app.db, "codex");
        b.approval_mode = "full".into();
        app.db.save_bot(&b).unwrap();
        let mut other = bot(&app.db, "codex");
        other.approval_mode = "full".into();
        app.db.save_bot(&other).unwrap();
        let r = routine(&app, &b.id, None);
        let current_id = app.db.queue(&b.id, "Test routine controls", 0).unwrap();
        let current = app.db.run(&current_id).unwrap();
        let args = json!({"id":r.id,"action":"run_now"});
        let first = text(
            runtime::call_tool(&app, &b, &current, "routine_control", args.clone())
                .await
                .unwrap(),
        );
        let second = text(
            runtime::call_tool(&app, &b, &current, "routine_control", args.clone())
                .await
                .unwrap(),
        );
        assert_eq!(first["run_id"], second["run_id"]);
        assert_eq!(
            app.db.run_routine_now(&r.id).unwrap(),
            first["run_id"].as_str().unwrap()
        );
        app.db
            .finish(
                first["run_id"].as_str().unwrap(),
                "completed",
                "Fixture",
                "",
            )
            .unwrap();
        let again = text(
            runtime::call_tool(&app, &b, &current, "routine_control", args)
                .await
                .unwrap(),
        );
        assert_eq!(again["run_id"], first["run_id"]);
        let pending = app.db.run_routine_now(&r.id).unwrap();
        let forbidden = runtime::call_tool(
            &app,
            &other,
            &current,
            "routine_control",
            json!({"id":r.id,"action":"remove"}),
        )
        .await
        .unwrap();
        assert_eq!(forbidden["failed"], true);
        assert!(remove_scheduled(&app, Some(&other.id), &r.id).is_err());
        let paused = text(
            runtime::call_tool(
                &app,
                &b,
                &current,
                "routine_control",
                json!({"id":r.id,"action":"pause"}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(paused["routine"]["enabled"], false);
        assert_eq!(paused["routine"]["next_run"], r.next_run);
        assert_eq!(app.db.run(&pending).unwrap().status, "cancelled");
        assert_ne!(app.db.run(&current_id).unwrap().status, "cancelled");
        let removed = text(
            runtime::call_tool(
                &app,
                &b,
                &current,
                "routine_control",
                json!({"id":r.id,"action":"remove"}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(removed["removed"], true);
        assert!(app.db.routines().unwrap().is_empty());
        assert_eq!(
            app.db
                .run(first["run_id"].as_str().unwrap())
                .unwrap()
                .output,
            "Fixture"
        );
    }

    #[tokio::test]
    async fn one_time_tool_validates_and_saves_real_schedule_without_self_disable_prompt() {
        let app = app();
        let mut b = bot(&app.db, "codex");
        b.approval_mode = "full".into();
        app.db.save_bot(&b).unwrap();
        let id = app.db.queue(&b.id, "Remind me once", 0).unwrap();
        let run = app.db.run(&id).unwrap();
        let at = db::now() + 120;
        let saved = text(
            runtime::call_tool(
                &app,
                &b,
                &run,
                "routine_create",
                json!({"name":"Reminder","prompt":"Check the QA file","run_at":at}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(saved["routine"]["run_at"], at);
        assert_eq!(saved["routine"]["next_run"], at);
        assert_eq!(saved["routine"]["prompt"], "Check the QA file");
        for args in [
            json!({"name":"Bad","prompt":"test","run_at":db::now()-1}),
            json!({"name":"Bad","prompt":"test","run_at":at,"interval_seconds":60}),
            json!({"trigger":"activity","prompt":"test","account_id":"test","run_at":at}),
        ] {
            assert_eq!(
                runtime::call_tool(&app, &b, &run, "routine_create", args)
                    .await
                    .unwrap()["failed"],
                true
            );
        }
        let routine_id = saved["routine"]["id"].as_str().unwrap();
        let changed = text(
            runtime::call_tool(
                &app,
                &b,
                &run,
                "routine_update",
                json!({"id":routine_id,"prompt":"Revised reminder"}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(changed["routine"]["next_run"], at);
        let changed = text(
            runtime::call_tool(
                &app,
                &b,
                &run,
                "routine_update",
                json!({"id":routine_id,"interval_seconds":300}),
            )
            .await
            .unwrap(),
        );
        assert!(changed["routine"]["run_at"].is_null());
        assert_eq!(app.db.routines().unwrap().len(), 1);
    }
}
