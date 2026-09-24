//! Conversation-owned lists and deterministic, one-time notification delivery.
use crate::{
    db::{self, Db, Run},
    runtime::Shared,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS checklists(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id),bot_id TEXT NOT NULL REFERENCES bots(id),run_id TEXT NOT NULL REFERENCES runs(id),request_key TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL,UNIQUE(chat_id,request_key));
    CREATE TABLE IF NOT EXISTS reminders(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id),bot_id TEXT NOT NULL REFERENCES bots(id),run_id TEXT NOT NULL REFERENCES runs(id),request_key TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,run_at INTEGER NOT NULL,status TEXT NOT NULL,created INTEGER NOT NULL,delivered_at INTEGER,UNIQUE(chat_id,request_key));
    CREATE INDEX IF NOT EXISTS reminders_due ON reminders(status,run_at);")?;
    Ok(())
}
fn text(v: &Value, key: &str, max: usize) -> Result<String> {
    let s = v[key].as_str().context(format!("Missing {key}"))?.trim();
    ensure!(
        !s.is_empty() && s.len() <= max,
        "{key} must contain 1 to {max} bytes"
    );
    Ok(s.to_owned())
}
fn sources(v: &mut Value) -> Result<()> {
    if v["sources"].is_null() {
        v["sources"] = json!([]);
    }
    let refs = v["sources"]
        .as_array()
        .context("Sources must be an array")?;
    ensure!(refs.len() <= 5, "Keep at most five source references");
    for source in refs {
        text(source, "title", 300)?;
        let raw = text(source, "url", 2000)?;
        let url = reqwest::Url::parse(&raw).context("Invalid source URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "Use a web source link without credentials"
        );
    }
    Ok(())
}
fn items(v: &mut Value, default_current: bool) -> Result<()> {
    let list = v["items"]
        .as_array_mut()
        .context("Items must be an array")?;
    ensure!(list.len() <= 100, "A checklist supports up to 100 items");
    let mut ids = std::collections::HashSet::new();
    let mut current = 0;
    for item in list.iter_mut() {
        item["title"] = json!(text(item, "title", 500)?);
        if item["id"].is_null() {
            item["id"] = json!(db::id());
        }
        let id = text(item, "id", 100)?;
        ensure!(ids.insert(id), "Checklist item IDs must be unique");
        if item["state"].is_null() {
            item["state"] = json!("pending");
        }
        ensure!(
            matches!(item["state"].as_str(), Some("pending" | "current" | "done")),
            "Invalid item state"
        );
        if item["owner"].is_null() {
            item["owner"] = json!("user");
        }
        ensure!(
            matches!(item["owner"].as_str(), Some("user" | "bot")),
            "Choose user or bot ownership"
        );
        if item["details"].is_null() {
            item["details"] = json!("");
        }
        ensure!(
            item["details"].as_str().is_some_and(|s| s.len() <= 4000),
            "Item details are too long"
        );
        sources(item)?;
        if item["state"] == "current" {
            current += 1;
        }
    }
    ensure!(current <= 1, "Only one checklist item can be current");
    if default_current && current == 0 {
        if let Some(item) = list.iter_mut().find(|i| i["state"] == "pending") {
            item["state"] = json!("current");
        }
    }
    Ok(())
}
fn allowed(c: &Connection, chat: &str, bot: Option<&str>, write: bool) -> Result<()> {
    let (members, archived): (String, bool) = c
        .query_row(
            "SELECT members,archived FROM chats WHERE id=?",
            [chat],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("Conversation not found")?;
    if let Some(bot) = bot {
        ensure!(
            serde_json::from_str::<Vec<String>>(&members)?
                .iter()
                .any(|id| id == bot),
            "This bot is not a member of the conversation"
        );
    }
    if write {
        ensure!(
            !archived && !crate::workspace_transfer::frozen(c)?,
            "Restore this conversation or resume the workspace before editing"
        );
    }
    Ok(())
}
pub fn record(c: &Connection, table: &str, id: &str) -> Result<Value> {
    ensure!(
        matches!(table, "checklists" | "reminders"),
        "Invalid planning record"
    );
    let (body, chat, bot, revision, created): (String, String, String, i64, i64) = c
        .query_row(
            &format!("SELECT body,chat_id,bot_id,revision,created FROM {table} WHERE id=?"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .context("List or reminder not found")?;
    let mut v: Value = serde_json::from_str(&body)?;
    v["id"] = json!(id);
    v["chat_id"] = json!(chat);
    v["bot_id"] = json!(bot);
    v["revision"] = json!(revision);
    v["created"] = json!(created);
    if table == "reminders" {
        let (at, status, delivered): (i64, String, Option<i64>) = c.query_row(
            "SELECT run_at,status,delivered_at FROM reminders WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        v["run_at"] = json!(at);
        v["status"] = json!(status);
        v["delivered_at"] = json!(delivered);
    }
    Ok(v)
}
fn card(c: &Connection, run: &Run, id: &str, kind: &str) -> Result<()> {
    c.execute(
        "INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,?, '',?)",
        params![run.chat_id, run.bot_id, id, kind, db::now()],
    )?;
    Ok(())
}
impl Db {
    pub fn tick_reminders(&self, time: i64) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if crate::workspace_transfer::frozen(&tx)? {
            return Ok(());
        }
        deliver(&tx, time)?;
        tx.commit()?;
        Ok(())
    }
    pub fn planning(&self, chat: &str, bot: Option<&str>) -> Result<Value> {
        if chat.is_empty() {
            return Ok(json!({"checklists":[],"reminders":[]}));
        }
        let c = self.0.lock().unwrap();
        allowed(&c, chat, bot, false)?;
        let mut out = json!({});
        for table in ["checklists", "reminders"] {
            let order = if table == "checklists" {
                "COALESCE(json_extract(body,'$.archived'),0),updated DESC"
            } else {
                "CASE WHEN status IN ('pending','paused') THEN 0 ELSE 1 END,run_at DESC"
            };
            let ids = c
                .prepare(&format!(
                    "SELECT id FROM {table} WHERE chat_id=? ORDER BY {order} LIMIT 100"
                ))?
                .query_map([chat], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out[table] = json!(
                ids.iter()
                    .map(|id| record(&c, table, id))
                    .collect::<Result<Vec<_>>>()?
            );
        }
        out["limit_per_kind"] = json!(100);
        Ok(out)
    }
    pub fn checklist_create(&self, run: &Run, mut v: Value) -> Result<Value> {
        let key = text(&v, "key", 100)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        allowed(&tx, &run.chat_id, Some(&run.bot_id), true)?;
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM checklists WHERE chat_id=? AND request_key=?",
                params![run.chat_id, key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return record(&tx, "checklists", &id);
        }
        v["title"] = json!(text(&v, "title", 200)?);
        if let Some(date) = v["local_date"].as_str() {
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .context("Use YYYY-MM-DD for the list date")?;
        }
        items(&mut v, true)?;
        v["archived"] = json!(false);
        let id = db::id();
        let now = db::now();
        tx.execute(
            "INSERT INTO checklists VALUES(?,?,?,?,?,1,?,?,?)",
            params![
                id,
                run.chat_id,
                run.bot_id,
                run.id,
                key,
                v.to_string(),
                now,
                now
            ],
        )?;
        card(&tx, run, &id, "checklist")?;
        let saved = record(&tx, "checklists", &id)?;
        tx.commit()?;
        Ok(saved)
    }
    pub fn checklist_update(
        &self,
        chat: &str,
        bot: Option<&str>,
        id: &str,
        patch: Value,
    ) -> Result<Value> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        allowed(&tx, chat, bot, true)?;
        let mut v = record(&tx, "checklists", id)?;
        ensure!(v["chat_id"] == chat, "This list is in another conversation");
        ensure!(
            patch["expected_revision"].as_i64() == v["revision"].as_i64(),
            "This list changed. Refresh its state before editing"
        );
        if !patch["title"].is_null() {
            v["title"] = json!(text(&patch, "title", 200)?);
        }
        if !patch["archived"].is_null() {
            v["archived"] = json!(
                patch["archived"]
                    .as_bool()
                    .context("Invalid archived state")?
            );
        }
        if !patch["items"].is_null() {
            v["items"] = patch["items"].clone();
        }
        if let Some(item_id) = patch["item_id"].as_str() {
            let state = text(&patch, "state", 10)?;
            ensure!(
                matches!(state.as_str(), "current" | "pending" | "done"),
                "Invalid item state"
            );
            let list = v["items"].as_array_mut().unwrap();
            let pos = list
                .iter()
                .position(|i| i["id"] == item_id)
                .context("Item no longer exists")?;
            let advance = list[pos]["state"] == "current" && state == "done";
            if state == "current" {
                for item in list.iter_mut() {
                    if item["state"] == "current" {
                        item["state"] = json!("pending");
                    }
                }
            }
            list[pos]["state"] = json!(state);
            if advance {
                if let Some(item) = list.iter_mut().find(|i| i["state"] == "pending") {
                    item["state"] = json!("current");
                }
            }
        }
        items(&mut v, false)?;
        tx.execute(
            "UPDATE checklists SET body=?,revision=revision+1,updated=? WHERE id=?",
            params![v.to_string(), db::now(), id],
        )?;
        // Keep one visible card at the latest edit, preserving old message IDs
        // for quotes while all versions resolve to the same checklist record.
        tx.execute("UPDATE chat_messages SET suppressed=1 WHERE chat_id=? AND kind='checklist' AND body=?", params![chat,id])?;
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'checklist','',?)", params![chat,v["bot_id"].as_str(),id,db::now()])?;
        let saved = record(&tx, "checklists", id)?;
        tx.commit()?;
        Ok(saved)
    }
    pub fn reminder_set(&self, run: &Run, mut v: Value) -> Result<Value> {
        let key = text(&v, "key", 100)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        allowed(&tx, &run.chat_id, Some(&run.bot_id), true)?;
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM reminders WHERE chat_id=? AND request_key=?",
                params![run.chat_id, key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return record(&tx, "reminders", &id);
        }
        v["message"] = json!(text(&v, "message", 2000)?);
        sources(&mut v)?;
        let at = reminder_time(&v)?;
        let id = db::id();
        tx.execute(
            "INSERT INTO reminders VALUES(?,?,?,?,?,1,?,?,'pending',?,NULL)",
            params![
                id,
                run.chat_id,
                run.bot_id,
                run.id,
                key,
                v.to_string(),
                at,
                db::now()
            ],
        )?;
        card(&tx, run, &id, "reminder_card")?;
        let saved = record(&tx, "reminders", &id)?;
        tx.commit()?;
        Ok(saved)
    }
    pub fn reminder_update(
        &self,
        chat: &str,
        bot: Option<&str>,
        id: &str,
        patch: Value,
    ) -> Result<Value> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        allowed(&tx, chat, bot, true)?;
        let mut v = record(&tx, "reminders", id)?;
        ensure!(
            v["chat_id"] == chat,
            "This reminder is in another conversation"
        );
        ensure!(
            patch["expected_revision"].as_i64() == v["revision"].as_i64(),
            "This reminder changed. Refresh its state before editing"
        );
        ensure!(
            v["status"] != "delivered",
            "This reminder has already been delivered. Create a new reminder for another occurrence"
        );
        let cancel = patch["cancel"] == true;
        if !patch["message"].is_null() {
            v["message"] = json!(text(&patch, "message", 2000)?);
        }
        if !patch["sources"].is_null() {
            v["sources"] = patch["sources"].clone();
            sources(&mut v)?;
        }
        let mut at = v["run_at"].as_i64().unwrap();
        let status = if cancel {
            "cancelled"
        } else if !patch["local_time"].is_null() {
            v["local_time"] = patch["local_time"].clone();
            if !patch["timezone"].is_null() {
                v["timezone"] = patch["timezone"].clone();
            }
            at = reminder_time(&v)?;
            "pending"
        } else {
            ensure!(
                v["status"] == "pending",
                "Supply a future local_time to resume this reminder"
            );
            "pending"
        };
        tx.execute(
            "UPDATE reminders SET body=?,run_at=?,status=?,revision=revision+1 WHERE id=?",
            params![v.to_string(), at, status, id],
        )?;
        let saved = record(&tx, "reminders", id)?;
        tx.commit()?;
        Ok(saved)
    }
}
fn reminder_time(v: &Value) -> Result<i64> {
    let zone = crate::timezone::parse(&text(v, "timezone", 100)?)?;
    let at = crate::timezone::resolve(&text(v, "local_time", 16)?, zone)?;
    ensure!(
        at > db::now(),
        "That reminder time has passed. Ask for a future time; do not silently move it to tomorrow"
    );
    Ok(at)
}
pub fn deliver(c: &Connection, time: i64) -> Result<()> {
    let ids=c.prepare("SELECT r.id FROM reminders r JOIN chats c ON c.id=r.chat_id JOIN bots b ON b.id=r.bot_id WHERE r.status='pending' AND r.run_at<=? AND c.archived=0 AND COALESCE(json_extract(b.profile,'$.archived'),0)=0 AND EXISTS(SELECT 1 FROM json_each(c.members) WHERE value=r.bot_id) ORDER BY r.run_at LIMIT 100")?.query_map([time],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        let v = record(c, "reminders", &id)?;
        let run: String = c.query_row("SELECT run_id FROM reminders WHERE id=?", [&id], |r| {
            r.get(0)
        })?;
        ensure!(c.execute("UPDATE reminders SET status='delivered',delivered_at=?,revision=revision+1 WHERE id=? AND status='pending'",params![time,id])?==1,"Reminder changed during delivery");
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'reminder','',?)",params![v["chat_id"].as_str(),v["bot_id"].as_str(),id,time])?;
        c.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'reminder',?,?)",
            params![
                run,
                json!({"id":id,"message":v["message"],"run_at":v["run_at"],"delivered_at":time})
                    .to_string(),
                time
            ],
        )?;
    }
    Ok(())
}
pub async fn list_route(
    State(app): State<Shared>,
    Path(chat): Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(app.db.planning(&chat, None)?))
}
pub async fn checklist_route(
    State(app): State<Shared>,
    Path((chat, id)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(app.db.checklist_update(&chat, None, &id, v)?))
}
pub async fn reminder_route(
    State(app): State<Shared>,
    Path((chat, id)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(app.db.reminder_update(&chat, None, &id, v)?))
}
