//! Communication cadence affects interim narration, never final answers or action cards.
use crate::db::{Bot, Db};
use anyhow::Result;
use serde_json::{Value, json};

pub fn valid(mode: &str) -> bool {
    matches!(mode, "calm" | "balanced" | "frequent" | "summaries")
}
pub fn resolved(bot: &Bot, general: &Value) -> String {
    let own = bot.profile.progress_updates.as_str();
    if valid(own) {
        own.into()
    } else {
        general["progress_updates"]
            .as_str()
            .filter(|s| valid(s))
            .unwrap_or("summaries")
            .into()
    }
}
pub fn seconds(mode: &str) -> i64 {
    match mode {
        "calm" => 180,
        "frequent" => 30,
        _ => 60,
    }
}
pub fn context(db: &Db, bot: &Bot) -> Result<Value> {
    let mode = resolved(bot, &db.setting("general")?.unwrap_or_default());
    let style = match mode.as_str() {
        "calm" => "Work quietly; only occasional meaningful updates on long tasks.",
        "frequent" => "Give frequent useful milestones, never narrate each action.",
        _ => "Give useful opening and milestone updates; stay quiet between them.",
    };
    Ok(json!({"mode":mode,"minimum_interval_seconds":seconds(&mode),"guidance":format!("{style} No click, coordinate, loading or retry narration. Questions, approvals, blockers and direct status answers stay immediate. Final-answer detail is unchanged. Explicit user requests and quiet routine rules take precedence.")}))
}

// Run-start and emission snapshots do not retrofit untagged historical messages.
pub fn start(c: &rusqlite::Connection, run: &mut crate::db::Run) -> Result<()> {
    if !run.progress_mode.is_empty() { return Ok(()); }
    use rusqlite::OptionalExtension;
    let profile:String=c.query_row("SELECT profile FROM bots WHERE id=?",[&run.bot_id],|r|r.get(0))?;
    let profile:Value=serde_json::from_str(&profile)?;
    let general:Option<String>=c.query_row("SELECT value FROM settings WHERE key='general'",[],|r|r.get(0)).optional()?;
    let general:Value=serde_json::from_str(general.as_deref().unwrap_or("{}"))?;
    run.progress_mode=profile["progress_updates"].as_str().filter(|v|valid(v)).or_else(||general["progress_updates"].as_str().filter(|v|valid(v))).unwrap_or("summaries").into();
    run.progress_started=crate::db::now();
    c.execute("UPDATE runs SET progress_mode=?,progress_started=? WHERE id=?",rusqlite::params![run.progress_mode,run.progress_started,run.id])?;
    Ok(())
}
// Only explicit task intent or actual work tools qualify new commentary.
// Tool names are protocol metadata; request/output text and elapsed time are not classifiers.
pub fn work_title(tool:&str)->Option<&'static str> {
    match tool {
        "computer_open_url"|"computer_browser_task"=>Some("Review website"),
        "computer_screenshot"|"computer_click"|"computer_type"|"computer_key"|"computer_scroll"=>Some("Work on bot computer"),
        "guest_exec"|"command_start"=>Some("Work on bot computer"),
        "local_list"|"local_read"|"local_write"|"local_mkdir"|"local_skill_scan"|"local_exec"|"local_screenshot"|"local_click"|"local_type"|"local_key"|"local_scroll"=>Some("Work on local computer"),
        "connector_execute"|"codex_connector"|"claude_connector"=>Some("Use connected service"),
        "artifact_read"|"artifact_create"|"artifact_update"|"artifact_publish_file"|"artifact_export"|"share_file"|"read_attachment"=>Some("Work with documents"),
        "skill_save"|"skill_import_local"|"skill_refresh_local"|"skill_rename"|"skill_delete"=>Some("Maintain bot skills"),
        "workspace_import_start"|"workspace_import_draft"|"workspace_sync_prepare"|"workspace_sync_apply"=>Some("Update workspace"),
        "send_to_bot"=>Some("Coordinate teammate work"),
        "checklist_create"|"checklist_update"|"reminder_set"|"reminder_update"|"routine_create"|"routine_update"|"routine_control"|"inbox_monitor_setup"|"inbox_monitor_save"=>Some("Manage plans and schedules"),
        "visual_panel"=>Some("Prepare interactive panel"),
        _=>None,
    }
}
fn phase_boundary(c:&rusqlite::Connection,run:&str)->Result<(i64,i64)> {
    Ok(c.query_row("SELECT COALESCE(MAX(p.message_seq),0),COALESCE(MAX(m.source_event_seq),0) FROM message_progress p JOIN chat_messages m ON m.seq=p.message_seq WHERE p.run_id=? AND p.phase='final_answer'",[run],|r|Ok((r.get(0)?,r.get(1)?)))?)
}
fn phase_title(c:&rusqlite::Connection,run:&str)->Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let (boundary,event)=phase_boundary(c,run)?;
    let title:Option<String>=c.query_row("SELECT json_extract(body,'$.title') FROM events WHERE run_id=? AND kind='task_work' AND seq>? ORDER BY seq DESC LIMIT 1",rusqlite::params![run,event],|r|r.get(0)).optional()?;
    if title.is_some(){return Ok(title);}
    // Only the first phase of an explicit, same-chat answered-question continuation inherits task intent.
    if boundary==0 {return Ok(c.query_row("SELECT r.task_title FROM questions q JOIN runs origin ON origin.id=q.run_id JOIN runs r ON r.id=q.continuation_run_id WHERE r.id=? AND q.status='answered' AND q.task_group_id!='' AND q.bot_id=r.bot_id AND origin.bot_id=r.bot_id AND origin.chat_id=r.chat_id AND q.delivery_chat_id=r.chat_id ORDER BY q.created DESC LIMIT 1",[run],|r|r.get(0)).optional()?);}
    Ok(None)
}
pub fn qualify(c:&rusqlite::Connection,run:&str,title:&str)->Result<()> {qualify_as(c,run,title,false)}
fn qualify_as(c:&rusqlite::Connection,run:&str,title:&str,replace:bool)->Result<()> {
    let active:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=? AND status='running')",[run],|r|r.get(0))?;
    if !active{return Ok(());}
    let previous=phase_title(c,run)?;
    let selected=if replace {title.to_owned()}else{previous.clone().unwrap_or_else(||title.to_owned())};
    let (boundary,event_boundary)=phase_boundary(c,run)?;
    let recorded:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='task_work' AND seq>?)",rusqlite::params![run,event_boundary],|r|r.get(0))?;
    if previous.as_deref()==Some(selected.as_str()) && recorded {return ensure_task_marker(c,run);}
    c.execute("UPDATE runs SET task_title=? WHERE id=?",rusqlite::params![selected,run])?;
    c.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'task_work',?,?)",rusqlite::params![run,json!({"title":selected}).to_string(),crate::db::now()])?;
    c.execute("UPDATE message_progress SET work_qualified=1,task_title=? WHERE run_id=? AND message_seq>? AND work_qualified IS NOT NULL",rusqlite::params![selected,run,boundary])?;
    ensure_task_marker(c,run)
}

// A task is real even when the provider does not narrate it. This is a timeline
// anchor, not an assistant message: no invented prose, notification or model input.
fn ensure_task_marker(c:&rusqlite::Connection,run:&str)->Result<()> {
    use rusqlite::OptionalExtension;
    let (boundary,event_boundary)=phase_boundary(c,run)?;
    let hidden:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?1) OR EXISTS(SELECT 1 FROM routine_execution WHERE run_id=?1) OR EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='tool_result' AND json_extract(body,'$.tool')='finish_quietly' AND COALESCE(json_extract(body,'$.failed'),0)=0)",[run],|r|r.get(0))?;
    if hidden || emission_mode(c,run)?!="summaries" {return Ok(());}
    let existing:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM message_progress WHERE run_id=? AND message_seq>? AND mode='summaries' AND phase='commentary')",rusqlite::params![run,boundary],|r|r.get(0))?;
    if existing {return Ok(());}
    let source:Option<(i64,i64)>=c.query_row("SELECT seq,created FROM events WHERE run_id=? AND kind='task_work' AND seq>? ORDER BY seq LIMIT 1",rusqlite::params![run,event_boundary],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((source,created))=source else{return Ok(());};
    let changed=c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created,source_event_seq,history_order) SELECT r.chat_id,r.bot_id,'','task_progress',r.id,?2,?3,(SELECT printf('%020d:%020d',MIN(m.seq)-1,?3) FROM chat_messages m WHERE m.run_id=r.id AND m.created=?2 AND (m.source_event_seq>?3 OR m.kind='result') HAVING MIN(m.seq) IS NOT NULL) FROM runs r JOIN chats chat ON chat.id=r.chat_id WHERE r.id=?1 AND NOT EXISTS(SELECT 1 FROM chat_messages WHERE source_event_seq=?3)",rusqlite::params![run,created,source])?;
    if changed>0 {record_message(c,c.last_insert_rowid(),run,"commentary")?;}
    Ok(())
}

// Repair only the latest event-proven phase. Older phases are immutable: their
// message sequence boundaries may already be referenced by questions/continuations.
pub fn repair_task_markers(c:&rusqlite::Connection)->Result<()> {
    let tx=c.unchecked_transaction()?;
    let runs:Vec<String>=c.prepare("SELECT r.id FROM runs r WHERE r.progress_mode='summaries' AND EXISTS(SELECT 1 FROM events WHERE run_id=r.id AND kind='task_work') AND NOT EXISTS(SELECT 1 FROM message_progress WHERE run_id=r.id AND phase='commentary' AND mode='summaries')")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for run in runs {ensure_task_marker(c,&run)?;}
    tx.commit()?;
    Ok(())
}

fn emission_mode(c:&rusqlite::Connection,run:&str)->Result<String> {
    use rusqlite::OptionalExtension;
    let profile:String=c.query_row("SELECT b.profile FROM bots b JOIN runs r ON r.bot_id=b.id WHERE r.id=?",[run],|r|r.get(0))?;
    let profile:Value=serde_json::from_str(&profile)?;
    let general:Option<String>=c.query_row("SELECT value FROM settings WHERE key='general'",[],|r|r.get(0)).optional()?;
    let general:Value=serde_json::from_str(general.as_deref().unwrap_or("{}"))?;
    Ok(profile["progress_updates"].as_str().filter(|v|valid(v)).or_else(||general["progress_updates"].as_str().filter(|v|valid(v))).unwrap_or("summaries").into())
}
pub fn observe_tool(c:&rusqlite::Connection,run:&str,tool:&str)->Result<()> {
    if let Some(title)=work_title(tool){qualify(c,run,title)?;}Ok(())
}
pub fn record_message(c:&rusqlite::Connection,seq:i64,run:&str,phase:&str)->Result<()> {
    if !matches!(phase,"commentary"|"final_answer") {return Ok(());}
    use rusqlite::OptionalExtension;
    // Resolve the preference when this message is emitted. Existing rows are
    // immutable, so switching a running task never reformats its history.
    let mode=emission_mode(c,run)?;
    let previous:Option<(String,String,String)>=c.query_row("SELECT mode,phase,group_id FROM message_progress WHERE run_id=? ORDER BY message_seq DESC LIMIT 1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    // A question answer is a persisted continuation, not a new unrelated task.
    // Bind bot and chat again at emission; never merge cross-chat delivery.
    let continuation:Option<String>=if mode=="summaries" && previous.is_none() {
        c.query_row("SELECT q.task_group_id FROM questions q JOIN runs origin ON origin.id=q.run_id JOIN runs child ON child.id=q.continuation_run_id WHERE child.id=?1 AND q.status='answered' AND q.task_group_id!='' AND q.bot_id=child.bot_id AND origin.bot_id=child.bot_id AND origin.chat_id=child.chat_id AND q.delivery_chat_id=child.chat_id ORDER BY q.created DESC LIMIT 1",[run],|r|r.get(0)).optional()?
    } else {None};
    let group=match previous {
        Some((old_mode,old_phase,group)) if old_mode==mode && old_phase!="final_answer" => group,
        _ => continuation.unwrap_or_else(||format!("{run}:segment:{seq}")),
    };
    let title=phase_title(c,run)?;let qualified=title.is_some();
    c.execute("INSERT INTO message_progress(message_seq,run_id,mode,phase,group_id,work_qualified,task_title) VALUES(?,?,?,?,?,?,?)",rusqlite::params![seq,run,mode,phase,group,qualified,title.unwrap_or_default()])?;
    Ok(())
}
// CLI providers stream interim messages without native phase metadata. Once
// completion is verified, retain the final reply outside the progress group.
pub fn finish_message(db: &Db, run: &str, text: &str) -> Result<()> {
    use rusqlite::OptionalExtension;
    if text.trim().is_empty(){return Ok(());}
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    let seq:Option<i64>=tx.query_row("SELECT seq FROM chat_messages WHERE run_id=? AND kind='assistant' AND body=? ORDER BY seq DESC LIMIT 1",rusqlite::params![run,text],|r|r.get(0)).optional()?;
    if let Some(seq)=seq {
        let changed=tx.execute("UPDATE message_progress SET phase='final_answer' WHERE message_seq=? AND run_id=?",rusqlite::params![seq,run])?;
        if changed==0 {record_message(&tx,seq,run,"final_answer")?;}
    }
    tx.commit()?;Ok(())
}

pub fn set_task_title(db: &Db, run: &crate::db::Run, title: &str) -> Result<Value> {
    let title = title.trim();
    anyhow::ensure!(!title.is_empty() && title.chars().count() <= 80 && !title.chars().any(char::is_control), "Task title must be a single line of 1-80 characters");
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    let changed = tx.execute(
        "UPDATE runs SET task_title=? WHERE id=? AND bot_id=? AND status='running'",
        rusqlite::params![title, run.id, run.bot_id],
    )?;
    anyhow::ensure!(changed == 1, "Only the current running task can be named");
    qualify_as(&tx,&run.id,title,true)?;tx.commit()?;
    Ok(json!({"title":title,"text":"Task title saved."}))
}

pub fn message(c:&rusqlite::Connection,seq:i64)->Result<Value> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row("SELECT p.run_id,p.mode,p.phase,p.group_id,COALESCE(p.task_title,r.task_title),p.work_qualified FROM message_progress p JOIN runs r ON r.id=p.run_id WHERE p.message_seq=?",[seq],|r|Ok(json!({"run_id":r.get::<_,String>(0)?,"mode":r.get::<_,String>(1)?,"phase":r.get::<_,String>(2)?,"group_id":r.get::<_,String>(3)?,"task_title":r.get::<_,String>(4)?,"work_qualified":r.get::<_,Option<bool>>(5)?}))).optional()?.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversation_updates, tests};

    #[test]
    fn silent_work_has_one_durable_anchor_without_becoming_a_message() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");
        let id=app.db.queue(&bot.id,"Review the workspace",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        let attention=app.db.attention().unwrap();let last=app.db.chat(&run.chat_id).unwrap().last_message;
        for _ in 0..28 {app.db.event(&id,"tool_started",json!({"tool":"guest_exec","args":{}})).unwrap();}
        set_task_title(&app.db,&run,"Review workspace scope").unwrap();
        let rows=app.db.chat_messages(&run.chat_id).unwrap();
        let markers:Vec<_>=rows.iter().filter(|m|m["kind"]=="task_progress").collect();assert_eq!(markers.len(),1);
        let marker=markers[0];assert_eq!(marker["text"],"");assert_eq!(marker["progress"]["work_qualified"],true);assert_eq!(marker["progress"]["task_title"],"Review workspace scope");
        assert_eq!(app.db.attention().unwrap()["chats"],attention["chats"]);assert_eq!(app.db.chat(&run.chat_id).unwrap().last_message,last);
        assert!(!crate::instructions::build(&app,&bot,&run,&[],Some(262144)).unwrap().contains("task_progress"));
        assert!(!app.db.bot_chat_read(&bot.id,&run.chat_id,0,30).unwrap()["messages"].as_array().unwrap().iter().any(|m|m["kind"]=="task_progress"));
        let connector=crate::connector_artifacts::create(&app.db,&run,&json!({"toolkit":"googledrive","tool_slug":"read_record","input":{}})).unwrap();
        let receipt=crate::connector_artifacts::record(&app.db.0.lock().unwrap(),&connector).unwrap();
        assert_eq!(receipt["progress_message_seq"],marker["seq"]);
        app.db.event(&id,"assistant",json!({"phase":"commentary","text":"Found the scope documents."})).unwrap();
        let rows=app.db.chat_messages(&run.chat_id).unwrap();let update=rows.iter().find(|m|m["text"]=="Found the scope documents.").unwrap();
        assert_eq!(marker["progress"]["group_id"],update["progress"]["group_id"]);
        app.db.event(&id,"assistant",json!({"phase":"final_answer","text":"Review complete."})).unwrap();app.db.finish(&id,"completed","Review complete.","").unwrap();
        let rows=app.db.chat_messages(&run.chat_id).unwrap();assert_eq!(rows.iter().filter(|m|m["kind"]=="task_progress").count(),1);
        assert_eq!(rows.iter().find(|m|m["text"]=="Review complete.").unwrap()["progress"]["phase"],"final_answer");
    }

    #[test]
    fn silent_anchor_respects_final_fences_preferences_and_routines() {
        for mode in ["summaries","calm"] {
            for routine in [false,true] {
                let app=tests::app();let bot=tests::bot(&app.db,"codex");app.db.save_setting("general",&json!({"progress_updates":mode})).unwrap();
                let id=app.db.queue(&bot.id,"Task",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
                if routine {app.db.0.lock().unwrap().execute("INSERT INTO routine_runs VALUES(?,'fixture',0)",[&id]).unwrap();}
                app.db.event(&id,"tool_started",json!({"tool":"remember"})).unwrap();assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="task_progress"));
                set_task_title(&app.db,&run,"Inspect documents").unwrap();
                let first=app.db.chat_messages(&run.chat_id).unwrap();assert_eq!(first.iter().filter(|m|m["kind"]=="task_progress").count(),usize::from(mode=="summaries"&&!routine));
                if routine {continue;}
                app.db.event(&id,"assistant",json!({"phase":"final_answer","text":"Done"})).unwrap();
                app.db.event(&id,"assistant",json!({"phase":"commentary","text":"An ordinary conversation"})).unwrap();
                repair_task_markers(&app.db.0.lock().unwrap()).unwrap();
                let rows=app.db.chat_messages(&run.chat_id).unwrap();assert_eq!(rows.iter().find(|m|m["text"]=="An ordinary conversation").unwrap()["progress"]["work_qualified"],false);
                app.db.event(&id,"assistant",json!({"phase":"final_answer","text":"You are welcome"})).unwrap();
                app.db.event(&id,"tool_started",json!({"tool":"local_read"})).unwrap();
                let rows=app.db.chat_messages(&run.chat_id).unwrap();let anchors:Vec<_>=rows.iter().filter(|m|m["kind"]=="task_progress").collect();
                assert_eq!(anchors.len(),if mode=="summaries"{2}else{0});
                if anchors.len()==2 {assert_ne!(anchors[0]["progress"]["group_id"],anchors[1]["progress"]["group_id"]);}
            }
        }
    }

    #[test]
    fn silent_existing_task_backfill_is_event_based_ordered_and_idempotent() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");let id=app.db.queue(&bot.id,"Existing quiet task",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        let c=app.db.0.lock().unwrap();
        c.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'task_work','{\"title\":\"Existing document review\"}',100)",[&id]).unwrap();
        c.execute("UPDATE runs SET task_title='Existing document review',status='completed',output='Done' WHERE id=?",[&id]).unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,'Done','result',?,100)",rusqlite::params![run.chat_id,bot.id,id]).unwrap();
        repair_task_markers(&c).unwrap();repair_task_markers(&c).unwrap();drop(c);
        let rows=app.db.chat_messages(&run.chat_id).unwrap();let marker=rows.iter().position(|m|m["kind"]=="task_progress").unwrap();let result=rows.iter().position(|m|m["text"]=="Done").unwrap();
        assert!(marker<result);assert_eq!(rows[marker]["created"],100);assert_eq!(rows.iter().filter(|m|m["kind"]=="task_progress").count(),1);
    }

    #[tokio::test]
    async fn silent_question_continuation_keeps_group_and_can_finish_quietly() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");let id=app.db.queue(&bot.id,"Prepare a report",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        set_task_title(&app.db,&run,"Prepare report").unwrap();
        let marker=app.db.chat_messages(&run.chat_id).unwrap().into_iter().find(|m|m["kind"]=="task_progress").unwrap();
        app.db.ask_question(&run,crate::questions::QuestionInput{topic_key:"report".into(),question:"Publish report?".into(),context:"The report is ready.".into(),options:vec!["Publish".into(),"Keep draft".into()]}).unwrap();
        let q=app.db.pending_questions().unwrap().remove(0);assert_eq!(q.task_group_id,marker["progress"]["group_id"].as_str().unwrap());
        app.db.finish(&id,"completed","","").unwrap();app.db.answer_question(&q.id,crate::questions::Answer{selected:Some(1),custom:None}).unwrap();let follow=app.db.claim_bot(&bot.id).unwrap().unwrap();
        app.db.event(&follow.id,"tool_started",json!({"tool":"artifact_read"})).unwrap();
        let marker2=app.db.chat_messages(&run.chat_id).unwrap().into_iter().find(|m|m["kind"]=="task_progress"&&m["run_id"]==follow.id).unwrap();
        assert_eq!(marker["progress"]["group_id"],marker2["progress"]["group_id"]);
        let quiet=crate::runtime::call_tool(&app,&bot,&follow,"finish_quietly",json!({})).await.unwrap();assert_eq!(quiet["finish_quietly"],true);
        assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="task_progress"&&m["run_id"]==follow.id));
        repair_task_markers(&app.db.0.lock().unwrap()).unwrap();
        assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["kind"]=="task_progress"&&m["run_id"]==follow.id));
    }

    #[test]
    fn classification_qualification_metadata_is_not_repeated_and_untagged_finals_fence_next_phase() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");let id=app.db.queue(&bot.id,"Read a file",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        app.db.event(&id,"tool_started",json!({"tool":"local_read","args":{}})).unwrap();
        for _ in 0..8 {app.db.event(&id,"tool_started",json!({"tool":"artifact_read","args":{}})).unwrap();}
        let count=||app.db.events(&id).unwrap().iter().filter(|e|e["kind"]=="task_work").count();assert_eq!(count(),1);
        set_task_title(&app.db,&run,"Review local document").unwrap();assert_eq!(count(),2);set_task_title(&app.db,&run,"Review local document").unwrap();assert_eq!(count(),2);
        app.db.event(&id,"assistant",json!({"text":"First final without phase"})).unwrap();finish_message(&app.db,&id,"First final without phase").unwrap();
        app.db.event(&id,"assistant",json!({"phase":"commentary","text":"An ordinary followup"})).unwrap();
        let rows=app.db.chat_messages(&run.chat_id).unwrap();assert_eq!(rows.iter().find(|m|m["text"]=="An ordinary followup").unwrap()["progress"]["work_qualified"],false);
        assert_eq!(rows.iter().find(|m|m["text"]=="First final without phase").unwrap()["progress"]["phase"],"final_answer");
    }
    #[test]
    fn classification_segment_boundaries_preserve_conversation_in_both_directions() {
        for first_work in [false,true] {
            let app=tests::app();let bot=tests::bot(&app.db,"claude-code");let id=app.db.queue(&bot.id,"Two different conversational turns",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
            if first_work {set_task_title(&app.db,&run,"Review first website").unwrap();}
            app.db.event(&id,"assistant",json!({"phase":"commentary","text":"First opening"})).unwrap();
            app.db.event(&id,"assistant",json!({"phase":"final_answer","text":"First final"})).unwrap();
            app.db.event(&id,"assistant",json!({"phase":"commentary","text":"Second opening"})).unwrap();
            if !first_work {app.db.event(&id,"tool_started",json!({"tool":"computer_open_url","args":{"url":"https://example.test"}})).unwrap();}
            let rows=app.db.chat_messages(&run.chat_id).unwrap();
            let a=rows.iter().find(|m|m["text"]=="First opening").unwrap();let b=rows.iter().find(|m|m["text"]=="Second opening").unwrap();
            assert_eq!(a["progress"]["work_qualified"],first_work,"{a}");assert_eq!(b["progress"]["work_qualified"],!first_work,"{b}");
            if first_work {assert_eq!(a["progress"]["task_title"],"Review first website");}else{assert_eq!(a["progress"]["task_title"],"");assert_eq!(b["progress"]["task_title"],"Review website");}
        }
    }
    #[test]
    fn classification_ordinary_conversation_and_memory_never_become_work() {
        for seconds in [14,20] {
            let app=tests::app();let bot=tests::bot(&app.db,"codex");
            let id=app.db.queue(&bot.id,"Thanks, please remember my preference",0).unwrap();
            let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
            app.db.0.lock().unwrap().execute("UPDATE runs SET progress_started=? WHERE id=?",rusqlite::params![crate::db::now()-seconds,id]).unwrap();
            app.db.event(&id,"assistant",json!({"phase":"commentary","text":"I understand your preference."})).unwrap();
            app.db.event(&id,"tool_started",json!({"tool":"remember","args":{"text":"My preference"}})).unwrap();
            let messages=app.db.chat_messages(&run.chat_id).unwrap();
            let m=messages.iter().find(|m|m["kind"]=="assistant").unwrap();
            assert_eq!(m["progress"]["work_qualified"],false,"{seconds}: {m}");
            assert_eq!(app.db.run(&id).unwrap().task_title,"");
        }
    }
    #[test]
    fn classification_first_real_work_promotes_opening_and_retains_model_title() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");
        let id=app.db.queue(&bot.id,"Review this website",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        app.db.event(&id,"assistant",json!({"phase":"commentary","text":"Checking the website."})).unwrap();
        app.db.event(&id,"tool_started",json!({"tool":"computer_open_url","args":{"url":"https://example.test"}})).unwrap();
        let m=app.db.chat_messages(&run.chat_id).unwrap().into_iter().find(|m|m["kind"]=="assistant").unwrap();
        assert_eq!(m["progress"]["work_qualified"],true);
        assert_eq!(app.db.run(&id).unwrap().task_title,"Review website");
        set_task_title(&app.db,&run,"Review website accessibility").unwrap();
        app.db.event(&id,"tool_started",json!({"tool":"guest_exec","args":{}})).unwrap();
        assert_eq!(app.db.run(&id).unwrap().task_title,"Review website accessibility");
        app.db.event(&id,"assistant",json!({"phase":"final_answer","text":"The review is complete."})).unwrap();
        let rows=app.db.chat_messages(&run.chat_id).unwrap();
        assert_eq!(rows.iter().find(|m|m["text"]=="The review is complete.").unwrap()["progress"]["phase"],"final_answer");
    }
    #[test]
    fn cli_updates_group_by_default_and_completion_stays_visible() {
        let app=tests::app();let bot=tests::bot(&app.db,"claude-code");
        let id=app.db.queue(&bot.id,"Long workflow",0).unwrap();
        let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        assert_eq!(run.progress_mode,"summaries");
        set_task_title(&app.db,&run,"Update workspace commands").unwrap();
        for text in ["Inspecting settings", "Updating commands", "Commands are ready"] {
            app.db.event(&id,"assistant",json!({"text":text,"phase":"commentary"})).unwrap();
        }
        finish_message(&app.db,&id,"Commands are ready").unwrap();
        let messages=app.db.chat_messages(&run.chat_id).unwrap();
        let updates:Vec<_>=messages.iter().filter(|m|m["kind"]=="assistant").collect();
        assert_eq!(updates.len(),3);
        assert_eq!(updates[0]["progress"]["mode"],"summaries");
        assert_eq!(updates[0]["progress"]["group_id"],updates[1]["progress"]["group_id"]);
        assert_eq!(updates[0]["progress"]["phase"],"commentary");
        assert_eq!(updates[2]["progress"]["phase"],"final_answer");
        assert_eq!(resolved(&bot,&json!({"progress_updates":"calm"})),"calm");
    }

    #[tokio::test]
    async fn model_task_title_is_scoped_persistent_and_available_in_history() {
        let mut app = tests::app();
        let path = std::env::temp_dir().join(format!("kindred-task-title-{}.db", crate::db::id()));
        std::sync::Arc::get_mut(&mut app).unwrap().db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = tests::bot(&app.db, "codex");
        let id = app.db.queue(&bot.id, "Please do a long multi-step workflow with Gmail filters", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let result = crate::runtime::call_tool(&app, &bot, &run, "set_task_title", json!({"title":"Set up Gmail bill filters"})).await.unwrap();
        assert_ne!(result["failed"], true, "{result}");
        assert_eq!(app.db.run(&id).unwrap().task_title, "Set up Gmail bill filters");
        assert_eq!(app.db.run(&id).unwrap().prompt, run.prompt);
        app.db.event(&id, "assistant", json!({"text":"Checking the existing filters.","phase":"commentary"})).unwrap();
        let other = tests::bot(&app.db, "codex");
        let mut foreign = run.clone(); foreign.bot_id = other.id;
        assert!(set_task_title(&app.db, &foreign, "Wrong task").is_err());
        for bad in ["".to_string(), "  ".into(), "x".repeat(81), "Two\nlines".into()] {
            assert!(set_task_title(&app.db, &run, &bad).is_err());
        }
        assert_eq!(app.db.run(&id).unwrap().task_title, "Set up Gmail bill filters");
        app.db.finish(&id, "completed", "Done", "").unwrap();
        assert!(set_task_title(&app.db, &run, "Late change").is_err());
        drop(app);
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(db.run(&id).unwrap().task_title, "Set up Gmail bill filters");
        let messages = db.chat_messages(&run.chat_id).unwrap();
        assert!(messages.iter().any(|m| m["progress"]["task_title"] == "Set up Gmail bill filters"));
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn live_mode_changes_only_affect_future_messages_and_separate_segments() {
        let app=tests::app();let mut bot=tests::bot(&app.db,"codex");
        app.db.save_setting("general",&json!({"progress_updates":"summaries"})).unwrap();
        let id=app.db.queue(&bot.id,"Long task",0).unwrap();
        let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        set_task_title(&app.db,&run,"Review task settings").unwrap();
        let emit=|text:&str,phase:&str|app.db.event(&id,"assistant",json!({"text":text,"phase":phase})).unwrap();
        emit("First summary update","commentary");emit("Second summary update","commentary");
        let original=app.db.chat_messages(&run.chat_id).unwrap();
        app.db.save_setting("general",&json!({"progress_updates":"calm"})).unwrap();
        emit("Normal update in the same task","commentary");
        app.db.save_setting("general",&json!({"progress_updates":"summaries"})).unwrap();
        emit("New summary segment","commentary");emit("End of phase","final_answer");emit("Next phase","commentary");
        bot.profile.progress_updates="frequent".into();app.db.save_bot(&bot).unwrap();
        emit("Bot preference wins immediately","commentary");
        emit("Legacy untagged reply","");
        let rows=app.db.chat_messages(&run.chat_id).unwrap();
        let tag=|text:&str|rows.iter().find(|m|m["text"]==text).unwrap()["progress"].clone();
        for old in original.iter().filter(|m|m["progress"].is_object()) {
            assert_eq!(rows.iter().find(|m|m["seq"]==old["seq"]).unwrap()["progress"],old["progress"]);
        }
        assert_eq!(tag("First summary update")["mode"],"summaries");
        assert_eq!(tag("First summary update")["group_id"],tag("Second summary update")["group_id"]);
        assert_eq!(tag("Normal update in the same task")["mode"],"calm");
        assert_ne!(tag("First summary update")["group_id"],tag("New summary segment")["group_id"]);
        assert_eq!(tag("New summary segment")["group_id"],tag("End of phase")["group_id"]);
        assert_ne!(tag("Next phase")["group_id"],tag("End of phase")["group_id"]);
        assert_eq!(tag("Bot preference wins immediately")["mode"],"frequent");
        assert_eq!(tag("Legacy untagged reply"),Value::Null);
        assert_eq!(crate::db::general_settings(None)["message_delivery"],"steer");
    }

    #[test]
    fn defaults_inherit_and_overrides_survive_identity_edits() {
        let app = tests::app();
        let mut bot = tests::bot(&app.db, "codex");
        assert_eq!(bot.profile.progress_updates, "inherit");
        assert_eq!(resolved(&bot, &json!({})), "summaries");
        assert_eq!(resolved(&bot, &json!({"progress_updates":"calm"})), "calm");
        bot.profile.progress_updates = "frequent".into();
        app.db.save_bot(&bot).unwrap();
        assert_eq!(
            resolved(&bot, &json!({"progress_updates":"calm"})),
            "frequent"
        );
        app.db
            .save_bot_identity(&bot.id, &bot.name, "", "", true, None)
            .unwrap();
        assert_eq!(
            app.db.bot(&bot.id).unwrap().profile.progress_updates,
            "frequent"
        );
        assert!(
            app.db
                .save_bot_identity(&bot.id, &bot.name, "", "", true, Some("invalid"))
                .is_err()
        );
        assert_eq!(
            app.db.bot(&bot.id).unwrap().profile.progress_updates,
            "frequent"
        );
    }

    #[test]
    fn live_reminders_follow_cadence_and_changes_during_work() {
        let app = tests::app();
        let mut bot = tests::bot(&app.db, "codex");
        app.db.queue(&bot.id, "Long work", 0).unwrap();
        let mut run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        for (mode, elapsed, expected) in [
            ("calm", 90, false),
            ("calm", 181, true),
            ("balanced", 31, false),
            ("balanced", 61, true),
            ("frequent", 31, true),
        ] {
            bot.profile.progress_updates = mode.into();
            app.db.save_bot(&bot).unwrap();
            run.created = crate::db::now() - elapsed;
            let output =
                conversation_updates::with_live_context(&app.db, &run, json!({"text":"Result"}))
                    .unwrap();
            let envelope: Value =
                serde_json::from_str(output["text"].as_str().unwrap()).unwrap_or_default();
            assert_eq!(
                envelope["kindred_live_context"]["progress_update_due"] == true,
                expected,
                "{mode} at {elapsed}"
            );
            assert_eq!(context(&app.db, &bot).unwrap()["mode"], mode);
        }
        // Calm is guidance, never a filter that could discard a blocker or final answer.
        bot.profile.progress_updates = "calm".into();
        app.db.save_bot(&bot).unwrap();
        for phase in ["commentary", "final_answer"] {
            app.db
                .event(
                    &run.id,
                    "assistant",
                    json!({"phase":phase,"text":"A result that must remain visible."}),
                )
                .unwrap();
        }
        assert_eq!(
            app.db
                .events(&run.id)
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "assistant")
                .count(),
            2
        );
    }
}
