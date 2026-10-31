//! Persistent, credential-free handoff from a bot to Slack's connection form.
use super::*;
use crate::{
    db::Run,
    questions::{Answer, Question, QuestionInput},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupInput {
    topic_key: String,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    account_name: String,
}

fn request_result(app: &App, run: &Run, q: &Question) -> Result<Value> {
    let instruction = if q.status == "pending" {
        "The Slack connection card is in your private chat. End quietly and wait. The server resumes this task after a verified connection or Not now. Tokens belong only in the secure connection form. No monitor has been created."
    } else if q.status == "dismissed" || q.selected == Some(1) {
        "The user deferred or dismissed Slack setup. Do not create a monitor, reopen this topic, or send an acknowledgement. Finish quietly."
    } else if q.continuation_run_id == run.id {
        "Continue the original monitoring request with the exact verified account in the saved answer. Inspect existing monitors, resolve channels with slack_channels, and call slack_monitor_save with ALL desired rules. Preserve existing rules unless the user asked to replace them. Connecting alone did not create a monitor. Report saved rules and connection health accurately; event delivery has not been proven."
    } else {
        "This setup already has a saved answer. Only its assigned continuation may act on it. Do not repeat setup or claim a monitor exists without checking its saved state."
    };
    Ok(
        json!({"deferred_question":q.status=="pending","text":json!({"question_id":q.id,"status":q.status,"answer":q.answer,"is_current_continuation":q.continuation_run_id==run.id,"instruction":instruction,"monitoring":view(app,Some(&run.bot_id))?}).to_string()}),
    )
}

pub fn setup_monitor(app: &App, run: &Run, args: &Value) -> Result<Value> {
    let input: SetupInput = serde_json::from_value(args.clone())?;
    ensure!(
        !input.topic_key.is_empty()
            && input.topic_key.len() <= 100
            && input
                .topic_key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
        "Use a stable Slack setup topic key of up to 100 letters, digits, dots, colons, hyphens or underscores"
    );
    let name = input.account_name.trim();
    ensure!(
        name.len() <= 80 && !name.chars().any(char::is_control),
        "Keep account names within 80 characters"
    );
    let topic = format!("slack.setup.{}", input.topic_key);
    let prior:Option<String>=app.db.0.lock().unwrap().query_row("SELECT id FROM questions WHERE bot_id=?1 AND (chat_id=?2 OR delivery_chat_id=?2) AND topic_key=?3 ORDER BY created DESC,rowid DESC LIMIT 1",params![run.bot_id,run.chat_id,topic],|r|r.get(0)).optional()?;
    if let Some(id) = prior {
        return request_result(app, run, &app.db.question(&id)?);
    }
    if !input.account_id.is_empty() {
        account(&app.db, &input.account_id)?;
    }
    let ready = accounts(&app.db)?.iter().any(|a| {
        a.status == "connected"
            && (input.account_id.is_empty() || a.id == input.account_id)
            && (name.is_empty() || a.name.eq_ignore_ascii_case(name))
    });
    if ready {
        return Ok(
            json!({"text":json!({"status":"ready","monitoring":view(app,Some(&run.bot_id))?,"instruction":"Use the exact Slack identity requested; ask the user if identity or scope is ambiguous. Resolve channel names with slack_channels, then save using slack_monitor_save. Reuse the existing monitor ID for this bot/account and supply ALL rules, preserving existing rules unless asked to replace them. Rules combine with OR: all_messages or mentions in channel, all_channels, or direct_messages. Do not create a polling routine. Report saved rules and connection health accurately; saved settings alone do not prove delivery. Reply permissions are unchanged."}).to_string()}),
        );
    }
    let result=app.db.ask_question(run,QuestionInput {
        topic_key:topic,
        question:if name.is_empty(){"Connect Slack monitoring".into()}else{format!("Connect Slack monitoring — {name}")},
        context:"Connect the requested Slack identity in the secure form, then select it on the card. This event connection is separate from ChatGPT Slack. The server checks the connection before resuming the original requested channel, mention or DM setup. Connecting alone creates no monitor and grants no new reply permissions.".into(),
        options:vec!["Connect Slack".into(),"Not now".into()],
    })?;
    let data: Value = serde_json::from_str(
        result["text"]
            .as_str()
            .context("Missing Slack setup request")?,
    )?;
    let qid = data["question"]["id"]
        .as_str()
        .context("Missing Slack setup question")?;
    let body=json!({"toolkit":"slack_events","question_id":qid,"bot_id":run.bot_id,"account_name":name,"account_id":input.account_id}).to_string();
    app.db.0.lock().unwrap().execute(
        "UPDATE chat_messages SET kind='connection_card',body=? WHERE kind='question' AND body=?",
        params![body, qid],
    )?;
    request_result(app, run, &app.db.question(qid)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteInput {
    account_id: String,
}

pub(super) async fn complete_setup(app: &App, question: &str, selected: &str) -> Result<Value> {
    // Serialize with disconnect/update. A browser callback is not proof of connection.
    let _lock = app.slack_lock.lock().await;
    let q = app.db.question(question)?;
    let body:String=app.db.0.lock().unwrap().query_row("SELECT body FROM chat_messages WHERE kind='connection_card' AND json_valid(body) AND json_extract(body,'$.question_id')=?",[question],|r|r.get(0))?;
    let card: Value = serde_json::from_str(&body)?;
    ensure!(
        card["toolkit"] == "slack_events" && card["bot_id"] == q.bot_id,
        "This is not a Slack monitoring setup request"
    );
    if let Some(expected) = card["account_id"].as_str().filter(|s| !s.is_empty()) {
        ensure!(
            selected == expected,
            "Use the requested Slack account for this setup"
        );
    }
    let a = account(&app.db, selected)?;
    ensure!(
        a.status == "connected",
        "Finish connecting Slack before continuing"
    );
    credentials(app, selected)?;
    let answer = format!(
        "Verified Slack event connection: account_id={selected}. Continue the original monitoring request using this exact account. Inspect existing monitors and preserve their rules when adding new ones. Connecting did not create a monitor or grant reply permissions."
    );
    let completed = app.db.answer_question(
        question,
        Answer {
            selected: None,
            custom: Some(answer),
        },
    )?;
    Ok(json!({"status":"connected","continuation_run_id":completed.continuation_run_id}))
}
pub async fn complete_setup_route(
    State(app): State<Shared>,
    Path(question): Path<String>,
    Json(input): Json<CompleteInput>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(
        complete_setup(&app, &question, &input.account_id).await?,
    ))
}
