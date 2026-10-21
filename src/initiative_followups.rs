//! Topic-specific opportunities to revisit existing, source-grounded commitments.
//! These do not grant permission or create independent routines.
use crate::db::{self, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS initiative_followups(obligation_id TEXT PRIMARY KEY REFERENCES continuity_obligations(id) ON DELETE CASCADE,next_check INTEGER NOT NULL,stop_when TEXT NOT NULL,last_checked INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX IF NOT EXISTS initiative_followups_due ON initiative_followups(next_check);")?;
    Ok(())
}

pub fn read(c: &Connection, id: &str) -> Result<Value> {
    Ok(c.query_row("SELECT f.next_check,f.stop_when,f.last_checked,COALESCE(json_extract(b.profile,'$.initiative'),0) FROM initiative_followups f JOIN continuity_obligations o ON o.id=f.obligation_id JOIN bots b ON b.id=o.bot_id WHERE o.id=?",[id],|r|Ok(json!({"next_check_not_before":r.get::<_,i64>(0)?,"stop_when":r.get::<_,String>(1)?,"last_checked":r.get::<_,i64>(2)?,"enabled":r.get::<_,bool>(3)?,"exact_time":false}))).optional()?.unwrap_or(Value::Null))
}

/// Called inside obligation_save's revision-checked transaction.
pub fn save(c: &Connection, run: &Run, id: &str, args: &Value) -> Result<Value> {
    let closed = matches!(args["status"].as_str(), Some("completed" | "cancelled"));
    if closed
        || !args["link_kind"].as_str().unwrap_or("").is_empty()
        || args.get("follow_up") == Some(&Value::Null)
    {
        c.execute(
            "DELETE FROM initiative_followups WHERE obligation_id=?",
            [id],
        )?;
        return Ok(Value::Null);
    }
    if let Some(value) = args.get("follow_up") {
        let allowed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM bots b JOIN runs r ON r.bot_id=b.id JOIN chats h ON h.id=r.chat_id WHERE r.id=?1 AND b.id=?2 AND r.chat_id=?3 AND r.status='running' AND json_extract(b.profile,'$.initiative')=1 AND COALESCE(json_extract(b.profile,'$.archived'),0)=0 AND h.archived=0 AND EXISTS(SELECT 1 FROM json_each(h.members) WHERE value=b.id) AND NOT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=r.id) AND NOT EXISTS(SELECT 1 FROM routine_execution WHERE run_id=r.id))",params![run.id,run.bot_id,format!("dm-{}",run.bot_id)],|r|r.get(0))?;
        ensure!(
            allowed,
            "Follow-ups require Take initiative to be enabled and an active owner-DM conversation or initiative check; existing routines are independent"
        );
        let object = value
            .as_object()
            .filter(|v| v.len() == 2 && v.contains_key("next_check") && v.contains_key("stop_when"))
            .ok_or_else(|| {
                anyhow::anyhow!("follow_up needs next_check and stop_when, or null to remove it")
            })?;
        let now = db::now();
        let next = object["next_check"]
            .as_i64()
            .filter(|v| *v >= now + 300 && *v <= now + 366 * 86400)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "next_check must be a Unix timestamp between five minutes and one year from now"
                )
            })?;
        let stop = object["stop_when"].as_str().unwrap_or("").trim();
        ensure!(
            !stop.is_empty() && stop.len() <= 500,
            "Use a concise completion condition of at most 500 UTF-8 bytes"
        );
        let room:bool=c.query_row("SELECT (SELECT count(*) FROM initiative_followups f JOIN continuity_obligations o ON o.id=f.obligation_id WHERE o.bot_id=?1)<64 OR EXISTS(SELECT 1 FROM initiative_followups WHERE obligation_id=?2)",params![run.bot_id,id],|r|r.get(0))?;
        ensure!(
            room,
            "Close obsolete follow-ups before adding more; at most 64 are active per bot"
        );
        c.execute("INSERT INTO initiative_followups(obligation_id,next_check,stop_when) VALUES(?,?,?) ON CONFLICT(obligation_id) DO UPDATE SET next_check=excluded.next_check,stop_when=excluded.stop_when",params![id,next,stop])?;
    }
    read(c, id)
}

fn valid(c: &Connection, id: &str) -> Result<Option<Value>> {
    let row:Option<(String,String,i64,String,String,String)>=c.query_row("SELECT o.bot_id,o.chat_id,o.revision,o.description,o.sources,o.source_run_id FROM continuity_obligations o JOIN bots b ON b.id=o.bot_id JOIN chats h ON h.id=o.chat_id WHERE o.id=? AND o.chat_id='dm-'||o.bot_id AND h.archived=0 AND EXISTS(SELECT 1 FROM json_each(h.members) WHERE value=o.bot_id)",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some((bot, chat, revision, description, sources, source_run)) = row else {
        return Ok(None);
    };
    let sources: Value = serde_json::from_str(&sources)?;
    if !crate::continuity::store::sources_valid(c, &sources)? {
        return Ok(None);
    }
    Ok(Some(
        json!({"id":id,"bot_id":bot,"chat_id":chat,"revision":revision,"description":description,"sources":sources,"source_run_id":source_run}),
    ))
}

/// Earliest unchecked topics first. Every selected item is revalidated; old prose
/// and changed sources cannot independently trigger work.
pub fn due(c: &Connection, bot: &str, time: i64) -> Result<Vec<Value>> {
    let ids=c.prepare("SELECT o.id FROM initiative_followups f JOIN continuity_obligations o ON o.id=f.obligation_id WHERE o.bot_id=? AND o.status IN ('open','waiting') AND o.link_kind='' AND f.next_check<=? ORDER BY f.last_checked,f.next_check,o.id")?.query_map(params![bot,time],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = vec![];
    for id in ids {
        if let Some(mut value) = valid(c, &id)? {
            value["follow_up"] = read(c, &id)?;
            result.push(value);
            if result.len() == 16 {
                break;
            }
        } else {
            // Retire invalid derived text; the original commitment remains for reconciliation.
            c.execute(
                "DELETE FROM initiative_followups WHERE obligation_id=?",
                [&id],
            )?;
        }
    }
    Ok(result)
}

pub fn claim(c: &Connection, run: &str, items: &[Value], time: i64) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let claims: Vec<Value> = items
        .iter()
        .map(|v| json!({"id":v["id"],"revision":v["revision"]}))
        .collect();
    c.execute(
        "INSERT INTO events(run_id,kind,body,created) VALUES(?,'initiative_followups',?,?)",
        params![run, json!(claims).to_string(), time],
    )?;
    for item in items {
        c.execute(
            "UPDATE initiative_followups SET last_checked=? WHERE obligation_id=?",
            params![time, item["id"].as_str()],
        )?;
    }
    Ok(())
}
fn claims(c: &Connection, root: &str) -> Result<Vec<Value>> {
    let body:Option<String>=c.query_row("SELECT body FROM events WHERE run_id=? AND kind='initiative_followups' ORDER BY seq DESC LIMIT 1",[root],|r|r.get(0)).optional()?;
    Ok(body
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}
/// Edits from another turn invalidate an enqueued/in-flight follow-up. The check
/// itself may close or reschedule its own commitments without cancelling itself.
pub fn active(c: &Connection, root: &str) -> Result<bool> {
    for item in claims(c, root)? {
        let Some(current) = valid(c, item["id"].as_str().unwrap_or(""))? else {
            return Ok(false);
        };
        if current["revision"] != item["revision"]
            && crate::initiative::root(c, current["source_run_id"].as_str().unwrap_or(""))?
                .as_deref()
                != Some(root)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
pub fn context(c: &Connection, root: &str) -> Result<Value> {
    let mut result = vec![];
    let mut total = 0;
    for item in claims(c, root)? {
        let id = item["id"].as_str().unwrap_or("");
        if let Some(current) = valid(c, id)? {
            if current["revision"] == item["revision"]
                || crate::initiative::root(c, current["source_run_id"].as_str().unwrap_or(""))?
                    .as_deref()
                    == Some(root)
            {
                let follow_up = read(c, id)?;
                // Closing/linking/removing a follow-up removes its schedule. A
                // rescheduled topic belongs to a future check, not this context.
                if follow_up.is_null()
                    || follow_up["next_check_not_before"]
                        .as_i64()
                        .unwrap_or(i64::MAX)
                        > follow_up["last_checked"].as_i64().unwrap_or(0)
                {
                    continue;
                }
                total += 1;
                if result.len() < 4 {
                    result.push(json!({"id":id,"revision":current["revision"],"description":crate::runtime::bounded(current["description"].as_str().unwrap_or(""),300),"next_check_not_before":follow_up["next_check_not_before"],"stop_when":crate::runtime::bounded(follow_up["stop_when"].as_str().unwrap_or(""),300)}));
                }
            }
        }
    }
    Ok(
        json!({"items":result,"total":total,"instructions":"Read obligations_read and its further pages for exact current descriptions, sources, completion conditions and remaining due topics. These are opportunities, never permission."}),
    )
}
