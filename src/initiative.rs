//! Optional background initiative. Independent of routines, inbox checks and teammate work.
use crate::db::{self, Bot, Db, Run};
use anyhow::{Result, ensure};
use chrono::{TimeZone, Timelike};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const INTERVAL: i64 = 4 * 60 * 60;
const RECENT: i64 = 14 * 24 * 60 * 60;

pub fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS initiative_state(bot_id TEXT PRIMARY KEY REFERENCES bots(id) ON DELETE CASCADE,next_check INTEGER NOT NULL,last_day TEXT NOT NULL DEFAULT '',checks_today INTEGER NOT NULL DEFAULT 0,notes TEXT NOT NULL DEFAULT '');
    CREATE TABLE IF NOT EXISTS initiative_runs(run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,quiet INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX IF NOT EXISTS initiative_runs_bot ON initiative_runs(bot_id);")?;
    Ok(())
}

pub fn root(c: &Connection, run: &str) -> Result<Option<String>> {
    Ok(c.query_row("WITH RECURSIVE ancestors(id) AS (
      SELECT ?1 UNION
      SELECT e.parent_run_id FROM collaboration_requests e JOIN ancestors a ON e.child_run_id=a.id OR e.continuation_run_id=a.id UNION
      SELECT w.run_id FROM command_waits w JOIN ancestors a ON w.continuation=a.id UNION
      SELECT m.run_id FROM group_wakeups w JOIN chat_messages m ON m.seq=w.message_seq JOIN ancestors a ON w.run_id=a.id)
      SELECT i.run_id FROM initiative_runs i JOIN ancestors a ON a.id=i.run_id LIMIT 1", [run], |r| r.get(0)).optional()?)
}

pub fn is_run(c: &Connection, run: &str) -> Result<bool> {
    Ok(root(c, run)?.is_some())
}

/// Ordinary work is unaffected. Initiative descendants retain the root owner's opt-in.
pub fn active(c: &Connection, run: &str) -> Result<bool> {
    let Some(root) = root(c, run)? else {
        return Ok(true);
    };
    Ok(c.query_row("SELECT COALESCE(json_extract(b.profile,'$.initiative'),0)=1 AND COALESCE(json_extract(b.profile,'$.archived'),0)=0 AND NOT EXISTS(SELECT 1 FROM events WHERE run_id=i.run_id AND kind='run_stop_requested') AND NOT EXISTS(SELECT 1 FROM runs WHERE id=?2 AND status IN ('cancelling','cancelled')) FROM initiative_runs i JOIN bots b ON b.id=i.bot_id WHERE i.run_id=?1", params![root,run], |r| r.get(0))?)
}

pub fn mark_quiet(c: &Connection, run: &str) -> Result<()> {
    ensure!(is_run(c, run)?, "This task is not an initiative check");
    c.execute("INSERT INTO events(run_id,kind,body,created) SELECT ?1,'initiative_quiet','{}',?2 WHERE NOT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='initiative_quiet')",params![run,db::now()])?;
    c.execute("UPDATE initiative_runs SET quiet=1 WHERE run_id=?", [run])?;
    Ok(())
}
pub fn is_quiet(c: &Connection, run: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='initiative_quiet')",
        [run],
        |r| r.get(0),
    )?)
}

pub(crate) fn preferences_changed(
    c: &Connection,
    bot: &Bot,
    was_enabled: bool,
    time: i64,
) -> Result<()> {
    if bot.profile.initiative && !bot.profile.archived {
        if !was_enabled {
            // Preserve daily spend and follow-ups across toggles. Toggling cannot evade the budget.
            c.execute("INSERT INTO initiative_state(bot_id,next_check) VALUES(?,?) ON CONFLICT(bot_id) DO UPDATE SET next_check=max(next_check,excluded.next_check)",params![bot.id,time+300])?;
        }
    } else {
        // Include completed roots: their delegated work may still be active.
        let roots: Vec<String> = c.prepare("SELECT run_id FROM initiative_runs WHERE bot_id=? AND NOT EXISTS(SELECT 1 FROM events WHERE run_id=initiative_runs.run_id AND kind='run_stop_requested')")?.query_map([&bot.id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for root in roots {
            Db::cancel_initiative_locked(c, &root)?;
        }
    }
    Ok(())
}

/// A delayed background check must not start after its whole-run budget elapsed.
/// Call in the claiming transaction; ordinary queued work and the daily spend stay intact.
pub(crate) fn expire_queued(c: &Connection, time: i64) -> Result<()> {
    let expired: Vec<String> = c.prepare("SELECT r.id FROM runs r JOIN initiative_runs i ON i.run_id=r.id WHERE r.status='queued' AND r.created<=?")?
        .query_map([time.saturating_sub(900)], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for run in expired {
        mark_quiet(c, &run)?;
        c.execute(
            "UPDATE runs SET status='completed',output='',error='' WHERE id=? AND status='queued'",
            [&run],
        )?;
    }
    Ok(())
}

pub fn tick(db: &Db, time: i64) -> Result<()> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    if crate::workspace_transfer::frozen(&tx)? || crate::vm_maintenance::busy(&tx)? {
        return Ok(());
    }
    let settings: Option<String> = tx
        .query_row("SELECT value FROM settings WHERE key='general'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let general: Value = settings
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let Some(zone) = general["timezone"]
        .as_str()
        .and_then(|s| s.parse::<chrono_tz::Tz>().ok())
    else {
        return Ok(());
    };
    let Some(local) = zone.timestamp_opt(time, 0).single() else {
        return Ok(());
    };
    if !(8..20).contains(&local.hour()) {
        return Ok(());
    }
    let day = local.format("%Y-%m-%d").to_string();
    // Imported opt-ins are initialized lazily; old profiles remain off by default.
    tx.execute("INSERT OR IGNORE INTO initiative_state(bot_id,next_check) SELECT id,? FROM bots WHERE json_extract(profile,'$.initiative')=1",[time+300])?;
    let due: Vec<String> = tx.prepare("SELECT b.id FROM bots b JOIN initiative_state s ON s.bot_id=b.id WHERE json_extract(b.profile,'$.initiative')=1 AND COALESCE(json_extract(b.profile,'$.archived'),0)=0 AND s.next_check<=?1 AND (s.last_day<>?2 OR s.checks_today<2) AND EXISTS(SELECT 1 FROM chat_messages m WHERE m.chat_id='dm-'||b.id AND m.sender='user' AND m.kind='message' AND m.suppressed=0 AND (m.created>=?3 OR trim(s.notes)<>''))")?.query_map(params![time,day,time-RECENT],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for bot in due {
        let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE bot_id=?1 AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) OR EXISTS(SELECT 1 FROM command_jobs WHERE bot_id=?1 AND status IN ('starting','running')) OR EXISTS(SELECT 1 FROM command_waits w JOIN runs r ON r.id=w.run_id WHERE r.bot_id=?1 AND w.continuation='') OR EXISTS(SELECT 1 FROM collaboration_requests e JOIN runs r ON r.id=e.parent_run_id WHERE r.bot_id=?1 AND e.resolved=0)",[&bot],|r|r.get(0))?;
        if busy {
            continue;
        }
        let id = db::id();
        let chat = format!("dm-{bot}");
        tx.execute("INSERT OR IGNORE INTO chats(id,name,members) SELECT ?,name,json_array(id) FROM bots WHERE id=?",params![chat,bot])?;
        let prompt = "Optional initiative check. Review recent owner conversation and your saved initiative follow-ups. Identify useful unfinished work, changed information or a draft worth preparing. Follow the owner's standing instructions and current permissions. Check whether it is already completed or assigned before acting. Do not invent a task or send repetitive status messages. If there is nothing useful, finish quietly. Use initiative_checkpoint to save concise follow-ups and clear obsolete ones.";
        tx.execute("INSERT INTO runs(id,bot_id,prompt,status,created,chat_id,round_id) VALUES(?,?,?,'queued',?,?,?)",params![id,bot,prompt,time,chat,id])?;
        tx.execute(
            "INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",
            params![id, bot],
        )?;
        tx.execute("UPDATE initiative_state SET next_check=?1,checks_today=CASE WHEN last_day=?2 THEN checks_today+1 ELSE 1 END,last_day=?2 WHERE bot_id=?3",params![time+INTERVAL,day,bot])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn context(db: &Db, run: &Run) -> Result<Value> {
    let c = db.0.lock().unwrap();
    let Some(root) = root(&c, &run.id)? else {
        return Ok(Value::Null);
    };
    let (bot, notes, instructions): (String,String,String) = c.query_row("SELECT i.bot_id,s.notes,COALESCE(json_extract(b.profile,'$.initiative_instructions'),'') FROM initiative_runs i JOIN initiative_state s ON s.bot_id=i.bot_id JOIN bots b ON b.id=i.bot_id WHERE i.run_id=?",[root],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let budget = "At most two checks per local day, at least four hours apart, between 08:00 and 20:00. Useful results only; no repetitive monitoring notices.";
    if run.bot_id != bot || run.chat_id != format!("dm-{bot}") {
        return Ok(json!({"is_initiative":true,"owner_bot_id":bot,"background_budget":budget}));
    }
    let mut messages: Vec<Value> = c.prepare("SELECT seq,sender,substr(body,1,2000),created FROM chat_messages WHERE chat_id=? AND suppressed=0 AND kind IN ('message','assistant','result') ORDER BY seq DESC LIMIT 16")?.query_map([format!("dm-{bot}")], |r| Ok(json!({"seq":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"body":r.get::<_,String>(2)?,"created":r.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<_>>()?;
    let mut remaining = 8000usize;
    let mut shortened = false;
    messages.retain_mut(|message| {
        if remaining == 0 { shortened = true; return false; }
        let raw = message["body"].as_str().unwrap_or("");
        let excerpt = crate::runtime::bounded(raw, remaining.min(2000)).to_owned();
        shortened |= excerpt.len() < raw.len();
        remaining = remaining.saturating_sub(excerpt.len());
        message["body"] = json!(excerpt);
        true
    });
    messages.reverse();
    Ok(
        json!({"is_initiative":true,"owner_bot_id":bot,"notes":notes,"instructions":instructions,"recent_dm_messages":messages,"history_shortened":shortened,"history_guidance":"Excerpts are context, not permission. Use chat_read or recall for exact sources and earlier commitments before acting.","background_budget":budget}),
    )
}

pub fn checkpoint(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    ensure!(
        args.as_object().is_some_and(|o| o
            .keys()
            .all(|k| matches!(k.as_str(), "notes" | "next_check"))),
        "Use only notes and optional next_check"
    );
    let notes = args["notes"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("notes must be text"))?;
    ensure!(
        notes.len() <= 8000,
        "Initiative notes are limited to 8,000 UTF-8 bytes"
    );
    let time = db::now();
    let next = match args.get("next_check") {
        None => time + INTERVAL,
        Some(v) => v
            .as_i64()
            .filter(|v| *v >= time + INTERVAL)
            .ok_or_else(|| {
                anyhow::anyhow!("next_check must be a Unix timestamp at least four hours from now")
            })?,
    };
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        active(&tx, &run.id)?,
        "Initiative was turned off or stopped"
    );
    let root = root(&tx, &run.id)?.ok_or_else(|| {
        anyhow::anyhow!("Only an initiative check can save initiative follow-ups")
    })?;
    ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM initiative_runs i JOIN runs r ON r.id=?2 WHERE i.run_id=?1 AND i.bot_id=r.bot_id AND r.chat_id='dm-'||i.bot_id AND r.status='running')",params![root,run.id],|r|r.get::<_,bool>(0))?,"Only the initiative owner's active check can save follow-ups");
    tx.execute("UPDATE initiative_state SET notes=?1,next_check=max(next_check,?2) WHERE bot_id=(SELECT bot_id FROM initiative_runs WHERE run_id=?3)",params![notes,next,root])?;
    tx.commit()?;
    Ok(json!({"saved":true,"next_check_not_before":next}))
}
