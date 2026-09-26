//! Durable continuity metadata extending notes, messages and existing execution records.
use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn check_version(c: &Connection) -> Result<()> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='continuity_schema')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        let latest: i64 = c.query_row(
            "SELECT COALESCE(max(version),0) FROM continuity_schema",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            latest <= 2,
            "Continuity schema is newer than this binary; restore a compatible backup or use the newer binary"
        );
    }
    Ok(())
}

pub fn migrate(c: &Connection) -> Result<()> {
    check_version(c)?;
    let tx = c.unchecked_transaction()?;
    tx.execute_batch(include_str!("migrations/continuity-2.sql"))?;
    // Retire old write triggers. The old disposable index can be removed offline;
    // dropping a large virtual table here would itself block startup.
    tx.execute_batch("DROP TRIGGER IF EXISTS history_search_insert; DROP TRIGGER IF EXISTS history_search_update; DROP TRIGGER IF EXISTS history_search_delete;")?;
    tx.commit()?;
    Ok(())
}

pub fn epoch(c: &Connection, chat: &str) -> Result<i64> {
    Ok(c.query_row(
        "SELECT epoch FROM continuity_epochs WHERE chat_id=?",
        [chat],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(0))
}

/// Deliberately conservative: the owner's private DM may recall owned history;
/// every other destination is restricted to its own conversation, even if the bot
/// belongs to both rooms. Server-shared rooms never inherit private DM context.
pub fn can_disclose(db: &Db, run: &Run, source: &str) -> Result<bool> {
    if !db.chat(source)?.members.contains(&run.bot_id)
        || !db.chat(&run.chat_id)?.members.contains(&run.bot_id)
    {
        return Ok(false);
    }
    Ok(source == run.chat_id || run.chat_id == format!("dm-{}", run.bot_id))
}

pub fn allowed_chats(db: &Db, run: &Run) -> Result<Vec<String>> {
    super::require_chat(db, run)?;
    if run.chat_id != format!("dm-{}", run.bot_id) {
        return Ok(vec![run.chat_id.clone()]);
    }
    let c = db.0.lock().unwrap();
    Ok(c.prepare("SELECT id FROM chats WHERE EXISTS(SELECT 1 FROM json_each(members) WHERE value=?) ORDER BY id")?.query_map([&run.bot_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// A bounded, restart-safe backfill. New writes are indexed immediately. Never
/// called by a startup migration; unindexed rows remain searchable by fallback.
pub fn backfill(db: &Db, limit: usize) -> Result<bool> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    if crate::workspace_transfer::frozen(&tx)? {
        return Ok(false);
    }
    // Retire the superseded disposable index outside startup, atomically. No
    // readers use it after migration; dropping also removes stale shadow rows.
    tx.execute_batch("DROP TABLE IF EXISTS history_search_index;")?;
    let (cursor, complete): (i64, bool) = tx.query_row(
        "SELECT cursor,complete FROM continuity_index_progress WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if complete {
        return Ok(true);
    }
    let rows = tx
        .prepare("SELECT seq,body FROM chat_messages WHERE seq>? ORDER BY seq LIMIT ?")?
        .query_map(params![cursor, limit.clamp(1, 256)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, body) in &rows {
        tx.execute("DELETE FROM continuity_search WHERE rowid=?", [seq])?;
        tx.execute(
            "INSERT INTO continuity_search(rowid,body) VALUES(?,?)",
            params![seq, body],
        )?;
    }
    let done = rows.len() < limit.clamp(1, 256);
    tx.execute(
        "UPDATE continuity_index_progress SET cursor=?,complete=? WHERE id=1",
        params![rows.last().map(|r| r.0).unwrap_or(cursor), done],
    )?;
    tx.commit()?;
    Ok(done)
}

pub async fn worker(app: crate::runtime::Shared) {
    loop {
        let owner = app.clone();
        let result = tokio::task::spawn_blocking(move || backfill(&owner.db, 128)).await;
        match result {
            Ok(Ok(true)) => return,
            Ok(Ok(false)) => {}
            _ => eprintln!("Continuity indexing paused; keyword fallback remains available"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn source_run_valid(c: &Connection, source: &str, chat: &str) -> Result<bool> {
    let source_epoch: i64 = c
        .query_row(
            "SELECT epoch FROM continuity_sessions WHERE run_id=?",
            [source],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(session_valid(c, source)? && source_epoch == epoch(c, chat)?)
}

pub fn source_refs(c: &Connection, run: &Run, args: &Value) -> Result<Value> {
    let refs = args["sources"].as_array().cloned().unwrap_or_default();
    ensure!(refs.len() <= 24, "Keep at most 24 exact source references");
    let mut checked = Vec::new();
    for source in refs {
        if let Some(seq) = source["message_seq"].as_i64() {
            let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM continuity_current_messages m WHERE m.seq=? AND m.chat_id=? AND m.suppressed=0 AND NOT EXISTS(SELECT 1 FROM continuity_source_state s WHERE s.message_seq=m.seq AND s.status<>'active'))",params![seq,run.chat_id],|r|r.get(0))?;
            ensure!(
                valid,
                "Source must be a current retained message in this conversation"
            );
            let body: String =
                c.query_row("SELECT body FROM chat_messages WHERE seq=?", [seq], |r| {
                    r.get(0)
                })?;
            checked.push(json!({"message_seq":seq,"digest":digest(&body)}));
        } else if let Some(seq) = source["event_seq"].as_i64() {
            let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM events e JOIN runs r ON r.id=e.run_id WHERE e.seq=? AND r.bot_id=? AND r.chat_id=? AND e.kind IN ('tool_result','tool_started','run_finished'))",params![seq,run.bot_id,run.chat_id],|r|r.get(0))?;
            ensure!(
                valid,
                "Event source must belong to this bot and conversation"
            );
            let source_run: String =
                c.query_row("SELECT run_id FROM events WHERE seq=?", [seq], |r| r.get(0))?;
            ensure!(
                source_run_valid(c, &source_run, &run.chat_id)?,
                "Event source was invalidated; inspect current evidence"
            );
            let body: String =
                c.query_row("SELECT body FROM events WHERE seq=?", [seq], |r| r.get(0))?;
            checked.push(json!({"event_seq":seq,"digest":digest(&body)}));
        } else {
            anyhow::bail!("Use message_seq or event_seq source references");
        }
    }
    Ok(json!(checked))
}

pub fn save_meta(c: &Connection, run: &Run, args: &Value, revision: i64) -> Result<()> {
    let kind = args["kind"].as_str().unwrap_or("work");
    let statement = args["statement_kind"].as_str().unwrap_or("inference");
    ensure!(
        matches!(kind, "work" | "knowledge" | "preference" | "checkpoint"),
        "Invalid continuity kind"
    );
    ensure!(
        matches!(statement, "user_statement" | "observation" | "inference"),
        "Invalid statement kind"
    );
    let sources = source_refs(c, run, args)?;
    if statement != "inference" {
        ensure!(
            !sources.as_array().unwrap().is_empty(),
            "Attributed statements require source references"
        );
    }
    if statement == "user_statement" {
        for source in sources.as_array().unwrap() {
            ensure!(
                source["message_seq"].is_i64(),
                "User statements require user message sources"
            );
            let sender: String = c.query_row(
                "SELECT sender FROM chat_messages WHERE seq=?",
                [source["message_seq"].as_i64().unwrap()],
                |r| r.get(0),
            )?;
            ensure!(
                sender == "user" || sender.starts_with("human:"),
                "Source is not a user statement"
            );
        }
    }
    c.execute("INSERT INTO continuity_note_meta VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(bot_id,chat_id,topic) DO UPDATE SET revision=excluded.revision,epoch=excluded.epoch,kind=excluded.kind,statement_kind=excluded.statement_kind,sources=excluded.sources",params![run.bot_id,run.chat_id,args["topic"].as_str().unwrap_or(""),revision,epoch(c,&run.chat_id)?,kind,statement,sources.to_string()])?;
    Ok(())
}

pub fn start_session(db: &Db, run: &Run, provider: &str, model: &str) -> Result<()> {
    if run.chat_id.is_empty() {
        return Ok(());
    }
    let chats = allowed_chats(db, run)?;
    let mut connection = db.0.lock().unwrap();
    let c = connection.transaction()?;
    c.execute("INSERT OR IGNORE INTO continuity_sessions VALUES(?,?,?,?,(SELECT COALESCE(max(seq),0) FROM chat_messages WHERE chat_id=?),?,?,?)",params![run.id,run.bot_id,run.chat_id,epoch(&c,&run.chat_id)?,run.chat_id,provider,model,db::now()])?;
    for chat in chats {
        c.execute(
            "INSERT OR IGNORE INTO continuity_dependencies VALUES(?,?,?)",
            params![run.id, chat, epoch(&c, &chat)?],
        )?;
    }
    c.commit()?;
    Ok(())
}

pub fn memory(db: &Db, run: &Run, value: &str) -> Result<Value> {
    let invalid: bool = db.0.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM continuity_memory_guard WHERE bot_id=? AND invalidated=1)",
        [&run.bot_id],
        |r| r.get(0),
    )?;
    Ok(if run.chat_id == format!("dm-{}", run.bot_id) && !invalid {
        json!(value)
    } else {
        Value::Null
    })
}

fn link(c: &Connection, run: &Run, kind: &str, id: &str) -> Result<Value> {
    if kind.is_empty() {
        ensure!(id.is_empty(), "Link kind required");
        return Ok(json!({"scheduled":false,"execution":"none"}));
    }
    let (sql, reminder) = match kind {
        "routine" => (
            "SELECT CASE WHEN enabled=1 THEN 'enabled' ELSE 'paused' END FROM routines WHERE id=? AND bot_id=? AND ('dm-'||bot_id)=?",
            false,
        ),
        "reminder" => (
            "SELECT status FROM reminders WHERE id=? AND bot_id=? AND chat_id=?",
            true,
        ),
        "run" => (
            "SELECT status FROM runs WHERE id=? AND bot_id=? AND chat_id=?",
            false,
        ),
        "command" => (
            "SELECT j.status FROM command_jobs j JOIN runs r ON r.id=j.run_id WHERE j.id=? AND j.bot_id=? AND r.chat_id=?",
            false,
        ),
        _ => anyhow::bail!("Invalid execution link kind"),
    };
    let status: String = c
        .query_row(sql, params![id, run.bot_id, run.chat_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| {
            anyhow::anyhow!("Execution link must belong to this bot and conversation")
        })?;
    Ok(
        json!({"kind":kind,"id":id,"status":status,"notification_only":reminder,"scheduled":(kind=="routine" && status=="enabled") || (reminder && status=="pending"),"outcome_requires_receipt":true}),
    )
}

pub fn obligation_save(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    super::require_chat(db, run)?;
    let key = args["key"].as_str().unwrap_or("").trim();
    let description = args["description"].as_str().unwrap_or("").trim();
    let status = args["status"].as_str().unwrap_or("open");
    ensure!(
        !key.is_empty() && key.len() <= 100 && !description.is_empty() && description.len() <= 2000,
        "Use a stable key and concise obligation"
    );
    ensure!(
        matches!(status, "open" | "waiting" | "completed" | "cancelled"),
        "Invalid obligation status"
    );
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let old: Option<(String,i64)> = tx.query_row("SELECT id,revision FROM continuity_obligations WHERE bot_id=? AND chat_id=? AND request_key=?",params![run.bot_id,run.chat_id,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    ensure!(
        session_valid(&tx, &run.id)?,
        "Sources changed during this session; rebuild context before updating an obligation"
    );
    let (id, revision) = old.unwrap_or_else(|| (db::id(), 0));
    ensure!(
        args["expected_revision"].as_i64() == Some(revision),
        "Obligation changed; read and merge before saving"
    );
    let sources = source_refs(&tx, run, args)?;
    ensure!(
        !sources.as_array().unwrap().is_empty(),
        "An obligation requires retained evidence; recording it does not schedule execution"
    );
    let kind = args["link_kind"].as_str().unwrap_or("");
    let target = args["link_id"].as_str().unwrap_or("");
    let execution = link(&tx, run, kind, target)?;
    tx.execute("INSERT INTO continuity_obligations VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(bot_id,chat_id,request_key) DO UPDATE SET revision=excluded.revision,description=excluded.description,status=excluded.status,source_run_id=excluded.source_run_id,sources=excluded.sources,link_kind=excluded.link_kind,link_id=excluded.link_id,updated=excluded.updated",params![id,run.bot_id,run.chat_id,key,revision+1,description,status,run.id,sources.to_string(),kind,target,db::now()])?;
    tx.commit()?;
    Ok(
        json!({"id":id,"revision":revision+1,"execution":execution,"saved":true,"instructions":"This record does not create a timer, monitor or executable task. Completion is an attributed claim; inspect receipts."}),
    )
}

pub fn obligations(db: &Db, run: &Run, after: &str, include_closed: bool) -> Result<Value> {
    let chats = allowed_chats(db, run)?;
    let c = db.0.lock().unwrap();
    let total:i64=c.query_row("SELECT count(*) FROM continuity_obligations WHERE bot_id=? AND chat_id IN(SELECT value FROM json_each(?)) AND (status IN('open','waiting') OR ?)",params![run.bot_id,json!(chats).to_string(),include_closed],|r|r.get(0))?;
    let mut rows=c.prepare("SELECT id,chat_id,request_key,revision,description,status,sources,link_kind,link_id,source_run_id FROM continuity_obligations WHERE bot_id=? AND chat_id IN(SELECT value FROM json_each(?)) AND (status IN('open','waiting') OR ?) AND id>? ORDER BY id LIMIT 17")?.query_map(params![run.bot_id,json!(chats).to_string(),include_closed,after],|r|Ok(json!({"id":r.get::<_,String>(0)?,"chat_id":r.get::<_,String>(1)?,"key":r.get::<_,String>(2)?,"revision":r.get::<_,i64>(3)?,"description":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"sources":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(json!([])),"link_kind":r.get::<_,String>(7)?,"link_id":r.get::<_,String>(8)?,"source_run_id":r.get::<_,String>(9)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > 16;
    rows.truncate(16);
    for row in &mut rows {
        let mut owner = run.clone();
        owner.chat_id = row["chat_id"].as_str().unwrap().into();
        row["execution"] = link(
            &c,
            &owner,
            row["link_kind"].as_str().unwrap(),
            row["link_id"].as_str().unwrap(),
        )
        .unwrap_or(json!({"available":false,"scheduled":false}));
        let mut invalid = false;
        for source in row["sources"].as_array().unwrap() {
            if let Some(seq) = source["message_seq"].as_i64() {
                let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM continuity_current_messages m WHERE seq=? AND suppressed=0 AND NOT EXISTS(SELECT 1 FROM continuity_source_state s WHERE s.message_seq=m.seq AND s.status<>'active'))",[seq],|r|r.get(0))?;
                invalid |= !valid;
                if let Some(expected) = source["digest"].as_str() {
                    let body: Option<String> = c
                        .query_row("SELECT body FROM chat_messages WHERE seq=?", [seq], |r| {
                            r.get(0)
                        })
                        .optional()?;
                    invalid |= body.is_none_or(|body| digest(&body) != expected);
                }
            }
            if let Some(seq) = source["event_seq"].as_i64() {
                let record: Option<(String,String,String)> = c.query_row(
                    "SELECT e.body,e.run_id,r.chat_id FROM events e JOIN runs r ON r.id=e.run_id WHERE e.seq=?", [seq], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))
                ).optional()?;
                invalid |= match record {
                    None => true,
                    Some((body, source_run, chat)) => {
                        !source_run_valid(&c, &source_run, &chat)?
                            || source["digest"]
                                .as_str()
                                .is_some_and(|expected| digest(&body) != expected)
                    }
                };
            }
        }
        row["requires_reconciliation"] = json!(invalid);
        if invalid {
            row["description"] =
                json!("Source changed or removed; inspect current records before continuing.");
        }
    }
    Ok(
        json!({"items":rows,"total":total,"next_after":if more{rows.last().map(|r|r["id"].clone())}else{None},"instructions":"Read further pages when total exceeds the injected selection. Obligations are never scheduled by saving memory."}),
    )
}

/// Live authoritative pending records; no search ranking or summaries decide
/// whether they exist. Payloads stay behind the corresponding existing tools.
pub fn pending(db: &Db, run: &Run) -> Result<Value> {
    let chats = allowed_chats(db, run)?;
    let c = db.0.lock().unwrap();
    let mut result = json!({});
    for (name, sql) in [
        (
            "questions",
            "SELECT id FROM questions WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status='pending'",
        ),
        (
            "reminders",
            "SELECT id FROM reminders WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status IN('pending','paused')",
        ),
        (
            "routines",
            "SELECT id FROM routines WHERE bot_id=?1 AND ('dm-'||bot_id) IN(SELECT value FROM json_each(?2)) AND enabled=1",
        ),
        (
            "runs",
            "SELECT id FROM runs WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status IN('queued','running','awaiting_user','awaiting_approval','interrupted')",
        ),
        (
            "commands",
            "SELECT j.id FROM command_jobs j JOIN runs r ON r.id=j.run_id WHERE j.bot_id=?1 AND r.chat_id IN(SELECT value FROM json_each(?2)) AND j.status IN('starting','running')",
        ),
    ] {
        let total: i64 = c.query_row(
            &format!("SELECT count(*) FROM ({sql})"),
            params![run.bot_id, json!(chats).to_string()],
            |r| r.get(0),
        )?;
        let ids = c
            .prepare(&format!("{sql} ORDER BY 1 LIMIT 8"))?
            .query_map(params![run.bot_id, json!(chats).to_string()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result[name] = json!({"total":total,"ids":ids,"shortened":total>8});
    }
    Ok(result)
}

/// Explicit source supersession/forgetting, used by the authenticated owner API.
/// No model inference automatically edits a user's original words.
pub fn revise_source(db: &Db, seq: i64, args: &Value) -> Result<Value> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        !crate::workspace_transfer::frozen(&tx)?,
        "Workspace is paused"
    );
    let (chat, body, source_run): (String, String, String) = tx.query_row(
        "SELECT chat_id,body,run_id FROM chat_messages WHERE seq=?",
        [seq],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        args["expected_text"].as_str() == Some(&body),
        "Source changed; read it again"
    );
    let revision: i64 = tx
        .query_row(
            "SELECT revision FROM continuity_source_state WHERE message_seq=?",
            [seq],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    ensure!(
        args["expected_revision"].as_i64() == Some(revision),
        "Source revision changed"
    );
    let action = args["action"].as_str().unwrap_or("");
    ensure!(
        matches!(action, "supersede" | "forget"),
        "Choose supersede or forget"
    );
    let successor = args["successor"].as_i64();
    if action == "supersede" {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_messages m WHERE seq=? AND seq>? AND chat_id=? AND suppressed=0 AND NOT EXISTS(SELECT 1 FROM continuity_source_state s WHERE s.message_seq=m.seq AND s.status<>'active'))",params![successor,seq,chat],|r|r.get(0))?;
        ensure!(
            valid,
            "Choose a newer retained correction in this conversation"
        );
    }
    tx.execute("INSERT INTO continuity_source_state VALUES(?,?,?,?,?,?) ON CONFLICT(message_seq) DO UPDATE SET revision=excluded.revision,status=excluded.status,successor=excluded.successor,updated=excluded.updated",params![seq,chat,revision+1,if action=="forget"{"deleted"}else{"superseded"},successor,db::now()])?;
    tx.execute("INSERT INTO continuity_epochs VALUES(?,1) ON CONFLICT(chat_id) DO UPDATE SET epoch=epoch+1",[&chat])?;
    tx.execute("INSERT INTO continuity_memory_guard SELECT value,1 FROM json_each((SELECT members FROM chats WHERE id=?)) WHERE value IN(SELECT id FROM bots) ON CONFLICT(bot_id) DO UPDATE SET invalidated=1",[&chat])?;
    if action == "forget" {
        tx.execute(
            "UPDATE chat_messages SET body='[Removed by owner]',suppressed=1 WHERE seq=?",
            [seq],
        )?;
        // Coarse redaction prevents untracked legacy derivations from returning.
        tx.execute("UPDATE continuity_notes SET summary='[Source removed]',status='closed' WHERE chat_id=?",[&chat])?;
        tx.execute("UPDATE continuity_revisions SET summary='[Source removed]',status='closed' WHERE chat_id=?",[&chat])?;
        tx.execute("UPDATE continuity_obligations SET description='Source removed; reconcile before continuing' WHERE chat_id=?",[&chat])?;
        tx.execute("UPDATE events SET body='{}' WHERE kind='context' AND run_id IN(SELECT id FROM runs WHERE chat_id=?)",[&chat])?;
        // Source run activity may quote the deleted input. Retain execution IDs,
        // statuses and receipt structure; redact natural-language payloads.
        if !source_run.is_empty() {
            tx.execute(
                "UPDATE runs SET prompt='[Source removed]',output='',error='' WHERE id=?",
                [&source_run],
            )?;
        }
    }
    // A checkpoint can have consumed this conversation through automatic recall.
    // Redact dependent derived prose on forgetting; supersession only invalidates.
    if action == "forget" {
        tx.execute("UPDATE continuity_notes SET summary='[Source removed]',status='closed' WHERE run_id IN(SELECT run_id FROM continuity_dependencies WHERE source_chat_id=?)",[&chat])?;
        tx.execute("UPDATE continuity_revisions SET summary='[Source removed]',status='closed' WHERE run_id IN(SELECT run_id FROM continuity_dependencies WHERE source_chat_id=?)",[&chat])?;
        tx.execute("UPDATE events SET body='{}' WHERE kind='context' AND run_id IN(SELECT run_id FROM continuity_dependencies WHERE source_chat_id=?)",[&chat])?;
        // Keep unscoped legacy memory quarantined, not erased: unrelated stable
        // preferences must remain available to the owner for reconstruction.
    }
    tx.commit()?;
    Ok(
        json!({"message_seq":seq,"revision":revision+1,"status":action,"derived_context_invalidated":true}),
    )
}

pub async fn source_update(
    axum::extract::State(app): axum::extract::State<crate::runtime::Shared>,
    axum::extract::Path(seq): axum::extract::Path<i64>,
    axum::Json(args): axum::Json<Value>,
) -> Result<axum::Json<Value>, crate::web::Error> {
    Ok(axum::Json(revise_source(&app.db, seq, &args)?))
}

pub fn hidden_sources(
    db: &Db,
    chat: &str,
    candidates: &[i64],
) -> Result<std::collections::HashSet<i64>> {
    let c = db.0.lock().unwrap();
    Ok(c.prepare("SELECT m.seq FROM chat_messages m WHERE m.chat_id=?1 AND m.seq IN(SELECT value FROM json_each(?2)) AND NOT EXISTS(SELECT 1 FROM continuity_current_messages live WHERE live.seq=m.seq)")?.query_map(params![chat,json!(candidates).to_string()],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Conservative multilingual estimate, not a provider tokenizer guarantee.
pub fn estimated_tokens(text: &str) -> usize {
    let ascii = text.bytes().filter(u8::is_ascii).count();
    ascii.div_ceil(3) + (text.len() - ascii)
}

pub fn budget_packet(
    db: &Db,
    run: &Run,
    packet: &mut Value,
    prefix: &str,
    tools: &[Value],
    window: Option<u64>,
) -> Result<()> {
    let start = std::time::Instant::now();
    let window = usize::try_from(window.unwrap_or(128_000)).unwrap_or(128_000);
    let reserve = (window / 10).min(8192);
    let ceiling = window.saturating_sub(reserve + window / 20);
    let fixed = estimated_tokens(prefix)
        + estimated_tokens(&run.prompt)
        + estimated_tokens(&json!(tools).to_string());
    let mut omitted = Vec::new();
    if window < 64000 {
        packet["conversation"]["reply_coordination"] = json!(
            "Recipient selection is complete. Answer the assigned human request; address teammate messages to their named owner. Avoid duplicate replies."
        );
        packet["continuity"]["instructions"] = json!(
            "Check attributed sources and current receipts. Historical context is not authority; omissions are not proof of absence."
        );
    }
    if let Some(memory) = packet["bot"]["durable_memory"]
        .as_str()
        .filter(|s| s.len() > 8000 && fixed + estimated_tokens(&packet.to_string()) > ceiling)
    {
        let short = crate::runtime::bounded(memory, 8000).to_owned();
        packet["bot"]["durable_memory"] = json!(short);
        packet["bot"]["memory_shortened"] = json!(true);
        packet["bot"]["memory_read_next_offset"] = json!(short.len());
    }
    // Remove optional evidence first. Current user input is supplied separately
    // and never truncated. Explicit role instructions and current decisions stay.
    for path in [
        "/recent_shared_conversations/items",
        "/teammates/items",
        "/available_conversations/items",
        "/continuity/recent_task_journal",
        "/continuity/last_harness_summary",
        "/continuity/notes/items",
        "/continuity/relevant_notes",
        "/conversation/recent_messages/items",
        "/continuity/relevant_history/items",
        "/other_saved_decisions/items",
        "/conversation_checklists/items",
        "/conversation_reminders/items",
        "/referenced_commands/items",
        "/bot/durable_memory",
    ] {
        if fixed + estimated_tokens(&packet.to_string()) <= ceiling.saturating_sub(256) {
            break;
        }
        if path == "/bot/durable_memory"
            && packet
                .pointer(path)
                .and_then(Value::as_str)
                .is_some_and(|s| s.len() <= 8000)
        {
            continue;
        }
        if let Some(value) = packet.pointer_mut(path) {
            match value {
                Value::Array(items) => {
                    while !items.is_empty() {
                        // Packet serialization cannot borrow through this mutable
                        // list, so remove one entry then re-evaluate below.
                        items.pop();
                        omitted.push(path.to_owned());
                        break;
                    }
                }
                Value::Null => {}
                _ => {
                    *value = Value::Null;
                    omitted.push(path.to_owned());
                }
            }
        }
        // Finish trimming this section if needed before touching a higher priority.
        while fixed + estimated_tokens(&packet.to_string()) > ceiling.saturating_sub(256)
            && packet
                .pointer(path)
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
        {
            packet
                .pointer_mut(path)
                .unwrap()
                .as_array_mut()
                .unwrap()
                .pop();
            omitted.push(path.to_owned());
        }
    }
    let mut omitted_sections = std::collections::BTreeMap::new();
    for path in &omitted {
        *omitted_sections.entry(path).or_insert(0usize) += 1;
    }
    packet["context_selection"] = json!({"estimated":true,"window_tokens":window,"reserved_output_tokens":reserve,"omitted_sections":omitted_sections,"instructions":"Omitted records remain available through tools. Pending counts do not imply their records were all loaded. Do not infer absence or completion."});
    let used = fixed + estimated_tokens(&packet.to_string());
    ensure!(
        used <= ceiling,
        "Selected model context is too small for the current request, tools and required instructions; use a larger-context model or shorten the request. No required instructions were silently discarded (estimated {used}, fixed {fixed}, ceiling {ceiling}, window {window})"
    );
    // No text, queries, embeddings, credentials or source excerpts in diagnostics.
    db.event(&run.id,"context_selection",json!({"version":2,"window_tokens":window,"estimated_input_tokens":used,"reserved_output_tokens":reserve,"omitted_items":omitted.len(),"budget_us":start.elapsed().as_micros().min(u64::MAX as u128) as u64,"private_memory_included":packet["bot"]["durable_memory"].is_string(),"history_items":packet["continuity"]["relevant_history"]["items"].as_array().map(Vec::len).unwrap_or(0),"open_obligations":packet["continuity"]["obligations"]["total"]}))?;
    Ok(())
}

fn digest(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}

pub fn session_valid(c: &Connection, run: &str) -> Result<bool> {
    Ok(!c.query_row("SELECT EXISTS(SELECT 1 FROM continuity_dependencies d LEFT JOIN continuity_epochs e ON e.chat_id=d.source_chat_id WHERE d.run_id=? AND d.epoch<>COALESCE(e.epoch,0))",[run],|r|r.get::<_,bool>(0))?)
}

pub fn pending_page(db: &Db, run: &Run, kind: &str, after: &str) -> Result<Value> {
    let chats = allowed_chats(db, run)?;
    let c = db.0.lock().unwrap();
    let sql = match kind {
        "questions" => {
            "SELECT id,status,chat_id FROM questions WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status='pending' AND id>?3"
        }
        "reminders" => {
            "SELECT id,status,chat_id FROM reminders WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status IN('pending','paused') AND id>?3"
        }
        "routines" => {
            "SELECT id,CASE WHEN enabled=1 THEN 'enabled' ELSE 'paused' END,('dm-'||bot_id) AS chat_id FROM routines WHERE bot_id=?1 AND ('dm-'||bot_id) IN(SELECT value FROM json_each(?2)) AND id>?3"
        }
        "runs" => {
            "SELECT id,status,chat_id FROM runs WHERE bot_id=?1 AND chat_id IN(SELECT value FROM json_each(?2)) AND status IN('queued','running','awaiting_user','awaiting_approval','interrupted') AND id>?3"
        }
        "commands" => {
            "SELECT j.id,j.status,r.chat_id FROM command_jobs j JOIN runs r ON r.id=j.run_id WHERE j.bot_id=?1 AND r.chat_id IN(SELECT value FROM json_each(?2)) AND j.status IN('starting','running') AND j.id>?3"
        }
        _ => anyhow::bail!("Unknown pending record kind"),
    };
    let mut rows=c.prepare(&format!("{sql} ORDER BY 1 LIMIT 33"))?.query_map(params![run.bot_id,json!(chats).to_string(),after],|r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"chat_id":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > 32;
    rows.truncate(32);
    Ok(
        json!({"items":rows,"next_after":if more{rows.last().map(|v|v["id"].clone())}else{None},"kind":kind}),
    )
}

pub fn receipts(db: &Db, run: &Run, source: &str, before: i64) -> Result<Value> {
    let owner = db.run(source)?;
    ensure!(
        owner.bot_id == run.bot_id && can_disclose(db, run, &owner.chat_id)?,
        "Run is outside this destination disclosure boundary"
    );
    let c = db.0.lock().unwrap();
    let valid = source_run_valid(&c, source, &owner.chat_id)?;
    let mut rows=c.prepare("SELECT seq,kind,body,created FROM events WHERE run_id=? AND seq<? AND kind IN('tool_requested','tool_started','tool_result','approval','run_finished','process_wait','question_wait') ORDER BY seq DESC LIMIT 17")?.query_map(params![source,before],|r|{
        let mut body:Value=serde_json::from_str(&r.get::<_,String>(2)?).unwrap_or(json!({}));
        if !valid {let old=body;body=json!({"tool":old["tool"],"call_id":old["call_id"],"failed":old["failed"],"exit_code":old["exit_code"],"payload_withheld":true});}
        let raw=body.to_string();
        Ok(json!({"seq":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"body_excerpt":crate::runtime::bounded(&raw,2400),"shortened":raw.len()>2400,"created":r.get::<_,i64>(3)?}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > 16;
    rows.truncate(16);
    Ok(
        json!({"run_id":source,"status":owner.status,"events":rows,"next_before":if more{rows.last().map(|r|r["seq"].clone())}else{None},"instructions":"A requested or started action is not verified success. A completed run is not proof each external action succeeded. Check tool results and current state; never repeat an action with uncertain outcome merely to reconstruct context."}),
    )
}
