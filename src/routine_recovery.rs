//! One durable recovery budget for a scheduled occurrence. Never infer retry
//! safety from a model report or an action name supplied as tool input.
use crate::{db::Run, runtime::App};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

pub const LIMIT: usize = 2;
pub fn scheduled(c: &Connection, run: &str) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?)", [run], |r| r.get(0))?)
}
pub fn boundary(c: &Connection, run: &str) -> Result<i64> {
    Ok(c.query_row("SELECT COALESCE(max(seq),0) FROM events WHERE run_id=? AND kind='provider_retry' AND json_extract(body,'$.phase')='retrying' AND json_extract(body,'$.scope')='routine'", [run], |r| r.get(0))?)
}
pub fn spent(c: &Connection, run: &str) -> Result<usize> {
    Ok(c.query_row("SELECT count(*) FROM events WHERE kind='provider_retry' AND json_extract(body,'$.phase')='retrying' AND (run_id=?1 OR run_id IN (SELECT run_id FROM routine_execution WHERE root_run_id=COALESCE((SELECT root_run_id FROM routine_execution WHERE run_id=?1),?1)))", [run], |r| r.get(0))?)
}
// Kindred's known read tools; never trust a caller's read_only assertion.
pub fn read_tool(name: &str) -> bool {
    matches!(name, "kindred_guide" | "memory_read" | "receipts_read" | "apps_list" |
        "connector_tools" | "codex_connector_tools" | "connector_result_read" |
        "commands_list" | "command_read" | "skills_list" | "bots_list" |
        "slack_channels" | "slack_monitors_list" | "computer_screenshot" | "tool_describe")
}
fn bookkeeping(name: &str) -> bool {
    matches!(name, "set_task_title" | "task_progress" | "routine_complete" | "finish_quietly")
}
// Only the verified dispatcher's persisted read-only artifact binds a connector
// call. Ambiguous concurrent calls fail closed; model input grants nothing.
pub fn read_call(c: &Connection, run: &str, name: &str, call: &str) -> Result<bool> {
    if read_tool(name) { return Ok(true); }
    if !matches!(name, "connector_execute" | "codex_connector") { return Ok(false); }
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM events requested JOIN events started ON started.run_id=requested.run_id AND started.seq>requested.seq AND started.kind='tool_started' JOIN connector_artifacts a ON a.id=json_extract(started.body,'$.args.artifact_id') AND a.run_id=requested.run_id WHERE requested.run_id=?1 AND requested.kind='tool_requested' AND json_extract(requested.body,'$.call_id')=?2 AND json_extract(requested.body,'$.tool')=?3 AND json_extract(started.body,'$.tool')=?3 AND json_extract(a.body,'$.read_only')=1 AND json_extract(started.body,'$.args.read_only')=1 AND NOT EXISTS(SELECT 1 FROM events newer WHERE newer.run_id=requested.run_id AND newer.seq>requested.seq AND newer.seq<started.seq AND newer.kind IN ('tool_requested','tool_result') AND json_extract(newer.body,'$.tool')=?3))", params![run,call,name], |r| r.get(0))?)
}
pub fn transport_error(error: &anyhow::Error) -> bool {
    if let Some(e) = error.downcast_ref::<crate::rpc::CodexTurnError>() { return e.routine_retryable; }
    if let Some(e) = error.downcast_ref::<HttpFailure>() { return matches!(e.status, 408 | 500 | 502 | 503 | 504); }
    // A known status wins over wrapping timeout text. A 429 may be quota.
    for cause in error.chain() {
        if let Some(e) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = e.status() { return matches!(status.as_u16(), 408 | 500 | 502 | 503 | 504); }
        }
    }
    error.chain().any(|cause| {
        cause.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_timeout() || e.is_connect())
            || cause.downcast_ref::<std::io::Error>().is_some_and(|e| matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof))
            || matches!(cause.to_string().as_str(), "Subscription provider timed out" | "Codex request timed out")
    })
}
// (Has unresolved failures, all have authoritative recovery receipts.)
// A successful identical read supersedes its failed read. Different arguments
// or another run cannot erase it. Original receipts stay in Activity.
fn unresolved(c: &Connection, run: &str) -> Result<Vec<(i64, bool)>> {
    let mut failures: BTreeMap<String, (i64, bool)> = BTreeMap::new();
    let mut q = c.prepare("SELECT e.seq,e.body,(SELECT r.body FROM events r WHERE r.run_id=e.run_id AND r.kind='tool_requested' AND r.seq<e.seq AND json_extract(r.body,'$.call_id')=json_extract(e.body,'$.call_id') ORDER BY r.seq DESC LIMIT 1),e.run_id FROM events e WHERE (e.run_id=?1 OR e.run_id IN (SELECT run_id FROM routine_execution WHERE root_run_id=COALESCE((SELECT root_run_id FROM routine_execution WHERE run_id=?1),?1))) AND e.kind='tool_result' ORDER BY e.seq")?;
    let rows = q.query_map([run], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?)))?;
    for row in rows {
        let (seq, body, request, source) = row?;
        let body: Value = serde_json::from_str(&body)?;
        let name = body["tool"].as_str().unwrap_or("");
        if matches!(name,"routine_complete"|"finish_quietly") { continue; }
        let request: Option<Value> = request.map(|s| serde_json::from_str(&s)).transpose()?;
        let key = request.as_ref().map(|v| json!([name,v["args"]]).to_string()).unwrap_or_else(|| format!("unbound:{seq}"));
        if body["failed"] == true || body["failed"] == 1 {
            let safe = request.as_ref().is_some_and(|v| v["tool"] == body["tool"]) && body["retryable_read"] == true && read_call(c,&source,name,body["call_id"].as_str().unwrap_or(""))?;
            let safe = safe && failures.get(&key).is_none_or(|(_, prior_safe)| *prior_safe);
            failures.insert(key, (seq, safe));
        } else if (body["failed"] == false || body["failed"] == 0) && request.as_ref().is_some_and(|v| v["tool"] == body["tool"])
            && read_call(c,&source,name,body["call_id"].as_str().unwrap_or(""))? {
            failures.remove(&key);
        }
    }
    Ok(failures.into_values().collect())
}
fn failures(c: &Connection, run: &str) -> Result<(bool, bool)> {
    let items = unresolved(c, run)?;
    Ok((!items.is_empty(), items.iter().all(|(_,safe)| *safe)))
}
pub fn latest_failure(c: &Connection, run: &str) -> Result<Option<i64>> {
    Ok(unresolved(c, run)?.into_iter().map(|(seq,_)|seq).max())
}
pub fn safe_to_resume(c: &Connection, run: &str) -> Result<bool> {
    let mut q = c.prepare("SELECT ?1 UNION SELECT run_id FROM routine_execution WHERE root_run_id=COALESCE((SELECT root_run_id FROM routine_execution WHERE run_id=?1),?1)")?;
    let sources = q.query_map([run], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for source in sources { if !safe_run(c, &source)? { return Ok(false); } }
    Ok(failures(c, run)?.1)
}
fn safe_run(c: &Connection, run: &str) -> Result<bool> {
    if c.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE run_id=?1 AND status!='approved') OR EXISTS(SELECT 1 FROM questions WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM user_tasks WHERE run_id=?1 AND status IN ('pending','ready')) OR EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='tool_result' AND (json_extract(body,'$.uncertain_effect')=1 OR json_extract(body,'$.timed_out')=1 OR json_extract(body,'$.stopped')=1)) OR EXISTS(SELECT 1 FROM connector_artifacts WHERE run_id=?1 AND (COALESCE(json_extract(body,'$.read_only'),0)!=1 OR status NOT IN ('completed','failed'))) OR EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND seq>?2 AND kind IN ('assistant','provider_diagnostic') AND json_extract(body,'$.status_notice')='provider_error')", params![run,boundary(c,run)?], |r| r.get::<_,bool>(0))? { return Ok(false); }
    // Legacy/result-only effects are not permission to replay either. A caller
    // cannot make an unknown write safe by omitting its request receipt.
    let mut results = c.prepare("SELECT body FROM events WHERE run_id=? AND kind='tool_result'")?;
    for row in results.query_map([run], |r|r.get::<_,String>(0))? {
        let body: Value = serde_json::from_str(&row?)?;
        let name = body["tool"].as_str().unwrap_or("");
        if !bookkeeping(name) && !read_call(c,run,name,body["call_id"].as_str().unwrap_or(""))? { return Ok(false); }
    }
    let mut q = c.prepare("SELECT body FROM events WHERE run_id=? AND kind='tool_requested'")?;
    for row in q.query_map([run], |r| r.get::<_,String>(0))? {
        let body: Value = serde_json::from_str(&row?)?;
        let name = body["tool"].as_str().unwrap_or("");
        let call = body["call_id"].as_str().unwrap_or("");
        if !bookkeeping(name) && !read_call(c,run,name,call)? { return Ok(false); }
        if !c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='tool_result' AND json_extract(body,'$.call_id')=?)", params![run,call], |r| r.get::<_,bool>(0))? { return Ok(false); }
    }
    Ok(true)
}
pub fn blocks_quiet(c: &Connection, run: &str) -> Result<bool> {
    // Retrying is not evidence of success. An unresolved read survives backoff.
    Ok(failures(c, run)?.0 || c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND seq>?2 AND ((kind IN ('assistant','provider_diagnostic') AND json_extract(body,'$.status_notice')='provider_error')))", params![run,boundary(c,run)?], |r| r.get::<_,bool>(0))?)
}
pub fn recoverable_outcome(c: &Connection, run: &str) -> Result<bool> {
    let outcome: Option<String> = c.query_row("SELECT outcome FROM routine_completion WHERE run_id=?", [run], |r| r.get(0)).optional()?;
    if outcome.as_deref().is_some_and(|v| v != "error") { return Ok(false); }
    Ok(failures(c, run)?.0 && safe_to_resume(c, run)?)
}
// Reservation and archiving a provisional error are atomic. A crash after
// reservation consumes the attempt; reentry cannot reset the occurrence budget.
pub fn enter(app: &App, run: &Run) -> Result<()> {
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(crate::routine_completion::current(&tx,&run.id)? && tx.query_row("SELECT status='running' FROM runs WHERE id=?", [&run.id], |r|r.get::<_,bool>(0))?, "This routine is no longer running");
    let dispatched: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND (kind IN ('routine_recovery_started','routine_provider_attempt') OR (kind='provider_retry' AND json_extract(body,'$.phase')='retrying')))", [&run.id], |r|r.get(0))?;
    ensure!(!dispatched, "A previous routine attempt already ran. Progress is saved; continue explicitly instead of replaying it.");
    tx.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'routine_recovery_started','{}',?)", params![run.id,crate::db::now()])?;
    tx.commit()?;
    Ok(())
}
pub fn reserve(app: &App, run: &Run, delay: Duration) -> Result<bool> {
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    let used = spent(&tx, &run.id)?;
    if used >= LIMIT || !safe_to_resume(&tx, &run.id)? || !crate::routine_completion::current(&tx, &run.id)?
        || !tx.query_row("SELECT status='running' FROM runs WHERE id=?", [&run.id], |r| r.get::<_,bool>(0))? { return Ok(false); }
    tx.execute("INSERT INTO events(run_id,kind,body,created) SELECT run_id,'routine_recovery_outcome',json_object('outcome',outcome,'report',report),? FROM routine_completion WHERE run_id=? AND outcome='error'", params![crate::db::now(),run.id])?;
    tx.execute("DELETE FROM routine_completion WHERE run_id=? AND outcome='error'", [&run.id])?;
    tx.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'provider_retry',?,?)", params![run.id,json!({"attempt":used+1,"limit":LIMIT,"scope":"routine","phase":"retrying","delay_seconds":delay.as_secs()}).to_string(),crate::db::now()])?;
    tx.commit()?;
    Ok(true)
}

#[derive(Debug)]
pub struct Unclassified(pub String);
impl std::fmt::Display for Unclassified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for Unclassified {}

// Preserve an HTTP status through sanitized connector diagnostics. Do not
// classify a model's prose or the contents of a failed connector response.
#[derive(Debug)]
pub struct HttpFailure { pub status: u16, pub message: String }
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for HttpFailure {}
