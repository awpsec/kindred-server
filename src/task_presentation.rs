//! Event-backed, phase-scoped task stages. Labels are intent; completed work is evidence.
use crate::db::{self, Db, Run};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    stages: Vec<String>,
    current_stage: usize,
    promoted: bool,
}
fn rejected(reason: &str) -> Value {
    json!({"accepted":false,"reason":reason})
}
fn visible(snapshot: Option<&Snapshot>) -> Value {
    match snapshot {
        Some(s) => {
            json!({"mode":if s.promoted {"staged"} else {"compact"},"stages":s.stages,"current_stage":s.current_stage,"promoted":s.promoted})
        }
        None => json!({"mode":"compact","stages":[],"current_stage":0,"promoted":false}),
    }
}
fn boundary(c: &Connection, run: &str, end: i64) -> Result<(i64, i64)> {
    Ok(c.query_row("SELECT COALESCE(MAX(p.message_seq),0),COALESCE(MAX(m.source_event_seq),0) FROM message_progress p JOIN chat_messages m ON m.seq=p.message_seq WHERE p.run_id=?1 AND p.phase='final_answer' AND (m.source_event_seq<?2 OR m.source_event_seq IS NULL)",params![run,end],|r|Ok((r.get(0)?,r.get(1)?)))?)
}
// A continuation may inherit only the state that existed when its question was
// asked. Unused work before that question can fund one child transition.
fn snapshot(
    c: &Connection,
    run: &str,
    start: i64,
    end: i64,
    seen: &mut HashSet<String>,
) -> Result<Option<(Snapshot, i64, bool)>> {
    if start >= end || seen.len() >= 32 || !seen.insert(run.into()) {
        return Ok(None);
    }
    let (final_message, final_event) = boundary(c, run, end)?;
    let start = start.max(final_event);
    let saved:Option<(String,i64)>=c.query_row("SELECT body,seq FROM events WHERE run_id=?1 AND kind='task_presentation' AND seq>?2 AND seq<=?3 ORDER BY seq DESC LIMIT 1",params![run,start,end],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((body, seq)) = saved {
        // A corrupt display event must not prevent the rest of chat from loading.
        let state = serde_json::from_str::<Snapshot>(&body)
            .ok()
            .filter(|state| {
                let mut unique = HashSet::new();
                (2..=8).contains(&state.stages.len())
                    && state.current_stage < state.stages.len()
                    && state.promoted == (state.current_stage > 0)
                    && state.stages.iter().all(|label| {
                        !label.is_empty()
                            && label == label.trim()
                            && label.chars().count() <= 48
                            && !label
                                .chars()
                                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                            && unique.insert(label.to_lowercase())
                    })
            });
        return Ok(state.map(|state| (state, seq, false)));
    }
    if start != 0 || final_message != 0 {
        return Ok(None);
    }
    let parent:Option<(String,i64)>=c.query_row("SELECT q.run_id,(SELECT MIN(e.seq) FROM events e WHERE e.run_id=q.run_id AND e.kind='question' AND json_extract(e.body,'$.id')=q.id) FROM questions q JOIN runs origin ON origin.id=q.run_id JOIN runs child ON child.id=q.continuation_run_id WHERE child.id=?1 AND q.status='answered' AND q.task_group_id!='' AND q.bot_id=child.bot_id AND origin.bot_id=child.bot_id AND origin.chat_id=child.chat_id AND q.delivery_chat_id=child.chat_id AND EXISTS(SELECT 1 FROM events e WHERE e.run_id=q.run_id AND e.kind='question' AND json_extract(e.body,'$.id')=q.id) ORDER BY q.created DESC,q.rowid DESC LIMIT 1",[run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((parent, end)) = parent {
        let (_, start) = boundary(c, &parent, end)?;
        if let Some((state, seq, pending)) = snapshot(c, &parent, start, end, seen)? {
            let pending = pending || evidence(c, &parent, start.max(seq), end)?;
            return Ok(Some((state, 0, pending)));
        }
    }
    Ok(None)
}

/// Event-sequence range is (start, end], matching progress phase serialization.
pub fn presentation(c: &Connection, run: &str, start: i64, end: i64) -> Result<Value> {
    Ok(visible(
        snapshot(c, run, start, end, &mut HashSet::new())?
            .as_ref()
            .map(|(state, _, _)| state),
    ))
}
fn substantive(tool: &str) -> bool {
    matches!(
        tool,
        "guest_exec"
            | "computer_open_url"
            | "computer_browser_task"
            | "computer_screenshot"
            | "computer_click"
            | "computer_type"
            | "computer_key"
            | "computer_scroll"
            | "local_list"
            | "local_read"
            | "local_write"
            | "local_mkdir"
            | "local_skill_scan"
            | "local_exec"
            | "local_screenshot"
            | "local_click"
            | "local_type"
            | "local_key"
            | "local_scroll"
            | "connector_execute"
            | "codex_connector"
            | "claude_connector"
            | "artifact_read"
            | "artifact_create"
            | "artifact_update"
            | "artifact_publish_file"
            | "artifact_export"
            | "share_file"
            | "read_attachment"
            | "workspace_import_draft"
            | "workspace_sync_prepare"
            | "workspace_sync_apply"
            | "send_to_bot"
    )
}
fn evidence(c: &Connection, run: &str, after: i64, end: i64) -> Result<bool> {
    let mut query=c.prepare("SELECT kind,body FROM events WHERE run_id=? AND seq>? AND seq<=? AND kind IN ('tool_started','tool_result') ORDER BY seq")?;
    let rows = query.query_map(params![run, after, end], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut started: Vec<(String, Option<String>, bool)> = Vec::new();
    for row in rows {
        let (kind, body) = row?;
        let Ok(body) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        let Some(tool) = body["tool"].as_str().filter(|tool| substantive(tool)) else {
            continue;
        };
        let id = body["call_id"].as_str().filter(|id| !id.is_empty());
        if kind == "tool_started" {
            let completed_action = !(matches!(tool, "guest_exec" | "local_exec")
                && body["args"]["background"] == true);
            started.push((tool.into(), id.map(str::to_owned), completed_action));
            continue;
        }
        // Native connectors have IDs; ordinary runtime starts currently have none.
        // Never match two different nonempty IDs, or count the same start twice.
        let matching = started
            .iter()
            .position(|(name, call, _)| name == tool && call.as_deref() == id)
            .or_else(|| {
                id.and_then(|_| {
                    started
                        .iter()
                        .position(|(name, call, _)| name == tool && call.is_none())
                })
            });
        if let Some(index) = matching {
            let (_, _, completed_action) = started.remove(index);
            if completed_action
                && body
                    .get("exit_code")
                    .is_none_or(|value| value.is_null() || value == 0)
                && body["failed"] == false
                && body["timed_out"] != true
                && body["stopped"] != true
                && body["uncertain_effect"] != true
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Validation is ordinary model feedback, never a failed action or new approval.
pub fn update(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    let Some(args) = args.as_object() else {
        return Ok(rejected("Provide exactly one plan or stage."));
    };
    if args.len() != 1 || !args.contains_key("plan") && !args.contains_key("stage") {
        return Ok(rejected("Provide exactly one plan or stage."));
    }
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=? AND bot_id=? AND chat_id=? AND status='running')",params![run.id,run.bot_id,run.chat_id],|r|r.get(0))?;
    if !active {
        return Ok(rejected(
            "Only the current running bot's task can update its stages.",
        ));
    }
    let (_, phase) = boundary(&tx, &run.id, i64::MAX)?;
    let previous = snapshot(&tx, &run.id, phase, i64::MAX, &mut HashSet::new())?;
    let next = if let Some(plan) = args.get("plan") {
        let Some(plan) = plan.as_object().filter(|p| p.len() == 1) else {
            return Ok(rejected("A plan must contain only stages."));
        };
        let Some(labels) = plan
            .get("stages")
            .and_then(Value::as_array)
            .filter(|v| (2..=8).contains(&v.len()))
        else {
            return Ok(rejected("A plan needs 2 to 8 stages."));
        };
        let mut unique = HashSet::new();
        let mut stages = Vec::new();
        for label in labels {
            let Some(raw) = label.as_str() else {
                return Ok(rejected("Stage labels must be text."));
            };
            let label = raw.trim();
            if label.is_empty()
                || label.chars().count() > 48
                || raw
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                || !unique.insert(label.to_lowercase())
            {
                return Ok(rejected(
                    "Use unique, single-line stage labels of 1 to 48 characters.",
                ));
            }
            stages.push(label.into());
        }
        if let Some((saved, _, _)) = &previous {
            if saved.stages != stages {
                return Ok(rejected(
                    "This phase already has a plan. Continue its stages; create a new plan only for a new task phase.",
                ));
            }
            let mut result = visible(Some(saved));
            result["accepted"] = json!(true);
            result["changed"] = json!(false);
            return Ok(result);
        }
        Snapshot {
            stages,
            current_stage: 0,
            promoted: false,
        }
    } else {
        let Some(stage) = args
            .get("stage")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Ok(rejected("Stage must be a zero-based integer."));
        };
        let Some((saved, seq, pending)) = previous else {
            return Ok(rejected("Provide a plan before advancing a stage."));
        };
        if stage == saved.current_stage {
            let mut result = visible(Some(&saved));
            result["accepted"] = json!(true);
            result["changed"] = json!(false);
            return Ok(result);
        }
        if stage >= saved.stages.len() || stage != saved.current_stage + 1 {
            return Ok(rejected(
                "Advance only to the next stage in the saved plan.",
            ));
        }
        if !pending && !evidence(&tx, &run.id, phase.max(seq), i64::MAX)? {
            return Ok(rejected(
                "Complete a substantive work tool successfully in this phase before advancing. A plan, timer, checklist or status update is not work evidence.",
            ));
        }
        Snapshot {
            current_stage: stage,
            promoted: true,
            ..saved
        }
    };
    tx.execute(
        "INSERT INTO events(run_id,kind,body,created) VALUES(?,'task_presentation',?,?)",
        params![run.id, serde_json::to_string(&next)?, db::now()],
    )?;
    tx.commit()?;
    let mut result = visible(Some(&next));
    result["accepted"] = json!(true);
    result["changed"] = json!(true);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{runtime::Shared, tests};
    fn fixture() -> (Shared, Run) {
        let app = tests::app();
        let bot = tests::bot(&app.db, "codex");
        app.db.queue(&bot.id, "Prepare a useful report", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        (app, run)
    }
    fn plan(app: &Shared, run: &Run) -> Value {
        update(
            &app.db,
            run,
            &json!({"plan":{"stages":["Research","Draft","Verify"]}}),
        )
        .unwrap()
    }
    fn event(app: &Shared, run: &Run, kind: &str, body: Value) -> i64 {
        app.db.event(&run.id, kind, body).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT max(seq) FROM events WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn work(app: &Shared, run: &Run, tool: &str) {
        event(
            app,
            run,
            "tool_started",
            json!({"tool":tool,"call_id":"work"}),
        );
        event(
            app,
            run,
            "tool_result",
            json!({"tool":tool,"call_id":"work","failed":false}),
        );
    }
    fn shown(app: &Shared, run: &Run) -> Value {
        presentation(&app.db.0.lock().unwrap(), &run.id, 0, i64::MAX).unwrap()
    }
    fn count(app: &Shared, run: &Run) -> i64 {
        app.db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM events WHERE run_id=? AND kind='task_presentation'",
                [&run.id],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn final_answer(app: &Shared, run: &Run) -> i64 {
        event(
            app,
            run,
            "assistant",
            json!({"text":"The first task is finished.","phase":"final_answer"}),
        )
    }
    #[test]
    fn task_presentation_plan_validation_and_idempotence() {
        let (app, run) = fixture();
        for bad in [
            json!(null),
            json!({}),
            json!({"stage":0,"plan":{}}),
            json!({"plan":[]}),
            json!({"plan":{"stages":["Only"]}}),
            json!({"plan":{"stages":["A","B"],"extra":true}}),
            json!({"plan":{"stages":["A",2]}}),
            json!({"plan":{"stages":[" ","B"]}}),
            json!({"plan":{"stages":[" Read ","read"]}}),
            json!({"plan":{"stages":["Bad\nlabel","B"]}}),
            json!({"plan":{"stages":["Bad\u{2028}label","B"]}}),
            json!({"plan":{"stages":["x".repeat(49),"B"]}}),
            json!({"plan":{"stages":["1","2","3","4","5","6","7","8","9"]}}),
        ] {
            let result = update(&app.db, &run, &bad).unwrap();
            assert_eq!(result["accepted"], false, "{bad}");
            assert!(result.get("failed").is_none());
        }
        assert_eq!(count(&app, &run), 0);
        assert_eq!(plan(&app, &run)["mode"], "compact");
        assert_eq!(count(&app, &run), 1);
        assert_eq!(
            update(
                &app.db,
                &run,
                &json!({"plan":{"stages":[" Research ","Draft","Verify"]}})
            )
            .unwrap()["changed"],
            false
        );
        assert_eq!(
            update(
                &app.db,
                &run,
                &json!({"plan":{"stages":["Different","Plan"]}})
            )
            .unwrap()["accepted"],
            false
        );
        assert_eq!(count(&app, &run), 1);
        assert_eq!(shown(&app, &run)["promoted"], false);
        let (app, run) = fixture();
        assert_eq!(
            update(
                &app.db,
                &run,
                &json!({"plan":{"stages":["é".repeat(48),"Done"]}})
            )
            .unwrap()["accepted"],
            true
        );
    }
    #[test]
    fn task_presentation_stage_advances_once_with_work_and_latches() {
        let (app, run) = fixture();
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
            false
        );
        plan(&app, &run);
        for stage in [json!(-1), json!(1.5), json!("1"), json!(2), json!(99)] {
            assert_eq!(
                update(&app.db, &run, &json!({"stage":stage})).unwrap()["accepted"],
                false
            );
        }
        assert_eq!(
            update(&app.db, &run, &json!({"stage":0})).unwrap()["changed"],
            false
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
            false
        );
        work(&app, &run, "local_read");
        let result = update(&app.db, &run, &json!({"stage":1})).unwrap();
        assert_eq!(result["mode"], "staged");
        assert_eq!(result["current_stage"], 1);
        assert_eq!(plan(&app, &run)["changed"], false);
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["changed"],
            false
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":2})).unwrap()["accepted"],
            false,
            "Old evidence must not be reused"
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":0})).unwrap()["accepted"],
            false
        );
        work(&app, &run, "artifact_create");
        assert_eq!(
            update(&app.db, &run, &json!({"stage":2})).unwrap()["accepted"],
            true
        );
        assert_eq!(shown(&app, &run)["mode"], "staged");
        assert_eq!(count(&app, &run), 3);
    }
    #[test]
    fn task_presentation_metadata_failed_or_unpaired_calls_are_not_evidence() {
        for tool in [
            "task_progress",
            "set_task_title",
            "checklist_create",
            "reminder_set",
            "routine_create",
            "inbox_monitor_save",
            "visual_panel",
            "command_wait",
            "command_status",
            "command_start",
            "workspace_import_start",
            "remember",
            "unknown",
        ] {
            let (app, run) = fixture();
            plan(&app, &run);
            work(&app, &run, tool);
            assert_eq!(
                update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                false,
                "{tool}"
            );
        }
        for failure in ["failed", "timed_out", "stopped", "uncertain_effect"] {
            let (app, run) = fixture();
            plan(&app, &run);
            event(
                &app,
                &run,
                "tool_started",
                json!({"tool":"guest_exec","call_id":"x"}),
            );
            let mut result = json!({"tool":"guest_exec","call_id":"x","failed":false});
            result[failure] = json!(true);
            event(&app, &run, "tool_result", result);
            // A second result cannot reuse the start consumed by a failed receipt.
            event(
                &app,
                &run,
                "tool_result",
                json!({"tool":"guest_exec","call_id":"x","failed":false}),
            );
            assert_eq!(
                update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                false,
                "{failure}"
            );
        }
        let (app, run) = fixture();
        work(&app, &run, "local_read");
        plan(&app, &run);
        event(
            &app,
            &run,
            "tool_result",
            json!({"tool":"local_read","failed":false}),
        );
        event(
            &app,
            &run,
            "tool_started",
            json!({"tool":"local_read","call_id":"a"}),
        );
        event(
            &app,
            &run,
            "tool_result",
            json!({"tool":"local_read","call_id":"b","failed":false}),
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
            false
        );
        event(
            &app,
            &run,
            "tool_result",
            json!({"tool":"local_read","call_id":"a","failed":false}),
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
            true
        );
    }
    #[test]
    fn task_presentation_background_launch_and_nonzero_exit_do_not_complete_a_stage() {
        for tool in ["guest_exec", "local_exec"] {
            for (background, exit) in [(true, 0), (false, 1), (false, -9)] {
                let (app, run) = fixture();
                plan(&app, &run);
                event(
                    &app,
                    &run,
                    "tool_started",
                    json!({"tool":tool,"args":{"background":background}}),
                );
                event(
                    &app,
                    &run,
                    "tool_result",
                    json!({"tool":tool,"call_id":"runtime","failed":false,"exit_code":exit}),
                );
                assert_eq!(
                    update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                    false,
                    "{tool} background={background} exit={exit}"
                );
                work(&app, &run, tool);
                assert_eq!(
                    update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                    true
                );
            }
        }
    }
    #[test]
    fn task_presentation_all_work_categories_and_runtime_no_id_starts() {
        for tool in [
            "guest_exec",
            "computer_click",
            "local_read",
            "connector_execute",
            "codex_connector",
            "claude_connector",
            "artifact_update",
            "workspace_sync_apply",
            "send_to_bot",
        ] {
            let (app, run) = fixture();
            plan(&app, &run);
            event(&app, &run, "tool_started", json!({"tool":tool}));
            event(
                &app,
                &run,
                "tool_result",
                json!({"tool":tool,"call_id":"runtime-result-id","failed":false,"text":"PRIVATE_TOOL_BODY"}),
            );
            assert_eq!(
                update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                true,
                "{tool}"
            );
            assert!(!shown(&app, &run).to_string().contains("PRIVATE_TOOL_BODY"));
        }
    }
    #[test]
    fn task_presentation_rejects_foreign_or_finished_runs() {
        let (app, run) = fixture();
        let mut wrong = run.clone();
        wrong.bot_id = "foreign".into();
        assert_eq!(plan(&app, &wrong)["accepted"], false);
        wrong = run.clone();
        wrong.chat_id = "foreign".into();
        assert_eq!(plan(&app, &wrong)["accepted"], false);
        app.db.finish(&run.id, "completed", "Done", "").unwrap();
        assert_eq!(plan(&app, &run)["accepted"], false);
        assert_eq!(count(&app, &run), 0);
    }
    #[test]
    fn task_presentation_final_answer_resets_plan_and_old_phase_never_sees_future() {
        let (app, run) = fixture();
        plan(&app, &run);
        work(&app, &run, "local_read");
        update(&app.db, &run, &json!({"stage":1})).unwrap();
        let before = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT max(seq) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let end = final_answer(&app, &run);
        assert_eq!(shown(&app, &run)["mode"], "compact");
        assert_eq!(shown(&app, &run)["stages"], json!([]));
        assert_eq!(
            update(&app.db, &run, &json!({"stage":2})).unwrap()["accepted"],
            false
        );
        assert_eq!(
            update(
                &app.db,
                &run,
                &json!({"plan":{"stages":["New work","Review"]}})
            )
            .unwrap()["accepted"],
            true
        );
        assert_eq!(
            update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
            false
        );
        work(&app, &run, "artifact_read");
        update(&app.db, &run, &json!({"stage":1})).unwrap();
        let c = app.db.0.lock().unwrap();
        for upper in [before, end] {
            let old = presentation(&c, &run.id, 0, upper).unwrap();
            assert_eq!(old["stages"][0], "Research");
            assert_eq!(old["current_stage"], 1);
        }
        assert_eq!(
            presentation(&c, &run.id, end, i64::MAX).unwrap()["stages"][0],
            "New work"
        );
        assert_eq!(
            presentation(&c, &run.id, end, end).unwrap()["mode"],
            "compact"
        );
    }
    fn answer(app: &Shared, run: &Run) -> Run {
        crate::progress_updates::set_task_title(&app.db, run, "Prepare report").unwrap();
        app.db
            .ask_question(
                run,
                crate::questions::QuestionInput {
                    topic_key: db::id(),
                    question: "Continue drafting?".into(),
                    context: "Research is ready.".into(),
                    options: vec!["Continue".into(), "Wait".into()],
                },
            )
            .unwrap();
        let q = app
            .db
            .pending_questions()
            .unwrap()
            .into_iter()
            .find(|q| q.run_id == run.id)
            .unwrap();
        assert!(!q.task_group_id.is_empty());
        app.db.finish(&run.id, "completed", "", "").unwrap();
        app.db
            .answer_question(
                &q.id,
                crate::questions::Answer {
                    selected: Some(0),
                    custom: None,
                },
            )
            .unwrap();
        app.db.claim_bot(&run.bot_id).unwrap().unwrap()
    }
    #[test]
    fn task_presentation_corrupt_snapshots_fall_back_to_compact() {
        for body in [
            "{".to_owned(),
            json!({"stages":["A","B"]}).to_string(),
            json!({"stages":[],"current_stage":0,"promoted":false}).to_string(),
            json!({"stages":["A","B"],"current_stage":2,"promoted":true}).to_string(),
            json!({"stages":["A","B"],"current_stage":0,"promoted":true}).to_string(),
            json!({"stages":["A","B"],"current_stage":1,"promoted":false}).to_string(),
            json!({"stages":[" A","B"],"current_stage":0,"promoted":false}).to_string(),
            json!({"stages":["A","a"],"current_stage":0,"promoted":false}).to_string(),
            json!({"stages":["A\nB","C"],"current_stage":0,"promoted":false}).to_string(),
        ] {
            let (app, run) = fixture();
            plan(&app, &run);
            app.db.0.lock().unwrap().execute(
                "INSERT INTO events(run_id,kind,body,created) VALUES(?,'task_presentation',?,?)",
                params![run.id,body,db::now()],
            ).unwrap();
            assert_eq!(shown(&app, &run), visible(None), "{body}");
            assert_eq!(
                update(&app.db, &run, &json!({"stage":1})).unwrap()["accepted"],
                false
            );
            assert_eq!(plan(&app, &run)["accepted"], true);
        }
    }
    #[test]
    fn task_presentation_question_carries_unused_work_once_with_exact_bounds() {
        for parent_work_before_question in [true, false] {
            let (app, run) = fixture();
            plan(&app, &run);
            if parent_work_before_question {
                work(&app, &run, "local_read");
            }
            let follow = answer(&app, &run);
            // Even a same-second receipt after the question must never qualify.
            work(&app, &run, "artifact_read");
            assert_eq!(
                update(&app.db, &follow, &json!({"stage":1})).unwrap()["accepted"],
                parent_work_before_question
            );
            if !parent_work_before_question {
                work(&app, &follow, "local_read");
                assert_eq!(
                    update(&app.db, &follow, &json!({"stage":1})).unwrap()["accepted"],
                    true
                );
            }
            assert_eq!(
                update(&app.db, &follow, &json!({"stage":2})).unwrap()["accepted"],
                false,
                "An inherited work receipt is consumed by the first child snapshot"
            );
            work(&app, &follow, "artifact_create");
            assert_eq!(
                update(&app.db, &follow, &json!({"stage":2})).unwrap()["accepted"],
                true
            );
        }
    }
    #[test]
    fn task_presentation_question_inherits_only_asked_phase_and_requires_fresh_work() {
        let (app, run) = fixture();
        plan(&app, &run);
        work(&app, &run, "artifact_read");
        update(&app.db, &run, &json!({"stage":1})).unwrap();
        let follow = answer(&app, &run);
        assert_eq!(shown(&app, &follow)["current_stage"], 1);
        assert_eq!(shown(&app, &follow)["mode"], "staged");
        assert_eq!(plan(&app, &follow)["changed"], false);
        assert_eq!(
            update(&app.db, &follow, &json!({"stage":2})).unwrap()["accepted"],
            false
        );
        // A source run's later events (even in the same second) must not replace
        // the state attached to the original question.
        event(
            &app,
            &run,
            "task_presentation",
            json!({"stages":["FUTURE","LEAK"],"current_stage":1,"promoted":true}),
        );
        assert_eq!(shown(&app, &follow)["stages"][0], "Research");
        work(&app, &follow, "artifact_update");
        assert_eq!(
            update(&app.db, &follow, &json!({"stage":2})).unwrap()["accepted"],
            true
        );
        final_answer(&app, &follow);
        assert_eq!(shown(&app, &follow)["mode"], "compact");
        assert_eq!(plan(&app, &follow)["changed"], true);
    }
    #[test]
    fn task_presentation_question_scope_and_missing_receipt_fail_closed() {
        for change in [
            "UPDATE questions SET bot_id='foreign'",
            "UPDATE questions SET delivery_chat_id='foreign'",
            "UPDATE questions SET task_group_id=''",
            "UPDATE questions SET status='dismissed'",
            "DELETE FROM events WHERE kind='question'",
            "UPDATE runs SET chat_id='foreign' WHERE id IN (SELECT run_id FROM questions)",
        ] {
            let (app, run) = fixture();
            plan(&app, &run);
            let follow = answer(&app, &run);
            assert!(
                !shown(&app, &follow)["stages"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            // Foreign-key constraints are deliberately disabled only to exercise
            // defensive reading of legacy/corrupt persisted linkage.
            app.db
                .0
                .lock()
                .unwrap()
                .execute_batch("PRAGMA foreign_keys=OFF")
                .unwrap();
            app.db.0.lock().unwrap().execute(change, []).unwrap();
            assert_eq!(shown(&app, &follow)["stages"], json!([]), "{change}");
        }
    }
    #[test]
    fn task_presentation_nested_continuation_and_cycle_are_bounded() {
        let (app, run) = fixture();
        plan(&app, &run);
        work(&app, &run, "local_read");
        update(&app.db, &run, &json!({"stage":1})).unwrap();
        let first = answer(&app, &run);
        let second = answer(&app, &first);
        assert_eq!(shown(&app, &second)["current_stage"], 1);
        app.db
            .0
            .lock()
            .unwrap()
            .execute("DELETE FROM events WHERE kind='task_presentation'", [])
            .unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE questions SET run_id=? WHERE continuation_run_id=?",
                params![second.id, first.id],
            )
            .unwrap();
        // Make the edited question's receipt belong to the cycle as well.
        app.db.0.lock().unwrap().execute("UPDATE events SET run_id=? WHERE kind='question' AND json_extract(body,'$.id')=(SELECT id FROM questions WHERE continuation_run_id=?)",params![second.id,first.id]).unwrap();
        assert_eq!(shown(&app, &second)["mode"], "compact");
    }
}
