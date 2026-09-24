//! Connector-native chat records. Reviewed inputs remain separate from execution receipts.
use crate::{
    db::{self, Bot, Db, Run},
    runtime::{self, App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS connector_artifacts(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),chat_id TEXT NOT NULL REFERENCES chats(id),bot_id TEXT NOT NULL REFERENCES bots(id),body TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,status TEXT NOT NULL,approval_id TEXT NOT NULL DEFAULT '',created INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS connector_artifacts_run ON connector_artifacts(run_id);
        CREATE TABLE IF NOT EXISTS connector_action_grants(bot_id TEXT NOT NULL,origin TEXT NOT NULL,account_key TEXT NOT NULL,connector_key TEXT NOT NULL,action TEXT NOT NULL,permission TEXT NOT NULL,PRIMARY KEY(bot_id,origin,account_key,connector_key,action));
        UPDATE connector_artifacts SET status='interrupted' WHERE status IN ('preparing','pending','approved','ready','executing');")?;
    Ok(())
}
fn str_at<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or("")
}
pub fn service(args: &Value) -> String {
    crate::connector_records::service(str_at(args, &["toolkit", "connection"]), tool_name(args))
}
pub fn tool_name(args: &Value) -> &str {
    str_at(args, &["tool_slug", "tool_name"])
}
pub fn input(args: &Value) -> &Value {
    if args.get("input").is_some() {
        &args["input"]
    } else {
        &args["arguments"]
    }
}
pub fn input_key(args: &Value) -> &'static str {
    if args.get("input").is_some() {
        "input"
    } else {
        "arguments"
    }
}
pub fn email_send(args: &Value) -> bool {
    let tool = tool_name(args)
        .split("__")
        .last()
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('_', "");
    matches!(
        service(args).as_str(),
        "gmail" | "outlook" | "microsoftoutlook"
    ) && matches!(
        tool.as_str(),
        "sendemail"
            | "sendmessage"
            | "senddraft"
            | "replytoemail"
            | "replyemail"
            | "gmailsendemail"
            | "gmailsendmessage"
            | "gmailsenddraft"
            | "gmailreplytoemail"
            | "outlooksendemail"
            | "outlooksendmessage"
            | "outlookreplytoemail"
            | "microsoftoutlooksendemail"
            | "sendmail"
            | "outlooksendmail"
            | "microsoftoutlooksendmail"
    )
}
pub fn read_only_hint(args: &Value) -> bool {
    let tool = tool_name(args)
        .split("__")
        .last()
        .unwrap_or("")
        .to_ascii_lowercase();
    // Presentation only: this hint never grants permission to execute a Claude tool.
    ["get", "list", "search", "fetch", "read", "query", "find"]
        .iter()
        .any(|p| tool.starts_with(p))
}
fn field_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_owned());
    }
    value
        .as_array()
        .filter(|a| a.iter().all(Value::is_string))
        .map(|a| {
            a.iter()
                .map(|s| s.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(", ")
        })
}
pub fn email_fields(value: &Value) -> Value {
    if value["message"].is_object() {
        return crate::connector_edits::extend_email_fields(value, json!({}));
    }
    let mut fields = serde_json::Map::new();
    for (label, aliases) in [
        (
            "to",
            &[
                "to",
                "recipient_email",
                "recipient",
                "recipients",
                "to_addresses",
            ][..],
        ),
        ("cc", &["cc", "cc_addresses"][..]),
        ("bcc", &["bcc", "bcc_addresses"][..]),
        (
            "from",
            &["from", "sender", "sender_email", "from_email"][..],
        ),
        ("subject", &["subject", "subject_line"][..]),
        (
            "body",
            &[
                "body",
                "message_body",
                "body_text",
                "body_html",
                "html_body",
                "content",
                "message",
            ][..],
        ),
    ] {
        if let Some((key, text)) = aliases
            .iter()
            .find_map(|k| value.get(*k).and_then(field_value).map(|s| (*k, s)))
        {
            fields.insert(label.into(), json!({"key":key,"text":text,"array":value[key].is_array(),"editable":label!="from"}));
        }
    }
    crate::connector_edits::extend_email_fields(value, json!(fields))
}
fn kind(args: &Value) -> &'static str {
    crate::connector_records::family(&service(args))
}
fn title(args: &Value) -> String {
    let value = input(args);
    if let Some(subject) = value["message"]["subject"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        return runtime::bounded(subject, 300).to_owned();
    }
    let subject = str_at(
        value,
        &[
            "subject",
            "subject_line",
            "title",
            "name",
            "summary",
            "item_name",
        ],
    );
    if !subject.is_empty() {
        return runtime::bounded(subject, 300).to_owned();
    }
    tool_name(args)
        .split("__")
        .last()
        .unwrap_or("Connector activity")
        .replace('_', " ")
}
pub fn record(c: &Connection, id: &str) -> Result<Value> {
    let (body, run, chat, bot, revision, status, approval, created): (String,String,String,String,i64,String,String,i64) = c.query_row(
        "SELECT body,run_id,chat_id,bot_id,revision,status,approval_id,created FROM connector_artifacts WHERE id=?", [id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).context("Connector card not found")?;
    let mut value: Value = serde_json::from_str(&body)?;
    for (k, v) in [
        ("id", json!(id)),
        ("run_id", json!(run)),
        ("chat_id", json!(chat)),
        ("bot_id", json!(bot)),
        ("revision", json!(revision)),
        ("status", json!(status)),
        ("approval_id", json!(approval)),
        ("created", json!(created)),
    ] {
        value[k] = v;
    }
    value["email"] = email_fields(&value["input"]);
    if crate::connector_edits::read_only(&value) {
        if let Some(fields) = value["email"].as_object_mut() {
            for field in fields.values_mut() {
                field["editable"] = json!(false);
            }
        }
    }
    value["edit_fields"] = crate::connector_edits::fields(&value);
    value["preview_records"] = json!(crate::connector_records::extract(
        value["connector"].as_str().unwrap_or(""),
        &value["input"]
    ));
    Ok(value)
}
pub fn create(db: &Db, run: &Run, args: &Value) -> Result<String> {
    ensure!(input(args).is_object(), "Invalid connector inputs");
    ensure!(
        input(args).to_string().len() <= 200_000,
        "Connector input too large for review"
    );
    let id = db::id();
    let body = json!({"connector":service(args),"connection":str_at(args,&["connection","toolkit"]),"source":match args["origin"].as_str(){Some("claude-account")=>"Claude",Some("codex-account")=>"Codex",_=>"Kindred"},"tool":tool_name(args),"kind":kind(args),"title":title(args),"email_send":email_send(args),"account":str_at(args,&["account_name","account_id"]),"original_input":input(args),"input":input(args),"records":[],"forced":args["forced"]==true,"read_only":args["read_only"]==true,"display_read_only":args["read_only"]==true || (args["origin"]=="claude-account" && read_only_hint(args))});
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    tx.execute("INSERT INTO connector_artifacts(id,run_id,chat_id,bot_id,body,status,created) VALUES(?,?,?,?,?,'preparing',?)",params![id,run.id,run.chat_id,run.bot_id,body.to_string(),db::now()])?;
    {
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'connector_artifact',?,?)",params![run.chat_id,run.bot_id,id,run.id,db::now()])?;
    }
    tx.commit()?;
    Ok(id)
}
pub async fn review(
    app: &App,
    bot: &Bot,
    run: &Run,
    tool: &str,
    args: &mut Value,
    force: bool,
) -> Result<bool> {
    let id = args["artifact_id"]
        .as_str()
        .context("Connector card missing")?
        .to_owned();
    let allowed = runtime::approve_required(app, bot, run, tool, args, force).await?;
    let value = record(&app.db.0.lock().unwrap(), &id)?;
    if !allowed {
        let feedback = value["feedback"].as_str().unwrap_or("");
        if !feedback.is_empty() {
            anyhow::bail!(
                "The user requested changes before this action. Nothing was sent by this call. Feedback: {feedback}. Prepare a revised action for review; do not repeat the unchanged action."
            );
        }
        set_status(&app.db, &id, "denied")?;
        return Ok(false);
    }
    let key = input_key(args);
    args[key] = value["input"].clone();
    set_status(&app.db, &id, "ready")?;
    Ok(true)
}
pub fn set_status(db: &Db, id: &str, status: &str) -> Result<()> {
    db.0.lock().unwrap().execute(
        "UPDATE connector_artifacts SET status=? WHERE id=?",
        params![status, id],
    )?;
    Ok(())
}
// Claim the exact reviewed call once. A changed permission can stop an automatic
// dispatch, but cannot erase the person's explicit approval for this one call.
pub fn dispatch(db: &Db, run: &Run, args: &Value) -> Result<()> {
    let id = args["artifact_id"]
        .as_str()
        .context("Connector card missing")?;
    let c = db.0.lock().unwrap();
    let explicit: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM connector_artifacts f JOIN approvals a ON a.id=f.approval_id WHERE f.id=? AND a.status='approved')",[id],|r|r.get(0))?;
    if !explicit && args["read_only"] != true {
        ensure!(
            crate::connector_policy::approval_override_locked(&c, &run.bot_id, args)?
                .unwrap_or(true),
            "Sending permission changed before dispatch. Review this action again."
        );
    }
    let changed = c.execute("UPDATE connector_artifacts SET status='executing' WHERE id=? AND run_id=? AND status='ready' AND EXISTS(SELECT 1 FROM runs WHERE id=? AND status='running')",params![id,run.id,run.id])?;
    ensure!(
        changed == 1,
        "This connector action is no longer ready to execute"
    );
    Ok(())
}
fn text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        runtime::bounded(s, 16000).to_owned()
    } else if value.is_number() || value.is_boolean() {
        value.to_string()
    } else {
        String::new()
    }
}
fn received_email(value: &Value) -> Option<Value> {
    let mut email = serde_json::Map::new();
    if let Some(headers) = value["payload"]["headers"].as_array() {
        for header in headers {
            let name = header["name"].as_str().unwrap_or("").to_ascii_lowercase();
            if matches!(name.as_str(), "subject" | "from" | "to" | "cc" | "bcc") {
                if let Some(v) = header["value"].as_str() {
                    email.insert(name, json!(runtime::bounded(v, 8000)));
                }
            }
        }
        fn body(part: &Value, depth: usize) -> Option<(String, bool)> {
            use base64::Engine;
            if depth > 5 {
                return None;
            }
            let mime = part["mimeType"].as_str().unwrap_or("");
            if matches!(mime, "text/plain" | "text/html") {
                if let Some(data) = part["body"]["data"].as_str() {
                    if data.len() < 200_000 {
                        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .decode(data.trim_end_matches('='))
                            .ok()?;
                        let text = String::from_utf8(bytes).ok()?;
                        return Some((
                            runtime::bounded(&text, 16000).to_owned(),
                            mime == "text/html",
                        ));
                    }
                }
            }
            let parts = part["parts"].as_array()?;
            for part in parts
                .iter()
                .filter(|p| p["mimeType"] == "text/plain")
                .chain(parts.iter().filter(|p| p["mimeType"] != "text/plain"))
            {
                if let Some(v) = body(part, depth + 1) {
                    return Some(v);
                }
            }
            None
        }
        if let Some((text, html)) = body(&value["payload"], 0) {
            email.insert(if html { "body_html" } else { "body" }.into(), json!(text));
        }
    } else if value.get("subject").is_some()
        && (value.get("from").is_some()
            || value.get("sender").is_some()
            || value.get("to").is_some()
            || value.get("toRecipients").is_some())
    {
        fn address(v: &Value) -> String {
            if let Some(a) = v.as_array() {
                return a
                    .iter()
                    .map(address)
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
            }
            if let Some(s) = v.as_str() {
                return s.to_owned();
            }
            let e = v.get("emailAddress").unwrap_or(v);
            let a = str_at(e, &["address", "email"]);
            let n = str_at(e, &["name"]);
            if !a.is_empty() && !n.is_empty() {
                format!("{n} <{a}>")
            } else {
                a.into()
            }
        }
        for (label, keys) in [
            ("subject", &["subject"][..]),
            ("from", &["from", "sender"][..]),
            ("to", &["to", "toRecipients"][..]),
            ("cc", &["cc", "ccRecipients"][..]),
        ] {
            if let Some(v) = keys.iter().find_map(|k| value.get(*k)) {
                let text = if label == "subject" {
                    text(v)
                } else {
                    address(v)
                };
                if !text.is_empty() {
                    email.insert(label.into(), json!(text));
                }
            }
        }
        if let Some(body) = value.get("body") {
            if body.is_string() {
                email.insert("body".into(), json!(text(body)));
            } else if let Some(content) = body["content"].as_str() {
                email.insert(
                    if body["contentType"]
                        .as_str()
                        .unwrap_or("")
                        .eq_ignore_ascii_case("html")
                    {
                        "body_html"
                    } else {
                        "body"
                    }
                    .into(),
                    json!(runtime::bounded(content, 16000)),
                );
            }
        }
        if !email.contains_key("body") && !email.contains_key("body_html") {
            if let Some(snippet) = value.get("snippet").or_else(|| value.get("bodyPreview")) {
                email.insert("body".into(), json!(text(snippet)));
            }
        }
    }
    if email.is_empty() {
        None
    } else {
        Some(json!(email))
    }
}
fn extract(value: &Value, out: &mut Vec<Value>, depth: usize) {
    if depth > 8 || out.len() >= 30 {
        return;
    }
    if let Some(s) = value.as_str() {
        if s.len() <= 200_000 {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                extract(&v, out, depth + 1);
            }
        }
        return;
    }
    if let Some(items) = value.as_array() {
        for item in items.iter().take(30) {
            extract(item, out, depth + 1);
        }
        return;
    }
    let Some(map) = value.as_object() else {
        return;
    };
    if let Some(email) = received_email(value) {
        out.push(json!({"title":email["subject"].as_str().unwrap_or("Email without a subject"),"email":email_fields(&email),"fields":{"id":value["id"]},"lines":[]}));
        return;
    }
    let nested = value
        .get("fields")
        .filter(|v| v.is_object())
        .unwrap_or(value);
    let heading = str_at(value, &["subject", "title", "name", "Summary", "DocNumber"]);
    let heading = if heading.is_empty() {
        str_at(nested, &["summary", "title", "name"])
    } else {
        heading
    };
    if !heading.is_empty() {
        let mut fields = serde_json::Map::new();
        for k in [
            "id",
            "key",
            "status",
            "state",
            "assignee",
            "due_date",
            "dueDate",
            "DueDate",
            "DocNumber",
            "TotalAmt",
            "Balance",
            "CurrencyRef",
            "CustomerRef",
            "customer",
            "board",
            "board_id",
            "url",
            "webUrl",
            "web_url",
            "description",
            "snippet",
            "body",
            "content",
            "from",
            "to",
            "subject",
        ] {
            let v = value.get(k).or_else(|| nested.get(k));
            if let Some(v) = v {
                let s = if v.is_object() {
                    str_at(v, &["name", "displayName", "value", "text"]).to_owned()
                } else {
                    text(v)
                };
                if !s.is_empty() {
                    fields.insert(k.into(), json!(s));
                }
            }
        }
        if let Some(columns) = value["column_values"].as_array() {
            for col in columns.iter().take(12) {
                let key = str_at(col, &["title", "id"]);
                let val = str_at(col, &["text"]);
                if !key.is_empty() && !val.is_empty() {
                    fields.insert(key.into(), json!(runtime::bounded(val, 1000)));
                }
            }
        }
        let lines=value["Line"].as_array().into_iter().flatten().take(30).filter_map(|line| {
            let detail=&line["SalesItemLineDetail"];if line["DetailType"]!="SalesItemLineDetail"{return None;}
            Some(json!({"description":str_at(line,&["Description"]),"quantity":detail["Qty"],"rate":detail["UnitPrice"],"amount":line["Amount"]}))
        }).collect::<Vec<_>>();
        out.push(json!({"title":runtime::bounded(heading,300),"fields":fields,"lines":lines}));
    }
    for key in [
        "data",
        "value",
        "results",
        "items",
        "issues",
        "nodes",
        "edges",
        "node",
        "pages",
        "messages",
        "emails",
        "Invoice",
        "QueryResponse",
        "invoices",
        "content",
        "text",
        "body",
        "storage",
    ] {
        if let Some(v) = map.get(key) {
            extract(v, out, depth + 1);
        }
    }
}
pub fn complete(db: &Db, id: &str, result: &Value, failed: bool) -> Result<()> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let mut value = record(&tx, id)?;
    let mut records = Vec::new();
    if !failed {
        records =
            crate::connector_records::extract(value["connector"].as_str().unwrap_or(""), result);
        if records.is_empty() {
            extract(result, &mut records, 0);
        }
    }
    // Also publish receipts for calls created by an older server before upgrade.
    tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT ?,?,?,'connector_artifact',?,? WHERE NOT EXISTS(SELECT 1 FROM chat_messages WHERE kind='connector_artifact' AND body=?)",params![value["chat_id"].as_str(),value["bot_id"].as_str(),id,value["run_id"].as_str(),value["created"].as_i64().unwrap_or_else(db::now),id])?;
    value["records"] = json!(records);
    value["outcome"] = json!(if failed {
        "The connector returned an error. Verify external state before retrying a change."
    } else {
        "The connector returned successfully."
    });
    tx.execute("UPDATE connector_artifacts SET body=?,status=?,revision=revision+1 WHERE id=? AND status NOT IN ('denied','changes_requested')",params![value.to_string(),if failed{"failed"}else{"completed"},id])?;
    tx.commit()?;
    Ok(())
}
#[cfg(test)]
fn apply_email_edit(current: &Value, edits: &Value) -> Result<Value> {
    crate::connector_edits::apply(&json!({"kind":"email","input":current}), edits)
}
pub async fn action(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(update(&app, &id, &v)?))
}
pub fn update(app: &App, id: &str, v: &Value) -> Result<Value> {
    let revision = v["revision"]
        .as_i64()
        .context("Review the current card before changing it")?;
    let action = v["action"].as_str().unwrap_or("");
    if action == "edit" {
        let mut c = app.db.0.lock().unwrap();
        let tx = c.transaction()?;
        let mut card = record(&tx, &id)?;
        ensure!(
            card["revision"] == revision && card["status"] == "pending",
            "This draft changed or is no longer waiting. Reload the current version."
        );
        let next = crate::connector_edits::apply(&card, &v["fields"])?;
        let approval = card["approval_id"].as_str().unwrap_or("").to_owned();
        let raw:String=tx.query_row("SELECT args FROM approvals WHERE id=? AND status='pending' AND run_id IN(SELECT id FROM runs WHERE status='awaiting_approval')",[&approval],|r|r.get(0)).context("This approval has ended")?;
        let mut args: Value = serde_json::from_str(&raw)?;
        let key = input_key(&args);
        args[key] = next.clone();
        card["title"] = json!(title(&args));
        card["input"] = next;
        card["edited_by_user"] = json!(true);
        tx.execute(
            "UPDATE approvals SET args=? WHERE id=?",
            params![args.to_string(), approval],
        )?;
        tx.execute(
            "UPDATE connector_artifacts SET body=?,revision=revision+1 WHERE id=?",
            params![card.to_string(), id],
        )?;
        tx.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'connector_draft_edited',?,?)",
            params![
                card["run_id"].as_str(),
                json!({"artifact_id":id,"revision":revision+1,"fields":v["fields"]}).to_string(),
                db::now()
            ],
        )?;
        tx.commit()?;
    } else {
        ensure!(
            matches!(action, "approve" | "deny" | "changes"),
            "Unsupported connector card action"
        );
        let card = record(&app.db.0.lock().unwrap(), &id)?;
        let feedback = if action == "changes" {
            Some(
                v["feedback"]
                    .as_str()
                    .context("Describe the requested changes")?,
            )
        } else {
            None
        };
        crate::connector_policy::decide_checked(
            &app.db,
            card["approval_id"].as_str().unwrap_or(""),
            action == "approve",
            v["choice"].as_str().unwrap_or(""),
            Some(revision),
            feedback,
        )?;
    }
    record(&app.db.0.lock().unwrap(), id)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Shared, Bot, Run, Value, String) {
        fixture_in_chat(false)
    }
    fn fixture_in_chat(group: bool) -> (Shared, Bot, Run, Value, String) {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        crate::connector_policy::catalogue(&app.db,&"a".repeat(64),&json!([{"origin":"claude-account","name":"claude.ai Gmail","display_name":"Gmail","connector_key":"gmail-1","status":"connected","tools":["mcp__claude_ai_Gmail__send_email"]}])).unwrap();
        let run = if group {
            let helper = crate::tests::bot(&app.db, "claude-code");
            let chat = crate::chats::Chat {
                id: crate::db::id(), name: "Review team".into(),
                description: "Review drafts before sending.".into(),
                members: vec![bot.id.clone(), helper.id.clone()],
                bot_only: false, archived: false, pinned: false, last_message: None,
            };
            app.db.save_chat(&chat).unwrap();
            app.db.chat_send(&chat.id, "Send the reviewed email", &[bot.id.clone()]).unwrap();
            app.db.claim_bot(&bot.id).unwrap().unwrap()
        } else {
            let id = app.db.queue(&bot.id, "Send the reviewed email", 0).unwrap();
            app.db.claim().unwrap();
            app.db.run(&id).unwrap()
        };
        let mut args = json!({"origin":"claude-account","account_key":"a".repeat(64),"connector_key":"gmail-1","connection":"claude.ai Gmail","tool_name":"mcp__claude_ai_Gmail__send_email","input":{"to":["review@example.invalid"],"subject":"Review draft","body":"Original body","thread_id":"preserve-thread","attachments":[{"id":"preserve-attachment"}]}});
        let id = create(&app.db, &run, &args).unwrap();
        args["artifact_id"] = json!(id);
        (app, bot, run, args, id)
    }
    fn pending(app: &App, run: &Run, args: &Value) -> i64 {
        app.db
            .request_approval(&run.id, "claude_connector", args)
            .unwrap();
        record(
            &app.db.0.lock().unwrap(),
            args["artifact_id"].as_str().unwrap(),
        )
        .unwrap()["revision"]
            .as_i64()
            .unwrap()
    }
    #[tokio::test]
    async fn edited_review_dispatches_exact_input_once_and_survives_chat_readback() {
        review_dispatch_roundtrip(false).await;
    }
    #[tokio::test]
    async fn group_connector_review_dispatches_exact_input_once_and_stays_in_group() {
        review_dispatch_roundtrip(true).await;
    }
    async fn review_dispatch_roundtrip(group: bool) {
        let (app, bot, run, mut args, id) = fixture_in_chat(group);
        let (a, b, r, mut v) = (app.clone(), bot.clone(), run.clone(), args.clone());
        let task = tokio::spawn(async move {
            let ok = review(&a, &b, &r, "claude_connector", &mut v, false)
                .await
                .unwrap();
            (ok, v)
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if !app.db.approvals().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let revision = record(&app.db.0.lock().unwrap(), &id).unwrap()["revision"]
            .as_i64()
            .unwrap();
        let next=update(&app,&id,&json!({"action":"edit","revision":revision,"fields":{"to":"one@example.invalid, two@example.invalid","subject":"Approved subject","body":"User edited body"}})).unwrap();
        assert_eq!(next["status"], "pending");
        assert!(update(&app, &id, &json!({"action":"approve","revision":revision})).is_err());
        let aid = next["approval_id"].as_str().unwrap();
        assert!(crate::connector_policy::decide(&app.db, aid, true, "").is_err());
        update(
            &app,
            &id,
            &json!({"action":"approve","revision":revision+1}),
        )
        .unwrap();
        let (ok, effective) = task.await.unwrap();
        assert!(ok);
        args = effective;
        assert_eq!(args["input"]["body"], "User edited body");
        assert_eq!(args["input"]["subject"], "Approved subject");
        assert_eq!(
            args["input"]["to"],
            json!(["one@example.invalid", "two@example.invalid"])
        );
        assert_eq!(args["input"]["thread_id"], "preserve-thread");
        assert_eq!(args["input"]["attachments"][0]["id"], "preserve-attachment");
        dispatch(&app.db, &run, &args).unwrap();
        assert!(dispatch(&app.db, &run, &args).is_err());
        complete(&app.db, &id, &json!({"id":"sent-fixture"}), false).unwrap();
        let cards = app.db.chat_messages(&run.chat_id).unwrap();
        let card = cards
            .iter()
            .find(|m| m["kind"] == "connector_artifact")
            .unwrap();
        assert_eq!(cards.iter().filter(|m| m["kind"] == "connector_artifact").count(), 1);
        if group {
            assert!(!run.chat_id.starts_with("dm-"));
            let outside: i64 = app.db.0.lock().unwrap().query_row(
                "SELECT count(*) FROM chat_messages WHERE run_id=?1 AND chat_id!=?2",
                rusqlite::params![run.id, run.chat_id], |row| row.get(0),
            ).unwrap();
            assert_eq!(outside, 0, "Group connector receipts must not leak into other chats");
        }
        assert_eq!(card["connector_artifact"]["status"], "completed");
        assert_eq!(
            card["connector_artifact"]["input"]["body"],
            "User edited body"
        );
        assert_eq!(
            record(&app.db.0.lock().unwrap(), &id).unwrap()["original_input"]["body"],
            "Original body"
        );
    }
    #[test]
    fn catalogue_records_survive_completion_and_history() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../tools/frontend/fixtures/connector-records.json"
        ))
        .unwrap();
        assert_eq!(cases.len(), crate::connector_records::catalog().len());
        let mut rendered = Vec::new();
        for case in &cases {
            let (app, _, run, _, _) = fixture();
            let args = json!({"toolkit":case["service"],"tool_slug":"GET_RECORD","input":{},"read_only":true});
            let id = create(&app.db, &run, &args).unwrap();
            complete(&app.db, &id, &case["result"], false).unwrap();
            let card = record(&app.db.0.lock().unwrap(), &id).unwrap();
            assert_eq!(
                card["records"][0]["title"], case["title"],
                "{}: {}",
                case["service"], card["records"]
            );
            assert!(
                card["records"]
                    .to_string()
                    .contains(case["contains"].as_str().unwrap()),
                "{} missing {}",
                case["service"],
                case["contains"]
            );
            assert!(!card["records"].to_string().contains("secret-host-token"));
            assert!(!card["records"].to_string().contains("secret-password"));
            assert_eq!(card["status"], "completed");
            rendered.push(card);
            complete(&app.db, &id, &case["result"], true).unwrap();
            let failed = record(&app.db.0.lock().unwrap(), &id).unwrap();
            assert_eq!(failed["status"], "failed");
            assert_eq!(failed["records"], json!([]));
        }
        if let Ok(path) = std::env::var("KINDRED_CONNECTOR_FIXTURE_OUTPUT") {
            // Stable fixture identities; all presented fields above came from
            // the actual completion/readback path, including new adapters.
            for card in &mut rendered {
                card["id"] = json!(format!("{}-fixture", card["connector"].as_str().unwrap()));
                card["bot_id"] = json!("piper");
                card["chat_id"] = json!("dm-piper");
                card["run_id"] = json!("run-fixture");
                card["created"] = json!(1789050000);
            }
            std::fs::write(path, serde_json::to_vec_pretty(&rendered).unwrap()).unwrap();
        }
    }
    #[tokio::test]
    async fn structured_and_non_email_drafts_use_revision_review_and_exact_dispatch() {
        let cases = [
            (
                "Outlook",
                "sendMail",
                json!({"message":{"subject":"Draft","body":{"contentType":"HTML","content":"<p>Old</p>"},"toRecipients":[{"emailAddress":{"address":"review@example.invalid"}}],"attachments":[{"id":"keep"}]},"saveToSentItems":false}),
                json!({"subject":"Reviewed","body":"<p>Approved</p>"}),
            ),
            (
                "Asana",
                "create_task",
                json!({"name":"Draft","notes":"Original","project_id":"keep-project","assignee":"keep-owner"}),
                json!({"name":"Reviewed task","notes":"Reviewed notes"}),
            ),
            (
                "Google Calendar",
                "create_event",
                json!({"summary":"Draft","description":"Original","start":{"dateTime":"2026-09-20T10:00:00-04:00","timeZone":"America/New_York"},"calendar_id":"keep-calendar"}),
                json!({"summary":"Reviewed meeting"}),
            ),
            (
                "Slack",
                "post_message",
                json!({"text":"Draft","channel":"keep-channel","thread_ts":"keep-thread"}),
                json!({"text":"Reviewed message"}),
            ),
        ];
        for (service, tool, original, edits) in cases {
            let (app, bot, run, _, _) = fixture();
            let name = format!("mcp__app__{tool}");
            let connection = format!("claude.ai {service}");
            crate::connector_policy::catalogue(&app.db,&"a".repeat(64),&json!([{"origin":"claude-account","name":connection,"display_name":service,"connector_key":"review-1","status":"connected","tools":[name]}])).unwrap();
            let mut args = json!({"origin":"claude-account","account_key":"a".repeat(64),"connector_key":"review-1","connection":connection,"tool_name":name,"input":original});
            let id = create(&app.db, &run, &args).unwrap();
            args["artifact_id"] = json!(id);
            let (a, b, r, mut v) = (app.clone(), bot.clone(), run.clone(), args.clone());
            let task = tokio::spawn(async move {
                assert!(
                    review(&a, &b, &r, "claude_connector", &mut v, false)
                        .await
                        .unwrap()
                );
                v
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while app.db.approvals().unwrap().is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let before = record(&app.db.0.lock().unwrap(), &id).unwrap();
            assert!(dispatch(&app.db, &run, &args).is_err());
            let next = update(
                &app,
                &id,
                &json!({"action":"edit","revision":before["revision"],"fields":edits}),
            )
            .unwrap();
            assert_eq!(next["status"], "pending");
            assert_eq!(next["original_input"], original);
            assert!(
                update(
                    &app,
                    &id,
                    &json!({"action":"approve","revision":before["revision"]})
                )
                .is_err()
            );
            let approval = app
                .db
                .approvals()
                .unwrap()
                .into_iter()
                .find(|a| a["id"] == next["approval_id"])
                .unwrap();
            assert_eq!(approval["args"]["input"], next["input"]);
            update(
                &app,
                &id,
                &json!({"action":"approve","revision":next["revision"]}),
            )
            .unwrap();
            let effective = task.await.unwrap();
            assert_eq!(effective["input"], next["input"]);
            dispatch(&app.db, &run, &effective).unwrap();
            assert!(dispatch(&app.db, &run, &effective).is_err());
            assert!(
                update(
                    &app,
                    &id,
                    &json!({"action":"edit","revision":next["revision"],"fields":edits})
                )
                .is_err()
            );
            complete(&app.db, &id, &json!({"id":"fixture-result"}), false).unwrap();
            assert_eq!(
                record(&app.db.0.lock().unwrap(), &id).unwrap()["input"],
                effective["input"]
            );
        }
    }
    #[test]
    fn feedback_denial_and_cancel_cannot_dispatch_or_approve_again() {
        let (app, _, run, args, id) = fixture();
        let rev = pending(&app, &run, &args);
        update(&app,&id,&json!({"action":"changes","revision":rev,"feedback":"Shorter and more specific, please."})).unwrap();
        complete(&app.db, &id, &json!({"error":"request denied"}), true).unwrap();
        let card = record(&app.db.0.lock().unwrap(), &id).unwrap();
        assert_eq!(card["status"], "changes_requested");
        assert!(card["feedback"].as_str().unwrap().contains("Shorter"));
        assert!(dispatch(&app.db, &run, &args).is_err());
        assert!(update(&app, &id, &json!({"action":"approve","revision":rev+1})).is_err());
        let (app, _, run, args, id) = fixture();
        let rev = pending(&app, &run, &args);
        app.db.cancel(&run.id).unwrap();
        assert!(update(&app, &id, &json!({"action":"approve","revision":rev})).is_err());
        assert!(
            update(
                &app,
                &id,
                &json!({"action":"edit","revision":rev,"fields":{"body":"Too late"}})
            )
            .is_err()
        );
        assert!(dispatch(&app.db, &run, &args).is_err());
    }
    #[test]
    fn email_permission_is_send_only_and_scoped_and_revocation_wins() {
        let (app, bot, run, args, id) = fixture();
        let rev = pending(&app, &run, &args);
        update(
            &app,
            &id,
            &json!({"action":"approve","revision":rev,"choice":"always_allow_email"}),
        )
        .unwrap();
        assert_eq!(
            crate::connector_policy::approval_override(&app.db, &bot.id, &args).unwrap(),
            Some(true)
        );
        for (key, value) in [
            ("tool_name", json!("mcp__claude_ai_Gmail__delete_email")),
            ("account_key", json!("b".repeat(64))),
            ("connector_key", json!("gmail-other")),
            ("forced", json!(true)),
        ] {
            let mut changed = args.clone();
            changed[key] = value;
            assert_eq!(
                crate::connector_policy::approval_override(&app.db, &bot.id, &changed).unwrap(),
                Some(false)
            );
        }
        let other = crate::tests::bot(&app.db, "claude-code");
        assert_eq!(
            crate::connector_policy::approval_override(&app.db, &other.id, &args).unwrap(),
            Some(false)
        );
        let mut revoke = args.clone();
        revoke["request"] = json!("email_sending");
        revoke["always_allow"] = json!(false);
        crate::connector_policy::save_settings(&app, &bot.id, &revoke).unwrap();
        assert_eq!(
            crate::connector_policy::approval_override(&app.db, &bot.id, &args).unwrap(),
            Some(false)
        );
        // The approved current action is still valid; a new automatic action is stopped.
        set_status(&app.db, &id, "ready").unwrap();
        dispatch(&app.db, &run, &args).unwrap();
        let mut fresh = args.clone();
        let next = create(&app.db, &run, &fresh).unwrap();
        fresh["artifact_id"] = json!(next);
        set_status(&app.db, &next, "ready").unwrap();
        assert!(dispatch(&app.db, &run, &fresh).is_err());
        let c = app.db.0.lock().unwrap();
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM connector_grants", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn presentation_hints_do_not_grant_read_permission_and_pending_reads_are_visible() {
        let (app, bot, run, mut args, _) = fixture();
        args["tool_name"] = json!("mcp__claude_ai_Gmail__search");
        let id = create(&app.db, &run, &args).unwrap();
        args["artifact_id"] = json!(id);
        assert_eq!(
            crate::connector_policy::approval_override(&app.db, &bot.id, &args).unwrap(),
            Some(false)
        );
        assert!(
            app.db
                .chat_messages(&run.chat_id)
                .unwrap()
                .iter()
                .any(|m| m["connector_artifact"]["id"] == id)
        );
        pending(&app, &run, &args);
        assert!(
            app.db
                .chat_messages(&run.chat_id)
                .unwrap()
                .iter()
                .any(|m| m["connector_artifact"]["id"] == id)
        );
    }
    #[test]
    fn routine_reads_publish_live_and_empty_result_receipts_without_duplicates() {
        let (app, _, run, mut args, _) = fixture();
        app.db.0.lock().unwrap().execute("INSERT INTO routine_runs VALUES(?,'fixture',1)", [&run.id]).unwrap();
        args["read_only"] = json!(true);
        for service in ["Confluence", "Monday", "Slack", "Google_Calendar"] {
            args["tool_name"] = json!(format!("mcp__claude_ai_{service}__search"));
            let id = create(&app.db, &run, &args).unwrap();
            let messages = app.db.chat_messages(&run.chat_id).unwrap();
            assert!(messages.iter().any(|m| m["connector_artifact"]["id"] == id && m["connector_artifact"]["status"] == "preparing"));
            complete(&app.db, &id, &json!({"results":[]}), false).unwrap();
            complete(&app.db, &id, &json!({"results":[]}), false).unwrap();
            let messages = app.db.chat_messages(&run.chat_id).unwrap();
            let cards: Vec<_> = messages.iter().filter(|m| m["connector_artifact"]["id"] == id).collect();
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0]["connector_artifact"]["status"], "completed");
        }
    }
    #[test]
    fn normalization_preserves_source_fields_and_invoice_lines_without_invented_amounts() {
        let mut records = Vec::new();
        extract(
            &json!({"data":{"issues":[{"key":"DEMO-12","fields":{"summary":"Review launch","status":{"name":"In progress"},"assignee":{"displayName":"Casey"}}}],"items":[{"name":"Monday task","column_values":[{"id":"status","text":"Working on it"}]}],"QueryResponse":{"Invoice":[{"DocNumber":"INV-42","TotalAmt":300,"Balance":100,"CustomerRef":{"name":"Example Co"},"Line":[{"DetailType":"SalesItemLineDetail","Description":"Review","Amount":300,"SalesItemLineDetail":{"Qty":2,"UnitPrice":150}}]}]}}}),
            &mut records,
            0,
        );
        assert_eq!(records.len(), 3);
        let invoice = records.iter().find(|v| v["title"] == "INV-42").unwrap();
        assert_eq!(invoice["fields"]["TotalAmt"], "300");
        assert_eq!(invoice["lines"][0]["quantity"], 2);
        let issue = records
            .iter()
            .find(|v| v["title"] == "Review launch")
            .unwrap();
        assert_eq!(issue["fields"]["status"], "In progress");
        assert_eq!(issue["fields"]["assignee"], "Casey");
    }
    #[test]
    fn received_emails_decode_gmail_and_outlook_without_rendering_active_content() {
        let mut records = Vec::new();
        extract(
            &json!({"messages":[{"id":"mail-1","payload":{"mimeType":"multipart/alternative","headers":[{"name":"Subject","value":"Release review"},{"name":"From","value":"Casey <casey@example.invalid>"},{"name":"To","value":"jordan@example.invalid"}],"parts":[{"mimeType":"text/plain","body":{"data":"UmVhZHkgZm9yIHJldmlldy4"}}]}}]}),
            &mut records,
            0,
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["email"]["body"]["text"], "Ready for review.");
        assert_eq!(
            records[0]["email"]["from"]["text"],
            "Casey <casey@example.invalid>"
        );
        records.clear();
        extract(
            &json!({"subject":"Budget review","from":{"emailAddress":{"name":"Casey","address":"casey@example.invalid"}},"toRecipients":[{"emailAddress":{"address":"jordan@example.invalid"}}],"body":{"contentType":"HTML","content":"<p>Please review.</p>"}}),
            &mut records,
            0,
        );
        assert_eq!(records[0]["email"]["body"]["key"], "body_html");
        assert_eq!(records[0]["email"]["to"]["text"], "jordan@example.invalid");
    }
    #[test]
    fn saved_cards_and_quotes_transfer_without_sending_permissions_and_old_packages_import() {
        let (app, _, run, args, id) = fixture();
        let rev = pending(&app, &run, &args);
        update(
            &app,
            &id,
            &json!({"action":"approve","revision":rev,"choice":"always_allow_email"}),
        )
        .unwrap();
        set_status(&app.db, &id, "ready").unwrap();
        dispatch(&app.db, &run, &args).unwrap();
        complete(&app.db, &id, &json!({"id":"fixture-receipt"}), false).unwrap();
        app.db.finish(&run.id, "completed", "Sent", " ").unwrap();
        let card = app
            .db
            .chat_messages(&run.chat_id)
            .unwrap()
            .into_iter()
            .find(|m| m["kind"] == "connector_artifact")
            .unwrap();
        let quote = crate::message_actions::quoted_message(
            &app.db.0.lock().unwrap(),
            &run.chat_id,
            card["seq"].as_i64().unwrap(),
        )
        .unwrap();
        assert!(quote["text"].as_str().unwrap().contains("Original body"));
        assert!(quote["text"].as_str().unwrap().contains("completed"));
        let package = app.db.prepare_transfer(&db::id(), "Cards fixture").unwrap();
        let target = Db::open(":memory:").unwrap();
        target.import_transfer(&package).unwrap();
        assert_eq!(
            record(&target.0.lock().unwrap(), &id).unwrap()["status"],
            "completed"
        );
        assert!(package["tables"].get("connector_action_grants").is_none());
        assert_eq!(
            target
                .0
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM connector_action_grants", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let source = Db::open(":memory:").unwrap();
        let mut old = source.prepare_transfer(&db::id(), "Old format").unwrap();
        old["tables"]
            .as_object_mut()
            .unwrap()
            .remove("connector_artifacts");
        Db::open(":memory:").unwrap().import_transfer(&old).unwrap();
    }
    #[test]
    fn editing_rejects_header_injection_unknown_fields_and_send_classification_is_exact() {
        let (app, _, _, args, _) = fixture();
        drop(app);
        assert!(
            apply_email_edit(
                input(&args),
                &json!({"to":"ok@example.invalid\nBcc: bad@example.invalid"})
            )
            .is_err()
        );
        assert!(apply_email_edit(input(&args), &json!({"thread_id":"different"})).is_err());
        assert!(apply_email_edit(input(&args), &json!({"from":"spoof@example.invalid"})).is_err());
        assert!(email_send(&args));
        let mut a = args.clone();
        a["tool_name"] = json!("mcp__claude_ai_Gmail__get_send_email_settings");
        assert!(!email_send(&a));
        a["tool_name"] = json!("mcp__claude_ai_Gmail__send_email_and_delete_everything");
        assert!(!email_send(&a));
    }
}
