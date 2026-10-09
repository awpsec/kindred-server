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
    let mode:String=c.query_row("SELECT progress_mode FROM runs WHERE id=?",[run],|r|r.get(0))?;
    if mode.is_empty() {return Ok(());}
    let completed_phases:i64=c.query_row("SELECT COUNT(*) FROM events WHERE run_id=? AND kind='assistant' AND json_extract(body,'$.phase')='final_answer'",[run],|r|r.get(0))?;
    let group=format!("{run}:{}",completed_phases);
    c.execute("INSERT INTO message_progress(message_seq,run_id,mode,phase,group_id) VALUES(?,?,?,?,?)",rusqlite::params![seq,run,mode,phase,group])?;
    Ok(())
}
pub fn message(c:&rusqlite::Connection,seq:i64)->Result<Value> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row("SELECT run_id,mode,phase,group_id FROM message_progress WHERE message_seq=?",[seq],|r|Ok(json!({"run_id":r.get::<_,String>(0)?,"mode":r.get::<_,String>(1)?,"phase":r.get::<_,String>(2)?,"group_id":r.get::<_,String>(3)?}))).optional()?.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversation_updates, tests};

    #[test]
    fn run_start_snapshot_and_emission_tags_survive_setting_changes_and_reopen() {
        let app=tests::app();let bot=tests::bot(&app.db,"codex");
        app.db.save_setting("general",&json!({"progress_updates":"summaries"})).unwrap();
        let id=app.db.queue(&bot.id,"Long task",0).unwrap();
        assert_eq!(app.db.run(&id).unwrap().progress_mode,"");
        let first=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(first.progress_mode,"summaries");
        app.db.event(&id,"assistant",json!({"text":"Milestone","phase":"commentary"})).unwrap();
        app.db.save_setting("general",&json!({"progress_updates":"calm"})).unwrap();
        app.db.event(&id,"assistant",json!({"text":"Second milestone","phase":"commentary"})).unwrap();
        app.db.event(&id,"assistant",json!({"text":"Final answer","phase":"final_answer"})).unwrap();
        let rows=app.db.chat_messages(&first.chat_id).unwrap();
        let tagged:Vec<_>=rows.iter().filter(|m|m["progress"]["phase"]=="commentary").collect();
        assert_eq!(tagged.len(),2);assert_eq!(tagged[0]["progress"]["group_id"],tagged[1]["progress"]["group_id"]);
        assert_eq!(tagged[1]["progress"]["mode"],"summaries");
        app.db.finish(&id,"completed","Final answer","").unwrap();
        app.db.queue(&bot.id,"New task",0).unwrap();let next=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(next.progress_mode,"calm");
        app.db.event(&next.id,"assistant",json!({"text":"Untagged phase"})).unwrap();
        let rows=app.db.chat_messages(&first.chat_id).unwrap();
        assert_eq!(rows.iter().find(|m|m["text"]=="Untagged phase").unwrap()["progress"],Value::Null);
        assert_eq!(rows.iter().find(|m|m["text"]=="Second milestone").unwrap()["progress"]["mode"],"summaries");
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
