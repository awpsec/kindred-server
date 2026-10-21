use crate::{
    db::{self, Bot, Db},
    initiative,
    tests::bot,
};
use chrono::{TimeZone, Utc};
use rusqlite::params;
use serde_json::json;

fn at(day: u32, hour: u32) -> i64 {
    Utc.with_ymd_and_hms(2026, 10, day, hour, 0, 0)
        .unwrap()
        .timestamp()
}
fn setup(time: i64) -> (Db, Bot) {
    let db = Db::open(":memory:").unwrap();
    db.save_setting("general", &json!({"timezone":"America/New_York"}))
        .unwrap();
    let mut b = bot(&db, "codex");
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    let c = db.0.lock().unwrap();
    c.execute(
        "INSERT OR IGNORE INTO chats(id,name,members) VALUES(?,?,?)",
        params![
            format!("dm-{}", b.id),
            b.name,
            serde_json::to_string(&vec![&b.id]).unwrap()
        ],
    )
    .unwrap();
    c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','Prepare the contract draft when useful','message',?)",params![format!("dm-{}",b.id),time]).unwrap();
    c.execute(
        "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
        [time],
    )
    .unwrap();
    drop(c);
    (db, b)
}
fn roots(db: &Db) -> Vec<String> {
    db.0.lock()
        .unwrap()
        .prepare("SELECT run_id FROM initiative_runs ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn finish(db: &Db) {
    for id in roots(db) {
        db.finish(&id, "completed", "", " ").unwrap();
    }
}

#[test]
fn initiative_default_off_and_profile_validation() {
    let old: db::BotProfile = serde_json::from_value(json!({"color":"#123456"})).unwrap();
    assert!(!old.initiative);
    assert!(old.initiative_instructions.is_empty());
    let db = Db::open(":memory:").unwrap();
    let mut b = bot(&db, "codex");
    db.save_setting("general", &json!({"timezone":"America/New_York"}))
        .unwrap();
    initiative::tick(&db, at(18, 14)).unwrap();
    assert!(roots(&db).is_empty());
    b.profile.initiative_instructions = "🦀".repeat(501);
    assert!(db.save_bot(&b).is_err());
    b.profile.initiative_instructions = "🦀".repeat(500);
    db.save_bot(&b).unwrap();
}
#[test]
fn initiative_cadence_coalesces_and_never_makes_routines() {
    let t = at(18, 14);
    let (db, _) = setup(t);
    initiative::tick(&db, t).unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
    finish(&db);
    initiative::tick(&db, t + 14399).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 2);
    finish(&db);
    initiative::tick(&db, t + 28800).unwrap();
    assert_eq!(roots(&db).len(), 2);
    initiative::tick(&db, at(19, 14)).unwrap();
    assert_eq!(roots(&db).len(), 3);
    let c = db.0.lock().unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM routines", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM routine_runs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn initiative_timezone_quiet_hours_dst_and_missing_timezone() {
    let t = at(18, 14);
    let (db, _) = setup(t);
    for value in [json!({}), json!({"timezone":"bad"})] {
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE settings SET value=? WHERE key='general'",
                [value.to_string()],
            )
            .unwrap();
        initiative::tick(&db, t).unwrap();
        assert!(roots(&db).is_empty());
    }
    db.save_setting("general", &json!({"timezone":"America/New_York"}))
        .unwrap();
    initiative::tick(&db, at(19, 11)).unwrap();
    assert!(roots(&db).is_empty()); // 07:00
    initiative::tick(&db, at(19, 12)).unwrap();
    assert_eq!(roots(&db).len(), 1);
    finish(&db);
    initiative::tick(&db, at(20, 0)).unwrap();
    assert_eq!(roots(&db).len(), 1); // 20:00
    let winter = Utc
        .with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
        .unwrap()
        .timestamp();
    db.0.lock()
        .unwrap()
        .execute("UPDATE initiative_state SET notes='track order'", [])
        .unwrap();
    initiative::tick(&db, winter).unwrap();
    assert_eq!(roots(&db).len(), 1); // 07:00 after DST
    initiative::tick(&db, winter + 3600).unwrap();
    assert_eq!(roots(&db).len(), 2); // 08:00
}
#[test]
fn initiative_requires_owner_history_and_recent_or_saved_followup() {
    let t = at(18, 14);
    let (db, _) = setup(t);
    db.0.lock()
        .unwrap()
        .execute("UPDATE chat_messages SET created=1", [])
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.0.lock()
        .unwrap()
        .execute("UPDATE initiative_state SET notes='track order'", [])
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
    finish(&db);
    db.0.lock()
        .unwrap()
        .execute("DELETE FROM chat_messages", [])
        .unwrap();
    initiative::tick(&db, at(19, 14)).unwrap();
    assert_eq!(roots(&db).len(), 1);
}
#[test]
fn initiative_defers_to_owner_and_commands() {
    let t = at(18, 14);
    let (db, b) = setup(t);
    let user = db.queue(&b.id, "Urgent owner work", 0).unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.finish(&user, "completed", "", " ").unwrap();
    db.0.lock().unwrap().execute("INSERT INTO command_jobs(id,run_id,bot_id,device_id,screen,title,created) VALUES('job',?,?,'',1,'Still working',?)",params![user,b.id,t]).unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.0.lock()
        .unwrap()
        .execute("UPDATE command_jobs SET status='completed'", [])
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
    let later = db.queue(&b.id, "User goes first", 0).unwrap();
    assert_eq!(db.claim_bot(&b.id).unwrap().unwrap().id, later);
}
#[test]
fn initiative_disable_cancels_only_owned_tree() {
    let t = at(18, 14);
    let (db, mut b) = setup(t);
    initiative::tick(&db, t).unwrap();
    let root = roots(&db)[0].clone();
    db.0.lock()
        .unwrap()
        .execute("UPDATE runs SET status='running' WHERE id=?", [&root])
        .unwrap();
    let child_bot = bot(&db, "codex");
    let child = db.queue(&child_bot.id, "Assigned research", 0).unwrap();
    let independent = db.queue(&b.id, "Owner work", 0).unwrap();
    db.0.lock().unwrap().execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![child,root,format!("dm-{}",b.id)]).unwrap();
    assert!(initiative::is_run(&db.0.lock().unwrap(), &child).unwrap());
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    assert_eq!(db.run(&root).unwrap().status, "cancelling");
    assert_eq!(db.run(&child).unwrap().status, "cancelled");
    assert_eq!(db.run(&independent).unwrap().status, "queued");
    assert!(!initiative::active(&db.0.lock().unwrap(), &root).unwrap());
    assert!(initiative::active(&db.0.lock().unwrap(), &independent).unwrap());
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    assert!(!initiative::active(&db.0.lock().unwrap(), &root).unwrap());
}
#[test]
fn initiative_checkpoint_scope_and_quiet_are_per_run() {
    let t = at(18, 14);
    let (db, mut b) = setup(t);
    initiative::tick(&db, t).unwrap();
    let root = roots(&db)[0].clone();
    db.0.lock()
        .unwrap()
        .execute("UPDATE runs SET status='running' WHERE id=?", [&root])
        .unwrap();
    let r = db.run(&root).unwrap();
    assert!(initiative::checkpoint(&db, &r, &json!({"notes":"watch tracking"})).is_ok());
    assert!(initiative::checkpoint(&db, &r, &json!({"notes":"x","initiative":true})).is_err());
    assert!(initiative::checkpoint(&db, &r, &json!({"notes":"x".repeat(8001)})).is_err());
    assert!(
        initiative::checkpoint(&db, &r, &json!({"notes":"","next_check":db::now()+1})).is_err()
    );
    initiative::checkpoint(&db, &r, &json!({"notes":""})).unwrap();
    assert_eq!(initiative::context(&db, &r).unwrap()["notes"], "");
    assert!(
        !initiative::context(&db, &r).unwrap()["recent_dm_messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let child_bot = bot(&db, "codex");
    let child = db.queue(&child_bot.id, "Research", 0).unwrap();
    {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![child,root,r.chat_id]).unwrap();
        c.execute("UPDATE runs SET status='running' WHERE id=?", [&child])
            .unwrap();
        initiative::mark_quiet(&c, &child).unwrap();
        assert!(initiative::is_quiet(&c, &child).unwrap());
        assert!(!initiative::is_quiet(&c, &root).unwrap());
    }
    let child_context = initiative::context(&db, &db.run(&child).unwrap()).unwrap();
    assert_eq!(child_context["is_initiative"], true);
    assert!(child_context.get("notes").is_none());
    assert!(child_context.get("recent_dm_messages").is_none());
    assert!(
        initiative::checkpoint(
            &db,
            &db.run(&child).unwrap(),
            &json!({"notes":"Child overwrite"})
        )
        .is_err()
    );
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    assert!(initiative::checkpoint(&db, &r, &json!({"notes":"late"})).is_err());
}

#[test]
fn initiative_frozen_archived_and_reenable_budget() {
    let t = at(18, 14);
    let (db, mut b) = setup(t);
    db.0.lock()
        .unwrap()
        .execute(
            "INSERT INTO workspace_transfers(id,state,package) VALUES('frozen','prepared','{}')",
            [],
        )
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.0.lock()
        .unwrap()
        .execute("DELETE FROM workspace_transfers", [])
        .unwrap();
    b.profile.archived = true;
    db.save_bot(&b).unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    b.profile.archived = false;
    db.save_bot(&b).unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
            [t],
        )
        .unwrap();
    initiative::tick(&db, t).unwrap();
    finish(&db);
    initiative::tick(&db, t + 14400).unwrap();
    finish(&db);
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
            [t + 28800],
        )
        .unwrap();
    initiative::tick(&db, t + 28800).unwrap();
    assert_eq!(roots(&db).len(), 2);
}
#[test]
fn initiative_restart_keeps_queue_cadence_and_quiet_history() {
    let path = std::env::temp_dir().join(format!("initiative-{}.db", db::id()));
    let path = path.to_str().unwrap();
    let t = at(18, 14);
    let b;
    {
        let db = Db::open(path).unwrap();
        db.save_setting("general", &json!({"timezone":"America/New_York"}))
            .unwrap();
        b = bot(&db, "codex");
        let mut enabled = b.clone();
        enabled.profile.initiative = true;
        db.save_bot(&enabled).unwrap();
        let user = db.queue(&b.id, "Draft a contract", 0).unwrap();
        db.finish(&user, "completed", "done", "").unwrap();
        {
            let c = db.0.lock().unwrap();
            c.execute(
                "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
                [t],
            )
            .unwrap();
            c.execute("UPDATE chat_messages SET created=?", [t])
                .unwrap();
        }
        initiative::tick(&db, t).unwrap();
        assert_eq!(roots(&db).len(), 1);
    }
    {
        let db = Db::open(path).unwrap();
        initiative::tick(&db, t + 1).unwrap();
        assert_eq!(roots(&db).len(), 1);
        let root = roots(&db)[0].clone();
        initiative::mark_quiet(&db.0.lock().unwrap(), &root).unwrap();
        db.finish(&root, "completed", "Nothing changed", "")
            .unwrap();
    }
    {
        let db = Db::open(path).unwrap();
        let root = roots(&db)[0].clone();
        assert!(initiative::is_quiet(&db.0.lock().unwrap(), &root).unwrap());
        let c = db.0.lock().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM chat_messages WHERE run_id=? AND kind='result'",
                [root],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

#[test]
fn initiative_first_check_delay_and_existing_routine_is_unchanged() {
    let t = at(18, 14);
    let (db, mut b) = setup(t);
    {
        let c = db.0.lock().unwrap();
        c.execute("DELETE FROM initiative_state", []).unwrap();
        initiative::preferences_changed(&c, &b, false, t).unwrap();
    }
    let routine = db::Routine {
        id: "existing-routine".into(),
        bot_id: b.id.clone(),
        name: "Inbox check".into(),
        prompt: "Read inbox".into(),
        interval_seconds: 3600,
        next_run: t,
        enabled: true,
        schedule: None,
        run_at: None,
    };
    db.save_routine(&routine).unwrap();
    initiative::tick(&db, t + 299).unwrap();
    assert!(roots(&db).is_empty());
    initiative::tick(&db, t + 300).unwrap();
    assert_eq!(roots(&db).len(), 1);
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    assert_eq!(db.run(&roots(&db)[0]).unwrap().status, "cancelled");
    db.tick(t + 301).unwrap();
    let c = db.0.lock().unwrap();
    assert!(
        c.query_row(
            "SELECT enabled FROM routines WHERE id='existing-routine'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(c.query_row("SELECT count(*) FROM routine_runs rr JOIN runs r ON r.id=rr.run_id WHERE r.status='queued'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
}

#[test]
fn initiative_expired_queue_finishes_quietly_without_changing_owner_queue_or_budget() {
    let t = db::now();
    let (db, b) = setup(at(18, 14));
    initiative::tick(&db, at(18, 14)).unwrap();
    let root = roots(&db)[0].clone();
    let owner = db.queue(&b.id, "Owner work still has priority", 0).unwrap();
    {
        let c = db.0.lock().unwrap();
        c.execute(
            "UPDATE runs SET created=? WHERE id=?",
            params![t - 900, root],
        )
        .unwrap();
    }
    let claimed = db.claim_bot(&b.id).unwrap().unwrap();
    assert_eq!(claimed.id, owner);
    let expired = db.run(&root).unwrap();
    assert_eq!(expired.status, "completed");
    assert_eq!(expired.output, "");
    assert_eq!(expired.error, "");
    let c = db.0.lock().unwrap();
    assert!(initiative::is_quiet(&c, &root).unwrap());
    assert_eq!(
        c.query_row(
            "SELECT checks_today FROM initiative_state WHERE bot_id=?",
            [&b.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM events WHERE run_id=? AND kind='run_finished'",
            [&root],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM chat_messages WHERE run_id=?",
            [&root],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn initiative_child_keeps_root_but_explicit_question_reply_is_direct_work() {
    let t = at(18, 14);
    let (db, mut b) = setup(t);
    initiative::tick(&db, t).unwrap();
    let root = roots(&db)[0].clone();
    let helper = bot(&db, "codex");
    let child = db.queue(&helper.id, "Assigned research", 0).unwrap();
    let answer = db.queue(&b.id, "Owner explicitly answered", 0).unwrap();
    let command = db.queue(&b.id, "Command continuation fixture", 0).unwrap();
    {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![child,root,format!("dm-{}",b.id)]).unwrap();
        c.execute("INSERT INTO questions(id,run_id,chat_id,bot_id,topic_key,question,context,options,status,answer,continuation_run_id,created) VALUES('question',?,?,?,'topic','Which?','','[]','answered','Proceed',?,?)",params![root,format!("dm-{}",b.id),b.id,answer,t]).unwrap();
        c.execute(
            "INSERT INTO command_waits(run_id,ids,context,continuation) VALUES(?,'[]','',?)",
            params![root, command],
        )
        .unwrap();
        assert_eq!(initiative::root(&c, &command).unwrap(), Some(root.clone()));
        assert_eq!(initiative::root(&c, &child).unwrap(), Some(root.clone()));
        assert!(initiative::root(&c, &answer).unwrap().is_none());
        // The lineage's deadline remains anchored to the original run, not the child's creation.
        let owner_created: i64 = c
            .query_row(
                "SELECT created FROM runs WHERE id=?",
                [initiative::root(&c, &child).unwrap().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner_created + 900, t + 900);
    }
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    // Opt-out preserves explicit owner follow-up while stopping background descendants.
    assert_eq!(db.run(&answer).unwrap().status, "queued");
    assert_eq!(db.run(&child).unwrap().status, "cancelled");
    assert_eq!(db.run(&command).unwrap().status, "cancelled");
    // Explicit Stop still cancels the whole dependency tree as before.
    db.cancel(&root).unwrap();
    assert_eq!(db.run(&answer).unwrap().status, "cancelled");
}

#[test]
fn initiative_helper_intermediate_assistant_does_not_resurrect_after_restart() {
    let file = std::env::temp_dir().join(format!("initiative-helper-{}.db", db::id()));
    let path = file.to_str().unwrap();
    let child;
    {
        let db = Db::open(path).unwrap();
        let mut owner = bot(&db, "codex");
        owner.profile.initiative = true;
        db.save_bot(&owner).unwrap();
        let root = db.queue(&owner.id, "Background check fixture", 0).unwrap();
        let helper = bot(&db, "codex");
        child = db.queue(&helper.id, "Assigned work", 0).unwrap();
        {
            let c = db.0.lock().unwrap();
            c.execute(
                "INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",
                params![root, owner.id],
            )
            .unwrap();
            c.execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![child,root,format!("dm-{}",owner.id)]).unwrap();
        }
        db.event(
            &child,
            "assistant",
            json!({"text":"Nothing useful changed; background-only thought"}),
        )
        .unwrap();
        {
            let c = db.0.lock().unwrap();
            initiative::mark_quiet(&c, &child).unwrap();
        }
        db.finish(&child, "completed", "", "").unwrap();
    }
    {
        let db = Db::open(path).unwrap();
        let c = db.0.lock().unwrap();
        assert_eq!(c.query_row("SELECT count(*) FROM chat_messages WHERE run_id=? AND kind='assistant' AND suppressed=0",[child],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

fn daytime_now_zone() -> &'static str {
    use chrono::Timelike;
    match Utc.timestamp_opt(db::now(), 0).single().unwrap().hour() {
        0..=7 => "Etc/GMT-9",
        8..=19 => "UTC",
        _ => "Etc/GMT+8",
    }
}
fn ready_now() -> (Db, Bot, i64) {
    let t = db::now();
    let (db, b) = setup(t);
    db.save_setting("general", &json!({"timezone":daytime_now_zone()}))
        .unwrap();
    (db, b, t)
}

#[test]
fn initiative_reliability_concurrent_tick_and_claim_starts_one_check() {
    use std::sync::{Arc, Barrier, Mutex};
    let (db, b, t) = ready_now();
    let db = Arc::new(db);
    let gate = Arc::new(Barrier::new(24));
    let claimed = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for worker in 0..24 {
            let db = db.clone();
            let gate = gate.clone();
            let claimed = claimed.clone();
            let bot = b.id.clone();
            scope.spawn(move || {
                gate.wait();
                for _ in 0..12 {
                    if worker % 3 == 0 {
                        initiative::tick(&db, t).unwrap();
                    } else if let Some(run) = db.claim_bot(&bot).unwrap() {
                        claimed.lock().unwrap().push(run.id);
                    }
                }
            });
        }
    });
    if let Some(run) = db.claim_bot(&b.id).unwrap() {
        claimed.lock().unwrap().push(run.id);
    }
    assert_eq!(roots(&db).len(), 1);
    assert_eq!(claimed.lock().unwrap().len(), 1);
    let c = db.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM runs WHERE status='running'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT checks_today FROM initiative_state WHERE bot_id=?",
            [&b.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn initiative_reliability_timezone_changes_cannot_reopen_spent_day() {
    let t = at(18, 16);
    let (db, _) = setup(t);
    db.save_setting("general", &json!({"timezone":"UTC"}))
        .unwrap();
    initiative::tick(&db, t).unwrap();
    finish(&db);
    db.save_setting("general", &json!({"timezone":"Pacific/Kiritimati"}))
        .unwrap();
    initiative::tick(&db, t + 14400).unwrap();
    finish(&db);
    assert_eq!(roots(&db).len(), 2);
    // Both prior checks now fall on October 18 in UTC-12. Changing the label
    // from October 19 back to October 18 must not create a third check that day.
    db.save_setting("general", &json!({"timezone":"Etc/GMT+12"}))
        .unwrap();
    initiative::tick(&db, t + 28800).unwrap();
    assert_eq!(roots(&db).len(), 2);
}

#[test]
fn initiative_reliability_clock_rollback_downtime_and_toggle_storm_keep_budget() {
    // save_bot uses the real clock when re-enabling. Keep the simulated checks
    // ahead of it instead of letting this regression expire after October 20.
    let t = (db::now() / 86400 + 1) * 86400 + 14 * 3600;
    let (db, mut b) = setup(t);
    initiative::tick(&db, t).unwrap();
    for _ in 0..32 {
        b.profile.initiative = false;
        db.save_bot(&b).unwrap();
        b.profile.initiative = true;
        db.save_bot(&b).unwrap();
    }
    initiative::tick(&db, t - 3600).unwrap();
    initiative::tick(&db, t + 14399).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 2);
    finish(&db);
    for _ in 0..32 {
        initiative::tick(&db, t + 28800).unwrap();
    }
    assert_eq!(roots(&db).len(), 2);
    // Missed days coalesce into one check, never replay every missed interval.
    initiative::tick(&db, t + 3 * 86400).unwrap();
    initiative::tick(&db, t + 3 * 86400).unwrap();
    assert_eq!(roots(&db).len(), 3);
}

#[test]
fn initiative_reliability_accounts_and_bots_keep_separate_permissions_and_notes() {
    let (first, mut owner, t) = ready_now();
    owner.profile.initiative_instructions = "First account only".into();
    first.save_bot(&owner).unwrap();
    let other = bot(&first, "codex");
    let inactive = bot(&first, "codex");
    let mut no_history = other.clone();
    no_history.profile.initiative = true;
    first.save_bot(&no_history).unwrap();
    first
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
            [t],
        )
        .unwrap();
    initiative::tick(&first, t).unwrap();
    assert_eq!(roots(&first).len(), 1);
    assert!(!first.bot(&inactive.id).unwrap().profile.initiative);
    let second = Db::open(":memory:").unwrap();
    second
        .save_setting("general", &json!({"timezone":daytime_now_zone()}))
        .unwrap();
    // Same bot identifier in a different account is still a separate namespace.
    let mut copy = owner.clone();
    copy.profile.initiative_instructions = "Second account only".into();
    second.save_bot(&copy).unwrap();
    let seed = second
        .queue(&copy.id, "My separate account request", 0)
        .unwrap();
    second.finish(&seed, "completed", "", "").unwrap();
    second
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
            [t],
        )
        .unwrap();
    initiative::tick(&second, t).unwrap();
    let r1 = first.claim_bot(&owner.id).unwrap().unwrap();
    let r2 = second.claim_bot(&copy.id).unwrap().unwrap();
    initiative::checkpoint(&first, &r1, &json!({"notes":"Private first account note"})).unwrap();
    initiative::checkpoint(
        &second,
        &r2,
        &json!({"notes":"Private second account note"}),
    )
    .unwrap();
    assert_eq!(
        initiative::context(&first, &r1).unwrap()["notes"],
        "Private first account note"
    );
    assert_eq!(
        initiative::context(&second, &r2).unwrap()["notes"],
        "Private second account note"
    );
    assert!(initiative::checkpoint(&first, &r2, &json!({"notes":"Wrong account"})).is_err());
    assert!(initiative::context(&first, &r2).unwrap().is_null());
    owner.profile.initiative = false;
    first.save_bot(&owner).unwrap();
    assert!(initiative::active(&second.0.lock().unwrap(), &r2.id).unwrap());
}

#[test]
fn initiative_reliability_late_descendant_after_disable_cannot_start() {
    let (db, mut b, t) = ready_now();
    initiative::tick(&db, t).unwrap();
    let root = roots(&db)[0].clone();
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    let helper = bot(&db, "codex");
    let late = db
        .queue(&helper.id, "Late delegated response fixture", 0)
        .unwrap();
    let user = db.queue(&helper.id, "Unrelated owner task", 0).unwrap();
    db.0.lock().unwrap().execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![late,root,format!("dm-{}",b.id)]).unwrap();
    // Ordinary owner work takes priority over queued automatic descendants.
    assert_eq!(db.claim_bot(&helper.id).unwrap().unwrap().id, user);
    assert_eq!(db.run(&late).unwrap().status, "queued");
    db.finish(&user, "completed", "Owner task finished", "").unwrap();
    assert!(db.claim_bot(&helper.id).unwrap().is_none());
    assert_eq!(db.run(&late).unwrap().status, "cancelled");
}

#[test]
fn initiative_reliability_all_active_statuses_and_unfinished_waits_defer() {
    for status in [
        "queued",
        "running",
        "awaiting_user",
        "awaiting_approval",
        "cancelling",
    ] {
        let (db, b, t) = ready_now();
        let user = db.queue(&b.id, "Owner task", 0).unwrap();
        db.0.lock()
            .unwrap()
            .execute("UPDATE runs SET status=? WHERE id=?", params![status, user])
            .unwrap();
        initiative::tick(&db, t).unwrap();
        assert!(roots(&db).is_empty(), "scheduled while {status}");
    }
    let (db, b, t) = ready_now();
    let user = db.queue(&b.id, "Command task", 0).unwrap();
    db.finish(&user, "completed", "", "").unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "INSERT INTO command_waits(run_id,ids,context) VALUES(?,'[]','Waiting')",
            [&user],
        )
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE command_waits SET continuation='finished' WHERE run_id=?",
            [&user],
        )
        .unwrap();
    let helper = bot(&db, "codex");
    let child = db.queue(&helper.id, "Research", 0).unwrap();
    db.finish(&child, "completed", "done", "").unwrap();
    db.0.lock().unwrap().execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![child,user,format!("dm-{}",b.id)]).unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    db.0.lock()
        .unwrap()
        .execute("UPDATE collaboration_requests SET resolved=1", [])
        .unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
}

#[test]
fn initiative_reliability_checkpoint_reports_actual_deferred_schedule_and_rejects_stale_write() {
    let (db, mut b, t) = ready_now();
    initiative::tick(&db, t).unwrap();
    let run = db.claim_bot(&b.id).unwrap().unwrap();
    let later = t + 7 * 86400;
    let first = initiative::checkpoint(
        &db,
        &run,
        &json!({"notes":"Check next week","next_check":later}),
    )
    .unwrap();
    assert_eq!(first["next_check_not_before"], later);
    let second = initiative::checkpoint(&db, &run, &json!({"notes":"Updated details"})).unwrap();
    assert_eq!(second["next_check_not_before"], later);
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    assert!(initiative::checkpoint(&db, &run, &json!({"notes":"Late result"})).is_err());
    assert_eq!(
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT notes FROM initiative_state WHERE bot_id=?",
                [&b.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "Updated details"
    );
}

#[test]
fn initiative_reliability_checkpoint_disable_race_is_atomic() {
    use std::sync::{Arc, Barrier};
    let (db, mut b, t) = ready_now();
    initiative::tick(&db, t).unwrap();
    let run = db.claim_bot(&b.id).unwrap().unwrap();
    let db = Arc::new(db);
    let gate = Arc::new(Barrier::new(2));
    b.profile.initiative = false;
    std::thread::scope(|scope| {
        let writer = db.clone();
        let first = gate.clone();
        let r = run.clone();
        scope.spawn(move || {
            first.wait();
            let _ = initiative::checkpoint(&writer, &r, &json!({"notes":"Concurrent saved note"}));
        });
        let off = db.clone();
        let second = gate.clone();
        scope.spawn(move || {
            second.wait();
            off.save_bot(&b).unwrap();
        });
    });
    assert!(!db.bot(&run.bot_id).unwrap().profile.initiative);
    assert!(initiative::checkpoint(&db, &run, &json!({"notes":"Must not win after off"})).is_err());
    let notes: String =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT notes FROM initiative_state WHERE bot_id=?",
                [&run.bot_id],
                |r| r.get(0),
            )
            .unwrap();
    assert!(notes.is_empty() || notes == "Concurrent saved note");
}

#[test]
fn initiative_reliability_real_file_restart_preserves_deferred_notes_and_single_writer() {
    let path = std::env::temp_dir().join(format!("initiative-durable-{}.db", db::id()));
    let path = path.to_str().unwrap();
    let t = db::now();
    let owner;
    let root;
    let later = t + 2 * 86400;
    {
        let db = Db::open(path).unwrap();
        db.save_setting("general", &json!({"timezone":daytime_now_zone()}))
            .unwrap();
        let mut b = bot(&db, "codex");
        b.profile.initiative = true;
        db.save_bot(&b).unwrap();
        owner = b.id.clone();
        let seed = db.queue(&b.id, "Track my order", 0).unwrap();
        db.finish(&seed, "completed", "", "").unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE initiative_state SET next_check=?1,enabled_after=?1",
                [t],
            )
            .unwrap();
        initiative::tick(&db, t).unwrap();
        let run = db.claim_bot(&owner).unwrap().unwrap();
        root = run.id.clone();
        initiative::checkpoint(
            &db,
            &run,
            &json!({"notes":"Order in transit; report only a changed ETA","next_check":later}),
        )
        .unwrap();
        assert!(Db::open(path).is_err());
    }
    {
        let db = Db::open(path).unwrap();
        assert_eq!(db.run(&root).unwrap().status, "interrupted");
        for _ in 0..20 {
            initiative::tick(&db, t + 10).unwrap();
        }
        assert_eq!(roots(&db), vec![root]);
        let c = db.0.lock().unwrap();
        let saved: (String, i64, i64) = c
            .query_row(
                "SELECT notes,next_check,checks_today FROM initiative_state WHERE bot_id=?",
                [owner],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            saved,
            (
                "Order in transit; report only a changed ETA".into(),
                later,
                1
            )
        );
    }
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

#[test]
fn initiative_reliability_expired_descendants_never_claim_and_recovery_settles() {
    let (db, b, t) = ready_now();
    initiative::tick(&db, t).unwrap();
    let root = db.claim_bot(&b.id).unwrap().unwrap();
    let helper = bot(&db, "codex");
    let child = db
        .chat_handoff(&root, &helper.id, "Prepare supporting research")
        .unwrap();
    db.finish(&root.id, "completed", "Draft progress already saved", "")
        .unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE runs SET created=? WHERE id=?",
            params![t - 901, root.id],
        )
        .unwrap();
    let c = db.0.lock().unwrap();
    assert_eq!(
        initiative::remaining_seconds(&c, &child, t).unwrap(),
        Some(0)
    );
    assert_eq!(
        initiative::remaining_seconds(&c, &child, t - 901).unwrap(),
        Some(900)
    );
    drop(c);
    assert!(db.claim_bot(&helper.id).unwrap().is_none());
    let expired = db.run(&child).unwrap();
    assert_eq!(expired.status, "interrupted");
    assert!(expired.error.contains("15-minute budget"));
    assert_eq!(
        db.run(&root.id).unwrap().output,
        "Draft progress already saved"
    );
    for _ in 0..4 {
        db.deliver_pending_completions().unwrap();
        assert!(db.claim_bot(&b.id).unwrap().is_none());
        assert!(db.claim_bot(&helper.id).unwrap().is_none());
    }
    assert!(db.queued_bots().unwrap().is_empty());
    let c = db.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM events WHERE run_id=? AND kind='run_finished'",
            [child],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM runs WHERE status='running'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn initiative_reliability_new_owner_topic_overrides_distant_defer_only_after_interval() {
    let t = at(20, 14);
    let (db, b) = setup(t);
    initiative::tick(&db, t).unwrap();
    let root = roots(&db)[0].clone();
    let later = t + 30 * 86400;
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET notes='Order arrives next month',next_check=?",
            [later],
        )
        .unwrap();
    finish(&db);
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(
        roots(&db).len(),
        1,
        "no new owner activity must honor deferred watch"
    );
    let message = db
        .queue(
            &b.id,
            "Separate topic: help me prepare tomorrow's contract",
            0,
        )
        .unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET created=? WHERE sender='user'",
            [t],
        )
        .unwrap(); // Same-second ordering is captured by seq, not timestamps.
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 1, "owner work remains higher priority");
    db.finish(&message, "completed", "Conversation finished", "")
        .unwrap();
    initiative::tick(&db, t + 14399).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 2);
    assert_ne!(roots(&db)[1], root);
    finish(&db);
    initiative::tick(&db, t + 28800).unwrap();
    assert_eq!(roots(&db).len(), 2, "new topic does not bypass daily spend");
}

#[test]
fn initiative_reliability_reenable_reopens_distant_defer_but_keeps_cooldown_and_spend() {
    let t = at(20, 14);
    let (db, mut b) = setup(t);
    initiative::tick(&db, t).unwrap();
    finish(&db);
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET notes='Keep this watch',next_check=?",
            [t + 30 * 86400],
        )
        .unwrap();
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    let enabled = t + 14400;
    {
        let c = db.0.lock().unwrap();
        initiative::preferences_changed(&c, &b, false, enabled).unwrap();
    }
    let message = db.queue(&b.id, "A new topic after enabling", 0).unwrap();
    db.finish(&message, "completed", "", "").unwrap();
    initiative::tick(&db, enabled + 299).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, enabled + 300).unwrap();
    assert_eq!(roots(&db).len(), 2);
    let c = db.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT notes FROM initiative_state WHERE bot_id=?",
            [&b.id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Keep this watch"
    );
    assert_eq!(
        c.query_row(
            "SELECT checks_today FROM initiative_state WHERE bot_id=?",
            [&b.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn initiative_reliability_legacy_state_migration_preserves_defer_notes_and_spend() {
    let t = at(20, 14);
    let (db, b) = setup(t);
    initiative::tick(&db, t).unwrap();
    finish(&db);
    let later = t + 30 * 86400;
    {
        let c = db.0.lock().unwrap();
        c.execute_batch("ALTER TABLE initiative_state RENAME TO old_initiative_state;CREATE TABLE initiative_state(bot_id TEXT PRIMARY KEY REFERENCES bots(id),next_check INTEGER NOT NULL,last_day TEXT NOT NULL DEFAULT '',checks_today INTEGER NOT NULL DEFAULT 0,notes TEXT NOT NULL DEFAULT '');INSERT INTO initiative_state SELECT bot_id,next_check,last_day,checks_today,notes FROM old_initiative_state;DROP TABLE old_initiative_state;").unwrap();
        c.execute(
            "UPDATE initiative_state SET next_check=?,notes='Saved order watch'",
            [later],
        )
        .unwrap();
        initiative::init(&c).unwrap();
        initiative::init(&c).unwrap();
        let saved:(i64,i64,String,i64)=c.query_row("SELECT next_check,checks_today,notes,last_owner_seq FROM initiative_state WHERE bot_id=?",[&b.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            (saved.0, saved.1, saved.2),
            (later, 1, "Saved order watch".into())
        );
        assert!(saved.3 > 0);
    }
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 1);
    let message = db
        .queue(&b.id, "New conversation after migration", 0)
        .unwrap();
    db.finish(&message, "completed", "", "").unwrap();
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 2);
    assert!(db.bot(&b.id).unwrap().profile.initiative);
}

#[test]
fn initiative_reliability_large_ordinary_queue_is_unchanged_by_unrelated_expiry() {
    for history in [false, true] {
        let (db, _, t) = ready_now();
        if history {
            initiative::tick(&db, t).unwrap();
            finish(&db);
        }
        let ordinary = bot(&db, "codex");
        let first = db.queue(&ordinary.id, "First owner request", 0).unwrap();
        let start = db.run(&first).unwrap().created;
        {
            let c = db.0.lock().unwrap();
            let mut insert=c.prepare("INSERT INTO runs(id,bot_id,prompt,status,created,chat_id,round_id) VALUES(?,?,'Ordinary owner queue','queued',?,?,?)").unwrap();
            for index in 1..1000 {
                let id = db::id();
                insert
                    .execute(params![
                        id,
                        ordinary.id,
                        start + index,
                        format!("dm-{}", ordinary.id),
                        id
                    ])
                    .unwrap();
            }
        }
        let before = std::time::Instant::now();
        assert_eq!(db.claim_bot(&ordinary.id).unwrap().unwrap().id, first);
        eprintln!(
            "ordinary 1000-row claim, initiative history={history}: {:?}",
            before.elapsed()
        );
        let c = db.0.lock().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM runs WHERE bot_id=? AND status='queued'",
                [&ordinary.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            999
        );
        assert_eq!(c.query_row("SELECT count(*) FROM events e JOIN runs r ON r.id=e.run_id WHERE r.bot_id=? AND e.kind IN ('initiative_quiet','run_finished')",[ordinary.id],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
}

// Exercise the real delegation and quiet-completion APIs, not a synthetic edge.
async fn initiative_quiet_handoff_roundtrip(
    handoff: fn(&Db, &db::Run, &str, &str) -> anyhow::Result<String>,
    initiative: bool,
    useful: bool,
) {
    let path = std::env::temp_dir().join(format!("initiative-handoff-{}.db", db::id()));
    let database = path.to_str().unwrap();
    let owner_id;
    let root_id;
    let child_id;
    let source_chat;
    {
        let mut app = crate::tests::app();
        std::sync::Arc::get_mut(&mut app).unwrap().db = Db::open(database).unwrap();
        let mut owner = bot(&app.db, "codex");
        owner.profile.initiative = initiative;
        app.db.save_bot(&owner).unwrap();
        let helper = bot(&app.db, "codex");
        owner_id = owner.id.clone();
        root_id = app.db.queue(&owner.id, "Check an existing topic", 0).unwrap();
        if initiative {
            app.db.0.lock().unwrap().execute(
                "INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",
                params![root_id, owner.id],
            ).unwrap();
        }
        let root = app.db.claim_bot(&owner.id).unwrap().unwrap();
        source_chat = root.chat_id.clone();
        child_id = handoff(&app.db, &root, &helper.id, "Research the existing topic").unwrap();
        let child = app.db.claim_bot(&helper.id).unwrap().unwrap();
        assert_eq!(child.id, child_id);
        assert_eq!(initiative::root(&app.db.0.lock().unwrap(), &child_id).unwrap().is_some(), initiative);
        assert_eq!(app.db.collaboration_waits(&source_chat).unwrap().len(), 1);
        if initiative && !useful {
            let outcome = crate::runtime::call_tool(&app, &helper, &child, "finish_quietly", json!({})).await.unwrap();
            assert_eq!(outcome["finish_quietly"], true);
        }
        app.db.finish(&child_id, "completed", if useful { "Verified research result" } else { "" }, "").unwrap();
        app.db.chat_complete(&child).unwrap();
        if initiative && !useful {
            let outcome = crate::runtime::call_tool(&app, &owner, &root, "finish_quietly", json!({})).await.unwrap();
            assert_eq!(outcome["finish_quietly"], true);
        }
        app.db.finish(&root_id, "completed", if useful { "One useful owner update" } else { "" }, "").unwrap();
        app.db.chat_complete(&root).unwrap();
        app.db.chat_complete(&root).unwrap();
        // Delegation/request history is preserved even when its UI link is quiet.
        let c = app.db.0.lock().unwrap();
        let stored: (i64, bool) = c.query_row(
            "SELECT count(*),COALESCE(max(suppressed),0) FROM chat_messages WHERE chat_id=? AND kind='collaboration' AND run_id=?",
            params![source_chat, root_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        assert_eq!(stored.0, 1);
        assert_eq!(c.query_row("SELECT count(*) FROM collaboration_requests WHERE child_run_id=? AND parent_run_id=?",params![child_id,root_id],|r|r.get::<_,i64>(0)).unwrap(),1);
        drop(c);
    }
    // Restart twice: startup projections must not resurrect background links.
    for _ in 0..2 {
        let db = Db::open(database).unwrap();
        assert_eq!(initiative::root(&db.0.lock().unwrap(), &child_id).unwrap().is_some(), initiative);
        let visible = db.chat_messages(&source_chat).unwrap();
        assert_eq!(visible.iter().filter(|m| m["kind"] == "collaboration").count(), usize::from(!initiative));
        if initiative && !useful { assert!(visible.iter().all(|m|m["sender"]=="user"), "Quiet handoff left a visible owner-DM message: {visible:?}"); }
        if useful { assert_eq!(visible.iter().filter(|m|m["text"]=="One useful owner update").count(),1); }
        assert_eq!(db.bot(&owner_id).unwrap().profile.initiative,initiative);
    }
    for suffix in ["", ".lock", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{database}{suffix}")); }
}

#[tokio::test]
async fn initiative_quiet_handoff_actual_api_survives_restart() {
    initiative_quiet_handoff_roundtrip(Db::chat_handoff, true, false).await;
}
#[tokio::test]
async fn initiative_quiet_handoff_ordinary_api_link_stays_visible() {
    initiative_quiet_handoff_roundtrip(Db::chat_handoff, false, false).await;
}
#[tokio::test]
async fn initiative_quiet_handoff_useful_final_still_delivers_once() {
    initiative_quiet_handoff_roundtrip(Db::chat_handoff, true, true).await;
}

#[tokio::test]
async fn initiative_quiet_handoff_later_ordinary_same_pair_has_one_visible_link() {
    let app = crate::tests::app();
    let mut owner = bot(&app.db, "codex");
    owner.profile.initiative = true;
    app.db.save_bot(&owner).unwrap();
    let helper = bot(&app.db, "codex");
    let root = app.db.queue(&owner.id, "Background check", 0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![root,owner.id]).unwrap();
    let background = app.db.claim_bot(&owner.id).unwrap().unwrap();
    let first = app.db.chat_handoff(&background, &helper.id, "Check the topic").unwrap();
    let child = app.db.claim_bot(&helper.id).unwrap().unwrap();
    assert_eq!(child.id, first);
    for (bot, run) in [(&helper,&child),(&owner,&background)] {
        assert_eq!(crate::runtime::call_tool(&app,bot,run,"finish_quietly",json!({})).await.unwrap()["finish_quietly"],true);
        app.db.finish(&run.id,"completed","","").unwrap();
        app.db.chat_complete(run).unwrap();
    }
    let before = app.db.chat_messages(&background.chat_id).unwrap();
    assert!(!before.iter().any(|m|m["kind"]=="collaboration"));
    let direct_id = app.db.queue(&owner.id,"An explicit owner request",0).unwrap();
    let direct = app.db.claim_bot(&owner.id).unwrap().unwrap();
    assert_eq!(direct.id,direct_id);
    assert!(!initiative::is_run(&app.db.0.lock().unwrap(),&direct.id).unwrap());
    let direct_child = app.db.chat_handoff(&direct,&helper.id,"Direct work for the same pair").unwrap();
    let repeated = app.db.chat_handoff(&direct,&helper.id,"Another direct request").unwrap();
    assert_ne!(repeated,direct_child);
    let visible = app.db.chat_messages(&direct.chat_id).unwrap();
    assert_eq!(visible.iter().filter(|m|m["kind"]=="collaboration").count(),1);
    assert_eq!(app.db.run(&first).unwrap().chat_id,app.db.run(&direct_child).unwrap().chat_id,"Reuse the exact same team conversation");
    // Hidden non-initiative history retains its old deduplication semantics.
    app.db.0.lock().unwrap().execute("UPDATE chat_messages SET suppressed=1 WHERE run_id=? AND kind='collaboration'",[&direct.id]).unwrap();
    app.db.chat_handoff(&direct,&helper.id,"Do not resurrect an ordinary hidden link").unwrap();
    assert!(!app.db.chat_messages(&direct.chat_id).unwrap().iter().any(|m|m["kind"]=="collaboration"));
    owner.profile.initiative=false;app.db.save_bot(&owner).unwrap();
    assert_eq!(app.db.run(&direct_child).unwrap().status,"queued","Off must preserve the independent direct delegation");
}

fn initiative_priority_owner_case(
    claim: fn(&Db, &str) -> anyhow::Result<Option<db::Run>>,
) {
    let app = crate::tests::app();
    let mut owner = bot(&app.db, "codex");
    owner.profile.initiative = true;
    app.db.save_bot(&owner).unwrap();
    let mut helper = bot(&app.db, "codex");
    helper.profile.initiative = true;
    app.db.save_bot(&helper).unwrap();
    let stamp = db::now();
    let root = app.db.queue(&owner.id, "Background root", 0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![root,owner.id]).unwrap();
    let root = app.db.claim_bot(&owner.id).unwrap().unwrap();
    let child = app.db.chat_handoff(&root,&helper.id,"Automatic supporting research").unwrap();
    let later_root = app.db.queue(&helper.id,"Another automatic check",0).unwrap();
    let direct = app.db.queue(&helper.id,"Explicit owner request",0).unwrap();
    {
        let c=app.db.0.lock().unwrap();
        c.execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![later_root,helper.id]).unwrap();
        for (id,created) in [(&root.id,stamp-60),(&child,stamp-30),(&later_root,stamp-20),(&direct,stamp-10)] {
            c.execute("UPDATE runs SET created=? WHERE id=?",params![created,id]).unwrap();
        }
        assert_eq!(initiative::root(&c,&child).unwrap(),Some(root.id.clone()));
        assert!(initiative::active(&c,&child).unwrap());
        assert!(initiative::remaining_seconds(&c,&child,stamp).unwrap().unwrap()>0);
        assert!(!initiative::is_run(&c,&direct).unwrap());
    }
    let chosen = claim(&app.db,&helper.id).unwrap().unwrap();
    assert_eq!(chosen.id,direct,"Explicit owner work must precede a live initiative descendant");
    app.db.finish(&chosen.id,"completed","Owner task done","").unwrap();
    app.db.chat_complete(&chosen).unwrap();
    let first_automatic=claim(&app.db,&helper.id).unwrap().unwrap();
    assert_eq!(first_automatic.id,child,"Keep automatic root/descendant FIFO after owner work");
    app.db.finish(&first_automatic.id,"completed","Research done","").unwrap();
    app.db.chat_complete(&first_automatic).unwrap();
    assert_eq!(claim(&app.db,&helper.id).unwrap().unwrap().id,later_root);
}
#[test]
fn initiative_priority_owner_precedes_live_descendants_and_keeps_automatic_fifo() {
    initiative_priority_owner_case(Db::claim_bot);
}
#[test]
fn initiative_priority_ordinary_helper_order_is_unchanged() {
    let app=crate::tests::app();
    let owner=bot(&app.db,"codex");let helper=bot(&app.db,"codex");
    let root=app.db.queue(&owner.id,"Ordinary owner task",0).unwrap();
    let root=app.db.claim_bot(&owner.id).unwrap().unwrap();
    let child=app.db.chat_handoff(&root,&helper.id,"Ordinary teammate work").unwrap();
    let direct=app.db.queue(&helper.id,"Later direct request",0).unwrap();
    assert!(!initiative::is_run(&app.db.0.lock().unwrap(),&child).unwrap());
    let chosen=app.db.claim_bot(&helper.id).unwrap().unwrap();
    assert_eq!(chosen.id,child,"Do not demote independent non-initiative teammate work");
    app.db.finish(&chosen.id,"completed","Done","").unwrap();
    assert_eq!(app.db.claim_bot(&helper.id).unwrap().unwrap().id,direct);
}
#[test]
fn initiative_priority_explicit_continue_is_direct() {
    let app=crate::tests::app();
    let mut owner=bot(&app.db,"codex");owner.profile.initiative=true;app.db.save_bot(&owner).unwrap();
    let mut helper=bot(&app.db,"codex");helper.profile.initiative=true;app.db.save_bot(&helper).unwrap();
    let stopped=app.db.queue(&helper.id,"Earlier initiative draft",0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![stopped,helper.id]).unwrap();
    assert_eq!(app.db.claim_bot(&helper.id).unwrap().unwrap().id,stopped);
    app.db.finish(&stopped,"interrupted","Draft saved","Budget reached").unwrap();
    let root=app.db.queue(&owner.id,"Automatic owner check",0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",params![root,owner.id]).unwrap();
    let root=app.db.claim_bot(&owner.id).unwrap().unwrap();
    let child=app.db.chat_handoff(&root,&helper.id,"Automatic supporting work").unwrap();
    let continued=app.db.continue_task(&stopped).unwrap();
    assert!(!initiative::is_run(&app.db.0.lock().unwrap(),&continued).unwrap());
    let chosen=app.db.claim_bot(&helper.id).unwrap().unwrap();
    assert_eq!(chosen.id,continued,"Explicit Continue must remain direct owner work");
    app.db.finish(&chosen.id,"completed","Continued draft done","").unwrap();
    assert_eq!(app.db.claim_bot(&helper.id).unwrap().unwrap().id,child);
}
