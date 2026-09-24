use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
pub struct Chat {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub bot_only: bool,
    pub members: Vec<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_deserializing)]
    pub last_message: Option<Value>,
}
// Preserve history while retiring bot-only rooms with no remaining active participant.
// Human conversations and server-managed membership are deliberately excluded.
pub(crate) fn archive_inactive_bot_chats(c: &Connection) -> Result<usize> {
    Ok(c.execute("UPDATE chats SET archived=1 WHERE archived=0 AND bot_only=1 AND id NOT LIKE 'dm-%' AND id NOT LIKE 'server-%' AND json_array_length(members)>0 AND NOT EXISTS(SELECT 1 FROM json_each(chats.members) m JOIN bots b ON b.id=m.value WHERE COALESCE(json_extract(b.profile,'$.archived'),0)=0) AND NOT EXISTS(SELECT 1 FROM runs WHERE chat_id=chats.id AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling'))", [])?)
}
pub(crate) fn record_recipient(c: &Connection, run: &str) -> Result<()> {
    c.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'recipient_selected','{}',?)", params![run,db::now()])?;
    Ok(())
}
pub fn migrate(c: &Connection) -> Result<()> {
    let transaction = c.unchecked_transaction()?;
    let c = &*transaction;
    for column in ["chat_id", "round_id", "reply_to"] {
        let exists = c
            .prepare("PRAGMA table_info(runs)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .contains(&column.to_string());
        if !exists {
            c.execute_batch(&format!(
                "ALTER TABLE runs ADD COLUMN {column} TEXT NOT NULL DEFAULT '';"
            ))?;
        }
    }
    c.execute_batch("CREATE TABLE IF NOT EXISTS chats(id TEXT PRIMARY KEY,name TEXT NOT NULL,members TEXT NOT NULL,archived INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS chat_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL REFERENCES chats(id),sender TEXT NOT NULL,body TEXT NOT NULL,kind TEXT NOT NULL,run_id TEXT NOT NULL DEFAULT '',created INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS message_reactions(message_seq INTEGER NOT NULL REFERENCES chat_messages(seq),bot_id TEXT NOT NULL REFERENCES bots(id),emoji TEXT NOT NULL,PRIMARY KEY(message_seq,bot_id));
      CREATE TABLE IF NOT EXISTS chat_completion_receipts(run_id TEXT PRIMARY KEY REFERENCES runs(id));
      CREATE TABLE IF NOT EXISTS chat_completion_pending(run_id TEXT PRIMARY KEY REFERENCES runs(id));
      CREATE TABLE IF NOT EXISTS chat_send_receipts(request_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id),body_hash BLOB NOT NULL,runs TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS chat_message_chat ON chat_messages(chat_id,seq);
      CREATE INDEX IF NOT EXISTS chat_message_timeline ON chat_messages(chat_id,created,seq);
      CREATE INDEX IF NOT EXISTS run_round ON runs(round_id);")?;
    for (table, column, definition) in [
        ("chats", "bot_only", "INTEGER NOT NULL DEFAULT 0"),
        ("chats", "pinned", "INTEGER NOT NULL DEFAULT 0"),
        ("chats", "description", "TEXT NOT NULL DEFAULT ''"),
        ("chat_messages", "source_event_seq", "INTEGER"),
        ("chat_messages", "history_order", "TEXT"),
        ("chat_messages", "suppressed", "INTEGER NOT NULL DEFAULT 0"),
        (
            "chat_messages",
            "linked_chat_id",
            "TEXT NOT NULL DEFAULT ''",
        ),
    ] {
        let exists = c
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == column);
        if !exists {
            c.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
            if table=="chats" && column=="bot_only" {
                c.execute("UPDATE chats SET bot_only=1 WHERE id LIKE 'team-%' AND NOT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=chats.id AND sender='user')",[])?;
            }
        }
    }
    c.execute_batch("INSERT OR IGNORE INTO chats(id,name,members) SELECT 'dm-'||id,name,json_array(id) FROM bots;
      INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT 'dm-'||bot_id,'user',prompt,'message','',created FROM runs WHERE chat_id='';
      INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT 'dm-'||bot_id,bot_id,CASE WHEN error='' THEN output ELSE error END,'result',id,created FROM runs WHERE chat_id='' AND status IN ('completed','failed','interrupted','cancelled');
      UPDATE runs SET chat_id='dm-'||bot_id,round_id=id WHERE chat_id='';")?;
    // Repair legacy repeated deliveries before linking results to their events.
    // Keep the original rows so reactions and quoted-message IDs remain valid.
    c.execute_batch("UPDATE chat_messages SET suppressed=1 WHERE kind='result' AND run_id!='' AND suppressed=0
      AND EXISTS(SELECT 1 FROM chat_messages earlier WHERE earlier.run_id=chat_messages.run_id AND earlier.chat_id=chat_messages.chat_id AND earlier.sender=chat_messages.sender AND earlier.kind='result' AND earlier.suppressed=0 AND earlier.seq<chat_messages.seq AND trim(earlier.body,char(9)||char(10)||char(13)||' ')=trim(chat_messages.body,char(9)||char(10)||char(13)||' '));")?;
    c.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS chat_message_source_event ON chat_messages(source_event_seq) WHERE source_event_seq IS NOT NULL;
      UPDATE chat_messages SET source_event_seq=(SELECT max(e.seq) FROM events e WHERE e.run_id=chat_messages.run_id AND e.kind='assistant' AND json_extract(e.body,'$.text')=chat_messages.body)
      WHERE kind='result' AND source_event_seq IS NULL AND suppressed=0
      AND NOT EXISTS(SELECT 1 FROM chat_messages linked WHERE linked.source_event_seq=(SELECT max(e.seq) FROM events e WHERE e.run_id=chat_messages.run_id AND e.kind='assistant' AND json_extract(e.body,'$.text')=chat_messages.body));
      UPDATE chat_messages SET created=max(created,COALESCE((SELECT max(e.created) FROM events e WHERE e.run_id=chat_messages.run_id AND e.kind='assistant'),created)) WHERE kind='result';
      UPDATE chat_messages SET history_order=printf('%020d:%020d',seq,COALESCE(source_event_seq,9223372036854775807)) WHERE kind='result' AND history_order IS NULL;
      INSERT OR IGNORE INTO chat_messages(chat_id,sender,body,kind,run_id,created,source_event_seq,history_order)
      SELECT r.chat_id,r.bot_id,json_extract(e.body,'$.text'),'assistant',r.id,e.created,e.seq,(SELECT printf('%020d:%020d',m.seq,e.seq) FROM chat_messages m WHERE m.run_id=r.id AND m.kind='result' LIMIT 1) FROM events e JOIN runs r ON r.id=e.run_id JOIN chats c ON c.id=r.chat_id
      WHERE e.kind='assistant' AND json_type(e.body,'$.text')='text' AND trim(json_extract(e.body,'$.text'))!=''
      AND NOT EXISTS(SELECT 1 FROM routine_runs rr WHERE rr.run_id=r.id)
      AND NOT EXISTS(SELECT 1 FROM chat_messages m WHERE m.run_id=r.id AND m.kind='result' AND instr(m.body,json_extract(e.body,'$.text'))>0) ORDER BY e.seq;
      UPDATE chat_messages SET kind='result' WHERE kind='assistant'
      AND source_event_seq=(SELECT max(e.seq) FROM events e WHERE e.run_id=chat_messages.run_id AND e.kind='assistant')
      AND EXISTS(SELECT 1 FROM runs r WHERE r.id=chat_messages.run_id AND r.status='completed' AND r.error='' AND r.output=chat_messages.body)
      AND NOT EXISTS(SELECT 1 FROM chat_messages m WHERE m.run_id=chat_messages.run_id AND m.kind='result');")?;
    c.execute_batch("CREATE INDEX IF NOT EXISTS chat_message_history_order ON chat_messages(chat_id,created,COALESCE(history_order,printf('%020d:%020d',seq,0)));")?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS chat_reads(chat_id TEXT PRIMARY KEY REFERENCES chats(id),message_seq INTEGER NOT NULL DEFAULT 0);
      CREATE INDEX IF NOT EXISTS chat_message_sender_time ON chat_messages(sender,created);")?;
    crate::bot_drafts::migrate(c)?;
    crate::uploads::migrate(c)?;
    crate::message_actions::migrate(c)?;
    crate::collaboration::migrate(c)?;
    transaction.commit()?;
    Ok(())
}
// Chat rows are a presentation of the immutable run/event history. Retain IDs,
// reactions and quoted-message references while hiding old duplicate deliveries.
pub fn repair_projection(c: &Connection) -> Result<()> {
    c.execute_batch("UPDATE chat_messages SET suppressed=1 WHERE kind='assistant' AND EXISTS(SELECT 1 FROM routine_runs rr WHERE rr.run_id=chat_messages.run_id);
      UPDATE chat_messages SET suppressed=1 WHERE kind='result' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=chat_messages.run_id AND r.status='completed' AND r.error='' AND (trim(chat_messages.body)='' OR EXISTS(SELECT 1 FROM routine_runs rr WHERE rr.run_id=r.id AND rr.quiet=1) OR EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind IN ('question_wait','process_wait'))));
      UPDATE chat_messages SET suppressed=1 WHERE kind='assistant' AND source_event_seq=(SELECT max(e.seq) FROM events e WHERE e.run_id=chat_messages.run_id AND e.kind='assistant') AND EXISTS(SELECT 1 FROM chat_messages result JOIN runs r ON r.id=result.run_id WHERE result.run_id=chat_messages.run_id AND result.kind='result' AND result.suppressed=0 AND r.status='completed' AND r.error='' AND trim(result.body,char(9)||char(10)||char(13)||' ')=trim(chat_messages.body,char(9)||char(10)||char(13)||' '));")?;
    crate::quiet_output::repair(c)?;
    crate::task_recovery::repair_legacy(c)?;
    Ok(())
}
pub(crate) fn insert_run(
    c: &Connection,
    chat: &Chat,
    target: &str,
    prompt: &str,
    round: &str,
    reply: &str,
    depth: i64,
) -> Result<String> {
    ensure!(
        chat.members.iter().any(|id| id == target) && !chat.archived,
        "Teammate is not in this active chat"
    );
    let archived: String = c.query_row("SELECT profile FROM bots WHERE id=?", [target], |r| {
        r.get(0)
    })?;
    ensure!(
        serde_json::from_str::<Value>(&archived)?["archived"] != true,
        "This bot is archived"
    );
    ensure!(
        !prompt.trim().is_empty() && prompt.len() <= 64000,
        "Message must be 1..64000 bytes"
    );
    let count: i64 = c.query_row("SELECT count(*) FROM runs WHERE round_id=?", [round], |r| {
        r.get(0)
    })?;
    ensure!(
        count < 24 && depth <= 12,
        "Collaboration reached its 24-turn or 12-level limit. Send a new message to continue."
    );
    let queued: i64 = c.query_row("SELECT count(*) FROM runs WHERE status='queued'", [], |r| {
        r.get(0)
    })?;
    ensure!(queued < 100, "Queue is full");
    let id = db::id();
    c.execute("INSERT INTO runs(id,bot_id,prompt,status,created,depth,chat_id,round_id,reply_to) VALUES(?,?,?,'queued',?,?,?,?,?)",params![id,target,prompt,db::now(),depth,chat.id,round,reply])?;
    Ok(id)
}
impl Db {
    pub fn react(&self, chat: &str, bot: &str, message: i64, emoji: &str) -> Result<()> {
        ensure!(
            ["👍", "❤️", "🎉", "👀", "✅", "🙏", "😂"].contains(&emoji),
            "Unsupported reaction"
        );
        let c = self.0.lock().unwrap();
        let belongs: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE seq=? AND chat_id=?)",
            params![message, chat],
            |r| r.get(0),
        )?;
        ensure!(belongs, "Message does not belong to this chat");
        c.execute("INSERT INTO message_reactions VALUES(?,?,?) ON CONFLICT(message_seq,bot_id) DO UPDATE SET emoji=excluded.emoji",params![message,bot,emoji])?;
        Ok(())
    }
    pub fn chats(&self) -> Result<Vec<Chat>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .prepare("SELECT c.id,c.name,c.members,c.archived,c.pinned,(SELECT json_object('sender',m.sender,'text',CASE WHEN m.kind='connection_card' THEN 'Connect account' WHEN m.kind='workspace_artifact' THEN COALESCE((SELECT json_extract(body,'$.title') FROM workspace_artifacts WHERE id=m.body),'Shared document') WHEN m.kind='checklist' THEN COALESCE((SELECT json_extract(body,'$.title') FROM checklists WHERE id=m.body),m.body) WHEN m.kind IN ('reminder_card','reminder') THEN COALESCE((SELECT json_extract(body,'$.message') FROM reminders WHERE id=m.body),m.body) WHEN m.kind='question' THEN COALESCE((SELECT CASE WHEN q.status='answered' THEN q.question||' '||q.answer ELSE q.question END FROM questions q WHERE q.id=m.body),m.body) ELSE m.body END,'created',m.created,'kind',m.kind) FROM chat_messages m WHERE m.chat_id=c.id AND m.suppressed=0 AND trim(m.body)!='' AND m.kind NOT IN ('notice','collaboration','connector_artifact') ORDER BY m.created DESC,COALESCE(m.history_order,printf('%020d:%020d',m.seq,0)) DESC LIMIT 1),c.description,c.bot_only FROM chats c ORDER BY c.rowid DESC")?
            .query_map([], |r| {
                Ok(Chat {
                    bot_only: r.get(7)?,
                    description: r.get(6)?,
                    id: r.get(0)?,
                    name: r.get(1)?,
                    members: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    archived: r.get(3)?,
                    pinned: r.get(4)?,
                    last_message: r.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str(&s).ok()),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn chat(&self, id: &str) -> Result<Chat> {
        self.chats()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("Chat not found"))
    }
    pub fn save_chat(&self, chat: &Chat) -> Result<()> {
        ensure!(
            !chat.name.trim().is_empty() && chat.name.len() <= 100,
            "Chat name must be 1..100 bytes"
        );
        ensure!(chat.description.chars().count() <= 2000, "Chat description must be at most 2000 characters");
        ensure!((1..=6).contains(&chat.members.len()), "Choose 1 to 6 bots");
        let c = self.0.lock().unwrap();
        let existing_members: Option<String> = c.query_row("SELECT members FROM chats WHERE id=?", [&chat.id], |r| r.get(0)).optional()?;
        let archiving_existing = chat.archived && existing_members.as_deref().is_some_and(|members| serde_json::from_str::<Vec<String>>(members).ok().as_ref() == Some(&chat.members));
        let mut unique = std::collections::HashSet::new();
        for id in &chat.members {
            ensure!(unique.insert(id), "Choose distinct bots");
            if !archiving_existing {
                let active: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND COALESCE(json_extract(profile,'$.archived'),0)=0)", [id], |r| r.get(0))?;
                ensure!(active, "Choose active bots; restore archived members before reopening this chat");
            }
        }
        let busy:i64=c.query_row("SELECT count(*) FROM runs WHERE chat_id=? AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')",[&chat.id],|r|r.get(0))?;
        let metadata_only = c.query_row("SELECT members=?2 AND archived=?3 FROM chats WHERE id=?1",params![chat.id,serde_json::to_string(&chat.members)?,chat.archived],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false);
        ensure!(
            busy == 0 || metadata_only,
            "Wait for this chat's work to finish before editing membership or archiving"
        );
        c.execute("INSERT INTO chats(id,name,members,archived,pinned,description,bot_only) VALUES(?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,members=excluded.members,archived=excluded.archived,description=excluded.description,bot_only=excluded.bot_only",params![chat.id,chat.name,serde_json::to_string(&chat.members)?,chat.archived,chat.pinned,chat.description,chat.bot_only])?;
        Ok(())
    }
    pub fn pin_chat(&self, id: &str, pinned: bool) -> Result<()> {
        let changed = self.0.lock().unwrap().execute(
            "UPDATE chats SET pinned=? WHERE id=? AND archived=0",
            params![pinned, id],
        )?;
        ensure!(changed == 1, "Active chat not found");
        Ok(())
    }
    pub fn pin_bot(&self, id: &str, pinned: bool) -> Result<()> {
        let changed = self.0.lock().unwrap().execute("UPDATE bots SET profile=json_set(profile,'$.pinned',json(?)) WHERE id=? AND coalesce(json_extract(profile,'$.archived'),0)=0", params![if pinned {"true"} else {"false"},id])?;
        ensure!(changed == 1, "Active bot not found");
        Ok(())
    }
    pub fn chat_messages(&self, id: &str) -> Result<Vec<Value>> {
        self.chat(id)?;
        let mut rows=self.0.lock().unwrap().prepare("SELECT seq,sender,body,kind,run_id,created,linked_chat_id,source_event_seq,COALESCE(history_order,printf('%020d:%020d',seq,0)) FROM chat_messages WHERE chat_id=? AND suppressed=0 ORDER BY created DESC,COALESCE(history_order,printf('%020d:%020d',seq,0)) DESC LIMIT 300")?.query_map([id],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"run_id":r.get::<_,String>(4)?,"created":r.get::<_,i64>(5)?,"linked_chat_id":r.get::<_,String>(6)?,"source_event_seq":r.get::<_,Option<i64>>(7)?,"history_order":r.get::<_,String>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        self.decorate_chat_messages(rows)
    }
    pub fn chat_message_page(
        &self,
        id: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Value> {
        self.chat_message_window(id, before, after, limit, false)
    }
    pub fn chat_message_window(
        &self,
        id: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: usize,
        inclusive: bool,
    ) -> Result<Value> {
        let chat = self.chat(id)?;
        ensure!(
            (1..=150).contains(&limit),
            "Message page limit must be between 1 and 150"
        );
        ensure!(
            before.is_none_or(|n| n > 0) && after.is_none_or(|n| n > 0),
            "Invalid message cursor"
        );
        let c = self.0.lock().unwrap();
        let resolve = |seq: Option<i64>| -> Result<Option<(i64, String)>> {
            seq.map(|seq| {
                c.query_row(
                    "SELECT created,COALESCE(history_order,printf('%020d:%020d',seq,0)) FROM chat_messages WHERE chat_id=? AND seq=?",
                    params![id, seq],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("Message cursor is not in this chat"))
            })
            .transpose()
        };
        let before_key = resolve(before)?;
        let after_key = resolve(after)?;
        ensure!(
            !matches!((&before_key, &after_key), (Some(b), Some(a)) if b < a || (!inclusive && b == a)),
            "Invalid message range"
        );
        let ascending = after.is_some();
        let order = if ascending { "ASC" } else { "DESC" };
        let before_sql = if before.is_none() {
            ""
        } else if inclusive {
            " AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))<=(?2,?3)"
        } else {
            " AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))<(?2,?3)"
        };
        let after_sql = if after.is_none() {
            ""
        } else if inclusive {
            " AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))>=(?4,?5)"
        } else {
            " AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))>(?4,?5)"
        };
        let mut rows = c.prepare(&format!("SELECT seq,sender,body,kind,run_id,created,linked_chat_id,source_event_seq,COALESCE(history_order,printf('%020d:%020d',seq,0)) FROM chat_messages WHERE chat_id=?1 AND suppressed=0{before_sql}{after_sql} ORDER BY created {order},COALESCE(history_order,printf('%020d:%020d',seq,0)) {order} LIMIT ?6"))?
            .query_map(params![id,before_key.as_ref().map(|k|k.0),before_key.as_ref().map(|k|&k.1),after_key.as_ref().map(|k|k.0),after_key.as_ref().map(|k|&k.1),limit as i64],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"run_id":r.get::<_,String>(4)?,"created":r.get::<_,i64>(5)?,"linked_chat_id":r.get::<_,String>(6)?,"source_event_seq":r.get::<_,Option<i64>>(7)?,"history_order":r.get::<_,String>(8)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !ascending {
            rows.reverse();
        }
        let first = rows.first().and_then(|r| r["seq"].as_i64());
        let last = rows.last().and_then(|r| r["seq"].as_i64());
        let first_time = rows.first().and_then(|r| r["created"].as_i64());
        let last_time = rows.last().and_then(|r| r["created"].as_i64());
        let (has_before, has_after): (bool, bool) = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=?1 AND suppressed=0 AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))<(?2,?3)), EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=?1 AND suppressed=0 AND (created,COALESCE(history_order,printf('%020d:%020d',seq,0)))>(?4,?5))",
            params![id,first_time,rows.first().and_then(|r|r["history_order"].as_str()),last_time,rows.last().and_then(|r|r["history_order"].as_str())], |r| Ok((r.get(0)?,r.get(1)?)))?;
        drop(c);
        Ok(
            json!({"chat":chat,"messages":self.decorate_chat_messages(rows)?,"pending_waits":self.collaboration_waits(id)?,"commands":crate::command_jobs::chat_jobs(self,id)?,"page":{"has_before":has_before,"has_after":has_after,"first":first,"last":last}}),
        )
    }
    fn decorate_chat_messages(&self, mut rows: Vec<Value>) -> Result<Vec<Value>> {
        for row in &mut rows {
            if row["sender"] == "user" {
                row["delivery"] = json!(self.followup_delivery(row["seq"].as_i64().unwrap())?);
            }
            row["files"] = json!(self.message_uploads(row["seq"].as_i64().unwrap())?);
            if row["kind"] == "checklist"
                || row["kind"] == "reminder_card"
                || row["kind"] == "reminder"
            {
                let table = if row["kind"] == "checklist" {
                    "checklists"
                } else {
                    "reminders"
                };
                let planning = crate::plans::record(
                    &self.0.lock().unwrap(),
                    table,
                    row["text"].as_str().unwrap_or(""),
                )?;
                row["text"] = if table == "checklists" {
                    planning["title"].clone()
                } else {
                    planning["message"].clone()
                };
                row["planning"] = planning;
            }
            if row["kind"] == "workspace_artifact" {
                let artifact=self.workspace_artifact_read(row["text"].as_str().unwrap_or(""))?;
                row["text"]=artifact["title"].clone();row["workspace_artifact"]=artifact;
            }
            if row["kind"] == "visual_panel" {
                row["visual_panel"]=crate::visual_panels::record(&self.0.lock().unwrap(),row["seq"].as_i64().unwrap())?;
            }
            if row["kind"] == "connector_artifact" {
                let card = crate::connector_artifacts::record(
                    &self.0.lock().unwrap(),
                    row["text"].as_str().unwrap_or(""),
                )?;
                row["text"] = card["title"].clone();
                row["connector_artifact"] = card;
            }
            if row["kind"] == "workspace_import" {
                row["workspace_import"] =
                    crate::workspace_import::card(self, row["text"].as_str().unwrap_or(""))?;
            }
            if row["kind"] == "bot_draft" {
                row["draft"] = self.bot_draft(row["text"].as_str().unwrap_or(""))?;
            }
            if row["kind"] == "connection_card" {
                if let Ok(mut card) = serde_json::from_str::<Value>(row["text"].as_str().unwrap_or("")) {
                    if let Some(id) = card["question_id"].as_str() {
                        let q = self.question(id)?;
                        card["completed"] = json!(q.status != "pending");
                        card["declined"] = json!(q.selected == Some(1));
                        row["text"] = json!(card.to_string());
                    }
                }
            }
            if row["kind"] == "question" {
                row["question"] = json!(self.question(row["text"].as_str().unwrap_or(""))?);
                row["text"] = row["question"]["question"].clone();
            }
            if row["kind"] == "result" {
                row["attachments"] = json!(self.attachments(row["run_id"].as_str().unwrap_or(""))?);
            }
            self.decorate_chat_status(row)?;
            row["reactions"] = json!(
                self.0
                    .lock()
                    .unwrap()
                    .prepare("SELECT bot_id,emoji FROM message_reactions WHERE message_seq=?")?
                    .query_map([row["seq"].as_i64().unwrap()], |r| Ok(
                        json!({"bot_id":r.get::<_,String>(0)?,"emoji":r.get::<_,String>(1)?})
                    ))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            );
            self.decorate_message_actions(row)?;
            row["command"] = crate::skill_import::message_command(
                &self.0.lock().unwrap(),
                row["seq"].as_i64().unwrap(),
            )?;
        }
        Ok(rows)
    }
    fn decorate_chat_status(&self, row: &mut Value) -> Result<()> {
        if row["kind"] == "continuation" {
            row["continuation"] = crate::task_recovery::metadata(
                &self.0.lock().unwrap(),
                row["run_id"].as_str().unwrap_or(""),
                "task_recovery",
            )?
            .unwrap_or(Value::Null);
        }
        if row["sender"] == "user" {
            return Ok(());
        }
        if row["kind"] == "notice" {
            row["status_notice"] = json!({"label":"Status","text":row["text"]});
            return Ok(());
        }
        let c = self.0.lock().unwrap();
        let context = c.query_row("SELECT r.status,r.error,COALESCE((SELECT json_extract(e.body,'$.provider') FROM events e WHERE e.run_id=r.id AND e.kind='model_selected' ORDER BY e.seq DESC LIMIT 1),'') FROM runs r WHERE r.id=?", [row["run_id"].as_str().unwrap_or("")], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        let Some((status, error, provider)) = context else {
            return Ok(());
        };
        if row["kind"] == "result"
            && (matches!(status.as_str(), "failed" | "cancelled" | "interrupted")
                || !error.is_empty())
        {
            let label = match status.as_str() {
                "cancelled" => "Task stopped",
                "interrupted" => "Task interrupted",
                _ => "Task failed",
            };
            row["status_notice"] = json!({"label":label,"text":if error.is_empty() {row["text"].as_str().unwrap_or("")} else {&error}});
        } else if row["kind"] == "assistant" {
            let explicit = c.query_row("SELECT COALESCE(json_extract(body,'$.status_notice')='provider_error',0) FROM events WHERE seq=? AND run_id=? AND kind='assistant'", params![row["source_event_seq"].as_i64(),row["run_id"].as_str().unwrap_or("")], |r|r.get::<_,bool>(0)).optional()?.unwrap_or(false);
            // Older bridges discarded Claude's error marker. Recognize only the
            // exact recorded diagnostic on a failed Claude task's final event.
            const LEGACY_REFRESH: &str = "Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh. This is usually transient; retry in a minute, and if it persists close other Claude Code processes or sign in again";
            let legacy = provider == "claude-code"
                && status == "failed"
                && error
                    == "The subscription provider could not complete this task. Check its official CLI connection and quota; no alternate provider was used."
                && row["text"] == LEGACY_REFRESH
                && c.query_row(
                    "SELECT max(seq)=? FROM events WHERE run_id=? AND kind='assistant'",
                    params![
                        row["source_event_seq"].as_i64(),
                        row["run_id"].as_str().unwrap_or("")
                    ],
                    |r| r.get::<_, Option<bool>>(0),
                )?
                .unwrap_or(false);
            if explicit || legacy {
                row["status_notice"] = json!({"label":"Provider error","text":row["text"]});
            }
        }
        if row.get("status_notice").is_some() {
            if let Some(receipt) = crate::task_recovery::metadata(
                &c,
                row["run_id"].as_str().unwrap_or(""),
                "task_continued",
            )? {
                row["status_notice"]["continued_by"] = receipt["run_id"].clone();
            }
        }
        Ok(())
    }
    pub fn chat_send(&self, id: &str, prompt: &str, mentions: &[String]) -> Result<Vec<String>> {
        self.chat_send_files(id, prompt, mentions, &[])
    }
    pub fn chat_send_files(
        &self,
        id: &str,
        prompt: &str,
        mentions: &[String],
        files: &[String],
    ) -> Result<Vec<String>> {
        self.chat_send_message(id, prompt, mentions, files, None)
    }
    pub fn chat_send_message(
        &self,
        id: &str,
        prompt: &str,
        mentions: &[String],
        files: &[String],
        reply_to: Option<i64>,
    ) -> Result<Vec<String>> {
        self.chat_send_request(id, prompt, mentions, files, reply_to, None)
    }
    pub fn chat_send_request(
        &self,
        id: &str,
        prompt: &str,
        mentions: &[String],
        files: &[String],
        reply_to: Option<i64>,
        request_id: Option<&str>,
    ) -> Result<Vec<String>> {
        if let Some(key) = request_id {
            ensure!(
                uuid::Uuid::parse_str(key).is_ok(),
                "Invalid message request ID"
            );
        }
        let body = serde_json::to_vec(&json!([id, prompt, mentions, files, reply_to]))?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &body);
        let chat = self.chat(id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if let Some(key) = request_id {
            if let Some((hash, runs)) = tx
                .query_row(
                    "SELECT body_hash,runs FROM chat_send_receipts WHERE request_id=?",
                    [key],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
            {
                ensure!(
                    hash == digest.as_ref(),
                    "This message request ID was already used for a different message"
                );
                return Ok(serde_json::from_str(&runs)?);
            }
        }
        anyhow::ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is being moved. Resume or cancel its transfer in Profile settings"
        );
        let command = crate::commands::resolve(&tx, prompt)?;
        let literal = crate::commands::literal(prompt);
        let effective_prompt = command
            .as_ref()
            .map(|c| c.prompt.as_str())
            .unwrap_or(literal);
        let quote = reply_to
            .map(|seq| crate::message_actions::quoted_message(&tx, id, seq))
            .transpose()?;
        // In a DM, names are references for the selected bot, not routing commands.
        let named =
            crate::team_chats::addressed(prompt, &crate::team_chats::members(&tx, &chat)?, false);
        let targets = if id.starts_with("dm-") {
            vec![chat.members[0].clone()]
        } else if !mentions.is_empty() {
            mentions.to_vec()
        } else if !named.is_empty() {
            named
        } else if let Some(sender) = quote
            .as_ref()
            .and_then(|q| q["sender"].as_str())
            .filter(|s| *s != "user")
        {
            ensure!(
                chat.members.iter().any(|m| m == sender),
                "The reply author is no longer in this chat. Mention a current teammate to continue."
            );
            vec![sender.to_string()]
        } else {
            let recent = tx.prepare("SELECT sender FROM chat_messages WHERE chat_id=? AND kind IN ('message','assistant','result') AND sender!='user' AND suppressed=0 ORDER BY seq DESC LIMIT 12")?
                .query_map([id], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            crate::team_chats::default_recipient(prompt, &chat.description, &crate::team_chats::members(&tx, &chat)?, &recent)
        };
        ensure!(targets.len() <= 6, "Too many mentions");
        let round = db::id();
        let mut runs = vec![];
        let mut seen = std::collections::HashSet::new();
        for target in targets {
            if seen.insert(target.clone()) {
                let run = insert_run(&tx, &chat, &target, effective_prompt, &round, "", 0)?;
                if !id.starts_with("dm-") { record_recipient(&tx, &run)?; }
                if let Some(command) = &command {
                    tx.execute(
                        "INSERT INTO run_commands(run_id,receipt) VALUES(?,?)",
                        params![run, command.receipt.to_string()],
                    )?;
                }
                runs.push(run);
            }
        }
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user',?,'message',?)",params![id,prompt,db::now()])?;
        let message_seq = tx.last_insert_rowid();
        crate::uploads::bind(&tx, id, message_seq, files)?;
        if let Some(target) = reply_to {
            tx.execute(
                "INSERT INTO message_replies VALUES(?,?)",
                params![message_seq, target],
            )?;
        }
        {
            for run in &runs {
                tx.execute(
                    "INSERT INTO run_message_sources VALUES(?,?)",
                    params![run, message_seq],
                )?;
            }
        }
        if let Some(key) = request_id {
            tx.execute(
                "INSERT INTO chat_send_receipts VALUES(?,?,?,?)",
                params![key, id, digest.as_ref(), serde_json::to_string(&runs)?],
            )?;
        }
        tx.commit()?;
        Ok(runs)
    }
    pub fn chat_handoff(&self, run: &Run, target: &str, message: &str) -> Result<String> {
        ensure!(target != run.bot_id, "Choose another teammate");
        let mut chat = self.chat(&run.chat_id)?;
        ensure!(!chat.archived, "This chat is archived");
        let sender = self.bot(&run.bot_id)?;
        let recipient = self.bot(target)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if chat.id.starts_with("dm-") {
            let source = chat.id.clone();
            // Reuse only a dedicated, active conversation with this exact pair.
            // User-created groups and chats with additional members stay separate.
            chat.id = tx.query_row("SELECT id FROM chats WHERE archived=0 AND id LIKE 'team-%' AND json_array_length(members)=2 AND EXISTS(SELECT 1 FROM json_each(members) WHERE value=?1) AND EXISTS(SELECT 1 FROM json_each(members) WHERE value=?2) ORDER BY rowid DESC LIMIT 1",params![sender.id,recipient.id],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||format!("team-{}", run.round_id));
            chat.bot_only = true;
            chat.name = format!("{}, {}", sender.name, recipient.name)
                .chars()
                .take(100)
                .collect();
            chat.members = vec![sender.id.clone()];
            tx.execute(
                "INSERT OR IGNORE INTO chats(id,name,members,bot_only) VALUES(?,?,?,1)",
                params![chat.id, chat.name, serde_json::to_string(&chat.members)?],
            )?;
            chat.members = serde_json::from_str(&tx.query_row(
                "SELECT members FROM chats WHERE id=? AND archived=0",
                [&chat.id],
                |r| r.get::<_, String>(0),
            )?)?;
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created,linked_chat_id) SELECT ?,?,?, 'collaboration',?,?,? WHERE NOT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=? AND linked_chat_id=?)",params![source,sender.id,format!("{} started a chat with {}.",sender.name,recipient.name),run.id,db::now(),chat.id,source,chat.id])?;
        }
        if !chat.members.iter().any(|id| id == target) {
            ensure!(
                chat.members.len() < 6 && !recipient.profile.archived,
                "This chat is full or the teammate is archived"
            );
            chat.members.push(target.into());
            tx.execute(
                "UPDATE chats SET members=? WHERE id=?",
                params![serde_json::to_string(&chat.members)?, chat.id],
            )?;
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'system',?,'notice',?)",params![chat.id,format!("{} invited {} to collaborate.",sender.name,recipient.name),db::now()])?;
        }
        let queued = insert_run(
            &tx,
            &chat,
            target,
            &format!(
                "Request from {} ({}), addressed to {} ({}):\n{}\nAnswer the request about the named person or task; you are {}, not the requester. If this message gives you an ongoing role, responsibility or preference assigned by the user, call remember to save it in YOUR durable memory before replying. Merge it with your existing memory and keep facts about other teammates separate from your own role. A conversational acknowledgement alone will not persist across chats. Only say you saved it after remember succeeds. Do not turn temporary requests into permanent responsibilities or replace an explicit user assignment with a teammate's speculation. Your final result will wake {} automatically. Do not send a separate acknowledgement or duplicate reply.",
                sender.name,
                sender.id,
                recipient.name,
                recipient.id,
                message,
                recipient.name,
                sender.name
            ),
            &run.round_id,
            &run.bot_id,
            run.depth + 1,
        )?;
        tx.execute("INSERT INTO collaboration_requests(child_run_id,parent_run_id,source_chat_id) VALUES(?,?,?)",params![queued,run.id,run.chat_id])?;
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'handoff',?,?)",params![chat.id,sender.id,format!("@{} {}",recipient.name,message),run.id,db::now()])?;
        tx.commit()?;
        Ok(queued)
    }
    pub fn chat_complete(&self, run: &Run) -> Result<()> {
        if run.chat_id.is_empty() {
            return Ok(());
        }
        let done = self.run(&run.id)?;
        let chat = self.chat(&run.chat_id)?;
        let bot = self.bot(&run.bot_id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if crate::workspace_transfer::frozen(&tx)? {
            return Ok(());
        }
        tx.execute(
            "DELETE FROM chat_completion_pending WHERE run_id=?",
            [&run.id],
        )?;
        if tx.execute(
            "INSERT OR IGNORE INTO chat_completion_receipts VALUES(?)",
            [&run.id],
        )? == 0
        {
            tx.commit()?;
            return Ok(());
        }
        let deferred: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind IN ('question_wait','process_wait'))",
            [&run.id],
            |r| r.get(0),
        )?;
        let stopped: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='run_stop_requested')",
            [&run.id],
            |r| r.get(0),
        )?;
        let quiet: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=? AND quiet=1)",
            [&run.id],
            |r| r.get(0),
        )?;
        let has_attachments:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM attachments WHERE run_id=?1) OR EXISTS(SELECT 1 FROM deliverables WHERE run_id=?1)",[&run.id],|r|r.get(0))?;
        let replacement = if done.status == "completed" && done.error.is_empty() {
            crate::quiet_output::routine_replacement(&tx, &run.id, &done.output)?
        } else {
            None
        };
        let output = replacement.unwrap_or(&done.output);
        let silent = done.status == "completed"
            && done.error.is_empty()
            && (deferred || quiet || (output.trim().is_empty() && !has_attachments));
        if silent {
            tx.execute(
                "UPDATE chat_messages SET suppressed=1 WHERE run_id=? AND kind='result'",
                [&run.id],
            )?;
            if deferred
                || (quiet && !crate::collaboration::is_child(&tx, &run.id)?)
                || (run.reply_to.is_empty() && !crate::collaboration::has_requests(&tx, &run.id)?)
            {
                tx.commit()?;
                return Ok(());
            }
        }
        let exists: i64 = tx.query_row(
            "SELECT count(*) FROM chat_messages WHERE run_id=? AND kind='result' AND suppressed=0",
            [&run.id],
            |r| r.get(0),
        )?;
        let text = if silent {
            "No written result was returned. Inspect the recorded tool results before drawing conclusions.".to_owned()
        } else if done.error.is_empty() {
            if output.trim().is_empty() && has_attachments {
                "Files attached.".to_owned()
            } else {
                output.trim().to_owned()
            }
        } else {
            format!("{}: {}", done.status, done.error)
        };
        if !silent && exists == 0 {
            let promoted = tx.execute("UPDATE chat_messages SET kind='result',body=?2 WHERE run_id=?1 AND kind='assistant' AND suppressed=0 AND trim(body,char(9)||char(10)||char(13)||' ')=?2 AND source_event_seq=(SELECT max(source_event_seq) FROM chat_messages WHERE run_id=?1)",params![run.id,text])?;
            if promoted == 0 {
                tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'result',?,?)",params![chat.id,bot.id,text,run.id,db::now()])?;
            }
        }
        if !deferred && !stopped {
            crate::collaboration::completed(&tx, &done)?;
            if !silent {
                crate::team_chats::route_completion(&tx, &chat, &done, &text)?;
            }
        }
        if !run.reply_to.is_empty()
            && done.status != "cancelled"
            && !deferred
            && !stopped
            && !crate::collaboration::is_child(&tx, &run.id)?
            && !crate::collaboration::has_requests(&tx, &run.id)?
        {
            // A failed wake-up must not discard the visible result.
            // Only the requester sees their original DM task on continuation.
            let original: String = tx.query_row("SELECT prompt FROM runs WHERE round_id=? AND bot_id=? ORDER BY depth ASC,rowid ASC LIMIT 1",params![run.round_id,run.reply_to],|r|r.get(0)).unwrap_or_default();
            let prompt = format!(
                "{} ({}) returned a result:\n{}\nYour original task: {}\nContinue the user's task using this result. If it supplies your own assigned role or lasting responsibilities, call remember to merge them into your memory before replying. Facts about another teammate do not become your own role. Saving their role in your memory does not save it for them; only say they retained it when their result confirms they saved it. Ask another teammate only if more work is needed; do not exchange courtesy acknowledgements.",
                bot.name,
                bot.id,
                text.chars().take(24000).collect::<String>(),
                original.chars().take(8000).collect::<String>()
            );
            if let Err(e) = insert_run(
                &tx,
                &chat,
                &run.reply_to,
                &prompt,
                &run.round_id,
                "",
                run.depth,
            ) {
                tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'system',?,'notice',?)",params![chat.id,format!("Automatic follow-up paused: {e}"),db::now()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn task_status_projection_preserves_replies_and_raw_history() {
        let db = Db::open(":memory:").unwrap();
        let mut bot = crate::tests::bot(&db, "claude-code");
        let chat = format!("dm-{}", bot.id);
        let generic = "The subscription provider could not complete this task. Check its official CLI connection and quota; no alternate provider was used.";
        let legacy = "Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh. This is usually transient; retry in a minute, and if it persists close other Claude Code processes or sign in again";
        for (status, label) in [
            ("failed", "Task failed"),
            ("cancelled", "Task stopped"),
            ("interrupted", "Task interrupted"),
            ("completed", ""),
        ] {
            bot.provider = "claude-code".into();
            db.save_bot(&bot).unwrap();
            let id = db.queue(&bot.id, "Explain this error", 0).unwrap();
            db.event(&id, "model_selected", json!({"provider":"claude-code"}))
                .unwrap();
            db.event(
                &id,
                "assistant",
                json!({"text":"I found the file; its contents mention an error."}),
            )
            .unwrap();
            db.event(&id, "assistant", json!({"text":legacy})).unwrap();
            db.finish(
                &id,
                status,
                if status == "completed" {
                    "Finished normally"
                } else {
                    ""
                },
                if status == "completed" { "" } else { generic },
            )
            .unwrap();
            db.chat_complete(&db.run(&id).unwrap()).unwrap();
            bot.provider = "codex".into();
            db.save_bot(&bot).unwrap();
            let before = db.events(&id).unwrap();
            let messages = db.chat_messages(&chat).unwrap();
            let rows: Vec<_> = messages.iter().filter(|m| m["run_id"] == id).collect();
            assert!(
                rows.iter()
                    .find(|m| m["text"].as_str().unwrap_or("").starts_with("I found"))
                    .unwrap()["status_notice"]
                    .is_null()
            );
            let diagnostic = rows.iter().find(|m| m["text"] == legacy).unwrap();
            assert_eq!(
                diagnostic["status_notice"]["label"] == "Provider error",
                status == "failed"
            );
            let result = rows.iter().find(|m| m["kind"] == "result").unwrap();
            if status == "completed" {
                assert!(result["status_notice"].is_null());
            } else {
                assert_eq!(result["status_notice"]["label"], label);
                assert_eq!(result["status_notice"]["text"], generic);
            }
            let page = db.chat_message_page(&chat, None, None, 150).unwrap();
            assert_eq!(
                page["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["seq"] == result["seq"])
                    .unwrap()["status_notice"],
                result["status_notice"]
            );
            assert_eq!(db.events(&id).unwrap(), before);
        }
        let id = db
            .chat_send(&chat, "Explain a provider error", &[])
            .unwrap()[0]
            .clone();
        db.event(
            &id,
            "assistant",
            json!({"text":"Provider-specific diagnostic","status_notice":"provider_error"}),
        )
        .unwrap();
        let messages = db.chat_messages(&chat).unwrap();
        assert_eq!(
            messages
                .iter()
                .find(|m| m["run_id"] == id && m["kind"] == "assistant")
                .unwrap()["status_notice"]["label"],
            "Provider error"
        );
        assert!(
            messages
                .iter()
                .filter(|m| m["sender"] == "user")
                .all(|m| m["status_notice"].is_null())
        );
    }
    #[test]
    fn empty_teammate_completion_wakes_the_requester_once_without_an_empty_bubble() {
        let db = Db::open(":memory:").unwrap();
        let requester = crate::tests::bot(&db, "codex");
        let teammate = crate::tests::bot(&db, "codex");
        let parent = db
            .queue(&requester.id, "Review this with your teammate", 0)
            .unwrap();
        let parent = db.run(&parent).unwrap();
        let child = db
            .chat_handoff(&parent, &teammate.id, "Review and acknowledge the task")
            .unwrap();
        db.finish(&parent.id, "completed", "Assigned the review", "")
            .unwrap();
        db.chat_complete(&parent).unwrap();
        db.finish(&child, "completed", "", "").unwrap();
        let child = db.run(&child).unwrap();
        db.chat_complete(&child).unwrap();
        db.chat_complete(&child).unwrap();
        let follow = db
            .runs(Some(&requester.id))
            .unwrap()
            .into_iter()
            .filter(|r| r.status == "queued")
            .collect::<Vec<_>>();
        assert_eq!(follow.len(), 1);
        assert!(follow[0].prompt.contains("No written result was returned"));
        assert!(follow[0].prompt.contains("Review this with your teammate"));
        assert!(
            !db.chat_messages(&child.chat_id)
                .unwrap()
                .iter()
                .any(|m| m["run_id"] == child.id && m["kind"] == "result")
        );
    }
    #[test]
    fn delivery_projection_survives_restarts_without_duplicate_or_quiet_bubbles() {
        let root = std::env::temp_dir().join(format!("kindred-delivery-{}", db::id()));
        let path = root.join("kindred.db");
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let chat = format!("dm-{}", bot.id);
        let normal = db.queue(&bot.id, "Answer normally", 0).unwrap();
        db.event(&normal, "assistant", json!({"text":"One answer.\n\n"}))
            .unwrap();
        db.finish(&normal, "completed", "One answer.", "").unwrap();
        db.chat_complete(&db.run(&normal).unwrap()).unwrap();
        assert_eq!(
            db.chat_messages(&chat)
                .unwrap()
                .iter()
                .filter(|m| m["run_id"] == normal)
                .count(),
            1
        );
        let question = db.queue(&bot.id, "Need a choice", 0).unwrap();
        db.event(
            &question,
            "assistant",
            json!({"text":"Choose your timezone.\n\n"}),
        )
        .unwrap();
        db.event(&question, "question_wait", json!({"question_id":"fixture"}))
            .unwrap();
        db.finish(&question, "completed", "Choose your timezone.", "")
            .unwrap();
        db.chat_complete(&db.run(&question).unwrap()).unwrap();
        assert_eq!(
            db.chat_messages(&chat)
                .unwrap()
                .iter()
                .filter(|m| m["run_id"] == question)
                .count(),
            1
        );
        let quiet = db.queue(&bot.id, "Quiet check", 0).unwrap();
        db.0.lock()
            .unwrap()
            .execute("INSERT INTO routine_runs VALUES(?,'fixture',1)", [&quiet])
            .unwrap();
        db.event(
            &quiet,
            "assistant",
            json!({"text":"Looking at the schedule…"}),
        )
        .unwrap();
        assert!(
            !db.chat_messages(&chat)
                .unwrap()
                .iter()
                .any(|m| m["run_id"] == quiet)
        );
        db.finish(&quiet, "completed", "Looking at the schedule…", "")
            .unwrap();
        db.chat_complete(&db.run(&quiet).unwrap()).unwrap();
        let failed = db.queue(&bot.id, "Failed check", 0).unwrap();
        db.0.lock()
            .unwrap()
            .execute("INSERT INTO routine_runs VALUES(?,'fixture',1)", [&failed])
            .unwrap();
        db.finish(&failed, "failed", "", "The calendar could not be reached")
            .unwrap();
        db.chat_complete(&db.run(&failed).unwrap()).unwrap();
        let c = db.0.lock().unwrap();
        // Reproduce old rows exactly, retaining event IDs and a quoted reference.
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'result',?,1)",params![chat,bot.id,"Choose your timezone.",question]).unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'assistant',?,1)",params![chat,bot.id,"Old quiet progress",quiet]).unwrap();
        let hidden_seq = c.last_insert_rowid();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'result',?,1)",params![chat,bot.id,"Old quiet progress",quiet]).unwrap();
        let raw_count: i64 = c
            .query_row("SELECT count(*) FROM chat_messages", [], |r| r.get(0))
            .unwrap();
        drop(c);
        db.mark_chat_read(&chat, hidden_seq).unwrap();
        drop(db);
        for _ in 0..2 {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            let messages = db.chat_messages(&chat).unwrap();
            assert!(!messages.iter().any(|m| m["run_id"] == quiet));
            assert_eq!(
                messages.iter().filter(|m| m["run_id"] == question).count(),
                1
            );
            assert_eq!(messages.iter().filter(|m| m["run_id"] == normal).count(), 1);
            assert!(messages.iter().any(|m| m["run_id"] == failed
                && m["text"].as_str().unwrap().contains("could not be reached")));
            assert_eq!(
                db.0.lock()
                    .unwrap()
                    .query_row("SELECT count(*) FROM chat_messages", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                raw_count
            );
            assert_eq!(db.run(&quiet).unwrap().output, "Looking at the schedule…");
            assert!(
                db.events(&quiet)
                    .unwrap()
                    .iter()
                    .any(|e| e["kind"] == "assistant")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
