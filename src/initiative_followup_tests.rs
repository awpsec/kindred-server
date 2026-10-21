use crate::{
    continuity::store,
    db::{self, Bot, Db, Run},
    initiative, initiative_followups as followups,
};
use rusqlite::params;
use serde_json::{Value, json};

fn fixture() -> (Db, Bot, Run, i64, i64) {
    let db = Db::open(":memory:").unwrap();
    db.save_setting("general", &json!({"timezone":"UTC"}))
        .unwrap();
    let mut b = crate::tests::bot(&db, "codex");
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    let id = db
        .queue(&b.id, "Prepare the draft and revisit the delivery", 0)
        .unwrap();
    let run = db.claim_bot(&b.id).unwrap().unwrap();
    assert_eq!(run.id, id);
    let c = db.0.lock().unwrap();
    c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','Track the order and prepare a draft; do not send it.','message',?)",params![run.chat_id,db::now()]).unwrap();
    let seq = c.last_insert_rowid();
    drop(c);
    let t = (db::now() / 86400 + 1) * 86400 + 12 * 3600;
    (db, b, run, seq, t)
}
fn args(key: &str, seq: i64, t: i64) -> Value {
    json!({"key":key,"description":format!("Revisit {key} and prepare a useful update"),"expected_revision":0,"status":"open","sources":[{"message_seq":seq}],"follow_up":{"next_check":t,"stop_when":"Delivered, completed or dismissed by the owner"}})
}
fn ready(db: &Db, run: &Run, t: i64) {
    db.finish(&run.id, "completed", "Saved the follow-ups", "")
        .unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE initiative_state SET next_check=?1,enabled_after=?2",
            params![t + 7 * 86400, t - 1],
        )
        .unwrap();
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

#[test]
fn initiative_followup_independent_topics_coalesce_and_survive_old_history() {
    let (db, b, run, seq, t) = fixture();
    for (key, at) in [("order", t), ("contract", t), ("later", t + 86400)] {
        store::obligation_save(&db, &run, &args(key, seq, at)).unwrap();
    }
    ready(&db, &run, t);
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET created=? WHERE chat_id=?",
            params![t - 30 * 86400, run.chat_id],
        )
        .unwrap();
    initiative::tick(&db, t - 1).unwrap();
    assert!(roots(&db).is_empty());
    initiative::tick(&db, t).unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
    let next = db.claim_bot(&b.id).unwrap().unwrap();
    let context = initiative::context(&db, &next).unwrap();
    assert_eq!(context["due_follow_ups"]["total"], 2);
    assert!(
        !context["due_follow_ups"]
            .to_string()
            .contains("Revisit later")
    );
    assert!(
        context["due_follow_ups"]
            .to_string()
            .contains("Revisit order")
    );
    assert_eq!(db.routines().unwrap().len(), 0);
}
#[test]
fn initiative_followup_keeps_cadence_budget_and_quiet_hours() {
    let (db, b, run, seq, t) = fixture();
    store::obligation_save(&db, &run, &args("order", seq, t)).unwrap();
    ready(&db, &run, t);
    initiative::tick(&db, t).unwrap();
    let first = db.claim_bot(&b.id).unwrap().unwrap();
    db.finish(&first.id, "completed", "", "").unwrap();
    initiative::tick(&db, t + 4 * 3600 - 1).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, t + 4 * 3600).unwrap();
    let second = db.claim_bot(&b.id).unwrap().unwrap();
    db.finish(&second.id, "completed", "", "").unwrap();
    initiative::tick(&db, t + 7 * 3600).unwrap();
    initiative::tick(&db, t + 8 * 3600).unwrap();
    assert_eq!(roots(&db).len(), 2);
    initiative::tick(&db, t + 20 * 3600).unwrap();
    assert_eq!(roots(&db).len(), 3);
}
#[test]
fn initiative_followup_omission_preserves_null_and_closure_remove() {
    for removal in ["null", "completed", "cancelled", "linked"] {
        let (db, _, run, seq, t) = fixture();
        let mut a = args("order", seq, t);
        let saved = store::obligation_save(&db, &run, &a).unwrap();
        a.as_object_mut().unwrap().remove("follow_up");
        a["expected_revision"] = json!(1);
        let kept = store::obligation_save(&db, &run, &a).unwrap();
        assert_eq!(kept["follow_up"], saved["follow_up"]);
        a["expected_revision"] = json!(2);
        match removal {
            "null" => a["follow_up"] = Value::Null,
            "linked" => {
                a["link_kind"] = json!("run");
                a["link_id"] = json!(run.id);
            }
            v => a["status"] = json!(v),
        }
        assert!(store::obligation_save(&db, &run, &a).unwrap()["follow_up"].is_null());
    }
}
#[test]
fn initiative_followup_stale_write_rolls_back_and_requires_opt_in() {
    let (db, mut b, run, seq, t) = fixture();
    let mut a = args("order", seq, t);
    let saved = store::obligation_save(&db, &run, &a).unwrap();
    a["follow_up"]["next_check"] = json!(t + 86400);
    assert!(store::obligation_save(&db, &run, &a).is_err());
    assert_eq!(
        followups::read(&db.0.lock().unwrap(), saved["id"].as_str().unwrap()).unwrap()["next_check_not_before"],
        t
    );
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    let unscheduled = args("other", seq, t);
    assert!(store::obligation_save(&db, &run, &unscheduled).is_err());
    assert_eq!(store::obligations(&db, &run, "", true).unwrap()["total"], 1);
    a["expected_revision"] = json!(1);
    a["follow_up"] = Value::Null;
    assert!(store::obligation_save(&db, &run, &a).is_ok());
}
#[test]
fn initiative_followup_validates_metadata_without_partial_commit() {
    for value in [
        json!({}),
        json!({"next_check":0,"stop_when":"done"}),
        json!({"next_check":db::now()+400,"stop_when":""}),
        json!({"next_check":db::now()+400,"stop_when":"x".repeat(501)}),
        json!({"next_check":db::now()+400,"stop_when":"done","unexpected":true}),
    ] {
        let (db, _, run, seq, t) = fixture();
        let mut a = args("bad", seq, t);
        a["follow_up"] = value;
        assert!(store::obligation_save(&db, &run, &a).is_err());
        assert_eq!(store::obligations(&db, &run, "", true).unwrap()["total"], 0);
    }
}
#[test]
fn initiative_followup_source_edits_retire_timers_without_reusing_old_text() {
    let (db, _, run, seq, t) = fixture();
    store::obligation_save(&db, &run, &args("old-secret", seq, t)).unwrap();
    ready(&db, &run, t);
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET body='Never mind that order' WHERE seq=?",
            [seq],
        )
        .unwrap();
    let read = store::obligations(&db, &run, "", false).unwrap();
    assert_eq!(read["items"][0]["requires_reconciliation"], true);
    assert!(read["items"][0]["follow_up"].is_null());
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    assert_eq!(
        db.0.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM initiative_followups", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn initiative_followup_external_edit_cancels_claim_but_own_completion_does_not() {
    for external in [false, true] {
        let (db, b, run, seq, t) = fixture();
        let mut a = args("order", seq, t);
        store::obligation_save(&db, &run, &a).unwrap();
        ready(&db, &run, t);
        initiative::tick(&db, t).unwrap();
        let next = db.claim_bot(&b.id).unwrap().unwrap();
        a["expected_revision"] = json!(1);
        a["status"] = json!("completed");
        a.as_object_mut().unwrap().remove("follow_up");
        store::obligation_save(&db, if external { &run } else { &next }, &a).unwrap();
        assert_eq!(
            initiative::active(&db.0.lock().unwrap(), &next.id).unwrap(),
            !external
        );
        assert_eq!(
            initiative::context(&db, &next).unwrap()["due_follow_ups"]["total"],
            0
        );
    }
}
#[test]
fn initiative_followup_cancelled_before_claim_cannot_start() {
    let (db, b, run, seq, t) = fixture();
    let mut a = args("order", seq, t);
    store::obligation_save(&db, &run, &a).unwrap();
    ready(&db, &run, t);
    initiative::tick(&db, t).unwrap();
    a["expected_revision"] = json!(1);
    a["status"] = json!("cancelled");
    a.as_object_mut().unwrap().remove("follow_up");
    store::obligation_save(&db, &run, &a).unwrap();
    assert!(db.claim_bot(&b.id).unwrap().is_none());
    assert_eq!(db.run(&roots(&db)[0]).unwrap().status, "cancelled");
}
#[test]
fn initiative_followup_off_pause_and_reenable_preserve_topics() {
    let (db, mut b, run, seq, t) = fixture();
    let saved = store::obligation_save(&db, &run, &args("order", seq, t)).unwrap();
    ready(&db, &run, t);
    b.profile.initiative = false;
    db.save_bot(&b).unwrap();
    initiative::tick(&db, t).unwrap();
    assert!(roots(&db).is_empty());
    assert_eq!(
        followups::read(&db.0.lock().unwrap(), saved["id"].as_str().unwrap()).unwrap()["enabled"],
        false
    );
    b.profile.initiative = true;
    db.save_bot(&b).unwrap();
    initiative::tick(&db, t).unwrap();
    assert_eq!(roots(&db).len(), 1);
}
#[test]
fn initiative_followup_restart_transfer_and_legacy_import_preserve_schedule() {
    let (db, b, run, seq, t) = fixture();
    let saved = store::obligation_save(&db, &run, &args("order", seq, t)).unwrap();
    ready(&db, &run, t);
    let c = db.0.lock().unwrap();
    followups::init(&c).unwrap();
    followups::init(&c).unwrap();
    drop(c);
    let exported = db
        .prepare_transfer(&db::id(), "Follow-up transfer")
        .unwrap();
    let path = std::env::temp_dir().join(format!("initiative-followup-{}.db", db::id()));
    let target = Db::open(path.to_str().unwrap()).unwrap();
    target.import_transfer(&exported).unwrap();
    drop(target);
    let target = Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        followups::read(&target.0.lock().unwrap(), saved["id"].as_str().unwrap()).unwrap()["next_check_not_before"],
        t
    );
    initiative::tick(&target, t).unwrap();
    assert!(
        target.claim_bot(&b.id).unwrap().is_none(),
        "Imported initiative remains off"
    );
    let mut imported = target.bot(&b.id).unwrap();
    imported.profile.initiative = true;
    target.save_bot(&imported).unwrap();
    initiative::tick(&target, t).unwrap();
    assert!(target.claim_bot(&b.id).unwrap().is_some());
    drop(target);
    for suffix in ["", ".lock", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let mut legacy = exported;
    legacy["tables"]
        .as_object_mut()
        .unwrap()
        .remove("initiative_followups");
    let target = Db::open(":memory:").unwrap();
    target.import_transfer(&legacy).unwrap();
    assert!(
        followups::read(&target.0.lock().unwrap(), saved["id"].as_str().unwrap())
            .unwrap()
            .is_null()
    );
}
#[test]
fn initiative_followup_bounded_selection_rotates_due_topics() {
    let (db, b, run, seq, t) = fixture();
    for n in 0..20 {
        store::obligation_save(&db, &run, &args(&format!("topic-{n}"), seq, t)).unwrap();
    }
    ready(&db, &run, t);
    initiative::tick(&db, t).unwrap();
    let first = db.claim_bot(&b.id).unwrap().unwrap();
    let first_ids = followups::context(&db.0.lock().unwrap(), &first.id).unwrap();
    assert_eq!(first_ids["total"], 16);
    assert_eq!(first_ids["items"].as_array().unwrap().len(), 4);
    assert!(first_ids.to_string().len() < 4000);
    db.finish(&first.id, "completed", "", "").unwrap();
    initiative::tick(&db, t + 14400).unwrap();
    let second = db.claim_bot(&b.id).unwrap().unwrap();
    let c = db.0.lock().unwrap();
    let unseen: i64 = c
        .query_row(
            "SELECT count(*) FROM initiative_followups WHERE last_checked=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unseen, 0);
    assert_ne!(
        followups::context(&c, &second.id).unwrap()["items"],
        first_ids["items"]
    );
}

#[test]
fn initiative_followup_rejects_routines_and_revoked_dm_membership() {
    for mode in ["routine", "archived", "removed"] {
        let (db, _, run, seq, t) = fixture();
        let c = db.0.lock().unwrap();
        match mode {
            "routine" => {
                c.execute(
                    "INSERT INTO routine_runs(run_id,routine_id) VALUES(?,'existing-routine')",
                    [&run.id],
                )
                .unwrap();
            }
            "archived" => {
                c.execute("UPDATE chats SET archived=1 WHERE id=?", [&run.chat_id])
                    .unwrap();
            }
            _ => {
                c.execute("UPDATE chats SET members='[]' WHERE id=?", [&run.chat_id])
                    .unwrap();
            }
        }
        drop(c);
        assert!(
            store::obligation_save(&db, &run, &args("order", seq, t)).is_err(),
            "{mode}"
        );
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM initiative_followups", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
#[test]
fn initiative_followup_revoked_dm_stops_claimed_work() {
    for sql in [
        "UPDATE chats SET archived=1 WHERE id=?",
        "UPDATE chats SET members='[]' WHERE id=?",
    ] {
        let (db, b, run, seq, t) = fixture();
        store::obligation_save(&db, &run, &args("order", seq, t)).unwrap();
        ready(&db, &run, t);
        initiative::tick(&db, t).unwrap();
        let next = db.claim_bot(&b.id).unwrap().unwrap();
        let c = db.0.lock().unwrap();
        c.execute(sql, [&run.chat_id]).unwrap();
        assert!(!initiative::active(&c, &next.id).unwrap());
        assert_eq!(followups::context(&c, &next.id).unwrap()["total"], 0);
    }
}
#[test]
fn initiative_followup_forgotten_sources_and_invalidated_event_epochs_retire_work() {
    for event_source in [false, true] {
        let (db, b, run, seq, t) = fixture();
        let mut a = args("private-topic", seq, t);
        if event_source {
            store::start_session(&db, &run, "pi", "model").unwrap();
            db.event(&run.id, "tool_result", json!({"text":"Delivery due"}))
                .unwrap();
            let seq =
                db.0.lock()
                    .unwrap()
                    .query_row(
                        "SELECT max(seq) FROM events WHERE run_id=?",
                        [&run.id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap();
            a["sources"] = json!([{"event_seq":seq}]);
        }
        store::obligation_save(&db, &run, &a).unwrap();
        ready(&db, &run, t);
        initiative::tick(&db, t).unwrap();
        let next = db.claim_bot(&b.id).unwrap().unwrap();
        if event_source {
            db.0.lock().unwrap().execute("INSERT INTO continuity_epochs VALUES(?,1) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1", [&run.chat_id]).unwrap();
        } else {
            store::revise_source(&db, seq, &json!({"action":"forget","expected_text":"Track the order and prepare a draft; do not send it.","expected_revision":0})).unwrap();
            assert_eq!(
                db.0.lock()
                    .unwrap()
                    .query_row("SELECT count(*) FROM initiative_followups", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        let c = db.0.lock().unwrap();
        assert!(!initiative::active(&c, &next.id).unwrap());
        assert_eq!(followups::context(&c, &next.id).unwrap()["total"], 0);
    }
}
#[test]
fn initiative_followup_own_reschedule_leaves_current_context_and_wakes_later() {
    let (db, b, run, seq, t) = fixture();
    let mut a = args("order", seq, t);
    store::obligation_save(&db, &run, &a).unwrap();
    ready(&db, &run, t);
    initiative::tick(&db, t).unwrap();
    let next = db.claim_bot(&b.id).unwrap().unwrap();
    a["expected_revision"] = json!(1);
    a["follow_up"]["next_check"] = json!(t + 86400);
    store::obligation_save(&db, &next, &a).unwrap();
    assert!(initiative::active(&db.0.lock().unwrap(), &next.id).unwrap());
    assert_eq!(
        initiative::context(&db, &next).unwrap()["due_follow_ups"]["total"],
        0
    );
    db.finish(&next.id, "completed", "", "").unwrap();
    initiative::tick(&db, t + 14400).unwrap();
    assert_eq!(roots(&db).len(), 1);
    initiative::tick(&db, t + 86400).unwrap();
    assert_eq!(roots(&db).len(), 2);
}
#[test]
fn initiative_followup_cap_does_not_block_updates_or_ordinary_obligations() {
    let (db, _, run, seq, t) = fixture();
    for n in 0..64 {
        store::obligation_save(&db, &run, &args(&format!("topic-{n}"), seq, t)).unwrap();
    }
    let mut a = args("overflow", seq, t);
    assert!(store::obligation_save(&db, &run, &a).is_err());
    a.as_object_mut().unwrap().remove("follow_up");
    assert!(store::obligation_save(&db, &run, &a).is_ok());
    a = args("topic-0", seq, t);
    a["expected_revision"] = json!(1);
    a["follow_up"]["next_check"] = json!(t + 86400);
    assert!(store::obligation_save(&db, &run, &a).is_ok());
}

#[test]
fn initiative_followup_context_trimming_preserves_obligation_readback_cursor() {
    let (db, _, run, seq, t) = fixture();
    for n in 0..10 {
        let mut a = args(&format!("topic-{n}"), seq, t);
        a["description"] = json!("x".repeat(1900));
        store::obligation_save(&db, &run, &a).unwrap();
    }
    for (window, prefix) in [(4096, String::new()), (2048, "x".repeat(2000))] {
        let all = store::obligations(&db, &run, "", false).unwrap();
        let mut packet = json!({"continuity":{"obligations":all}});
        store::budget_packet(&db, &run, &mut packet, &prefix, &[], Some(window)).unwrap();
        let kept = packet["continuity"]["obligations"]["items"]
            .as_array()
            .unwrap();
        assert!(kept.len() < 10);
        if window == 2048 {
            assert!(kept.is_empty());
        }
        assert_eq!(packet["continuity"]["obligations"]["total"], 10);
        let after = packet["continuity"]["obligations"]["next_after"]
            .as_str()
            .unwrap();
        assert_eq!(
            after,
            kept.last().and_then(|v| v["id"].as_str()).unwrap_or("")
        );
        let remaining = store::obligations(&db, &run, after, false).unwrap();
        let recovered = kept
            .iter()
            .chain(remaining["items"].as_array().unwrap())
            .map(|v| v["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(recovered.len(), 10);
        assert!(
            packet["context_selection"]["omitted_sections"]["/continuity/obligations/items"]
                .as_u64()
                .unwrap()
                > 0
        );
    }
}
