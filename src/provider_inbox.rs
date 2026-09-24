//! Scheduled inbox reviews through the existing Claude account connection.
//! These are model runs, not Gmail history polling or a push subscription.
use crate::{
    db::{self, Bot, Db, Routine, Run},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{Json, extract::State};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub account_key: String,
    pub connector_key: String,
    pub name: String,
    pub since: i64,
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS provider_inbox_routines(routine_id TEXT PRIMARY KEY REFERENCES routines(id) ON DELETE CASCADE,account_key TEXT NOT NULL,connector_key TEXT NOT NULL,body TEXT NOT NULL,UNIQUE(account_key,connector_key));
        CREATE TABLE IF NOT EXISTS provider_inbox_runs(run_id TEXT PRIMARY KEY REFERENCES runs(id),body TEXT NOT NULL);")?;
    Ok(())
}
pub fn sources(app: &App, bot: &Bot) -> Result<Vec<Value>> {
    if bot.provider != "claude-code" {
        return Ok(vec![]);
    }
    let inventory = crate::connector_policy::inventory(app, bot)?;
    Ok(inventory["provider_connections"].as_array().into_iter().flatten().filter(|row|
        row["status"]=="connected" && row["origin"]=="claude-account" &&
        row["display_name"].as_str().is_some_and(|name|name.eq_ignore_ascii_case("gmail")) &&
        row["tools"].as_array().is_some_and(|tools|!tools.is_empty())
    ).map(|row|json!({"bot_id":bot.id,"bot_name":bot.name,"account_key":inventory["provider_account_key"],"connector_key":row["connector_key"],"name":"Gmail via Claude","requires_approval":row["requires_approval"]})).collect())
}
fn question(
    app: &App,
    run: &Run,
    topic: &str,
    phase: &str,
    title: &str,
    context: &str,
    options: Vec<String>,
    metadata: Value,
) -> Result<Value> {
    let mut result = app.db.ask_question(
        run,
        crate::questions::QuestionInput {
            topic_key: format!("{topic}.{phase}"),
            question: title.into(),
            context: context.into(),
            options,
        },
    )?;
    let mut text: Value = serde_json::from_str(result["text"].as_str().unwrap())?;
    text["inbox_setup"] = metadata;
    result["text"] = json!(serde_json::to_string(&text)?);
    Ok(result)
}
pub fn choose_source(app: &App, run: &Run, topic: &str) -> Result<Value> {
    question(
        app,
        run,
        topic,
        "source",
        "Which Gmail connection should I use?",
        "Claude can use your existing Gmail connection for scheduled reviews; each check uses AI. Kindred's direct Gmail connection supports activity detection without AI on an unchanged inbox. No routine has been created.",
        vec![
            "Use Gmail through Claude".into(),
            "Use Kindred's Gmail connection".into(),
            "Not now".into(),
        ],
        json!({"phase":"source","topic_key":topic,"next":"Respect the saved answer. Call inbox_monitor_setup again with the same topic_key and source=claude or source=kindred. Do not reconnect Gmail or create a routine before the person chooses."}),
    )
}
pub fn setup(
    app: &App,
    bot: &Bot,
    run: &Run,
    args: &Value,
    topic: &str,
    sources: &[Value],
) -> Result<Value> {
    ensure!(
        !sources.is_empty(),
        "This Claude bot cannot see a connected Gmail tool. Check its Claude account connection; a Kindred Gmail reconnect is not required for scheduled Claude reviews."
    );
    let key = args["connector_key"].as_str().unwrap_or("");
    let selected = if key.is_empty() && sources.len() == 1 {
        &sources[0]
    } else {
        sources
            .iter()
            .find(|row| row["connector_key"] == key)
            .context("Choose the exact Claude Gmail connector_key from connectors_list")?
    };
    let criteria=args["instructions"].as_str().unwrap_or("Alert me about mail that needs a response or an important action. Stay quiet about routine mail. Do not send or change messages.");
    ensure!(criteria.len() <= 4000, "Keep alert preferences within 4 KB");
    question(
        app,
        run,
        topic,
        "claude-frequency",
        "How often should I check Gmail through Claude?",
        &format!(
            "Your existing Claude Gmail connection will be used. Each scheduled review runs Claude and uses your provider allowance, even when the inbox is unchanged. This is not instant or Constant activity monitoring. Existing connector permission prompts still apply. Alert preferences: {criteria}"
        ),
        vec![
            "Every 5 minutes".into(),
            "Every 15 minutes".into(),
            "Every hour".into(),
            "Set a custom schedule".into(),
        ],
        json!({"phase":"frequency","source":"claude","topic_key":topic,"bot_id":bot.id,"account_key":selected["account_key"],"connector_key":selected["connector_key"],"next":"Respect the selected timing and Not now. Use routine_create with source=claude, this exact account_key and connector_key, trigger=schedule, a name and alert criteria in prompt, plus the chosen interval_seconds or weekly schedule. Do not use trigger=activity or reconnect in Marketplace. Resolve missing custom timing with ask_question. Inspect routines_list first; use routine_update for existing work. Only claim monitoring is configured after a successful save; the first successful check is separate."}),
    )
}
pub fn binding(app: &App, bot: &Bot, args: &Value) -> Result<Option<Binding>> {
    if args["source"].as_str().unwrap_or("kindred") != "claude" {
        return Ok(None);
    }
    ensure!(
        args["trigger"] != "activity",
        "Claude Gmail supports scheduled reviews. Choose a timed schedule; activity detection requires a Kindred Gmail connection."
    );
    let selected=sources(app,bot)?.into_iter().find(|row|row["account_key"]==args["account_key"]&&row["connector_key"]==args["connector_key"]).context("Choose this Claude bot's connected Gmail from inbox_monitor_setup; its connection may have changed")?;
    Ok(Some(Binding {
        account_key: selected["account_key"].as_str().unwrap().into(),
        connector_key: selected["connector_key"].as_str().unwrap().into(),
        name: "Gmail via Claude".into(),
        since: db::now(),
    }))
}
pub fn existing(db: &Db, binding: &Binding) -> Result<Option<String>> {
    Ok(db.0.lock().unwrap().query_row("SELECT routine_id FROM provider_inbox_routines WHERE account_key=? AND connector_key=?",params![binding.account_key,binding.connector_key],|r|r.get(0)).optional()?)
}
pub fn save_binding(c: &Connection, routine: &Routine, binding: &Binding) -> Result<()> {
    let provider: String = c.query_row(
        "SELECT provider FROM bots WHERE id=?",
        [&routine.bot_id],
        |r| r.get(0),
    )?;
    ensure!(
        provider == "claude-code",
        "A Claude Gmail routine needs a Claude bot"
    );
    c.execute(
        "INSERT INTO provider_inbox_routines VALUES(?,?,?,?)",
        params![
            routine.id,
            binding.account_key,
            binding.connector_key,
            serde_json::to_string(binding)?
        ],
    )?;
    Ok(())
}
pub fn snapshot(c: &Connection, run: &str, routine: &str) -> Result<()> {
    c.execute("INSERT OR IGNORE INTO provider_inbox_runs SELECT ?,body FROM provider_inbox_routines WHERE routine_id=?",params![run,routine])?;
    Ok(())
}
fn run_binding(db: &Db, run: &Run) -> Result<Option<Binding>> {
    let body: Option<String> =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT body FROM provider_inbox_runs WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )
            .optional()?;
    body.map(|body| Ok(serde_json::from_str(&body)?))
        .transpose()
}
pub fn inventory_for_run(db: &Db, run: &Run, inventory: &mut Value) -> Result<bool> {
    let Some(binding) = run_binding(db, run)? else {
        return Ok(false);
    };
    inventory["preferred_source"] = json!("provider");
    inventory["resolved_preferred_source"] = json!("claude");
    inventory["scheduled_inbox_connection"] = json!({"account_key":binding.account_key,"connector_key":binding.connector_key,"source":"claude","read_only_review":true});
    Ok(true)
}
pub fn instructions(db: &Db, bot: &Bot, run: &Run) -> Result<String> {
    let Some(binding) = run_binding(db, run)? else {
        return Ok(String::new());
    };
    ensure!(
        bot.provider == "claude-code",
        "This inbox routine is bound to Claude. Restore that provider or pause the routine; it cannot switch Gmail connections."
    );
    let last:Option<i64>=db.0.lock().unwrap().query_row("SELECT max(r.created) FROM routine_runs prior JOIN runs r ON r.id=prior.run_id WHERE prior.routine_id=(SELECT routine_id FROM routine_runs WHERE run_id=?) AND r.status='completed' AND r.id<>? AND EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind='tool_result' AND json_extract(e.body,'$.tool')='claude_connector' AND json_extract(e.body,'$.args.account_key')=? AND json_extract(e.body,'$.args.connector_key')=? AND json_extract(e.body,'$.failed')=0) AND NOT EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind='tool_result' AND json_extract(e.body,'$.tool')='claude_connector' AND json_extract(e.body,'$.failed')=1)",params![run.id,run.id,binding.account_key,binding.connector_key],|r|r.get(0))?;
    Ok(format!(
        "\n\nScheduled Gmail review through Claude: {}. Use only the bound Claude Gmail connector {} for this review, never another mailbox source or browser login. Start with mail received after Unix timestamp {} (UTC), with a small overlap if needed; check prior alerts in this conversation to avoid repeating them. The initial check starts at setup time, not the historical inbox backlog. This is a scheduled AI check, not constant or instant activity monitoring. Fetch actual inbox messages before claiming a successful check. Read and summarize only; do not send, reply, archive, delete, label or otherwise modify mail. If access is missing or denied, report the failure; do not reconnect, switch sources or finish quietly as if the check succeeded. Use finish_quietly when an actual successful check has nothing new worth reporting. The saved user criteria follow in the task prompt.",
        binding.name,
        binding.connector_key,
        last.unwrap_or(binding.since)
    ))
}
pub fn verify_catalogue(db: &Db, bot: &Bot, run: &Run, account: &str, rows: &Value) -> Result<()> {
    let Some(binding) = run_binding(db, run)? else {
        return Ok(());
    };
    ensure!(
        bot.provider == "claude-code" && binding.account_key == account,
        "The Claude account for this inbox routine changed. Restore the original account or replace the paused routine."
    );
    ensure!(
        rows.as_array()
            .into_iter()
            .flatten()
            .any(
                |row| row["connector_key"] == binding.connector_key && row["status"] == "connected"
            ),
        "The bound Claude Gmail connection is unavailable. Restore it in Claude or pause this routine."
    );
    Ok(())
}
pub fn guard_tool(db: &Db, run: &Run, tool: &str, args: &Value) -> Result<()> {
    let Some(binding) = run_binding(db, run)? else {
        return Ok(());
    };
    if tool == "claude_connector" {
        ensure!(
            args["account_key"] == binding.account_key
                && args["connector_key"] == binding.connector_key,
            "This scheduled review is bound to its original Claude Gmail connection"
        );
        // This narrows the task; normal connector permission and forced reviews still apply.
        let operation = args["tool_name"]
            .as_str()
            .unwrap_or("")
            .split("__")
            .last()
            .unwrap_or("")
            .to_ascii_lowercase();
        ensure!(
            matches!(
                operation.as_str(),
                "search"
                    | "fetch"
                    | "search_messages"
                    | "read_message"
                    | "read_thread"
                    | "list_messages"
                    | "get_message"
                    | "get_thread"
                    | "gmail_search_messages"
                    | "gmail_read_message"
                    | "gmail_read_thread"
            ),
            "This inbox review supports Gmail search/read tools only; this tool is not a supported read operation"
        );
    } else if matches!(tool, "connector_execute" | "codex_connector") {
        anyhow::bail!(
            "Use the bound Claude Gmail connection for this scheduled review; switching sources is not allowed"
        );
    }
    Ok(())
}
pub fn verify_success(db: &Db, run: &Run) -> Result<()> {
    let Some(binding) = run_binding(db, run)? else {
        return Ok(());
    };
    let (success,failed):(i64,i64)=db.0.lock().unwrap().query_row("SELECT COALESCE(sum(CASE WHEN json_extract(body,'$.failed')=0 THEN 1 ELSE 0 END),0),COALESCE(sum(CASE WHEN json_extract(body,'$.failed')=1 THEN 1 ELSE 0 END),0) FROM events WHERE run_id=? AND kind='tool_result' AND json_extract(body,'$.tool')='claude_connector' AND json_extract(body,'$.args.account_key')=? AND json_extract(body,'$.args.connector_key')=?",params![run.id,binding.account_key,binding.connector_key],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        success > 0 && failed == 0,
        "This scheduled Gmail review did not complete a successful inbox read. Check the Claude connection and tool results; the inbox was not confirmed clear."
    );
    Ok(())
}
pub fn view(db: &Db, routine: &str) -> Result<Option<Value>> {
    let body: Option<String> =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT body FROM provider_inbox_routines WHERE routine_id=?",
                [routine],
                |r| r.get(0),
            )
            .optional()?;
    body.map(|body| Ok(serde_json::from_str(&body)?))
        .transpose()
}
pub async fn save(
    State(app): State<Shared>,
    Json(args): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(save_input(&app, args)?))
}
pub fn save_input(app: &App, args: Value) -> Result<Value> {
    let bot = app.db.bot(crate::runtime::string(&args, "bot_id")?)?;
    ensure!(
        !app.account_disabled() && !bot.profile.archived && app.db.transfer_status()?.is_null(),
        "Choose an active bot in an active workspace"
    );
    let mut args = args;
    args["source"] = json!("claude");
    let binding = binding(&app, &bot, &args)?.unwrap();
    ensure!(
        existing(&app.db, &binding)?.is_none(),
        "A routine already uses this Claude Gmail connection. Edit it in Routines instead of creating a duplicate."
    );
    let interval = args["interval_seconds"]
        .as_i64()
        .context("Choose a review interval")?;
    ensure!(
        (60..=31536000).contains(&interval),
        "Choose an interval from one minute to one year"
    );
    let routine = Routine {
        id: db::id(),
        bot_id: bot.id,
        name: args["name"]
            .as_str()
            .unwrap_or("Review Gmail via Claude")
            .into(),
        prompt: crate::runtime::string(&args, "prompt")?.into(),
        interval_seconds: interval,
        next_run: db::now() + interval,
        enabled: true,
        schedule: None,
        run_at: None,
    };
    app.db.save_routine_with_inbox(&routine, Some(&binding))?;
    Ok(
        json!({"saved":true,"routine":routine,"connection":binding,"delivery":"Scheduled Claude review; each check uses AI, including an unchanged inbox"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn catalogue() -> Value {
        json!([{"origin":"claude-account","name":"claude.ai Gmail","display_name":"Gmail","connector_key":"gmail-fixture","status":"connected","tools":["mcp__claude_ai_Gmail__search","mcp__claude_ai_Gmail__read_thread"]}])
    }
    #[test]
    fn existing_claude_gmail_offers_timing_without_kindred_reconnect() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        crate::connector_policy::catalogue(&app.db, &"a".repeat(64), &catalogue()).unwrap();
        app.db.queue(&bot.id, "Monitor Gmail", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let result =
            crate::mail_watch::setup(&app, &bot, &run, &json!({"topic_key":"gmail"})).unwrap();
        let value: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["inbox_setup"]["source"], "claude");
        assert_eq!(value["inbox_setup"]["connector_key"], "gmail-fixture");
        assert_eq!(value["inbox_setup"]["account_key"], "a".repeat(64));
        assert!(app.db.routines().unwrap().is_empty());
        assert!(crate::mail_watch::watches(&app.db).unwrap().is_empty());
        assert!(binding(&app, &bot, &json!({"source":"claude","trigger":"activity"})).is_err());
    }
    #[test]
    fn review_watermark_only_advances_after_a_verified_inbox_read() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        crate::connector_policy::catalogue(&app.db, &"a".repeat(64), &catalogue()).unwrap();
        let saved=save_input(&app,json!({"bot_id":bot.id,"account_key":"a".repeat(64),"connector_key":"gmail-fixture","prompt":"Read important mail","interval_seconds":900})).unwrap();
        let id = saved["routine"]["id"].as_str().unwrap();
        let since = saved["connection"]["since"].as_i64().unwrap();
        let first = app.db.run(&app.db.run_routine_now(id).unwrap()).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET status='completed',created=? WHERE id=?",
                params![since + 100, first.id],
            )
            .unwrap();
        let next = app.db.run(&app.db.run_routine_now(id).unwrap()).unwrap();
        assert!(
            instructions(&app.db, &bot, &next)
                .unwrap()
                .contains(&format!("timestamp {since} (UTC)")),
            "A deferred question without a mail read must not skip mail"
        );
        let event = json!({"tool":"claude_connector","args":{"account_key":"a".repeat(64),"connector_key":"gmail-fixture","tool_name":"search"},"failed":false});
        app.db
            .event(&first.id, "tool_result", event.clone())
            .unwrap();
        assert!(
            instructions(&app.db, &bot, &next)
                .unwrap()
                .contains(&format!("timestamp {} (UTC)", since + 100))
        );
        let mut failure = event;
        failure["failed"] = json!(true);
        app.db.event(&first.id, "tool_result", failure).unwrap();
        assert!(
            instructions(&app.db, &bot, &next)
                .unwrap()
                .contains(&format!("timestamp {since} (UTC)"))
        );
    }
    #[test]
    fn scheduled_review_is_account_bound_atomic_and_keeps_existing_permissions() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        crate::connector_policy::catalogue(&app.db, &"a".repeat(64), &catalogue()).unwrap();
        let input = json!({"bot_id":bot.id,"source":"claude","account_key":"a".repeat(64),"connector_key":"gmail-fixture","prompt":"Alert about important mail","interval_seconds":900});
        let saved = save_input(&app, input.clone()).unwrap();
        let id = saved["routine"]["id"].as_str().unwrap();
        assert!(save_input(&app, input).is_err());
        assert_eq!(app.db.routines().unwrap().len(), 1);
        assert!(
            sources(&app, &bot).unwrap()[0]["requires_approval"] == true,
            "Scheduling must not grant connector permission"
        );
        let run = app.db.run(&app.db.run_routine_now(id).unwrap()).unwrap();
        assert!(verify_success(&app.db, &run).is_err());
        assert!(
            instructions(&app.db, &bot, &run)
                .unwrap()
                .contains("Unix timestamp")
        );
        verify_catalogue(&app.db, &bot, &run, &"a".repeat(64), &catalogue()).unwrap();
        assert!(verify_catalogue(&app.db, &bot, &run, &"b".repeat(64), &catalogue()).is_err());
        assert!(verify_catalogue(&app.db, &bot, &run, &"a".repeat(64), &json!([])).is_err());
        let args = json!({"account_key":"a".repeat(64),"connector_key":"gmail-fixture","tool_name":"mcp__claude_ai_Gmail__search"});
        guard_tool(&app.db, &run, "claude_connector", &args).unwrap();
        app.db.event(&run.id,"tool_result",json!({"tool":"claude_connector","args":args,"failed":false,"text":"No new messages"})).unwrap();
        verify_success(&app.db, &run).unwrap();
        app.db
            .event(
                &run.id,
                "tool_result",
                json!({"tool":"claude_connector","args":args,"failed":true,"text":"Read failed"}),
            )
            .unwrap();
        assert!(verify_success(&app.db, &run).is_err());
        let mut wrong = args.clone();
        wrong["connector_key"] = json!("other");
        assert!(guard_tool(&app.db, &run, "claude_connector", &wrong).is_err());
        let mut write = args;
        write["tool_name"] = json!("mcp__claude_ai_Gmail__send_email");
        assert!(guard_tool(&app.db, &run, "claude_connector", &write).is_err());
        assert!(guard_tool(&app.db, &run, "connector_execute", &json!({})).is_err());
        let mut profile = write.clone();
        profile["tool_name"] = json!("mcp__claude_ai_Gmail__get_profile");
        assert!(guard_tool(&app.db, &run, "claude_connector", &profile).is_err());
        let binding: Binding = serde_json::from_value(saved["connection"].clone()).unwrap();
        let mut duplicate = app.db.routines().unwrap()[0].clone();
        duplicate.id = db::id();
        assert!(
            app.db
                .save_routine_with_inbox(&duplicate, Some(&binding))
                .is_err()
        );
        assert_eq!(app.db.routines().unwrap().len(), 1);
        app.db
            .0
            .lock()
            .unwrap()
            .execute("DELETE FROM routines WHERE id=?", [id])
            .unwrap();
        // A running task keeps its binding even if its routine was removed.
        assert!(guard_tool(&app.db, &run, "connector_execute", &json!({})).is_err());
    }
}
