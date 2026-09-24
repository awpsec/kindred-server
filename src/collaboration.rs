//! Durable request edges keep the requester attached through questions and nested work.
use crate::{
    chats,
    db::{self, Db, Run},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS collaboration_requests(child_run_id TEXT PRIMARY KEY REFERENCES runs(id),parent_run_id TEXT NOT NULL REFERENCES runs(id),source_chat_id TEXT NOT NULL REFERENCES chats(id),resolved INTEGER NOT NULL DEFAULT 0,continuation_run_id TEXT NOT NULL DEFAULT ''); CREATE INDEX IF NOT EXISTS collaboration_parent ON collaboration_requests(parent_run_id);")?;
    Ok(())
}
pub fn has_requests(c: &Connection, run: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM collaboration_requests WHERE parent_run_id=?)",
        [run],
        |r| r.get(0),
    )?)
}
pub fn is_child(c: &Connection, run: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM collaboration_requests WHERE child_run_id=?)",
        [run],
        |r| r.get(0),
    )?)
}
pub fn reattach(c: &Connection, old: &str, new: &str) -> Result<()> {
    c.execute("UPDATE collaboration_requests SET child_run_id=?2,resolved=0 WHERE child_run_id=?1 AND continuation_run_id=''",params![old,new])?;
    Ok(())
}
pub fn completed(c: &Connection, run: &Run) -> Result<()> {
    // The last helper can finish before its requester releases the computer.
    // Both completion paths therefore attempt the same transactional wake-up.
    if has_requests(c, &run.id)? {
        resume(c, &run.id)?;
        return Ok(());
    }
    let parent: Option<String> = c
        .query_row(
            "SELECT parent_run_id FROM collaboration_requests WHERE child_run_id=?",
            [&run.id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(parent) = parent {
        c.execute(
            "UPDATE collaboration_requests SET resolved=1 WHERE child_run_id=?",
            [&run.id],
        )?;
        resume(c, &parent)?;
    }
    Ok(())
}
pub fn recover(c: &Connection) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    let runs=tx.prepare("SELECT r.* FROM runs r WHERE r.status IN ('completed','failed','interrupted','cancelled') AND (EXISTS(SELECT 1 FROM collaboration_requests WHERE child_run_id=r.id AND resolved=0 AND continuation_run_id='') OR EXISTS(SELECT 1 FROM collaboration_requests WHERE parent_run_id=r.id AND continuation_run_id='')) AND NOT EXISTS(SELECT 1 FROM events WHERE run_id=r.id AND kind IN ('question_wait','process_wait')) ORDER BY r.depth DESC,r.created")?.query_map([],db::run_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    for run in runs {
        completed(&tx, &run)?;
    }
    tx.commit()?;
    Ok(())
}
fn resume(c: &Connection, parent_id: &str) -> Result<()> {
    let process_wait: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM command_waits WHERE run_id=? AND continuation='')",
        [parent_id],
        |r| r.get(0),
    )?;
    if process_wait {
        return Ok(());
    }

    let parent: Run = c.query_row("SELECT * FROM runs WHERE id=?", [parent_id], db::run_row)?;
    if !matches!(
        parent.status.as_str(),
        "completed" | "failed" | "interrupted"
    ) {
        return Ok(());
    }
    let ready:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM collaboration_requests WHERE parent_run_id=?1) AND NOT EXISTS(SELECT 1 FROM collaboration_requests e JOIN runs r ON r.id=e.child_run_id WHERE e.parent_run_id=?1 AND (e.resolved=0 OR e.continuation_run_id!='' OR r.status IN ('cancelled','cancelling'))) AND NOT EXISTS(SELECT 1 FROM questions WHERE run_id=?1 AND status='pending')",[parent_id],|r|r.get(0))?;
    if !ready {
        return Ok(());
    }
    let results=c.prepare("SELECT b.name,r.status,CASE WHEN trim(r.output)='' THEN 'No written result was returned. Inspect the recorded tool results before drawing conclusions.' ELSE r.output END,r.error,COALESCE((SELECT answer FROM questions WHERE continuation_run_id=r.id),'') FROM collaboration_requests e JOIN runs r ON r.id=e.child_run_id JOIN bots b ON b.id=r.bot_id WHERE e.parent_run_id=? ORDER BY r.created,r.rowid")?.query_map([parent_id],|r|Ok(json!({"name":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"message":r.get::<_,String>(2)?,"error":r.get::<_,String>(3)?,"user_decision":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let source: String = c.query_row(
        "SELECT source_chat_id FROM collaboration_requests WHERE parent_run_id=? LIMIT 1",
        [parent_id],
        |r| r.get(0),
    )?;
    let chat: chats::Chat = c.query_row(
        "SELECT id,name,members,archived,description,bot_only FROM chats WHERE id=?",
        [&source],
        |r| {
            Ok(chats::Chat {
                bot_only: r.get(5)?,
                description: r.get(4)?,
                id: r.get(0)?,
                name: r.get(1)?,
                members: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                archived: r.get(3)?,
                pinned: false,
                last_message: None,
            })
        },
    )?;
    let prompt = format!(
        "Your teammates returned results for your request. Continue the user's task from these results, respecting any saved user decisions. A failed or empty result is not success. Do not exchange courtesy acknowledgements or repeat completed actions. Preserve your own role and existing action approval policy. If these results supply your own assigned role or lasting responsibilities, call remember to merge them into your memory before replying. Facts about another teammate do not become your own role. Saving their role in your memory does not save it for them; only say they retained it when their result confirms they saved it.\nOriginal task: {}\nTeammate results: {}",
        crate::runtime::bounded(&parent.prompt, 16000),
        crate::runtime::bounded(&json!(results).to_string(), 32000)
    );
    match chats::insert_run(
        c,
        &chat,
        &parent.bot_id,
        &prompt,
        &parent.round_id,
        &parent.reply_to,
        parent.depth,
    ) {
        Ok(next) => {
            c.execute(
                "UPDATE collaboration_requests SET continuation_run_id=?2 WHERE parent_run_id=?1",
                params![parent_id, next],
            )?;
            reattach(c, parent_id, &next)?;
        }
        Err(e) => {
            // Record a terminal delivery outcome once; the visible results remain available.
            c.execute("UPDATE collaboration_requests SET continuation_run_id='paused' WHERE parent_run_id=?",[parent_id])?;
            c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'system',?,'notice',?)",params![source,format!("Automatic follow-up paused: {e}"),db::now()])?;
        }
    }
    Ok(())
}
impl Db {
    pub fn collaboration_waits(&self, chat: &str) -> Result<Vec<Value>> {
        let c = self.0.lock().unwrap();
        Ok(c.prepare("SELECT e.parent_run_id,p.bot_id,r.bot_id,r.id,r.chat_id,r.status,b.name FROM collaboration_requests e JOIN runs p ON p.id=e.parent_run_id JOIN runs r ON r.id=e.child_run_id JOIN bots b ON b.id=r.bot_id WHERE (e.source_chat_id=?1 OR r.chat_id=?1) AND e.resolved=0 AND e.continuation_run_id='' AND p.status NOT IN ('cancelled','cancelling') AND r.status NOT IN ('cancelled','cancelling') ORDER BY p.created,r.created,r.rowid")?.query_map([chat],|r|Ok(json!({"parent_run_id":r.get::<_,String>(0)?,"requester_bot_id":r.get::<_,String>(1)?,"bot_id":r.get::<_,String>(2)?,"run_id":r.get::<_,String>(3)?,"chat_id":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"name":r.get::<_,String>(6)?})))?.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn finish(db: &Db, id: &str, text: &str) {
        db.finish(id, "completed", text, "").unwrap();
        db.chat_complete(&db.run(id).unwrap()).unwrap();
    }
    fn queued(db: &Db, bot: &str) -> Vec<Run> {
        db.runs(Some(bot))
            .unwrap()
            .into_iter()
            .filter(|r| r.status == "queued")
            .collect()
    }
    #[test]
    fn fanout_waits_for_all_results_and_parent_then_wakes_once_in_original_chat() {
        let db = Db::open(":memory:").unwrap();
        let a = crate::tests::bot(&db, "codex");
        let b = crate::tests::bot(&db, "codex");
        let c = crate::tests::bot(&db, "codex");
        let parent = db.queue(&a.id, "Combine both checks", 0).unwrap();
        let parent = db.run(&parent).unwrap();
        let first = db.chat_handoff(&parent, &b.id, "Check A").unwrap();
        let second = db.chat_handoff(&parent, &c.id, "Check B").unwrap();
        assert_eq!(db.collaboration_waits(&parent.chat_id).unwrap().len(), 2);
        finish(&db, &first, "A verified");
        finish(&db, &second, "B verified");
        assert_eq!(queued(&db, &a.id).len(), 1); // Only the original turn, still queued in this fixture.
        finish(&db, &parent.id, "");
        let next = queued(&db, &a.id);
        assert_eq!(next.len(), 1);
        assert_ne!(next[0].id, parent.id);
        assert_eq!(next[0].chat_id, parent.chat_id);
        assert!(next[0].prompt.contains("A verified") && next[0].prompt.contains("B verified"));
        db.chat_complete(&db.run(&second).unwrap()).unwrap();
        assert_eq!(queued(&db, &a.id).len(), 1);
        assert!(db.collaboration_waits(&parent.chat_id).unwrap().is_empty());
    }
    #[test]
    fn nested_helper_returns_through_continuation_to_original_requester() {
        let db = Db::open(":memory:").unwrap();
        let a = crate::tests::bot(&db, "codex");
        let b = crate::tests::bot(&db, "codex");
        let c = crate::tests::bot(&db, "codex");
        let parent = db.queue(&a.id, "Original A task", 0).unwrap();
        let p = db.run(&parent).unwrap();
        let child = db.chat_handoff(&p, &b.id, "B task").unwrap();
        finish(&db, &parent, "");
        let leaf = db
            .chat_handoff(&db.run(&child).unwrap(), &c.id, "C task")
            .unwrap();
        finish(&db, &child, "");
        assert!(queued(&db, &a.id).is_empty());
        finish(&db, &leaf, "C evidence");
        let bnext = queued(&db, &b.id);
        assert_eq!(bnext.len(), 1);
        assert_eq!(bnext[0].reply_to, a.id);
        assert_eq!(
            db.collaboration_waits(&p.chat_id).unwrap()[0]["run_id"],
            bnext[0].id
        );
        finish(&db, &bnext[0].id, "B conclusion from C");
        let anext = queued(&db, &a.id);
        assert_eq!(anext.len(), 1);
        assert!(anext[0].prompt.contains("B conclusion from C"));
    }
    #[test]
    fn question_answer_keeps_request_link_and_saved_decline_reaches_requester() {
        let db = Db::open(":memory:").unwrap();
        let a = crate::tests::bot(&db, "codex");
        let b = crate::tests::bot(&db, "codex");
        let parent = db.queue(&a.id, "Consult B", 0).unwrap();
        let p = db.run(&parent).unwrap();
        let child = db.chat_handoff(&p, &b.id, "Review choice").unwrap();
        finish(&db, &parent, "");
        db.finish(&child, "running", "", "").unwrap();
        let q=db.ask_question(&db.run(&child).unwrap(),serde_json::from_value(json!({"topic_key":"choice","question":"Proceed with the optional review?","context":"No action has happened.","options":["Proceed","Defer"]})).unwrap()).unwrap();
        let q: Value = serde_json::from_str(q["text"].as_str().unwrap()).unwrap();
        let id = q["question"]["id"].as_str().unwrap();
        finish(&db, &child, "");
        assert_eq!(db.collaboration_waits(&p.chat_id).unwrap().len(), 1);
        assert!(queued(&db, &a.id).is_empty());
        let answer = db
            .answer_question(
                id,
                crate::questions::Answer {
                    selected: Some(1),
                    custom: None,
                },
            )
            .unwrap();
        let continuation = db.run(&answer.continuation_run_id).unwrap();
        assert_eq!(continuation.round_id, p.round_id);
        assert_eq!(continuation.reply_to, a.id);
        finish(&db, &continuation.id, "");
        assert!(queued(&db, &a.id)[0].prompt.contains("Defer"));
        assert_eq!(
            db.answer_question(
                id,
                crate::questions::Answer {
                    selected: Some(1),
                    custom: None
                }
            )
            .unwrap()
            .continuation_run_id,
            continuation.id
        );
    }
    #[test]
    fn cancelling_wait_stops_queued_helper_and_prevents_late_wakeup() {
        let db = Db::open(":memory:").unwrap();
        let a = crate::tests::bot(&db, "codex");
        let b = crate::tests::bot(&db, "codex");
        let parent = db.queue(&a.id, "Ask B", 0).unwrap();
        let p = db.run(&parent).unwrap();
        let child = db.chat_handoff(&p, &b.id, "Review").unwrap();
        finish(&db, &parent, "");
        db.cancel(&parent).unwrap();
        db.cancel(&parent).unwrap();
        let notices:i64=db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM chat_messages WHERE body='Requested collaboration stopped. Other tasks continue.' AND kind='notice'",[],|r|r.get(0)).unwrap();
        assert_eq!(
            notices, 2,
            "one durable stop notice in each conversation, even on retry"
        );
        assert!(db.collaboration_waits(&p.chat_id).unwrap().is_empty());
        assert_eq!(db.run(&child).unwrap().status, "cancelled");
        db.chat_complete(&db.run(&child).unwrap()).unwrap();
        assert!(queued(&db, &a.id).is_empty());
    }
    #[test]
    fn restart_recovers_a_result_saved_before_delivery_without_duplicate_wakeup() {
        let path = std::env::temp_dir().join(format!("kindred-collaboration-{}", db::id()));
        let (parent, child, bot, chat);
        {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            let a = crate::tests::bot(&db, "codex");
            let b = crate::tests::bot(&db, "codex");
            bot = a.id.clone();
            parent = db.queue(&a.id, "Resume after restart", 0).unwrap();
            let p = db.run(&parent).unwrap();
            chat = p.chat_id.clone();
            child = db.chat_handoff(&p, &b.id, "Check").unwrap();
            finish(&db, &parent, "");
            db.finish(&child, "completed", "Durable result", "")
                .unwrap();
        }
        for _ in 0..2 {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            assert!(db.collaboration_waits(&chat).unwrap().is_empty());
            let next = queued(&db, &bot);
            assert_eq!(next.len(), 1);
            assert!(next[0].prompt.contains("Durable result"));
        }
        for suffix in ["", ".lock", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
