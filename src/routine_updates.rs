//! Patch an existing scheduled routine without recreating it or moving its next check.
use crate::{
    db::{self, Routine},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::params;
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Changes {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
    pub interval_seconds: Option<i64>,
    pub schedule: Option<crate::schedules::WeeklySchedule>,
    pub run_at: Option<i64>,
    pub expected_prompt: Option<String>,
}

pub fn update(app: &App, owner: Option<&str>, id: &str, changes: Changes) -> Result<Routine> {
    ensure!(!app.account_disabled(), "This workspace is paused");
    ensure!(
        changes.name.is_some()
            || changes.prompt.is_some()
            || changes.enabled.is_some()
            || changes.interval_seconds.is_some()
            || changes.schedule.is_some()
            || changes.run_at.is_some(),
        "Supply at least one routine field to change"
    );
    ensure!(
        [
            changes.interval_seconds.is_some(),
            changes.schedule.is_some(),
            changes.run_at.is_some()
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            <= 1,
        "Choose one-time, interval or weekly timing, not more than one"
    );
    ensure!(
        changes.interval_seconds.is_none() || changes.schedule.is_none(),
        "Choose an interval or a weekly schedule, not both"
    );
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        !crate::workspace_transfer::frozen(&tx)?,
        "This workspace is paused"
    );
    let stored: String = tx.query_row("SELECT json_object('id',id,'bot_id',bot_id,'name',name,'prompt',prompt,'enabled',json(CASE WHEN enabled THEN 'true' ELSE 'false' END),'interval_seconds',interval_seconds,'next_run',next_run,'schedule',json(schedule),'run_at',run_at) FROM routines WHERE id=?", [id], |r|r.get(0)).context("Scheduled routine not found. Inspect routines_list; Constant activity routines use inbox_monitor_save")?;
    let mut routine: Routine = serde_json::from_str(&stored)?;
    ensure!(
        owner.is_none_or(|owner| owner == routine.bot_id),
        "Routine not found for this bot"
    );
    if let Some(expected) = changes.expected_prompt {
        ensure!(
            expected == routine.prompt,
            "The routine instructions changed since they were read. List routines again before editing"
        );
    }
    let was_enabled = routine.enabled;
    let old_interval = routine.interval_seconds;
    let old_schedule = routine.schedule.clone();
    let old_run_at = routine.run_at;
    if let Some(name) = changes.name {
        routine.name = name;
    }
    if let Some(prompt) = changes.prompt {
        routine.prompt = prompt;
    }
    if let Some(enabled) = changes.enabled {
        routine.enabled = enabled;
    }
    if let Some(interval) = changes.interval_seconds {
        routine.interval_seconds = interval;
        routine.schedule = None;
        routine.run_at = None;
    }
    if let Some(schedule) = changes.schedule {
        schedule.validate()?;
        routine.interval_seconds = i64::from(schedule.every_minutes) * 60;
        routine.schedule = Some(schedule);
        routine.run_at = None;
    }
    if let Some(at) = changes.run_at {
        ensure!(
            chrono::DateTime::from_timestamp(at, 0).is_some() && at > db::now(),
            "Choose a future one-time date"
        );
        routine.run_at = Some(at);
        routine.schedule = None;
        routine.interval_seconds = 60;
    }
    ensure!(
        !routine.name.trim().is_empty() && routine.name.len() <= 100,
        "Routine name must be 1 to 100 bytes"
    );
    ensure!(
        !routine.prompt.trim().is_empty() && routine.prompt.len() <= 64000,
        "Routine instructions must be 1 to 64000 bytes"
    );
    ensure!(
        (60..=31536000).contains(&routine.interval_seconds),
        "Interval must be 60 seconds to one year"
    );
    ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM routines WHERE bot_id=? AND id<>? AND lower(name)=lower(?))", params![routine.bot_id,id,routine.name],|r|r.get::<_,bool>(0))?, "Another routine already has that name. Update the intended routine instead of merging or duplicating it");
    if routine.enabled {
        crate::commands::resolve(&tx, &routine.prompt)?;
    }
    if old_interval != routine.interval_seconds
        || old_schedule != routine.schedule
        || old_run_at != routine.run_at
        || (!was_enabled && routine.enabled)
    {
        routine.next_run = if let Some(at) = routine.run_at {
            ensure!(
                at > db::now(),
                "Choose a new future date to enable this one-time reminder"
            );
            at
        } else {
            match &routine.schedule {
                Some(schedule) => schedule.next_after(db::now())?,
                None => db::now()
                    .checked_add(routine.interval_seconds)
                    .context("Invalid interval")?,
            }
        };
    }
    tx.execute("UPDATE routines SET name=?,prompt=?,enabled=?,interval_seconds=?,schedule=?,next_run=?,run_at=? WHERE id=?",params![routine.name,routine.prompt,routine.enabled,routine.interval_seconds,routine.schedule.as_ref().map(serde_json::to_string).transpose()?,routine.next_run,routine.run_at,id])?;
    if !routine.enabled {
        crate::routine_controls::cancel_queued(
            &tx,
            id,
            "Routine paused before this check started",
        )?;
    }
    tx.commit()?;
    Ok(routine)
}

pub async fn route(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(changes): Json<Changes>,
) -> Result<Json<Routine>, crate::web::Error> {
    Ok(Json(update(&app, None, &id, changes)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime,
        tests::{app, bot},
    };
    use serde_json::{Value, json};
    fn routine(app: &App, bot_id: &str, enabled: bool) -> Routine {
        let r=Routine{run_at:None,id:db::id(),bot_id:bot_id.into(),name:"Daily check".into(),prompt:"Report the game and where to watch.".into(),enabled,interval_seconds:3600,next_run:db::now()+86400,schedule:Some(serde_json::from_value(json!({"timezone":"America/New_York","days":[1,2,3,4,5,6,7],"start":"09:00","end":"09:00","every_minutes":60})).unwrap())};
        app.db.save_routine(&r).unwrap();
        r
    }
    fn value(r: &Routine) -> Value {
        serde_json::to_value(r).unwrap()
    }
    fn patch(v: Value) -> Changes {
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    async fn tool_edits_existing_instructions_without_recreating_or_rescheduling() {
        let app = app();
        let mut b = bot(&app.db, "codex");
        b.approval_mode = "full".into();
        app.db.save_bot(&b).unwrap();
        let original = routine(&app, &b.id, true);
        let id = app
            .db
            .queue(&b.id, "Use no emojis and one watch line", 0)
            .unwrap();
        let run = app.db.run(&id).unwrap();
        let duplicate=runtime::call_tool(&app,&b,&run,"routine_create",json!({"name":original.name,"prompt":"Revised instructions","schedule":original.schedule})).await.unwrap();
        assert_eq!(duplicate["failed"], true);
        assert!(
            duplicate["text"]
                .as_str()
                .unwrap()
                .contains("routine_update")
        );
        assert!(duplicate["text"].as_str().unwrap().contains(&original.id));
        let args = json!({"id":original.id,"prompt":"Report the game without emojis. Keep where to watch to one line."});
        let result = runtime::call_tool(&app, &b, &run, "routine_update", args.clone())
            .await
            .unwrap();
        assert_ne!(result["failed"], true);
        let saved: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
        assert_eq!(saved["routine"]["prompt"], args["prompt"]);
        let mut expected = value(&original);
        expected["prompt"] = args["prompt"].clone();
        assert_eq!(saved["routine"], expected);
        let repeated = runtime::call_tool(&app, &b, &run, "routine_update", args)
            .await
            .unwrap();
        assert_ne!(repeated["failed"], true);
        assert_eq!(app.db.routines().unwrap().len(), 1);
        assert_eq!(value(&app.db.routines().unwrap()[0]), expected);
        assert_eq!(app.db.runs(None).unwrap().len(), 1);
        assert_eq!(app.db.bot(&b.id).unwrap().memory, b.memory);
        let listing = runtime::call_tool(&app, &b, &run, "routines_list", json!({}))
            .await
            .unwrap();
        let listed: Value = serde_json::from_str(listing["text"].as_str().unwrap()).unwrap();
        assert_eq!(listed[0]["prompt"], expected["prompt"]);
        // Future scheduled work uses the edited prompt; past/current runs stay pinned.
        let mut due = app.db.routines().unwrap().remove(0);
        due.schedule = None;
        due.next_run = db::now() - 1;
        app.db.save_routine(&due).unwrap();
        app.db.finish(&id, "completed", "Acknowledged", "").unwrap();
        app.db.tick(db::now()).unwrap();
        assert_eq!(app.db.run(&id).unwrap().prompt, run.prompt);
        assert!(
            app.db
                .runs(None)
                .unwrap()
                .iter()
                .any(|r| r.id != id && r.prompt == expected["prompt"].as_str().unwrap())
        );
    }

    #[test]
    fn edits_preserve_paused_state_and_reject_stale_or_unowned_changes() {
        let app = app();
        let b = bot(&app.db, "codex");
        let other = bot(&app.db, "codex");
        let original = routine(&app, &b.id, false);
        let saved = update(
            &app,
            Some(&b.id),
            &original.id,
            patch(json!({"prompt":"Short report"})),
        )
        .unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.next_run, original.next_run);
        assert_eq!(saved.schedule, original.schedule);
        let before = value(&saved);
        for (owner, id, change) in [
            (
                other.id.as_str(),
                original.id.as_str(),
                json!({"prompt":"Cross-bot edit"}),
            ),
            (b.id.as_str(), "missing", json!({"prompt":"No creation"})),
            (
                b.id.as_str(),
                original.id.as_str(),
                json!({"prompt":"Overwrite","expected_prompt":original.prompt}),
            ),
            (b.id.as_str(), original.id.as_str(), json!({"prompt":""})),
            (
                b.id.as_str(),
                original.id.as_str(),
                json!({"interval_seconds":0}),
            ),
            (b.id.as_str(), original.id.as_str(), json!({})),
        ] {
            assert!(update(&app, Some(owner), id, patch(change)).is_err());
            assert_eq!(value(&app.db.routines().unwrap()[0]), before);
        }
        assert!(
            serde_json::from_value::<Changes>(json!({"bot_id":other.id,"prompt":"Reassign"}))
                .is_err()
        );
        assert!(serde_json::from_value::<Changes>(json!({"next_run":0})).is_err());
    }

    #[test]
    fn timing_changes_validate_and_retries_do_not_postpone_the_next_run() {
        let app = app();
        let b = bot(&app.db, "codex");
        let original = routine(&app, &b.id, false);
        let interval = update(
            &app,
            Some(&b.id),
            &original.id,
            patch(json!({"interval_seconds":1200})),
        )
        .unwrap();
        assert!(interval.schedule.is_none());
        assert!(!interval.enabled);
        assert!(interval.next_run >= db::now() + 1199);
        let again = update(
            &app,
            Some(&b.id),
            &original.id,
            patch(json!({"interval_seconds":1200})),
        )
        .unwrap();
        assert_eq!(again.next_run, interval.next_run);
        let schedule = json!({"timezone":"America/New_York","days":[1,2,3,4,5],"start":"10:30","end":"10:30","every_minutes":60});
        let weekly = update(
            &app,
            Some(&b.id),
            &original.id,
            patch(json!({"schedule":schedule,"enabled":true})),
        )
        .unwrap();
        assert!(weekly.enabled);
        assert_eq!(
            weekly.schedule.as_ref().unwrap().timezone,
            "America/New_York"
        );
        assert_eq!(weekly.interval_seconds, 3600);
        let again = update(
            &app,
            Some(&b.id),
            &original.id,
            patch(json!({"schedule":schedule,"enabled":true})),
        )
        .unwrap();
        assert_eq!(again.next_run, weekly.next_run);
        assert!(
            update(
                &app,
                Some(&b.id),
                &original.id,
                patch(json!({"schedule":schedule,"interval_seconds":600}))
            )
            .is_err()
        );
        let mut invalid = schedule;
        invalid["timezone"] = json!("Not/A_Timezone");
        assert!(
            update(
                &app,
                Some(&b.id),
                &original.id,
                patch(json!({"schedule":invalid}))
            )
            .is_err()
        );
        assert_eq!(value(&app.db.routines().unwrap()[0]), value(&weekly));
    }

    #[tokio::test]
    async fn update_obeys_normal_approval_and_denial() {
        let app = app();
        let b = bot(&app.db, "codex");
        let original = routine(&app, &b.id, true);
        let id = app.db.queue(&b.id, "Edit routine", 0).unwrap();
        app.db.claim().unwrap();
        let run = app.db.run(&id).unwrap();
        for allowed in [false, true] {
            let work_app = app.clone();
            let work_bot = b.clone();
            let work_run = run.clone();
            let args = json!({"id":original.id,"prompt":"Approved new instructions"});
            let work = tokio::spawn(async move {
                runtime::call_tool(&work_app, &work_bot, &work_run, "routine_update", args)
                    .await
                    .unwrap()
            });
            let approval = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if let Some(a) = app.db.approvals().unwrap().first() {
                        break a["id"].as_str().unwrap().to_owned();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(app.db.routines().unwrap()[0].prompt, original.prompt);
            app.db.decide(&approval, allowed).unwrap();
            let result = work.await.unwrap();
            assert_eq!(result["failed"] == true, !allowed);
        }
        assert_eq!(
            app.db.routines().unwrap()[0].prompt,
            "Approved new instructions"
        );
        assert_eq!(app.db.routines().unwrap()[0].next_run, original.next_run);
    }

    #[tokio::test]
    async fn authenticated_patch_merges_current_fields_and_persists_after_reopen() {
        let dir = std::env::temp_dir().join(format!("kindred-routine-update-{}", db::id()));
        let file = dir.join("kindred.db");
        let mut app = app();
        std::sync::Arc::get_mut(&mut app).unwrap().db =
            db::Db::open(file.to_str().unwrap()).unwrap();
        let b = bot(&app.db, "codex");
        let original = routine(&app, &b.id, true);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/api/routines/{}",
            listener.local_addr().unwrap(),
            original.id
        );
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .patch(&base)
                .json(&json!({"prompt":"Denied"}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let mut concurrent = original.clone();
        concurrent.enabled = false;
        concurrent.next_run += 3600;
        app.db.save_routine(&concurrent).unwrap();
        let result = client
            .patch(&base)
            .bearer_auth(&app.token)
            .json(&json!({"prompt":"No emojis; one watch line.","expected_prompt":original.prompt}))
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 200);
        let saved: Routine = result.json().await.unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.next_run, concurrent.next_run);
        assert_eq!(saved.schedule, original.schedule);
        let response = client
            .patch(&base)
            .bearer_auth(&app.token)
            .json(&json!({"prompt":"Stale edit","expected_prompt":original.prompt}))
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
        server.abort();
        let _ = server.await;
        drop(app);
        let reopened = db::Db::open(file.to_str().unwrap()).unwrap();
        assert_eq!(value(&reopened.routines().unwrap()[0]), value(&saved));
    }
}
