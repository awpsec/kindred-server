pub const BOT_INSTRUCTIONS_MAX_BYTES: usize = 32_000;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn inherit_approval() -> String {
    "inherit".into()
}
pub fn valid_approval(mode: &str) -> bool {
    matches!(mode, "ask" | "auto" | "full")
}

/// Older profiles can have only a subset of the settings fields. Add defaults
/// without replacing saved preferences or granting local access.
pub fn general_settings(saved: Option<Value>) -> Value {
    let mut settings = json!({"name":"You","identity":"","theme":"dark",
        "reduced_motion":false,"approval_mode":"ask","show_activity":false,"separate_bot_chats":true,
        "default_provider":"codex","model_defaults":{},"local_access":false,"notifications":"all","timezone":"","timezone_mode":"auto"});
    if let Some(Value::Object(saved)) = saved {
        settings.as_object_mut().unwrap().extend(saved);
    }
    settings
}

pub fn apply_model_default(bot: &mut Bot, settings: &Value) -> Result<()> {
    if !bot.model.is_empty() { return Ok(()); }
    let saved = &settings["model_defaults"][&bot.provider];
    bot.model = saved["model"].as_str().unwrap_or("").to_owned();
    if bot.reasoning_effort.is_empty() {
        bot.reasoning_effort = saved["reasoning_effort"].as_str().unwrap_or("").to_owned();
    }
    ensure!(bot.provider == "codex" || !bot.model.is_empty(), "Choose a default model for this provider in Settings → General, or select a model for this bot.");
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bot {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub memory: String,
    pub auto_approve: bool,
    #[serde(default = "inherit_approval")]
    pub approval_mode: String,
    #[serde(default)]
    pub profile: BotProfile,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotProfile {
    pub label: String,
    pub description: String,
    pub shape: String,
    pub color: String,
    pub eyes: String,
    pub animated: bool,
    pub pinned: bool,
    pub archived: bool,
    pub notifications: bool,
    pub local_access: bool,
    pub local_device_id: String,
}
impl Default for BotProfile {
    fn default() -> Self {
        Self {
            label: String::new(),
            description: String::new(),
            shape: "round".into(),
            color: "#d8d8d8".into(),
            eyes: "curious".into(),
            animated: true,
            pinned: false,
            archived: false,
            notifications: true,
            local_access: false,
            local_device_id: String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub bot_id: String,
    pub prompt: String,
    pub status: String,
    pub output: String,
    pub error: String,
    pub created: i64,
    pub depth: i64,
    pub chat_id: String,
    pub round_id: String,
    pub reply_to: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    pub bot_id: String,
    pub name: String,
    pub prompt: String,
    pub interval_seconds: i64,
    pub next_run: i64,
    pub enabled: bool,
    #[serde(default)]
    pub schedule: Option<crate::schedules::WeeklySchedule>,
    #[serde(default)]
    pub run_at: Option<i64>,
}
pub struct Db(
    pub Mutex<Connection>,
    #[allow(dead_code)] Option<DatabaseLock>,
);
// Drop after the connection. Explicit unlock prevents a concurrently forked
// subprocess from briefly retaining the lease until its close-on-exec completes.
struct DatabaseLock(std::fs::File);
impl Drop for DatabaseLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(crate) fn bot_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Bot> {
    Ok(Bot {
        id: r.get(0)?,
        name: r.get(1)?,
        instructions: r.get(2)?,
        provider: r.get(3)?,
        model: r.get(4)?,
        memory: r.get(5)?,
        auto_approve: r.get(6)?,
        profile: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        reasoning_effort: r.get(8)?,
        approval_mode: r.get(9)?,
    })
}
pub(crate) fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        bot_id: r.get(1)?,
        prompt: r.get(2)?,
        status: r.get(3)?,
        output: r.get(4)?,
        error: r.get(5)?,
        created: r.get(6)?,
        depth: r.get(7)?,
        chat_id: r.get(8)?,
        round_id: r.get(9)?,
        reply_to: r.get(10)?,
    })
}
impl Db {
    pub fn open(path: &str) -> Result<Self> {
        if path != ":memory:" {
            if let Some(p) = Path::new(path).parent() {
                std::fs::create_dir_all(p)?;
            }
        }
        let lock = if path != ":memory:" {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(format!("{path}.lock"))?;
            fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
                anyhow::anyhow!("This database is already in use by another Kindred server")
            })?;
            let _ = options.open(path)?;
            Some(DatabaseLock(file))
        } else {
            None
        };
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS bots(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,memory TEXT NOT NULL,auto_approve INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id),prompt TEXT NOT NULL,status TEXT NOT NULL,output TEXT NOT NULL DEFAULT '',error TEXT NOT NULL DEFAULT '',created INTEGER NOT NULL,depth INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS runs_status ON runs(status,created);
        CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL REFERENCES runs(id),kind TEXT NOT NULL,body TEXT NOT NULL,created INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),tool TEXT NOT NULL,args TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'pending');
        CREATE TABLE IF NOT EXISTS skills(name TEXT PRIMARY KEY,body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS screens(bot_id TEXT PRIMARY KEY REFERENCES bots(id),slot INTEGER UNIQUE NOT NULL,takeover INTEGER NOT NULL DEFAULT 0,quiet_until INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS routines(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id),name TEXT NOT NULL,prompt TEXT NOT NULL,interval_seconds INTEGER NOT NULL,next_run INTEGER NOT NULL,enabled INTEGER NOT NULL);")?;
        crate::deliverables::migrate(&c)?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS attachments(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),title TEXT NOT NULL,png BLOB NOT NULL,created INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS attachments_run ON attachments(run_id);")?;
        let columns = c
            .prepare("PRAGMA table_info(bots)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|v| v == "profile") {
            c.execute(
                "ALTER TABLE bots ADD COLUMN profile TEXT NOT NULL DEFAULT '{}'",
                [],
            )?;
        }
        if !columns.iter().any(|v| v == "reasoning_effort") {
            c.execute(
                "ALTER TABLE bots ADD COLUMN reasoning_effort TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !columns.iter().any(|v| v == "approval_mode") {
            c.execute(
                "ALTER TABLE bots ADD COLUMN approval_mode TEXT NOT NULL DEFAULT 'inherit'",
                [],
            )?;
            // Existing broad VM approvals retain routine access, never gain external write access.
            c.execute(
                "UPDATE bots SET approval_mode='auto' WHERE auto_approve=1",
                [],
            )?;
        }
        crate::provider_accounts::migrate(&c)?;
        crate::connector_policy::migrate(&c)?;
        crate::local_access::migrate(&c)?;
        crate::commands::migrate(&c)?;
        crate::schedules::migrate(&c)?;
        crate::provider_inbox::migrate(&c)?;
        crate::chats::migrate(&c)?;
        crate::workspace_transfer::migrate(&c)?;
        crate::workspace_import::migrate(&c)?;
        crate::user_tasks::migrate(&c)?;
        crate::screen_control::migrate(&c)?;
        crate::vm_maintenance::migrate(&c)?;
        crate::questions::migrate(&c)?;
        crate::plans::migrate(&c)?;
        crate::artifact_library::migrate(&c)?;
        crate::connector_artifacts::migrate(&c)?;
        crate::mail_watch::migrate(&c)?;
        crate::conversation_updates::migrate(&c)?;
        crate::team_chats::migrate(&c)?;
        crate::continuity::migrate(&c)?;
        crate::visual_panels::migrate(&c)?;
        crate::workspace_artifacts::migrate(&c)?;
        crate::command_jobs::migrate(&c)?;
        crate::command_jobs::recover_waits(&c)?;
        // A saved wait ended its provider turn; it is safe to recover the wait,
        // not to replay the preceding command or provider session.
        c.execute("UPDATE runs SET status='completed',error='',output='' WHERE status='running' AND EXISTS(SELECT 1 FROM events WHERE run_id=runs.id AND kind='process_wait')", [])?;
        // An interrupted external action must never be replayed automatically.
        let interrupted=c.execute("UPDATE runs SET status='interrupted',error='Service restarted during this run. Review tool activity before retrying.' WHERE status IN ('running','awaiting_user','awaiting_approval','cancelling')", [])?;
        c.execute(
            "UPDATE local_requests SET status='expired' WHERE status IN ('queued','claimed')",
            [],
        )?;
        if interrupted > 0 {
            c.execute("INSERT INTO settings VALUES('quiet_until',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[(now()+65).to_string()])?;
        }
        c.execute(
            "UPDATE approvals SET status='expired' WHERE status='pending'",
            [],
        )?;
        c.execute_batch("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT chat_id,bot_id,CASE WHEN error='' THEN output ELSE error END,'result',id,max(created,COALESCE((SELECT max(m.created) FROM chat_messages m WHERE m.run_id=runs.id),created)) FROM runs WHERE chat_id!='' AND status IN ('completed','failed','interrupted','cancelled') AND NOT EXISTS(SELECT 1 FROM chat_messages WHERE run_id=runs.id AND kind='result') AND NOT (status='completed' AND error='' AND (trim(output)='' OR EXISTS(SELECT 1 FROM routine_runs WHERE run_id=runs.id AND quiet=1) OR EXISTS(SELECT 1 FROM events WHERE run_id=runs.id AND kind IN ('question_wait','process_wait'))));")?;
        crate::chats::repair_projection(&c)?;
        crate::routine_controls::remove_archived_bot_schedules(&c)?;
        crate::chats::archive_inactive_bot_chats(&c)?;
        let db = Self(Mutex::new(c), lock);
        if let Err(error) = db.deliver_pending_completions() {
            eprintln!("Pending chat delivery will be retried: {error}");
        }
        Ok(db)
    }
    pub fn bots(&self) -> Result<Vec<Bot>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .prepare("SELECT * FROM bots ORDER BY name")?
            .query_map([], bot_row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn bot(&self, id: &str) -> Result<Bot> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT * FROM bots WHERE id=?", [id], bot_row)?)
    }
    #[cfg(test)]
    pub fn save_bot(&self, b: &Bot) -> Result<()> {
        self.save_bot_preferences(b, false)
    }
    pub fn create_bot(&self, b: &Bot) -> Result<()> {
        validate_bot(b)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?)",
            [&b.id],
            |r| r.get(0),
        )?;
        ensure!(!exists, "Bot already exists");
        write_bot(&tx, b, false)?;
        tx.execute(
            "INSERT INTO chats(id,name,members) VALUES(?,?,?)",
            params![
                format!("dm-{}", b.id),
                b.name,
                serde_json::to_string(&vec![&b.id])?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn save_bot_preferences(&self, b: &Bot, preserve_text: bool) -> Result<()> {
        validate_bot(b)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let already_archived: bool = tx.query_row(
            "SELECT COALESCE((SELECT json_extract(profile,'$.archived') FROM bots WHERE id=?),0)",
            [&b.id],
            |r| r.get(0),
        )?;
        if b.profile.archived && !already_archived {
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE bot_id=? AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling'))",[&b.id],|r| r.get::<_,bool>(0))?, "Wait for or stop this bot's tasks before archiving.");

        }
        write_bot(&tx, b, preserve_text)?;
        if b.profile.archived {
            crate::routine_controls::remove_archived_bot_schedules(&tx)?;
            crate::chats::archive_inactive_bot_chats(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn save_bot_text(
        &self,
        id: &str,
        field: &str,
        value: &str,
        expected: Option<&str>,
    ) -> Result<()> {
        ensure!(
            matches!(field, "instructions" | "memory"),
            "Unknown text field"
        );
        let limit = if field == "instructions" { BOT_INSTRUCTIONS_MAX_BYTES } else { 16_000 };
        ensure!(value.len() <= limit, "Text is limited to {limit} UTF-8 bytes");
        let changed = self.0.lock().unwrap().execute(
            &format!("UPDATE bots SET {field}=?1 WHERE id=?2 AND (?3 IS NULL OR {field}=?3)"),
            params![value, id, expected],
        )?;
        ensure!(
            changed == 1,
            "This text changed while you were editing. Reopen the editor to review the latest version; your edits are still here."
        );
        Ok(())
    }
    pub fn setting(&self, key: &str) -> Result<Option<Value>> {
        let text: Option<String> = self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(text.and_then(|v| serde_json::from_str(&v).ok()))
    }
    pub fn save_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO settings VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
    pub fn delete_skill(&self, name: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .execute("DELETE FROM skills WHERE name=?", [name])?;
        Ok(())
    }
    pub fn queue(&self, bot_id: &str, prompt: &str, depth: i64) -> Result<String> {
        ensure!(depth == 0, "Use a shared chat for teammate requests");
        let chat_id = format!("dm-{bot_id}");
        if self.chat(&chat_id).is_err() {
            let b = self.bot(bot_id)?;
            self.save_chat(&crate::chats::Chat {
                bot_only: false,
                description: String::new(),
                id: chat_id.clone(),
                name: b.name,
                members: vec![bot_id.into()],
                archived: false,
                pinned: false,
                last_message: None,
            })?;
        }
        Ok(self.chat_send(&chat_id, prompt, &[])?.remove(0))
    }
    pub fn runs(&self, bot: Option<&str>) -> Result<Vec<Run>> {
        let c = self.0.lock().unwrap();
        let rows = c.prepare("SELECT * FROM runs WHERE (?1 IS NULL OR bot_id=?1) AND (status IN ('running','awaiting_user','awaiting_approval','cancelling') OR id IN (SELECT run_id FROM command_jobs WHERE status IN ('starting','running')) OR rowid IN (SELECT rowid FROM runs WHERE (?1 IS NULL OR bot_id=?1) ORDER BY created DESC,rowid DESC LIMIT 100)) ORDER BY created DESC,rowid DESC")?.query_map([bot],run_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn run(&self, id: &str) -> Result<Run> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT * FROM runs WHERE id=?", [id], run_row)?)
    }
    pub fn screen(&self, bot: &str) -> Result<i64> {
        self.bot(bot)?;
        let c = self.0.lock().unwrap();
        if let Some(slot) = c
            .query_row("SELECT slot FROM screens WHERE bot_id=?", [bot], |r| {
                r.get(0)
            })
            .optional()?
        {
            return Ok(slot);
        }
        let slot: i64 = c.query_row("SELECT COALESCE(MAX(slot),0)+1 FROM screens", [], |r| {
            r.get(0)
        })?;
        ensure!(
            slot <= 32,
            "This VM has reached its 32 persistent screen limit"
        );
        let legacy: bool = slot == 1
            && c.query_row("SELECT value FROM settings WHERE key='takeover'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .as_deref()
                == Some("true");
        c.execute(
            "INSERT INTO screens(bot_id,slot,takeover) VALUES(?,?,?)",
            params![bot, slot, legacy],
        )?;
        Ok(slot)
    }
    pub fn screen_ids(&self) -> Result<Vec<i64>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .prepare("SELECT slot FROM screens")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn screen_takeover(&self, slot: i64) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT takeover FROM screens WHERE slot=?", [slot], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(false))
    }
    pub fn screen_set_takeover(&self, slot: i64, enabled: bool) -> Result<()> {
        crate::screen_control::set(self, slot, enabled, "manual")
    }
    pub fn screen_quiet(&self, slot: i64) -> Result<i64> {
        let local: i64 = self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT quiet_until FROM screens WHERE slot=?",
                [slot],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(local.max(self.quiet_until()?))
    }
    pub fn screen_set_quiet(&self, slot: i64, until: i64) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE screens SET quiet_until=? WHERE slot=?",
            params![until, slot],
        )?;
        Ok(())
    }
    pub fn queued_bots(&self) -> Result<Vec<String>> {
        Ok(self.0.lock().unwrap().prepare("SELECT bot_id FROM runs WHERE status='queued' GROUP BY bot_id ORDER BY MIN(created),MIN(rowid)")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn claim_bot(&self, bot: &str) -> Result<Option<Run>> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if crate::workspace_transfer::frozen(&tx)? || crate::vm_maintenance::busy(&tx)? {
            return Ok(None);
        }
        let run=tx.query_row("SELECT * FROM runs WHERE status='queued' AND bot_id=?1 AND EXISTS(SELECT 1 FROM bots WHERE id=?1 AND COALESCE(json_extract(profile,'$.archived'),0)=0) AND NOT EXISTS(SELECT 1 FROM runs WHERE bot_id=?1 AND status IN ('running','awaiting_user','awaiting_approval','cancelling')) ORDER BY created,rowid LIMIT 1",[bot],run_row).optional()?;
        if let Some(r) = &run {
            tx.execute("UPDATE runs SET status='running' WHERE id=?", [&r.id])?;
        }
        tx.commit()?;
        Ok(run)
    }
    #[cfg(test)]
    pub fn claim(&self) -> Result<Option<Run>> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let run = tx
            .query_row(
                "SELECT * FROM runs WHERE status='queued' ORDER BY created,rowid LIMIT 1",
                [],
                run_row,
            )
            .optional()?;
        if let Some(r) = &run {
            tx.execute("UPDATE runs SET status='running' WHERE id=?", [&r.id])?;
        }
        tx.commit()?;
        Ok(run)
    }
    pub fn finish(&self, id: &str, status: &str, output: &str, error: &str) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let c = c.transaction()?;
        c.execute("UPDATE user_tasks SET status='expired' WHERE run_id=? AND status IN ('pending','ready')", [id])?;
        // Stop may be recorded after the provider returns but before completion
        // is saved. Keep it authoritative in the same transaction as cleanup.
        c.execute(
            "UPDATE runs SET status=CASE WHEN status IN ('cancelling','cancelled') THEN 'cancelled' ELSE ?1 END,output=?2,error=CASE WHEN status IN ('cancelling','cancelled') THEN 'Stopped by the user. Completed external actions are not undone.' ELSE ?3 END WHERE id=?4",
            params![status, output, error, id],
        )?;
        c.execute(
            "UPDATE approvals SET status='expired' WHERE run_id=? AND status='pending'",
            [id],
        )?;
        c.execute("UPDATE connector_artifacts SET status='interrupted',revision=revision+1 WHERE run_id=? AND status IN ('preparing','pending','approved','ready','executing')",[id])?;
        c.execute("INSERT OR IGNORE INTO chat_completion_pending SELECT id FROM runs WHERE id=? AND chat_id!='' AND status IN ('completed','failed','interrupted','cancelled')", [id])?;
        c.commit()?;
        Ok(())
    }
    pub fn deliver_pending_completions(&self) -> Result<()> {
        // Only finish() records pending delivery. Never reinterpret historical
        // replies without a receipt as fresh instructions during an upgrade.
        let runs = {
            let c = self.0.lock().unwrap();
            if crate::workspace_transfer::frozen(&c)? {
                return Ok(());
            }
            crate::collaboration::recover(&c)?;
            // recover() above resolves collaboration dependencies child first.
            // Delivery itself uses queue order so retries cannot monopolize a page.
            c.prepare("SELECT r.* FROM runs r JOIN chat_completion_pending p ON p.run_id=r.id WHERE r.status IN ('completed','failed','interrupted','cancelled') ORDER BY p.rowid LIMIT 100")?.query_map([], run_row)?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut first_error = None;
        for run in runs {
            if let Err(error) = self.chat_complete(&run) {
                first_error.get_or_insert(error);
                let mut c = self.0.lock().unwrap();
                let tx = c.transaction()?;
                if !crate::workspace_transfer::frozen(&tx)?
                    && tx.execute(
                        "DELETE FROM chat_completion_pending WHERE run_id=?",
                        [&run.id],
                    )? > 0
                {
                    // Move the retry to the tail atomically. A concurrent successful
                    // delivery has removed the pending row and is never reinserted.
                    tx.execute(
                        "INSERT INTO chat_completion_pending(run_id) VALUES(?)",
                        [&run.id],
                    )?;
                }
                tx.commit()?;
            }
        }
        // A bad receipt stays pending, but must not hold up unrelated results.
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.run(id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        // Stop this task and its descendants, never its parallel group siblings
        // or the user's independent queued messages.
        let mut affected = vec![id.to_string()];
        let mut seen = std::collections::HashSet::from([id.to_string()]);
        let mut at = 0;
        while at < affected.len() {
            let children:Vec<String>=tx.prepare("SELECT r.id FROM runs r WHERE r.id IN (SELECT child_run_id FROM collaboration_requests WHERE parent_run_id=?1 UNION SELECT continuation_run_id FROM collaboration_requests WHERE parent_run_id=?1 UNION SELECT continuation_run_id FROM questions WHERE run_id=?1 UNION SELECT continuation FROM command_waits WHERE run_id=?1 UNION SELECT w.run_id FROM group_wakeups w JOIN chat_messages m ON m.seq=w.message_seq WHERE m.run_id=?1)")?.query_map([&affected[at]],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for child in children {
                if seen.insert(child.clone()) {
                    affected.push(child);
                }
            }
            at += 1;
        }
        let ids = serde_json::to_string(&affected)?;
        // A completed reply can still be awaiting delivery. Remember Stop even
        // when its historical run status is already terminal, so it cannot
        // create a fresh teammate wake-up after cancellation.
        tx.execute("INSERT INTO events(run_id,kind,body,created) SELECT id,'run_stop_requested','{}',?1 FROM runs WHERE id IN(SELECT value FROM json_each(?2)) AND NOT EXISTS(SELECT 1 FROM events WHERE run_id=runs.id AND kind='run_stop_requested')",params![now(),ids])?;
        tx.execute("UPDATE command_jobs SET stop=1 WHERE run_id IN(SELECT value FROM json_each(?1)) OR id IN(SELECT item.value FROM command_waits w,json_each(w.ids) item WHERE w.run_id IN(SELECT value FROM json_each(?1)))",[&ids])?;
        let waiting_chats:Vec<String>=tx.prepare("SELECT source_chat_id FROM collaboration_requests WHERE continuation_run_id='' AND (parent_run_id IN(SELECT value FROM json_each(?1)) OR child_run_id IN(SELECT value FROM json_each(?1))) UNION SELECT r.chat_id FROM collaboration_requests e JOIN runs r ON r.id=e.child_run_id WHERE e.continuation_run_id='' AND (e.parent_run_id IN(SELECT value FROM json_each(?1)) OR e.child_run_id IN(SELECT value FROM json_each(?1)))")?.query_map([&ids],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        tx.execute("UPDATE runs SET status=CASE WHEN status='queued' THEN 'cancelled' ELSE 'cancelling' END,error=CASE WHEN status='queued' THEN 'Stopped by user' ELSE error END WHERE id IN(SELECT value FROM json_each(?)) AND status IN('queued','running','awaiting_user','awaiting_approval')",[&ids])?;
        tx.execute("UPDATE connector_artifacts SET status='interrupted',revision=revision+1 WHERE status IN('preparing','pending','approved','ready','executing') AND run_id IN(SELECT value FROM json_each(?))",[&ids])?;
        tx.execute("UPDATE questions SET status='cancelled' WHERE status='pending' AND run_id IN(SELECT value FROM json_each(?))",[&ids])?;
        tx.execute("UPDATE approvals SET status='expired' WHERE status='pending' AND run_id IN(SELECT value FROM json_each(?))",[&ids])?;
        tx.execute("UPDATE collaboration_requests SET continuation_run_id='cancelled',resolved=1 WHERE continuation_run_id='' AND (parent_run_id IN(SELECT value FROM json_each(?1)) OR child_run_id IN(SELECT value FROM json_each(?1)))",[&ids])?;
        for chat in waiting_chats.iter().filter(|chat| !chat.is_empty()) {
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'system','Requested collaboration stopped. Other tasks continue.','notice',?,?)",params![chat,id,now()])?;
        }
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT chat_id,bot_id,error,'result',id,?1 FROM runs WHERE status='cancelled' AND id IN(SELECT value FROM json_each(?2)) AND NOT EXISTS(SELECT 1 FROM chat_messages WHERE run_id=runs.id AND kind='result')",params![now(),ids])?;
        tx.commit()?;
        Ok(())
    }
    pub fn cancelled(&self, id: &str) -> bool {
        self.run(id)
            .map(|r| matches!(r.status.as_str(), "cancelling" | "cancelled"))
            .unwrap_or(true)
    }
    #[cfg(test)]
    pub fn takeover(&self) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key='takeover'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .as_deref()
            == Some("true"))
    }
    #[cfg(test)]
    pub fn set_takeover(&self, value: bool) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO settings VALUES('takeover',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[if value{"true"}else{"false"}])?;
        Ok(())
    }
    pub fn quiet_until(&self) -> Result<i64> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key='quiet_until'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0))
    }
    pub fn event(&self, run: &str, kind: &str, body: Value) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let created = now();
        tx.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,?,?,?)",
            params![run, kind, body.to_string(), created],
        )?;
        if kind == "assistant"
            && !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?)",
                [run],
                |r| r.get::<_, bool>(0),
            )?
        {
            if let Some(text) = body["text"].as_str().filter(|text| !text.trim().is_empty()) {
                let event_seq = tx.last_insert_rowid();
                tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created,source_event_seq) SELECT r.chat_id,r.bot_id,?,'assistant',r.id,?,? FROM runs r JOIN chats c ON c.id=r.chat_id WHERE r.id=?",params![text,created,event_seq,run])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn events(&self, run: &str) -> Result<Vec<Value>> {
        Ok(self.0.lock().unwrap().prepare("SELECT seq,kind,body,created FROM events WHERE run_id=? ORDER BY seq LIMIT 1000")?.query_map([run], |r| {
            let body: String=r.get(2)?; Ok(json!({"seq":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"body":serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),"created":r.get::<_,i64>(3)?}))
        })?.collect::<rusqlite::Result<_>>()?)
    }
    /// Assignment saved by routing before any model call.
    pub fn recipient_selected(&self, run: &str) -> Result<bool> {
        Ok(self.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='recipient_selected')", [run], |r| r.get(0))?)
    }
    /// Unassigned context reads stay quiet; assigned human work is already participation.
    pub fn group_activity_started(&self, run: &str) -> Result<bool> {
        Ok(self.0.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND (kind='recipient_selected' OR (kind='assistant' AND length(trim(COALESCE(json_extract(body,'$.text'),'')))>0) OR (kind='tool_started' AND COALESCE(json_extract(body,'$.tool'),'') NOT IN ('','finish_quietly','chat_read','chats_list','bots_list','bot_instructions_get','memory_search','memory_read','recall','remember','instructions_read','kindred_guide','skills_list'))))",
            [run], |r| r.get(0))?)
    }

    pub fn activity_events(&self, run: &str) -> Result<Vec<Value>> {
        let mut events:Vec<Value> = self.0.lock().unwrap().prepare("SELECT kind,body,created FROM events WHERE run_id=? AND kind IN ('tool_requested','tool_started','tool_result','model_progress','run_finished') ORDER BY seq DESC LIMIT 3")?.query_map([run], |r| {
            let body:String=r.get(1)?;
            let b:Value=serde_json::from_str(&body).unwrap_or(Value::Null);
            Ok(json!({"kind":r.get::<_,String>(0)?,"created":r.get::<_,i64>(2)?,"body":{"state":b["state"],"tool":b["tool"],"failed":b["failed"],"args":{"command":b["args"]["command"],"tool_slug":b["args"]["tool_slug"]}}}))
        })?.collect::<rusqlite::Result<_>>()?;
        events.reverse();
        Ok(events)
    }
    pub fn request_approval(&self, run: &str, tool: &str, args: &Value) -> Result<String> {
        let id = id();
        let c = self.0.lock().unwrap();
        c.execute(
            "INSERT INTO approvals(id,run_id,tool,args) VALUES(?,?,?,?)",
            params![id, run, tool, args.to_string()],
        )?;
        c.execute(
            "UPDATE runs SET status='awaiting_approval' WHERE id=? AND status='running'",
            [run],
        )?;
        if let Some(artifact) = args["artifact_id"].as_str() {
            c.execute("UPDATE connector_artifacts SET approval_id=?,status='pending' WHERE id=? AND run_id=?",params![id,artifact,run])?;
            c.execute("UPDATE connector_artifacts SET body=json_set(body,'$.forced',json(?)),revision=revision+1 WHERE id=?",params![if args["forced"]==true{"true"}else{"false"},artifact])?;
            c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) SELECT chat_id,bot_id,id,'connector_artifact',run_id,created FROM connector_artifacts WHERE id=? AND NOT EXISTS(SELECT 1 FROM chat_messages WHERE kind='connector_artifact' AND body=?)",params![artifact,artifact])?;
        }
        Ok(id)
    }
    pub fn approval(&self, id: &str) -> Result<String> {
        Ok(self.0.lock().unwrap().query_row(
            "SELECT status FROM approvals WHERE id=?",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn decide(&self, id: &str, approved: bool) -> Result<()> {
        let c = self.0.lock().unwrap();
        let n=c.execute("UPDATE approvals SET status=? WHERE id=? AND status='pending' AND run_id IN (SELECT id FROM runs WHERE status='awaiting_approval')",params![if approved{"approved"}else{"denied"},id])?;
        ensure!(n == 1, "approval is no longer pending");
        c.execute("UPDATE runs SET status='running' WHERE id=(SELECT run_id FROM approvals WHERE id=?) AND status='awaiting_approval'",[id])?;
        Ok(())
    }
    pub fn approvals(&self) -> Result<Vec<Value>> {
        Ok(self.0.lock().unwrap().prepare("SELECT id,run_id,tool,args FROM approvals WHERE status='pending'")?.query_map([], |r| Ok(json!({"id":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"tool":r.get::<_,String>(2)?,"args":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn run_approvals(&self, run: &str) -> Result<Vec<Value>> {
        Ok(self.0.lock().unwrap().prepare("SELECT id,run_id,tool,args,status FROM approvals WHERE run_id=? ORDER BY rowid LIMIT 1000")?.query_map([run], |r| Ok(json!({"id":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"tool":r.get::<_,String>(2)?,"args":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"status":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn routines(&self) -> Result<Vec<Routine>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .prepare("SELECT * FROM routines ORDER BY name")?
            .query_map([], |r| {
                Ok(Routine {
                    id: r.get(0)?,
                    bot_id: r.get(1)?,
                    name: r.get(2)?,
                    prompt: r.get(3)?,
                    interval_seconds: r.get(4)?,
                    next_run: r.get(5)?,
                    enabled: r.get(6)?,
                    run_at: r.get(8)?,
                    schedule: r
                        .get::<_, Option<String>>(7)?
                        .map(|s| serde_json::from_str(&s))
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn save_routine(&self, r: &Routine) -> Result<()> {
        self.save_routine_with_inbox(r, None)
    }
    pub(crate) fn save_routine_with_inbox(
        &self,
        r: &Routine,
        inbox: Option<&crate::provider_inbox::Binding>,
    ) -> Result<()> {
        ensure!(
            r.run_at.is_none() || r.schedule.is_none(),
            "Choose a one-time date or a repeating schedule, not both"
        );
        if let Some(at) = r.run_at {
            ensure!(
                chrono::DateTime::from_timestamp(at, 0).is_some() && at > 0,
                "Invalid one-time date"
            );
            ensure!(
                r.next_run == at,
                "One-time date does not match its next run"
            );
        }
        if let Some(schedule) = &r.schedule {
            schedule.validate()?;
            ensure!(
                r.interval_seconds == schedule.every_minutes as i64 * 60,
                "Schedule interval does not match repeat minutes"
            );
        }
        ensure!(
            r.interval_seconds >= 60 && r.interval_seconds <= 31536000,
            "interval must be 60 seconds to one year"
        );
        ensure!(
            !r.prompt.trim().is_empty()
                && r.prompt.len() <= 64000
                && !r.name.trim().is_empty()
                && r.name.len() <= 100,
            "invalid routine name/prompt"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let c = &tx;
        ensure!(
            !crate::workspace_transfer::frozen(c)?,
            "This workspace is paused"
        );
        if r.enabled {
            crate::commands::resolve(&c, &r.prompt)?;
        }
        let owner: Option<String> = c
            .query_row("SELECT bot_id FROM routines WHERE id=?", [&r.id], |row| {
                row.get(0)
            })
            .optional()?;
        ensure!(
            owner.is_none_or(|owner| owner == r.bot_id),
            "This routine belongs to another bot"
        );
        c.execute("INSERT INTO routines(id,bot_id,name,prompt,interval_seconds,next_run,enabled,schedule,run_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,prompt=excluded.prompt,interval_seconds=excluded.interval_seconds,next_run=excluded.next_run,enabled=excluded.enabled,schedule=excluded.schedule,run_at=excluded.run_at",params![r.id,r.bot_id,r.name,r.prompt,r.interval_seconds,r.next_run,r.enabled,r.schedule.as_ref().map(serde_json::to_string).transpose()?,r.run_at])?;
        if let Some(binding) = inbox {
            crate::provider_inbox::save_binding(c, r, binding)?;
        }
        if !r.enabled {
            crate::routine_controls::cancel_queued(
                c,
                &r.id,
                "Routine paused before this check started",
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn tick(&self, time: i64) -> Result<()> {
        // Notification delivery commits separately so a broken executable routine
        // cannot hold up a user's already-saved reminder.
        self.tick_reminders(time)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if crate::workspace_transfer::frozen(&tx)? {
            return Ok(());
        }
        let due=tx.prepare("SELECT id,bot_id,prompt,interval_seconds,schedule,run_at FROM routines WHERE enabled=1 AND next_run<=?")?.query_map([time],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<i64>>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (routine, bot, prompt, interval, schedule, run_at) in due {
            let schedule: Option<crate::schedules::WeeklySchedule> =
                schedule.map(|s| serde_json::from_str(&s)).transpose()?;
            let eligible = schedule
                .as_ref()
                .map(|s| s.inside_window(time))
                .transpose()?
                .unwrap_or(true);
            let archived: bool = tx.query_row(
                "SELECT COALESCE(json_extract(profile,'$.archived'),0) FROM bots WHERE id=?",
                [&bot],
                |r| r.get(0),
            )?;
            let active:i64=tx.query_row("SELECT (SELECT count(*) FROM runs WHERE bot_id=?1 AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) + (SELECT count(*) FROM command_jobs WHERE bot_id=?1 AND status IN ('starting','running')) + (SELECT count(*) FROM command_waits w JOIN runs r ON r.id=w.run_id WHERE r.bot_id=?1 AND w.continuation='')",[&bot],|r|r.get(0))?;
            if active == 0 && eligible && !archived {
                let run_id = id();
                let chat_id = format!("dm-{bot}");
                tx.execute("INSERT OR IGNORE INTO chats(id,name,members) SELECT ?,name,json_array(id) FROM bots WHERE id=?",params![chat_id,bot])?;
                tx.execute("INSERT INTO runs(id,bot_id,prompt,status,created,chat_id,round_id) VALUES(?,?,?,'queued',?,?,?)",params![run_id,bot,prompt,time,chat_id,run_id])?;
                tx.execute(
                    "INSERT INTO routine_runs(run_id,routine_id) VALUES(?,?)",
                    params![run_id, routine],
                )?;
                crate::commands::snapshot_routine(&tx, &run_id, &prompt)?;
                crate::provider_inbox::snapshot(&tx, &run_id, &routine)?;
                if run_at.is_some() {
                    // Claim the one-time delivery atomically with its task. The model
                    // never has to disable itself, and restart cannot enqueue it again.
                    tx.execute("UPDATE routines SET enabled=0 WHERE id=?", [&routine])?;
                }
                // Keep the check's instructions in task history, not as a new
                // user message every time the schedule fires.
            }
            if run_at.is_some() {
                continue;
            }
            // Coalesce missed ticks rather than flooding the queue after downtime.
            tx.execute(
                "UPDATE routines SET next_run=? WHERE id=?",
                params![
                    match schedule {
                        Some(s) => s.next_after(time)?,
                        None => time + interval,
                    },
                    routine
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn validate_bot(b: &Bot) -> Result<()> {
    ensure!(
        b.approval_mode == "inherit" || valid_approval(&b.approval_mode),
        "invalid approval mode"
    );
    ensure!(
        !b.name.trim().is_empty() && b.name.len() <= 80,
        "name must have 1..80 characters"
    );
    ensure!(
        b.instructions.len() <= BOT_INSTRUCTIONS_MAX_BYTES && b.memory.len() <= 16000,
        "Instructions are limited to 32,000 UTF-8 bytes; memory to 16,000 UTF-8 bytes"
    );
    ensure!(
        crate::provider_accounts::valid_id(&b.provider),
        "unsupported provider"
    );
    ensure!(
        b.model.len() <= 200,
        "Model ID is too long"
    );
    ensure!(
        b.reasoning_effort.len() <= 40
            && b.reasoning_effort
                .bytes()
                .all(|v| v.is_ascii_lowercase() || v == b'_'),
        "invalid thinking level"
    );
    ensure!(
        b.profile.label.len() <= 80
            && b.profile.description.len() <= 2000
            && (b.profile.local_device_id.is_empty()
                || b.profile.local_device_id == "*"
                || uuid::Uuid::parse_str(&b.profile.local_device_id).is_ok())
            && (!b.profile.local_access || !b.profile.local_device_id.is_empty()),
        "bot label or description is too long"
    );
    ensure!(
        matches!(
            b.profile.shape.as_str(),
            "round"
                | "pebble"
                | "square"
                | "capsule"
                | "triangle"
                | "bean"
                | "ghost"
                | "hexagon"
                | "cloud"
                | "drop"
        ),
        "unknown bot shape"
    );
    ensure!(
        matches!(
            b.profile.eyes.as_str(),
            "curious" | "happy" | "sleepy" | "wide"
        ),
        "unknown eye style"
    );
    ensure!(
        b.profile.color.len() == 7
            && b.profile.color.starts_with('#')
            && b.profile.color[1..].bytes().all(|v| v.is_ascii_hexdigit()),
        "Use a six-digit hex color such as #14bfc7"
    );
    Ok(())
}

pub(crate) fn write_bot(c: &Connection, b: &Bot, preserve_text: bool) -> Result<()> {
    // Preserve custom titles; only replace the exact old participant-generated name.
    let groups: Vec<(String,String,String)> = c.prepare("SELECT id,name,members FROM chats WHERE id NOT LIKE 'dm-%' AND id NOT LIKE 'server-%' AND EXISTS(SELECT 1 FROM json_each(members) WHERE value=?)")?
        .query_map([&b.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (id, title, members) in groups {
        let members: Vec<String> = serde_json::from_str(&members)?;
        let mut old = Vec::new();
        let mut new = Vec::new();
        for member in members {
            let name: Option<String> = c.query_row("SELECT name FROM bots WHERE id=?", [&member], |r| r.get(0)).optional()?;
            if let Some(name) = name {
                new.push(if member == b.id { b.name.clone() } else { name.clone() });
                old.push(name);
            }
        }
        if title == old.join(", ") {
            c.execute("UPDATE chats SET name=? WHERE id=?", params![new.join(", "),id])?;
        }
    }
    c.execute("INSERT INTO bots(id,name,instructions,provider,model,memory,auto_approve,profile,reasoning_effort,approval_mode) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,instructions=CASE WHEN ?11 THEN bots.instructions ELSE excluded.instructions END,provider=excluded.provider,model=excluded.model,memory=CASE WHEN ?11 THEN bots.memory ELSE excluded.memory END,auto_approve=excluded.auto_approve,profile=excluded.profile,reasoning_effort=excluded.reasoning_effort,approval_mode=excluded.approval_mode", params![b.id,b.name,b.instructions,b.provider,b.model,b.memory,b.auto_approve,serde_json::to_string(&b.profile)?,b.reasoning_effort,b.approval_mode,preserve_text])?;
    Ok(())
}

#[cfg(all(test, unix))]
mod database_lock_tests {
    use super::*;
    #[test]
    fn close_releases_lease_even_if_a_child_retains_a_duplicate_descriptor() {
        let root = std::env::temp_dir().join(format!("kindred-db-lease-{}", id()));
        let path = root.join("data.db");
        let first = Db::open(path.to_str().unwrap()).unwrap();
        assert!(Db::open(path.to_str().unwrap()).is_err());
        let inherited = first.1.as_ref().unwrap().0.try_clone().unwrap();
        drop(first);
        let reopened = Db::open(path.to_str().unwrap()).unwrap();
        assert!(Db::open(path.to_str().unwrap()).is_err());
        drop(inherited);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
