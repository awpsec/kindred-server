//! Communication cadence affects interim narration, never final answers or action cards.
use crate::db::{Bot, Db};
use anyhow::Result;
use serde_json::{Value, json};

pub fn valid(mode: &str) -> bool {
    matches!(mode, "calm" | "balanced" | "frequent")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversation_updates, tests};

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
