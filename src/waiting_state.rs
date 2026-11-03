//! One read-only projection from authoritative pending records. No cached wait
//! flags, state mutations, automatic answers or follow-up turns during readback.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub fn for_run(c: &Connection, run: &str) -> Result<Value> {
    let status: Option<String> = c.query_row("SELECT status FROM runs WHERE id=?",[run],|r|r.get(0)).optional()?;
    if status.is_none() || matches!(status.as_deref(), Some("cancelled" | "cancelling" | "steered")) {
        return Ok(json!({"kind":"","label":"","reasons":[]}));
    }
    let mut reasons = Vec::new();
    for (kind, sql) in [
        ("approval", "SELECT id FROM approvals WHERE run_id=? AND status='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=run_id AND status='awaiting_approval')"),
        ("user", "SELECT id FROM user_tasks WHERE run_id=? AND status='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=run_id AND status='awaiting_user')"),
        ("question", "SELECT id FROM questions WHERE run_id=? AND status='pending'"),
        ("teammate", "SELECT e.child_run_id FROM collaboration_requests e JOIN runs child ON child.id=e.child_run_id WHERE e.parent_run_id=? AND e.resolved=0 AND e.continuation_run_id='' AND child.status NOT IN ('cancelled','cancelling','steered')"),
        ("process", "SELECT j.id FROM command_waits w JOIN json_each(w.ids) x JOIN command_jobs j ON j.id=x.value WHERE w.run_id=? AND w.continuation='' AND j.status IN ('starting','running')"),
    ] {
        let ids = c.prepare(sql)?.query_map([run],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        if !ids.is_empty() { reasons.push(json!({"kind":kind,"ids":ids})); }
    }
    let kind = reasons.first().and_then(|r|r["kind"].as_str()).unwrap_or("");
    let label = match kind {
        "approval" => "Waiting for your approval",
        "user" => "Waiting for you",
        "question" => "Waiting for your answer",
        "teammate" => "Waiting on a teammate",
        "process" => "Waiting for a command",
        _ => "",
    };
    Ok(json!({"kind":kind,"label":label,"reasons":reasons}))
}

pub fn for_chat(c: &Connection, chat: &str) -> Result<Vec<Value>> {
    let ids = c.prepare("SELECT id FROM runs r WHERE chat_id=? AND (status IN ('running','awaiting_user','awaiting_approval') OR EXISTS(SELECT 1 FROM questions WHERE run_id=r.id AND status='pending') OR EXISTS(SELECT 1 FROM collaboration_requests WHERE parent_run_id=r.id AND resolved=0 AND continuation_run_id='') OR EXISTS(SELECT 1 FROM command_waits WHERE run_id=r.id AND continuation=''))")?
        .query_map([chat],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut waits = Vec::new();
    for id in ids {
        let mut value = for_run(c,&id)?;
        if value["kind"] != "" { value["run_id"]=json!(id); waits.push(value); }
    }
    Ok(waits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authoritative_approval_does_not_survive_expiry_or_cancellation() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db,"codex");
        let id = app.db.queue(&bot.id,"Review a change",0).unwrap();
        app.db.claim().unwrap();
        let approval = app.db.request_approval(&id,"guest_exec",&json!({"command":"echo test"})).unwrap();
        let c = app.db.0.lock().unwrap();
        assert_eq!(for_run(&c,&id).unwrap()["kind"],"approval");
        assert!(for_chat(&c,"other-chat").unwrap().is_empty());
        c.execute("UPDATE approvals SET status='expired' WHERE id=?",[approval]).unwrap();
        assert_eq!(for_run(&c,&id).unwrap()["kind"],"");
        assert_eq!(for_run(&c,"missing").unwrap()["kind"],"");
    }
}
