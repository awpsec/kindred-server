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
    c.execute("UPDATE initiative_state SET next_check=?", [time])
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
        .execute("UPDATE initiative_state SET next_check=?", [t])
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
        .execute("UPDATE initiative_state SET next_check=?", [t + 28800])
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
            c.execute("UPDATE initiative_state SET next_check=?", [t])
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
