//! Disposable, destination-scoped provider checkpoints. Chat history and action
//! receipts remain authoritative. A checkpoint is never an execution queue.
use crate::{db::{self, Bot, Db, Run}, runtime::{self, App}};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const MAX_CHECKPOINT: usize = 4 * 1024 * 1024;
const MAX_AGE: i64 = 7 * 86400;
// A retained CLI conversation is a working set, not the durable archive.
const CLAUDE_REUSE_BUDGET: u64 = 256_000;

pub fn migrate(c: &Connection) -> Result<()> {
    // Deliberately absent from workspace_transfer's export allow-list. A restored
    // workspace must bootstrap from its own authoritative history and credentials.
    c.execute_batch("CREATE TABLE IF NOT EXISTS harness_contexts(
        bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
        chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
        fingerprint TEXT NOT NULL, session_id TEXT NOT NULL,
        lease TEXT NOT NULL, run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
        checkpoint TEXT, checkpoint_hash TEXT, updated INTEGER NOT NULL,
        PRIMARY KEY(bot_id,chat_id));
        CREATE TRIGGER IF NOT EXISTS harness_context_source_revision AFTER UPDATE OF epoch ON continuity_epochs
        BEGIN DELETE FROM harness_contexts WHERE chat_id=NEW.chat_id OR run_id IN
            (SELECT run_id FROM continuity_dependencies WHERE source_chat_id=NEW.chat_id); END;
        CREATE TRIGGER IF NOT EXISTS harness_context_source_first_revision AFTER INSERT ON continuity_epochs
        BEGIN DELETE FROM harness_contexts WHERE chat_id=NEW.chat_id OR run_id IN
            (SELECT run_id FROM continuity_dependencies WHERE source_chat_id=NEW.chat_id); END;
        CREATE TRIGGER IF NOT EXISTS harness_context_membership AFTER UPDATE OF members ON chats
        WHEN OLD.members<>NEW.members BEGIN DELETE FROM harness_contexts; END;
        CREATE TRIGGER IF NOT EXISTS harness_context_failed AFTER UPDATE OF status ON runs
        WHEN NEW.status IN ('failed','cancelled','interrupted','withdrawn')
        BEGIN DELETE FROM harness_contexts WHERE run_id=NEW.id; END;")?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS harness_connection_generations(provider TEXT PRIMARY KEY,generation INTEGER NOT NULL);")?;
    Ok(())
}

pub fn connection_generation(db: &Db, provider: &str) -> Result<i64> {
    Ok(db.0.lock().unwrap().query_row("SELECT generation FROM harness_connection_generations WHERE provider=?",[provider],|r|r.get(0)).optional()?.unwrap_or(0))
}

pub fn connection_changed(db: &Db, provider: &str) -> Result<()> {
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    tx.execute("INSERT INTO harness_connection_generations VALUES(?,1) ON CONFLICT(provider) DO UPDATE SET generation=generation+1",[provider])?;
    tx.execute("DELETE FROM harness_contexts WHERE bot_id IN(SELECT id FROM bots WHERE provider=?)",[provider])?;
    tx.commit()?;
    Ok(())
}

fn digest(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref().iter().map(|v| format!("{v:02x}")).collect()
}

/// Only IDs, membership, epochs and access configuration enter this fingerprint;
/// source text and secrets never enter diagnostics. DB ownership scopes profiles.
fn policy(db: &Db, run: &Run) -> Result<Value> {
    let chats = crate::continuity::store::allowed_chats(db, run)?;
    let bot = db.bot(&run.bot_id)?;
    let general = db::general_settings(db.setting("general")?);
    let c = db.0.lock().unwrap();
    let mut scopes = Vec::new();
    for chat in chats {
        let members: String = c.query_row("SELECT members FROM chats WHERE id=?", [&chat], |r| r.get(0))?;
        scopes.push(json!([chat, crate::continuity::store::epoch(&c, &chat)?, members]));
    }
    let grants = c.prepare("SELECT origin,account_key,connector_key,permission FROM connector_grants WHERE bot_id=? ORDER BY 1,2,3")?
        .query_map([&run.bot_id], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let actions = c.prepare("SELECT origin,account_key,connector_key,action,permission FROM connector_action_grants WHERE bot_id=? ORDER BY 1,2,3,4")?
        .query_map([&run.bot_id], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let accounts = c.prepare("SELECT origin,account_key FROM connector_catalogue ORDER BY origin")?
        .query_map([], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let devices = c.prepare("SELECT id,mode FROM local_devices ORDER BY id")?
        .query_map([], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Revisions include disabled rules: omitting a revoked scope from the new
    // snapshot must also discard native history that carried its former grant.
    let work_rules = c.prepare("SELECT rule_key,revision,source_chat,source_seq,source_hash,scope,enabled,body FROM standing_work_rules WHERE bot_id=? ORDER BY rule_key")?
        .query_map([&run.bot_id], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Vec<u8>>(4)?,r.get::<_,String>(5)?,r.get::<_,bool>(6)?,digest(&r.get::<_,String>(7)?)])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let memory_invalid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM continuity_memory_guard WHERE bot_id=? AND invalidated=1)", [&run.bot_id], |r| r.get(0))?;
    Ok(json!({"scopes":scopes,"name":bot.name,"role":bot.instructions,"profile":bot.profile,
        "approval":bot.approval_mode,"auto_approve":bot.auto_approve,"general_approval":general["approval_mode"],
        "local_access":general["local_access"],"identity":general["identity"],
        "grants":grants,"actions":actions,"accounts":accounts,"devices":devices,"memory_invalid":memory_invalid,
        "memory_digest":digest(&bot.memory),"work_rules":work_rules}))
}

pub struct Prepared {
    pub instructions: String,
    pub live_context: String,
    pub session_id: String,
    pub checkpoint: Option<Value>,
    pub resumed: bool,
    fingerprint: String,
    base: String,
    lease: String,
    enabled: bool,
}
impl Prepared {
    pub fn retention_enabled(&self) -> bool { self.enabled }
    pub fn input(&self, prompt: &str) -> String {
        format!("{}\n\n# Current user input\n{}", self.live_context, prompt)
    }
}

pub fn prepare(app: &App, bot: &Bot, run: &Run, tools: &[Value], window: Option<u64>, engine: &str, provider_identity: &Value) -> Result<Prepared> {
    let assembled = runtime::instructions_for(app, bot, run, tools, window)?;
    let (contract, live) = assembled.split_once(crate::instructions::LIVE_BOUNDARY)
        .context("Kindred context boundary missing")?;
    let instructions = format!("{contract}\n\nLive task snapshots follow retained history. The newest snapshot defines the current task, permissions, time, pending decisions and memory. Earlier task snapshots and prior tool results are historical evidence, not new requests or current authorization. Never repeat completed actions merely because their messages appear in retained context.\n");
    let live_context = format!("# Current Kindred task context\n{live}");
    let base = digest(&json!({"version":1,"engine":engine,"provider":bot.provider,"model":bot.model,
        "connection_generation":connection_generation(&app.db,&bot.provider)?,
        "identity":provider_identity,"instructions":instructions,"tools":tools,"window":window}).to_string());
    let mut prepared = Prepared { instructions, live_context, session_id:db::id(), checkpoint:None, resumed:false,
        fingerprint:String::new(), base, lease:db::id(), enabled:!run.chat_id.is_empty() };
    if !prepared.enabled { return Ok(prepared); }
    if provider_identity["context_identity_verified"] == false {
        prepared.enabled = false;
        return Ok(prepared);
    }
    // Voice private tool runs have their own independent lifecycle. Do not let a
    // call overwrite the ordinary chat's checkpoint, or vice versa.
    if crate::voice::session::is_run(&app.db.0.lock().unwrap(), &run.id)? {
        prepared.enabled = false;
        return Ok(prepared);
    }
    prepared.fingerprint = digest(&format!("{}{}", prepared.base, policy(&app.db, run)?));
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    tx.execute("DELETE FROM harness_contexts WHERE updated<?", [db::now()-MAX_AGE])?;
    let previous: Option<(String,String,String,Option<String>,Option<String>,i64)> = tx.query_row(
        "SELECT fingerprint,session_id,run_id,checkpoint,checkpoint_hash,updated FROM harness_contexts WHERE bot_id=? AND chat_id=?",
        params![run.bot_id,run.chat_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let mut reason = "new";
    let mut retained_tokens = None;
    let snapshot_tokens = crate::continuity::store::estimated_tokens(&prepared.live_context) as u64
        + crate::continuity::store::estimated_tokens(&run.prompt) as u64;
    let reuse_budget = window.map(|n| n.saturating_mul(8) / 10).unwrap_or(if bot.provider=="claude-code" {CLAUDE_REUSE_BUDGET} else {1_000_000});
    if let Some((fingerprint,session,owner,body,hash,updated)) = previous {
        reason = "invalidated";
        let completed: bool = tx.query_row("SELECT status='completed' FROM runs WHERE id=?", [&owner], |r| r.get(0)).optional()?.unwrap_or(false);
        if fingerprint == prepared.fingerprint && completed && owner != run.id
            && (0..=MAX_AGE).contains(&db::now().saturating_sub(updated))
            && crate::continuity::store::session_valid(&tx, &owner)?
            && let Some(body) = body.filter(|b| b.len() <= MAX_CHECKPOINT && hash.as_deref() == Some(digest(b).as_str()))
            && let Ok(checkpoint) = serde_json::from_str::<Value>(&body)
            && checkpoint["schema"] == 1
        {
            let estimated = checkpoint["estimated_tokens"].as_u64();
            // Bound context growth at a checkpoint boundary; a cold start uses
            // the already-budgeted authoritative continuity snapshot. Never slice
            // raw tool messages or lose call/result pairing to make a cache fit.
            retained_tokens = estimated;
            // Native compaction preserves the in-flight working state. This
            // boundary guard also covers older CLIs or an oversized last result.
            // Rebuild from durable context, never truncate a live tool exchange.
            if estimated.is_some_and(|n| n.saturating_add(snapshot_tokens) < reuse_budget) {
                prepared.session_id = session;
                prepared.checkpoint = Some(checkpoint);
                prepared.resumed = true;
                reason = "reused";
            } else { reason = "budget_checkpoint"; }
        }
    }
    // Consuming the previous checkpoint first makes crashes, ambiguous outcomes,
    // interrupted tools and partial turns cold-start next time. No stale replay.
    tx.execute("INSERT INTO harness_contexts VALUES(?,?,?,?,?,?,NULL,NULL,?) ON CONFLICT(bot_id,chat_id) DO UPDATE SET fingerprint=excluded.fingerprint,session_id=excluded.session_id,lease=excluded.lease,run_id=excluded.run_id,checkpoint=NULL,checkpoint_hash=NULL,updated=excluded.updated",
        params![run.bot_id,run.chat_id,prepared.fingerprint,prepared.session_id,prepared.lease,run.id,db::now()])?;
    tx.commit()?;
    drop(c);
    app.db.event(&run.id,"harness_context",json!({"state":reason,"engine":engine,"resumed":prepared.resumed,"retained_tokens":retained_tokens,"snapshot_tokens":snapshot_tokens,"reuse_budget":reuse_budget}))?;
    Ok(prepared)
}

/// Persistence is optional: inability to retain a cache must never turn a
/// successful external action into a failed/retryable task.
pub fn complete(app: &App, _bot: &Bot, run: &Run, prepared: &Prepared, checkpoint: &Value) -> Result<bool> {
    if !prepared.enabled || checkpoint["schema"] != 1 || checkpoint["estimated_tokens"].as_u64().is_none() { return Ok(false); }
    let body = serde_json::to_string(checkpoint)?;
    if body.len() > MAX_CHECKPOINT || digest(&format!("{}{}",prepared.base,policy(&app.db,run)?)) != prepared.fingerprint { return Ok(false); }
    let mut c = app.db.0.lock().unwrap();
    if !crate::continuity::store::session_valid(&c, &run.id)? { return Ok(false); }
    let tx=c.transaction()?;
    let saved=tx.execute("UPDATE harness_contexts SET checkpoint=?,checkpoint_hash=?,updated=? WHERE bot_id=? AND chat_id=? AND lease=? AND run_id=? AND fingerprint=?",
        params![body,digest(&body),db::now(),run.bot_id,run.chat_id,prepared.lease,run.id,prepared.fingerprint])? == 1;
    // A profile has a bounded disposable cache, independent of retained chats.
    // Eviction discards whole checkpoints, never slices provider message pairs.
    loop {
        let bytes:i64=tx.query_row("SELECT COALESCE(sum(length(CAST(checkpoint AS BLOB))),0) FROM harness_contexts",[],|r|r.get(0))?;
        if bytes <= 64*1024*1024 {break;}
        tx.execute("DELETE FROM harness_contexts WHERE rowid=(SELECT rowid FROM harness_contexts WHERE checkpoint IS NOT NULL ORDER BY updated,rowid LIMIT 1)",[])?;
    }
    tx.commit()?;
    Ok(saved)
}
