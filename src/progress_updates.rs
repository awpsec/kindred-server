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
            .unwrap_or("balanced")
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
    run.progress_mode=profile["progress_updates"].as_str().filter(|v|valid(v)).or_else(||general["progress_updates"].as_str().filter(|v|valid(v))).unwrap_or("balanced").into();
    run.progress_started=crate::db::now();
    c.execute("UPDATE runs SET progress_mode=?,progress_started=? WHERE id=?",rusqlite::params![run.progress_mode,run.progress_started,run.id])?;
    Ok(())
}
pub fn record_message(c:&rusqlite::Connection,seq:i64,run:&str,phase:&str)->Result<()> {
    if !matches!(phase,"commentary"|"final_answer") {return Ok(());}
    use rusqlite::OptionalExtension;
    // Resolve the preference when this message is emitted. Existing rows are
    // immutable, so switching a running task never reformats its history.
    let profile:String=c.query_row("SELECT b.profile FROM bots b JOIN runs r ON r.bot_id=b.id WHERE r.id=?",[run],|r|r.get(0))?;
    let profile:Value=serde_json::from_str(&profile)?;
    let general:Option<String>=c.query_row("SELECT value FROM settings WHERE key='general'",[],|r|r.get(0)).optional()?;
    let general:Value=serde_json::from_str(general.as_deref().unwrap_or("{}"))?;
    let mode=profile["progress_updates"].as_str().filter(|v|valid(v)).or_else(||general["progress_updates"].as_str().filter(|v|valid(v))).unwrap_or("balanced");
    let previous:Option<(String,String,String)>=c.query_row("SELECT mode,phase,group_id FROM message_progress WHERE run_id=? ORDER BY message_seq DESC LIMIT 1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let group=match previous {
        Some((old_mode,old_phase,group)) if old_mode==mode && old_phase!="final_answer" => group,
        _ => format!("{run}:segment:{seq}"),
    };
    c.execute("INSERT INTO message_progress(message_seq,run_id,mode,phase,group_id) VALUES(?,?,?,?,?)",rusqlite::params![seq,run,mode,phase,group])?;
    Ok(())
}
pub fn set_task_title(db: &Db, run: &crate::db::Run, title: &str) -> Result<Value> {
    let title = title.trim();
    anyhow::ensure!(!title.is_empty() && title.chars().count() <= 80 && !title.chars().any(char::is_control), "Task title must be a single line of 1-80 characters");
    let changed = db.0.lock().unwrap().execute(
        "UPDATE runs SET task_title=? WHERE id=? AND bot_id=? AND status='running'",
        rusqlite::params![title, run.id, run.bot_id],
    )?;
    anyhow::ensure!(changed == 1, "Only the current running task can be named");
    Ok(json!({"title":title,"text":"Task title saved."}))
}

pub fn message(c:&rusqlite::Connection,seq:i64)->Result<Value> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row("SELECT p.run_id,p.mode,p.phase,p.group_id,r.task_title FROM message_progress p JOIN runs r ON r.id=p.run_id WHERE p.message_seq=?",[seq],|r|Ok(json!({"run_id":r.get::<_,String>(0)?,"mode":r.get::<_,String>(1)?,"phase":r.get::<_,String>(2)?,"group_id":r.get::<_,String>(3)?,"task_title":r.get::<_,String>(4)?}))).optional()?.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversation_updates, tests};

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
        assert_eq!(resolved(&bot, &json!({})), "balanced");
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
