//! Durable scheduled outcomes. A successful provider final can be a report;
//! quiet success requires an explicit receipt. Never infer an all-clear from prose.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub const UNCLASSIFIED: &str = "This routine did not classify its result. Review task details and retry the check; no successful all-clear was recorded.";

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS routine_execution(run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,root_run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE);
    CREATE TABLE IF NOT EXISTS routine_completion(run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,outcome TEXT NOT NULL CHECK(outcome IN ('quiet','report','error')),report TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS routine_publications(root_run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE);")?;
    c.execute_batch(
        "CREATE INDEX IF NOT EXISTS routine_execution_root ON routine_execution(root_run_id);",
    )?;
    Ok(())
}
pub fn root(c: &Connection, run: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT root_run_id FROM routine_execution WHERE run_id=?",
        [run],
        |r| r.get(0),
    )
    .optional()?)
}
pub fn register(c: &Connection, run: &str, original: &str) -> Result<()> {
    ensure!(c.query_row("SELECT EXISTS(SELECT 1 FROM runs a JOIN runs b ON b.id=?2 JOIN routine_runs x ON x.run_id=a.id JOIN routine_runs y ON y.run_id=b.id WHERE a.id=?1 AND a.bot_id=b.bot_id AND a.chat_id=b.chat_id AND x.routine_id=y.routine_id)",params![run,original],|r|r.get::<_,bool>(0))?,"Scheduled execution identity changed");
    if let Some(old) = root(c, run)? {
        ensure!(old == original, "Scheduled execution root changed");
    } else {
        c.execute(
            "INSERT INTO routine_execution VALUES(?,?)",
            params![run, original],
        )?;
    }
    Ok(())
}
pub fn inherit(c: &Connection, source: &str, next: &str, original: &str) -> Result<()> {
    let original_routine: Option<String> = c
        .query_row(
            "SELECT routine_id FROM routine_runs WHERE run_id=?",
            [original],
            |r| r.get(0),
        )
        .optional()?;
    let source_routine: Option<String> = c
        .query_row(
            "SELECT routine_id FROM routine_runs WHERE run_id=?",
            [source],
            |r| r.get(0),
        )
        .optional()?;
    if original_routine.is_none() && source_routine.is_none() {
        return Ok(());
    }
    ensure!(
        original_routine.is_some()
            && (source_routine.is_none() || source_routine == original_routine),
        "Scheduled recovery membership changed"
    );
    ensure!(c.query_row("SELECT EXISTS(SELECT 1 FROM runs a JOIN runs b ON b.id=?2 JOIN runs n ON n.id=?3 WHERE a.id=?1 AND a.bot_id=b.bot_id AND a.chat_id=b.chat_id AND n.bot_id=b.bot_id AND n.chat_id=b.chat_id)",params![source,original,next],|r|r.get::<_,bool>(0))?,"Scheduled recovery ownership changed");
    if let Some(old) = root(c, source)? {
        ensure!(old == original, "Scheduled recovery root changed");
    }
    c.execute(
        "INSERT OR IGNORE INTO routine_runs(run_id,routine_id,quiet) VALUES(?,?,0)",
        params![next, original_routine],
    )?;
    register(c, original, original)?;
    register(c, next, original)?;
    Ok(())
}
pub fn current(c: &Connection, run: &str) -> Result<bool> {
    let Some(original) = root(c, run)? else {
        return Ok(true);
    };
    ensure!(c.query_row("SELECT EXISTS(SELECT 1 FROM runs a JOIN runs b ON b.id=?2 JOIN routine_runs x ON x.run_id=a.id JOIN routine_runs y ON y.run_id=b.id WHERE a.id=?1 AND a.bot_id=b.bot_id AND a.chat_id=b.chat_id AND x.routine_id=y.routine_id)",params![run,original],|r|r.get::<_,bool>(0))?,"Scheduled execution membership changed");
    let latest: String = c.query_row(
        "SELECT run_id FROM routine_execution WHERE root_run_id=? ORDER BY rowid DESC LIMIT 1",
        [original],
        |r| r.get(0),
    )?;
    Ok(latest == run
        && !c.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='task_continued')",
            [run],
            |r| r.get::<_, bool>(0),
        )?)
}
pub fn complete(c: &Connection, run: &str, args: &Value) -> Result<Value> {
    ensure!(
        args.as_object()
            .is_some_and(|v| v.keys().all(|k| matches!(k.as_str(), "outcome" | "report"))),
        "Use only outcome and report for routine completion"
    );
    let outcome = args["outcome"].as_str().unwrap_or("");
    ensure!(
        matches!(outcome, "quiet" | "report" | "error"),
        "Choose quiet, report or error for this routine"
    );
    let report = args
        .get("report")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("Routine report must be text"))
        })
        .transpose()?
        .unwrap_or("")
        .trim();
    ensure!(
        report.len() <= 16000
            && !report.contains('\0')
            && ((outcome == "quiet" && report.is_empty())
                || (outcome != "quiet" && !report.is_empty())),
        "A report or error needs nonempty text; quiet must have no report"
    );
    if root(c, run)?.is_none() {
        register(c, run, run)?;
    }
    ensure!(current(c, run)?, "This scheduled attempt was superseded");
    ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=? AND status IN ('queued','running'))",
            [run],
            |r| r.get::<_, bool>(0)
        )?,
        "This routine is no longer running"
    );
    if outcome == "quiet" {
        ensure!(!crate::routine_recovery::blocks_quiet(c,run)? && !c.query_row("SELECT EXISTS(SELECT 1 FROM attachments WHERE run_id=?1) OR EXISTS(SELECT 1 FROM deliverables WHERE run_id=?1) OR EXISTS(SELECT 1 FROM approvals WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM user_tasks WHERE run_id=?1 AND status IN ('pending','ready')) OR EXISTS(SELECT 1 FROM questions WHERE run_id=?1 AND status='pending')",[run],|r|r.get::<_,bool>(0))?,"This routine has an error, files or a pending decision; report it instead of finishing quietly");
    }
    let previous: Option<(String, String)> = c
        .query_row(
            "SELECT outcome,report FROM routine_completion WHERE run_id=?",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some(previous) = previous {
        ensure!(
            previous == (outcome.into(), report.into()),
            "Routine completion was already recorded differently"
        );
    } else {
        c.execute(
            "INSERT INTO routine_completion VALUES(?,?,?)",
            params![run, outcome, report],
        )?;
    }
    c.execute(
        "UPDATE routine_runs SET quiet=? WHERE run_id=?",
        params![outcome == "quiet", run],
    )?;
    Ok(
        json!({"routine_complete":true,"outcome":outcome,"text":"Routine outcome recorded. No further public reply is needed."}),
    )
}
// The provider's successful terminal response is itself an explicit report.
// Do not force a replay of completed work just to obtain a bookkeeping tool.
// Quiet success still requires its durable tool receipt, and real failures or
// unfinished actions cannot be laundered into success by nonempty prose.
fn terminal_report(c: &Connection, run: &str, output: &str) -> Result<bool> {
    if output.trim().is_empty()
        || crate::quiet_output::is_marker(output)
        || crate::routine_recovery::blocks_quiet(c, run)?
    {
        return Ok(false);
    }
    Ok(!c.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM questions WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM user_tasks WHERE run_id=?1 AND status IN ('pending','ready')) OR EXISTS(SELECT 1 FROM connector_artifacts WHERE run_id=?1 AND status IN ('preparing','pending','approved','ready','executing','interrupted')) OR EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='tool_result' AND (json_extract(body,'$.uncertain_effect')=1 OR json_extract(body,'$.timed_out')=1 OR json_extract(body,'$.stopped')=1)) OR EXISTS(SELECT 1 FROM events requested WHERE requested.run_id=?1 AND requested.kind='tool_requested' AND NOT EXISTS(SELECT 1 FROM events result WHERE result.run_id=requested.run_id AND result.seq>requested.seq AND result.kind='tool_result' AND json_extract(result.body,'$.call_id')=json_extract(requested.body,'$.call_id')))",[run],|r|r.get::<_,bool>(0))?)
}
// None preserves the real provider failure/yield/legacy policy. Provisional final
// prose remains in the private event record when an explicit report replaces it.
pub fn finalize(
    c: &Connection,
    run: &str,
    status: &str,
    output: &str,
    error: &str,
) -> Result<Option<(String, String, String)>> {
    if root(c, run)?.is_none() || status != "completed" || !error.is_empty() {
        return Ok(None);
    }
    if c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind IN ('question_wait','process_wait'))",[run],|r|r.get::<_,bool>(0))? {return Ok(None);}
    let mut outcome: Option<(String, String)> = c
        .query_row(
            "SELECT outcome,report FROM routine_completion WHERE run_id=?",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if outcome.is_none() && terminal_report(c, run, output)? {
        c.execute(
            "INSERT INTO routine_completion VALUES(?,'report',?)",
            params![run, output],
        )?;
        c.execute("UPDATE routine_runs SET quiet=0 WHERE run_id=?", [run])?;
        c.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'routine_terminal_report','{}',?)", params![run,crate::db::now()])?;
        outcome = Some(("report".into(), output.into()));
    }
    if !output.is_empty() {
        c.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'routine_provider_final',?,?)",
            params![run, json!({"text":output}).to_string(), crate::db::now()],
        )?;
    }
    Ok(Some(match outcome {
        Some((kind, report)) if kind == "report" => ("completed".into(), report, String::new()),
        Some((kind, report)) if kind == "error" => ("failed".into(), output.into(), report),
        Some((_kind, _)) => ("completed".into(), String::new(), String::new()),
        None => ("failed".into(), output.into(), UNCLASSIFIED.into()),
    }))
}
pub fn publish(c: &Connection, run: &str, completed: bool) -> Result<bool> {
    let Some(original) = root(c, run)? else {
        return Ok(true);
    };
    if !current(c, run)? {
        return Ok(false);
    }
    if !completed {
        return Ok(true);
    }
    let report: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM routine_completion WHERE run_id=? AND outcome='report')",
        [run],
        |r| r.get(0),
    )?;
    if !report {
        return Ok(true);
    }
    c.execute(
        "INSERT OR IGNORE INTO routine_publications VALUES(?,?)",
        params![original, run],
    )?;
    let winner: String = c.query_row(
        "SELECT run_id FROM routine_publications WHERE root_run_id=?",
        [original],
        |r| r.get(0),
    )?;
    if winner != run {
        c.execute("UPDATE routine_runs SET quiet=1 WHERE run_id=?", [run])?;
    }
    Ok(winner == run)
}

// Repair only this exact bookkeeping failure. Keep the original event evidence,
// existing message sequence, timestamps and unread position. No provider call,
// external action, fresh notification or old report redelivery is scheduled.
pub fn repair_terminal_reports(c: &Connection) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    let rows=tx.prepare("SELECT r.id,r.output FROM runs r JOIN routine_execution x ON x.run_id=r.id WHERE r.status='failed' AND r.error=? AND NOT EXISTS(SELECT 1 FROM routine_completion WHERE run_id=r.id) AND NOT EXISTS(SELECT 1 FROM routine_publications p WHERE p.root_run_id=x.root_run_id AND p.run_id!=r.id) AND NOT EXISTS(SELECT 1 FROM approvals WHERE run_id=r.id AND status!='approved') AND NOT EXISTS(SELECT 1 FROM user_tasks WHERE run_id=r.id AND status NOT IN ('completed','resumed')) AND EXISTS(SELECT 1 FROM events e WHERE e.run_id=r.id AND e.kind='routine_provider_final' AND json_extract(e.body,'$.text')=r.output)")?
        .query_map([UNCLASSIFIED],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (run, output) in rows {
        if !current(&tx, &run)? || !terminal_report(&tx, &run, &output)? {
            continue;
        }
        tx.execute(
            "INSERT INTO routine_completion VALUES(?,'report',?)",
            params![run, output],
        )?;
        tx.execute("UPDATE routine_runs SET quiet=0 WHERE run_id=?", [&run])?;
        ensure!(
            publish(&tx, &run, true)?,
            "Routine repair publication changed"
        );
        tx.execute(
            "UPDATE runs SET status='completed',error='' WHERE id=?",
            [&run],
        )?;
        // Record the verified projection repair before changing its display.
        // Continuity must distinguish this from editing the original evidence.
        tx.execute("INSERT INTO events(run_id,kind,body,created) VALUES(?,'routine_terminal_report_repaired',?,?)",params![run,json!({"previous_status":"failed","previous_error":UNCLASSIFIED}).to_string(),crate::db::now()])?;
        tx.execute(
            "UPDATE chat_messages SET body=? WHERE run_id=? AND kind='result'",
            params![output, run],
        )?;
    }
    tx.commit()?;
    Ok(())
}
