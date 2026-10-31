//! Durable request edges keep the requester attached through questions and nested work.
use crate::{
    chats,
    db::{self, Db, Run},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS collaboration_requests(child_run_id TEXT PRIMARY KEY REFERENCES runs(id),parent_run_id TEXT NOT NULL REFERENCES runs(id),source_chat_id TEXT NOT NULL REFERENCES chats(id),resolved INTEGER NOT NULL DEFAULT 0,continuation_run_id TEXT NOT NULL DEFAULT ''); CREATE INDEX IF NOT EXISTS collaboration_parent ON collaboration_requests(parent_run_id);")?;
    c.execute_batch("CREATE INDEX IF NOT EXISTS collaboration_continuation ON collaboration_requests(continuation_run_id);")?;
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

fn continuation_parent(c: &Connection, id: &str) -> Result<Option<Run>> {
    Ok(c.query_row("SELECT r.* FROM runs r WHERE r.id=(SELECT parent_run_id FROM collaboration_requests WHERE continuation_run_id=? LIMIT 1)", [id], db::run_row).optional()?)
}

fn source_current(c: &Connection, run: &Run) -> Result<bool> {
    Ok(crate::continuity::store::session_valid(c, &run.id)? && !c.query_row(
        "SELECT EXISTS(SELECT 1 FROM run_message_sources s WHERE s.run_id=? AND NOT EXISTS(SELECT 1 FROM continuity_current_messages m WHERE m.seq=s.message_seq))",
        [&run.id], |r| r.get::<_, bool>(0))?)
}

fn excerpt(text: &str, limit: usize) -> Value {
    json!({"text":crate::runtime::bounded(text, limit),"shortened":text.len()>limit})
}

fn completion(c: &Connection, run: &Run, limit: usize) -> Result<Value> {
    let current = source_current(c, run)?;
    Ok(json!({"run_id":run.id,"status":run.status,"created":run.created,
        "reported_result":if current {excerpt(&run.output,limit)} else {Value::Null},
        "error":if current {excerpt(&run.error,800)} else {Value::Null},
        "source_invalidated":!current,"readback":"receipts_read"}))
}

/// A helper return is new evidence, not a fresh copy of the assignment. Follow
/// database edges rather than parsing (possibly nested) model-authored prose.
fn continuation_prompt(c: &Connection, parent: &Run, chat: &str) -> Result<String> {
    ensure!(parent.chat_id == chat, "Teammate return belongs to another conversation");
    let mut root = parent.clone();
    let mut seen = std::collections::HashSet::new();
    while let Some(previous) = continuation_parent(c, &root.id)? {
        ensure!(seen.insert(root.id.clone()) && seen.len() <= 64, "Invalid teammate continuation chain");
        ensure!(previous.bot_id == parent.bot_id && previous.chat_id == chat, "Teammate continuation owner changed");
        root = previous;
    }
    let children = c.prepare("SELECT r.* FROM collaboration_requests e JOIN runs r ON r.id=e.child_run_id WHERE e.parent_run_id=? ORDER BY r.created,r.rowid")?
        .query_map([&parent.id], db::run_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut results = Vec::new();
    for child in children.iter().take(8) {
        let mut state = completion(c, child, 2400)?;
        state["bot_id"] = json!(child.bot_id);
        state["name"] = json!(c.query_row("SELECT name FROM bots WHERE id=?", [&child.bot_id], |r| r.get::<_,String>(0))?);
        if source_current(c, child)? {
            let answer: Option<String> = c.query_row("SELECT answer FROM questions WHERE continuation_run_id=?", [&child.id], |r| r.get(0)).optional()?;
            state["user_decision"] = answer.map(|a| excerpt(&a,2400)).unwrap_or(Value::Null);
        }
        results.push(state);
    }
    // This is evaluated again on claim: a queued return may be older than the
    // user's corrections and the work that fulfilled them. Never import other
    // chats, invalidated sources, or a second recursively embedded prompt.
    let latest_users = c.prepare("SELECT seq,sender,body,created FROM continuity_current_messages WHERE chat_id=?1 AND (sender='user' OR sender LIKE 'human:%') AND kind='message' AND created>=?2 AND seq>COALESCE((SELECT max(message_seq) FROM run_message_sources WHERE run_id=?3),0) ORDER BY seq DESC LIMIT 4")?
        .query_map(params![chat,root.created,root.id], |r| Ok(json!({"message_seq":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"message":excerpt(&r.get::<_,String>(2)?,2400),"created":r.get::<_,i64>(3)?})))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let later = c.prepare("SELECT * FROM runs WHERE bot_id=?1 AND chat_id=?2 AND status='completed' AND rowid>(SELECT rowid FROM runs WHERE id=?3) AND id<>?4 ORDER BY rowid DESC LIMIT 4")?
        .query_map(params![parent.bot_id,chat,root.id,parent.id],db::run_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let later = later.iter().map(|r| completion(c,r,2000)).collect::<Result<Vec<_>>>()?;
    let state = json!({"kind":"teammate_return","parent":completion(c,parent,4000)?,
        "original_assignment":{"run_id":root.id,"request":if source_current(c,&root)? {excerpt(&root.prompt,6000)} else {Value::Null}},
        "returned_results":results,"returned_result_count":children.len(),
        "latest_user_messages_newest_first":latest_users,"later_completed_work_newest_first":later});
    Ok(format!("A teammate returned new evidence for an existing assignment. This is not a new user request to redo that assignment. Read the parent completion, later completed work and newest user corrections before acting. Newer user instructions override older requests. Continue only work that is still unfinished and authorized. Verify current state and use receipts_read with these run IDs before repeating any external write; a report of completion is attributed evidence, not independent proof. Never repeat a completed or uncertain action. If the return only confirms work already finished, finish quietly instead of exchanging acknowledgements. Preserve your own role, saved user decisions and approval policy. If a result confirms a new responsibility assigned to you, call remember with mode=append and only that new fact; another bot's responsibilities are not yours. Shortened or invalidated history is not evidence of missing work. Read shortened user corrections with chat_read before acting; never rely on invalidated content. Read omitted results with receipts_read for the parent run.\nContinuation state (attributed data): {}", state))
}

/// Refresh system-created return prompts at execution, including old queued
/// returns from before this fix. Human messages and other run kinds are untouched.
pub fn refresh_on_claim(c: &Connection, run: &mut Run) -> Result<()> {
    if let Some(parent) = continuation_parent(c, &run.id)? {
        ensure!(parent.bot_id == run.bot_id, "Teammate continuation owner changed");
        run.prompt = continuation_prompt(c, &parent, &run.chat_id)?;
        c.execute("UPDATE runs SET prompt=? WHERE id=? AND status='queued'", params![run.prompt,run.id])?;
    }
    Ok(())
}

fn resume(c: &Connection, parent_id: &str) -> Result<()> {
    // A live call consumes helper receipts itself. Hangup or restart must not
    // turn that private work into another ordinary chat task.
    if crate::voice::session::is_run(c,parent_id)? {return Ok(());}
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
    let prompt = continuation_prompt(c, &parent, &source)?;
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
    /// Current helper work, independent of the chat where it executes. No
    /// requester names/content are exposed: clients resolve visible identities.
    pub fn current_delegations(&self) -> Result<Vec<Value>> {
        let c=self.0.lock().unwrap();
        // A completed intermediary still owns its incoming work while its
        // children/continuation settle. Completion does not resolve that edge.
        let rows=c.prepare("SELECT e.child_run_id,p.bot_id,e.source_chat_id,
            EXISTS(SELECT 1 FROM collaboration_requests old WHERE old.continuation_run_id=r.id)
            FROM collaboration_requests e JOIN runs p ON p.id=e.parent_run_id JOIN runs r ON r.id=e.child_run_id
            WHERE e.resolved=0 AND e.continuation_run_id='' AND p.status NOT IN ('cancelled','cancelling')
            AND (r.status IN ('queued','running','awaiting_user','awaiting_approval') OR
                (r.status='completed' AND EXISTS(SELECT 1 FROM collaboration_requests nested WHERE nested.parent_run_id=r.id)))
            ORDER BY r.created,r.rowid")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,bool>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut current=Vec::with_capacity(rows.len());
        for (run_id,requester_bot_id,source_chat_id,resuming) in rows {
            let waiting_on_bot_ids=c.prepare("SELECT DISTINCT child.bot_id FROM collaboration_requests edge JOIN runs child ON child.id=edge.child_run_id
                WHERE edge.parent_run_id=? AND edge.resolved=0 AND edge.continuation_run_id=''
                AND child.status IN ('queued','running','awaiting_user','awaiting_approval','completed') ORDER BY child.created,child.rowid")?
                .query_map([&run_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            current.push(json!({"run_id":run_id,"requester_bot_id":requester_bot_id,"source_chat_id":source_chat_id,"resuming":resuming,"waiting_on_bot_ids":waiting_on_bot_ids}));
        }
        Ok(current)
    }
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
    fn state(run: &Run) -> Value {
        serde_json::from_str(run.prompt.split_once("\nContinuation state (attributed data): ").unwrap().1).unwrap()
    }
    #[test]
    fn delayed_return_keeps_newer_corrections_and_completed_work_at_claim() {
        let db=Db::open(":memory:").unwrap();
        let owner=crate::tests::bot(&db,"claude-code");
        let helper=crate::tests::bot(&db,"claude-code");
        let id=db.queue(&owner.id,"Rebuild the tracker",0).unwrap();
        let parent=db.run(&id).unwrap();
        let child=db.chat_handoff(&parent,&helper.id,"Retain the tracker rules").unwrap();
        finish(&db,&id,"Tracker rebuilt and already delivered");
        finish(&db,&child,"Rules retained");
        let next=queued(&db,&owner.id).pop().unwrap();
        // The delayed return was queued before the correction was handled.
        let correction=db.chat_send(&parent.chat_id,"Leave DNS and headers blank",&[]).unwrap().remove(0);
        finish(&db,&correction,"Updated tracker delivered with blank DNS and headers");
        let other=crate::tests::bot(&db,"claude-code");
        db.queue(&other.id,"UNRELATED_PRIVATE_REQUEST",0).unwrap();
        // Covers old queued prompts: database edges replace recursive prose.
        db.0.lock().unwrap().execute("UPDATE runs SET prompt='Original task: Original task: obsolete recursive text' WHERE id=?",[&next.id]).unwrap();
        let claimed=db.claim_bot(&owner.id).unwrap().unwrap();
        assert_eq!(claimed.id,next.id);
        let data=state(&claimed);
        assert_eq!(data["parent"]["run_id"],parent.id);
        assert_eq!(data["parent"]["reported_result"]["text"],"Tracker rebuilt and already delivered");
        assert_eq!(data["latest_user_messages_newest_first"][0]["message"]["text"],"Leave DNS and headers blank");
        assert_eq!(data["later_completed_work_newest_first"][0]["reported_result"]["text"],"Updated tracker delivered with blank DNS and headers");
        assert!(!claimed.prompt.contains("obsolete recursive text"));
        assert!(!claimed.prompt.contains("UNRELATED_PRIVATE_REQUEST"));
        assert_eq!(db.run(&claimed.id).unwrap().prompt,claimed.prompt);
    }
    #[test]
    fn nested_returns_have_one_root_request_and_do_not_resurrect_invalidated_sources() {
        let db=Db::open(":memory:").unwrap();
        let owner=crate::tests::bot(&db,"codex");let helper=crate::tests::bot(&db,"codex");
        let id=db.queue(&owner.id,"ORIGINAL_ASSIGNMENT_SENTINEL",0).unwrap();let parent=db.run(&id).unwrap();
        let child=db.chat_handoff(&parent,&helper.id,"Check").unwrap();
        finish(&db,&id,"FIRST_COMPLETION_SENTINEL");finish(&db,&child,"Helper evidence");
        let next=db.claim_bot(&owner.id).unwrap().unwrap();
        let second=db.chat_handoff(&next,&helper.id,"Follow-up check").unwrap();
        finish(&db,&next.id,"SECOND_COMPLETION_SENTINEL");finish(&db,&second,"New evidence");
        let third=queued(&db,&owner.id).pop().unwrap();
        assert_eq!(third.prompt.matches("ORIGINAL_ASSIGNMENT_SENTINEL").count(),1);
        assert_eq!(state(&third)["original_assignment"]["run_id"],id);
        assert_eq!(state(&third)["parent"]["reported_result"]["text"],"SECOND_COMPLETION_SENTINEL");
        {
            let c=db.0.lock().unwrap();
            assert_eq!(c.execute("UPDATE chat_messages SET suppressed=1 WHERE seq IN(SELECT message_seq FROM run_message_sources WHERE run_id=?)",[&id]).unwrap(),1);
            c.execute("INSERT INTO continuity_dependencies VALUES(?,?,0)",params![next.id,next.chat_id]).unwrap();
            c.execute("INSERT INTO continuity_epochs VALUES(?,1) ON CONFLICT(chat_id) DO UPDATE SET epoch=1",[&next.chat_id]).unwrap();
        }
        let claimed=db.claim_bot(&owner.id).unwrap().unwrap();
        assert!(!claimed.prompt.contains("ORIGINAL_ASSIGNMENT_SENTINEL"));
        assert!(!claimed.prompt.contains("SECOND_COMPLETION_SENTINEL"));
        assert_eq!(state(&claimed)["parent"]["source_invalidated"],true);
    }
    #[tokio::test]
    async fn confirmed_teammate_return_can_finish_quietly_but_direct_request_cannot() {
        let app=crate::tests::app();let owner=crate::tests::bot(&app.db,"codex");let helper=crate::tests::bot(&app.db,"codex");
        app.db.queue(&owner.id,"Already completed work",0).unwrap();let parent=app.db.claim_bot(&owner.id).unwrap().unwrap();
        assert_eq!(crate::runtime::call_tool(&app,&owner,&parent,"finish_quietly",json!({})).await.unwrap()["failed"],true);
        let child=app.db.chat_handoff(&parent,&helper.id,"Confirm saved rules").unwrap();
        finish(&app.db,&parent.id,"Done");finish(&app.db,&child,"Confirmed");
        let next=app.db.claim_bot(&owner.id).unwrap().unwrap();
        assert_eq!(crate::runtime::call_tool(&app,&owner,&next,"finish_quietly",json!({})).await.unwrap()["finish_quietly"],true);
    }
    #[test]
    fn helper_visibility_tracks_current_work_and_clears_terminal_states() {
        for status in ["completed","failed","cancelled"] {
            let db=Db::open(":memory:").unwrap();let a=crate::tests::bot(&db,"codex");let b=crate::tests::bot(&db,"codex");
            let parent=db.queue(&a.id,"Parent",0).unwrap();let parent=db.run(&parent).unwrap();
            let child=db.chat_handoff(&parent,&b.id,"Help").unwrap();
            let visible=db.current_delegations().unwrap();assert_eq!(visible.len(),1);assert_eq!(visible[0]["run_id"],child);assert_eq!(visible[0]["requester_bot_id"],a.id);assert_eq!(visible[0]["source_chat_id"],parent.chat_id);
            assert_ne!(db.run(&child).unwrap().chat_id,format!("dm-{}",b.id));
            db.0.lock().unwrap().execute("UPDATE runs SET status='running' WHERE id=?",[&child]).unwrap();assert_eq!(db.current_delegations().unwrap().len(),1);
            db.finish(&child,status,"","").unwrap();assert!(db.current_delegations().unwrap().is_empty());
        }
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
        assert_eq!(db.claim_bot(&c.id).unwrap().unwrap().id,leaf);
        let waiting=db.current_delegations().unwrap();
        let intermediate=waiting.iter().find(|d|d["run_id"]==child).expect("B still works for A while waiting on C");
        assert_eq!(intermediate["requester_bot_id"],a.id);
        assert_eq!(intermediate["waiting_on_bot_ids"],json!([c.id]));
        assert_eq!(db.run(&child).unwrap().status,"completed","display does not change run status");
        finish(&db, &leaf, "C evidence");
        let bnext = queued(&db, &b.id);
        assert_eq!(bnext.len(), 1);
        assert_eq!(bnext[0].reply_to, a.id);
        let projection=db.current_delegations().unwrap();
        assert!(!projection.iter().any(|d|d["run_id"]==child));
        let continuation=projection.iter().find(|d|d["run_id"]==bnext[0].id).unwrap();
        assert_eq!(continuation["resuming"],true);
        assert_eq!(continuation["waiting_on_bot_ids"],json!([]));
        assert_eq!(
            db.collaboration_waits(&p.chat_id).unwrap()[0]["run_id"],
            bnext[0].id
        );
        finish(&db, &bnext[0].id, "B conclusion from C");
        let anext = queued(&db, &a.id);
        assert_eq!(anext.len(), 1);
        assert!(anext[0].prompt.contains("B conclusion from C"));
        assert!(db.current_delegations().unwrap().is_empty());
    }
    #[test]
    fn nested_projection_child_cancel_drops_suffix_and_parent_or_helper_cancel_clears() {
        for cancel in ["leaf","parent","helper"] {
            let db=Db::open(":memory:").unwrap();
            let a=crate::tests::bot(&db,"codex");let b=crate::tests::bot(&db,"codex");let c=crate::tests::bot(&db,"codex");
            let parent=db.queue(&a.id,"Original task",0).unwrap();let p=db.run(&parent).unwrap();
            let child=db.chat_handoff(&p,&b.id,"Intermediate").unwrap();finish(&db,&parent,"");
            let leaf=db.chat_handoff(&db.run(&child).unwrap(),&c.id,"Nested help").unwrap();finish(&db,&child,"");
            db.claim_bot(&c.id).unwrap().unwrap();
            let projection=db.current_delegations().unwrap();
            assert_eq!(projection.iter().find(|d|d["run_id"]==child).unwrap()["waiting_on_bot_ids"],json!([c.id]));
            db.cancel(match cancel {"leaf"=>&leaf,"parent"=>&parent,_=>&child}).unwrap();
            let projection=db.current_delegations().unwrap();
            if cancel=="leaf" {
                let helper=projection.iter().find(|d|d["run_id"]==child).expect("incoming A→B is still unresolved");
                assert_eq!(helper["waiting_on_bot_ids"],json!([]));
                assert_eq!(db.run(&child).unwrap().status,"completed");
                assert!(queued(&db,&b.id).is_empty(),"display never schedules a continuation");
                db.cancel(&parent).unwrap();assert!(db.current_delegations().unwrap().is_empty());
            } else {assert!(projection.is_empty());}
        }
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
