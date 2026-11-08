//! Structured, receipt-backed engagement checkpoints. These are model-authored
//! claims with evidence, never fresh observations or authority to perform actions.
use crate::{
    continuity::store,
    db::{self, Run},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS continuity_work_states(
      bot_id TEXT NOT NULL,chat_id TEXT NOT NULL,topic TEXT NOT NULL,revision INTEGER NOT NULL,
      run_id TEXT NOT NULL,epoch INTEGER NOT NULL,body TEXT NOT NULL,updated INTEGER NOT NULL,
      PRIMARY KEY(bot_id,chat_id,topic,revision),
      FOREIGN KEY(bot_id,chat_id,topic) REFERENCES continuity_notes(bot_id,chat_id,topic) ON DELETE CASCADE);")?;
    Ok(())
}
fn text(v: &Value, limit: usize, empty: bool) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Work state fields must be text"))?
        .trim();
    ensure!(
        s.len() <= limit && !s.contains('\0') && (empty || !s.is_empty()),
        "Work state text is empty or exceeds its byte limit ({limit})"
    );
    Ok(s.into())
}
fn lines(v: &Value, max: usize) -> Result<Vec<String>> {
    let a = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Work state lists must be arrays"))?;
    ensure!(a.len() <= max, "Keep at most {max} work state items");
    a.iter().map(|v| text(v, 300, false)).collect()
}
pub fn save(c: &Connection, run: &Run, args: &Value, revision: i64) -> Result<()> {
    let Some(value) = args.get("work_state") else {
        return Ok(());
    };
    let body = if value.is_null() {
        Value::Null
    } else {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("work_state must be an object or null"))?;
        ensure!(
            object.keys().all(|k| matches!(
                k.as_str(),
                "objective" | "completed" | "remaining" | "blockers" | "next_action"
            )),
            "Unknown work_state field"
        );
        let objective = text(&value["objective"], 500, false)?;
        let remaining = lines(&value["remaining"], 12)?;
        let blockers = lines(&value["blockers"], 8)?;
        let next = text(&value["next_action"], 500, true)?;
        let completed = value["completed"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("completed must be an array"))?;
        ensure!(
            completed.len() <= 12,
            "Keep at most 12 recent completions; older receipts and revisions remain retained"
        );
        let mut receipts = Vec::new();
        for item in completed {
            ensure!(
                item.as_object().is_some_and(|o| o.len() == 2
                    && o.contains_key("summary")
                    && o.contains_key("event_seq")),
                "A completion needs summary and event_seq only"
            );
            let summary = text(&item["summary"], 500, false)?;
            let seq = item["event_seq"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    anyhow::anyhow!("A completion needs an exact event_seq from receipts_read")
                })?;
            let refs = store::source_refs(c, run, &json!({"sources":[{"event_seq":seq}]}))?;
            let (created, tool): (i64, String) = c.query_row(
                "SELECT created,COALESCE(json_extract(body,'$.tool'),'') FROM events WHERE seq=? AND kind='tool_result' AND json_extract(body,'$.failed')=0 AND COALESCE(json_extract(body,'$.timed_out'),0)=0 AND COALESCE(json_extract(body,'$.stopped'),0)=0 AND COALESCE(json_extract(body,'$.uncertain_effect'),0)=0",
                [seq], |r| Ok((r.get(0)?,r.get(1)?)))
                .map_err(|_| anyhow::anyhow!("Completion {seq} needs a successful tool_result receipt; a started tool or assistant claim is not completion evidence"))?;
            ensure!(
                !tool.trim().is_empty()
                    && !matches!(
                        tool.as_str(),
                        "continuity_save"
                            | "remember"
                            | "memory_compact"
                            | "obligation_save"
                            | "finish_quietly"
                            | "routine_complete"
                            | "set_task_title"
                            | "task_progress"
                    ),
                "Saving a claim or status is not evidence that external work completed"
            );
            receipts.push(
                json!({"summary":summary,"source":refs[0],"observed_at":created,"tool":tool}),
            );
        }
        json!({"objective":objective,"completed":receipts,"remaining":remaining,"blockers":blockers,"next_action":next})
    };
    c.execute(
        "INSERT INTO continuity_work_states VALUES(?,?,?,?,?,?,?,?)",
        params![
            run.bot_id,
            run.chat_id,
            args["topic"].as_str().unwrap_or("").trim(),
            revision,
            run.id,
            store::epoch(c, &run.chat_id)?,
            body.to_string(),
            db::now()
        ],
    )?;
    Ok(())
}
pub fn read(c: &Connection, bot: &str, chat: &str, topic: &str, revision: i64) -> Result<Value> {
    let row: Option<(i64,String,i64,String,i64)> = c.query_row(
        "SELECT revision,run_id,epoch,body,updated FROM continuity_work_states WHERE bot_id=? AND chat_id=? AND topic=? AND revision<=? ORDER BY revision DESC LIMIT 1",
        params![bot,chat,topic,revision], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((revision, run, epoch, body, updated)) = row else {
        return Ok(Value::Null);
    };
    let mut body: Value = serde_json::from_str(&body)?;
    if body.is_null() {
        return Ok(Value::Null);
    }
    let sources: Vec<Value> = body["completed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v["source"].clone())
        .collect();
    if epoch != store::epoch(c, chat)?
        || !store::session_valid(c, &run)?
        || (!sources.is_empty() && !store::sources_valid(c, &json!(sources))?)
    {
        return Ok(
            json!({"revision":revision,"invalidated":true,"readback":"Reconcile changed sources before updating this engagement"}),
        );
    }
    body["revision"] = json!(revision);
    body["updated"] = json!(updated);
    body["evidence_policy"] = json!(
        "Receipt-backed historical claims. Inspect each receipt and current state before asserting completion or repeating an action. observed_at is receipt time, not a fresh verification."
    );
    Ok(body)
}
pub fn preview(mut state: Value) -> Value {
    if !state.is_object() || state["invalidated"] == true {
        return state;
    }
    for key in ["completed", "remaining", "blockers"] {
        let previous_count = state[format!("{key}_count")].as_u64();
        if let Some(a) = state[key].as_array_mut() {
            let count = previous_count.map(|n| n as usize).unwrap_or(a.len());
            a.truncate(3);
            for row in a {
                if let Some(s) = row["summary"].as_str() {
                    row["summary"] = json!(crate::runtime::bounded(s, 200));
                }
            }
            state[format!("{key}_count")] = json!(count);
        }
    }
    state["readback"] = json!(
        "continuity_read with this topic returns the complete saved work state; missing preview items are not missing work"
    );
    state
}
pub fn schema() -> Value {
    json!({"type":["object","null"],"properties":{
      "objective":{"type":"string","maxLength":500},
      "completed":{"type":"array","maxItems":12,"items":{"type":"object","properties":{"summary":{"type":"string","maxLength":500},"event_seq":{"type":"integer","minimum":1}},"required":["summary","event_seq"],"additionalProperties":false}},
      "remaining":{"type":"array","maxItems":12,"items":{"type":"string","maxLength":300}},
      "blockers":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":300}},
      "next_action":{"type":"string","maxLength":500}},
      "required":["objective","completed","remaining","blockers","next_action"],"additionalProperties":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{continuity, db::Db, tests::bot};
    fn setup(db: &Db) -> Run {
        let bot = bot(db, "codex");
        db.run(
            &db.queue(&bot.id, "Continue the Orchard engagement", 0)
                .unwrap(),
        )
        .unwrap()
    }
    fn receipt(db: &Db, run: &Run, kind: &str, tool: &str, failed: bool) -> i64 {
        db.event(
            &run.id,
            kind,
            json!({"tool":tool,"failed":failed,"text":"Synthetic receipt"}),
        )
        .unwrap();
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT MAX(seq) FROM events WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn args(seq: i64) -> Value {
        json!({"topic":"orchard","expected_revision":0,"summary":"Orchard report drafted; send awaits approval","status":"active","work_state":{
          "objective":"Deliver Orchard report","completed":[{"summary":"Draft stored","event_seq":seq}],"remaining":["Review and send"],"blockers":["Owner approval needed"],"next_action":"Review the draft"}})
    }
    fn state(db: &Db, run: &Run) -> Value {
        continuity::notes(db, run, Some("orchard"), i64::MAX).unwrap()["items"][0]["work_state"]
            .clone()
    }
    #[test]
    fn restart_preserves_receipts_and_separates_observation_time_from_save_time() {
        let path = std::env::temp_dir().join(format!("kindred-work-state-{}.sqlite", db::id()));
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let run = setup(&db);
        let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
        db.0.lock()
            .unwrap()
            .execute("UPDATE events SET created=1234 WHERE seq=?", [seq])
            .unwrap();
        continuity::save(&db, &run, &args(seq)).unwrap();
        drop(db);
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let value = state(&db, &run);
        assert_eq!(value["completed"][0]["observed_at"], 1234);
        assert!(value["updated"].as_i64().unwrap() > 1234);
        assert_eq!(value["remaining"][0], "Review and send");
        assert_eq!(value["completed"][0]["source"]["event_seq"], seq);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn false_receipts_and_foreign_context_cannot_checkpoint_completed_work() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        let foreign = setup(&db);
        let seqs = [
            receipt(&db, &run, "tool_started", "connector_execute", false),
            receipt(&db, &run, "tool_result", "connector_execute", true),
            receipt(&db, &run, "tool_result", "continuity_save", false),
            receipt(&db, &run, "tool_result", "", false),
            receipt(&db, &foreign, "tool_result", "connector_execute", false),
            999999,
        ];
        for seq in seqs {
            assert!(continuity::save(&db, &run, &args(seq)).is_err());
        }
        assert!(
            continuity::notes(&db, &run, None, i64::MAX).unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty(),
            "validation rolls back note and metadata too"
        );
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM continuity_work_states", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn uncertain_or_interrupted_results_are_not_completion_evidence() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        for flag in ["timed_out", "stopped", "uncertain_effect"] {
            let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
            db.0.lock()
                .unwrap()
                .execute(
                    "UPDATE events SET body=json_set(body,?,1) WHERE seq=?",
                    params![format!("$.{flag}"), seq],
                )
                .unwrap();
            assert!(continuity::save(&db, &run, &args(seq)).is_err());
        }
    }
    #[test]
    fn concurrent_edits_carry_forward_and_clear_without_erasing_history() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
        let mut a = args(seq);
        a["topic"] = json!(" orchard ");
        continuity::save(&db, &run, &a).unwrap();
        assert!(continuity::save(&db, &run, &a).is_err());
        a["expected_revision"] = json!(1);
        a.as_object_mut().unwrap().remove("work_state");
        continuity::save(&db, &run, &a).unwrap();
        assert_eq!(state(&db, &run)["completed"][0]["source"]["event_seq"], seq);
        a["expected_revision"] = json!(2);
        a["work_state"] = Value::Null;
        continuity::save(&db, &run, &a).unwrap();
        assert!(state(&db, &run).is_null());
        let old = continuity::work_state_at(&db, &run, "orchard", 1).unwrap();
        assert_eq!(old["historical"], true);
        assert_eq!(
            old["work_state"]["completed"][0]["source"]["event_seq"],
            seq
        );
        assert!(continuity::work_state_at(&db, &run, "orchard", 4).is_err());
    }
    #[test]
    fn source_changes_invalidate_and_forgetting_redacts_work_state() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
        continuity::save(&db, &run, &args(seq)).unwrap();
        db.0.lock()
            .unwrap()
            .execute("UPDATE events SET body='{}' WHERE seq=?", [seq])
            .unwrap();
        assert_eq!(read(&db.0.lock().unwrap(), &run.bot_id, &run.chat_id, "orchard", 1).unwrap()["invalidated"], true);
        let message = {
            let c = db.0.lock().unwrap();
            c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user','Private Orchard facts','message',?,1)",params![run.chat_id,run.id]).unwrap();
            c.last_insert_rowid()
        };
        continuity::store::revise_source(&db,message,&json!({"action":"forget","expected_revision":0,"expected_text":"Private Orchard facts"})).unwrap();
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row("SELECT body FROM continuity_work_states", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "null"
        );
    }
    #[test]
    fn previews_preserve_counts_and_full_state_remains_pageable() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
        let mut a = args(seq);
        a["work_state"]["remaining"] = json!(vec!["Another item"; 8]);
        continuity::save(&db, &run, &a).unwrap();
        let preview =
            continuity::context(&db, &run).unwrap()["notes"]["items"][0]["work_state"].clone();
        assert_eq!(preview["remaining"].as_array().unwrap().len(), 3);
        assert_eq!(preview["remaining_count"], 8);
        assert_eq!(super::preview(preview.clone())["remaining_count"], 8);
        let listing=continuity::notes(&db,&run,None,i64::MAX).unwrap();
        assert_eq!(listing["items"][0]["work_state"]["remaining"].as_array().unwrap().len(),3);
        assert_eq!(listing["items"][0]["work_state"]["remaining_count"],8);
        assert_eq!(state(&db, &run)["remaining"].as_array().unwrap().len(), 8);
    }
    #[test]
    fn pending_only_engagement_keeps_uncertainty_without_inventing_receipts() {
        let db=Db::open(":memory:").unwrap();let run=setup(&db);
        let mut a=args(0);a["work_state"]["completed"]=json!([]);
        continuity::save(&db,&run,&a).unwrap();
        let saved=state(&db,&run);
        assert!(saved["completed"].as_array().unwrap().is_empty());
        assert_eq!(saved["remaining"][0],"Review and send");
        assert_ne!(saved["invalidated"],true);
    }
    #[test]
    fn transfer_retains_structured_state_and_revision_history() {
        let db = Db::open(":memory:").unwrap();
        let run = setup(&db);
        let seq = receipt(&db, &run, "tool_result", "connector_execute", false);
        continuity::save(&db, &run, &args(seq)).unwrap();
        db.finish(&run.id, "completed", "Draft ready", "").unwrap();
        let package = db
            .prepare_transfer(&db::id(), "Synthetic workspace")
            .unwrap();
        let target = Db::open(":memory:").unwrap();
        target.import_transfer(&package).unwrap();
        assert_eq!(
            state(&target, &run)["completed"][0]["source"]["event_seq"],
            seq
        );
    }
}
