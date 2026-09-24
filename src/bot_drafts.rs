use crate::db::{self, Bot, BotProfile, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS bot_drafts(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),chat_id TEXT NOT NULL REFERENCES chats(id),payload TEXT NOT NULL,created_bot_id TEXT NOT NULL DEFAULT '',created INTEGER NOT NULL);")?;
    Ok(())
}
impl Db {
    pub fn draft_bot(&self, run: &Run, args: &Value) -> Result<Value> {
        let actor = self.bot(&run.bot_id)?;
        let text = |key| crate::runtime::string(args, key).map(str::to_string);
        let chosen_color = text("color")?;
        let color = match chosen_color.trim().to_ascii_lowercase().as_str() {
            "black" | "white" | "black / white" => "#ffffff",
            "grey" | "gray" => "#858a8a",
            "sky" => "#21b3ff",
            "blue" => "#2475ff",
            "periwinkle" => "#7960ff",
            "lilac" => "#b24cf2",
            "rose" => "#f24d93",
            "coral" => "#ff6952",
            "apricot" => "#ff9638",
            "honey" => "#ffbe16",
            "lime" => "#a3d92b",
            "sage" => "#2ec767",
            "mint" => "#24d5a4",
            "teal" => "#14bfc7",
            _ => chosen_color.trim(),
        }
        .to_string();
        let bot = Bot {
            id: String::new(),
            name: text("name")?,
            instructions: text("instructions")?,
            provider: actor.provider,
            model: actor.model,
            reasoning_effort: actor.reasoning_effort,
            memory: String::new(),
            auto_approve: false,
            approval_mode: "inherit".into(),
            profile: BotProfile {
                label: text("role")?,
                description: text("description")?,
                shape: text("shape")?,
                color,
                ..Default::default()
            },
        };
        db::validate_bot(&bot)?;
        ensure!(
            !bot.instructions.trim().is_empty()
                && !bot.profile.label.trim().is_empty()
                && !bot.profile.description.trim().is_empty(),
            "Fill in instructions, role and description before proposing a teammate"
        );
        let id = db::id();
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let allowed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runs r JOIN chats c ON c.id=r.chat_id JOIN bots b ON b.id=r.bot_id WHERE r.id=? AND r.bot_id=? AND r.chat_id=? AND r.status='running' AND c.archived=0 AND COALESCE(json_extract(b.profile,'$.archived'),0)=0)", params![run.id,run.bot_id,run.chat_id], |r|r.get(0))?;
        ensure!(allowed, "Drafts require an active task in an active chat");
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM bot_drafts WHERE run_id=?",
            [&run.id],
            |r| r.get(0),
        )?;
        ensure!(count < 5, "This task already has five teammate drafts");
        tx.execute(
            "INSERT INTO bot_drafts(id,run_id,chat_id,payload,created) VALUES(?,?,?,?,?)",
            params![
                id,
                run.id,
                run.chat_id,
                serde_json::to_string(&bot)?,
                db::now()
            ],
        )?;
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'bot_draft',?,?)",params![run.chat_id,run.bot_id,id,run.id,db::now()])?;
        tx.commit()?;
        Ok(
            json!({"draft_id":id,"name":bot.name,"status":"pending","message":"Draft card shown in chat. The user must click Create to add this teammate, or Details to edit it. No bot has been created yet."}),
        )
    }
    pub fn bot_draft(&self, id: &str) -> Result<Value> {
        let (payload, created): (String, String) = self.0.lock().unwrap().query_row(
            "SELECT payload,created_bot_id FROM bot_drafts WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(json!({"id":id,"bot":serde_json::from_str::<Value>(&payload)?,"created_bot_id":created}))
    }
    pub fn create_drafted_bot(&self, id: &str, edited: Option<Bot>) -> Result<Bot> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let (payload,created,archived):(String,String,bool)=tx.query_row("SELECT d.payload,d.created_bot_id,c.archived FROM bot_drafts d JOIN chats c ON c.id=d.chat_id WHERE d.id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        // Retries from any client return the original creation, even after archiving.
        if !created.is_empty() {
            let bot = tx.query_row("SELECT * FROM bots WHERE id=?", [created], db::bot_row)?;
            return Ok(bot);
        }
        ensure!(
            !archived,
            "Restore this chat before creating its proposed teammate"
        );
        let mut bot = edited.unwrap_or(serde_json::from_str(&payload)?);
        bot.id = db::id();
        bot.memory = String::new();
        bot.auto_approve = false;
        bot.approval_mode = "inherit".into();
        bot.profile.archived = false;
        db::validate_bot(&bot)?;
        db::write_bot(&tx, &bot, false)?;
        tx.execute(
            "INSERT INTO chats(id,name,members) VALUES(?,?,?)",
            params![
                format!("dm-{}", bot.id),
                bot.name,
                serde_json::to_string(&vec![&bot.id])?
            ],
        )?;
        tx.execute(
            "UPDATE bot_drafts SET created_bot_id=?,payload=? WHERE id=?",
            params![bot.id, serde_json::to_string(&bot)?, id],
        )?;
        tx.commit()?;
        Ok(bot)
    }
}
