use crate::{db, routine_completion as policy, runtime, tests};
use serde_json::json;
fn scheduled(app: &runtime::Shared, bot: &db::Bot) -> String {
    let id = app
        .db
        .queue(&bot.id, "Synthetic scheduled check", 0)
        .unwrap();
    let c = app.db.0.lock().unwrap();
    c.execute(
        "INSERT INTO routine_runs VALUES(?,'synthetic-routine',0)",
        [&id],
    )
    .unwrap();
    policy::register(&c, &id, &id).unwrap();
    id
}
fn outcome(app: &runtime::Shared, id: &str, kind: &str, text: &str) {
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction().unwrap();
    policy::complete(&tx, id, &json!({"outcome":kind,"report":text})).unwrap();
    tx.commit().unwrap();
}
fn finish(app: &runtime::Shared, id: &str, status: &str, text: &str, error: &str) {
    app.db.finish(id, status, text, error).unwrap();
    app.db.event(id, "run_finished", json!({})).unwrap();
    app.db.chat_complete(&app.db.run(id).unwrap()).unwrap();
}
fn results(app: &runtime::Shared, id: &str) -> Vec<serde_json::Value> {
    app.db
        .chat_messages(&app.db.run(id).unwrap().chat_id)
        .unwrap()
        .into_iter()
        .filter(|v| v["run_id"] == id && v["kind"] == "result")
        .collect()
}
#[test]
fn structured_report_replaces_provisional_text_and_delivers_once() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    app.db
        .event(
            &id,
            "assistant",
            json!({"text":"Private check in progress"}),
        )
        .unwrap();
    outcome(&app, &id, "report", "A synthetic game changed.");
    finish(&app, &id, "completed", "Unclassified provider prose", "");
    app.db.chat_complete(&app.db.run(&id).unwrap()).unwrap();
    assert_eq!(app.db.run(&id).unwrap().output, "A synthetic game changed.");
    assert_eq!(results(&app, &id).len(), 1);
    assert_eq!(results(&app, &id)[0]["text"], "A synthetic game changed.");
    assert!(
        !app.db
            .chat_messages(&app.db.run(&id).unwrap().chat_id)
            .unwrap()
            .iter()
            .any(|v| v["text"] == "Private check in progress")
    );
    let c = app.db.0.lock().unwrap();
    let raw: String = c
        .query_row(
            "SELECT body FROM events WHERE run_id=? AND kind='routine_provider_final'",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.contains("Unclassified provider prose"));
}
#[test]
fn omission_is_an_actionable_failure_but_provider_failure_is_preserved() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    finish(&app, &id, "completed", "No change prose", "");
    let run = app.db.run(&id).unwrap();
    assert_eq!(run.status, "failed");
    assert!(run.error.contains("did not classify"));
    assert_eq!(results(&app, &id).len(), 1);
    let other = scheduled(&app, &bot);
    finish(
        &app,
        &other,
        "failed",
        "",
        "Specific synthetic provider outage",
    );
    assert_eq!(
        app.db.run(&other).unwrap().error,
        "Specific synthetic provider outage"
    );
}
#[test]
fn quiet_has_no_public_result_or_completion_alert() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    outcome(&app, &id, "quiet", "");
    finish(&app, &id, "completed", "Ignored no-change prose", "");
    assert!(results(&app, &id).is_empty());
    assert_eq!(app.db.run(&id).unwrap().output, "");
    assert!(
        !app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["run_id"] == id)
    );
}
#[test]
fn outcome_validation_refuses_empty_errors_model_lineage_and_error_hiding() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    {
        let c = app.db.0.lock().unwrap();
        for value in [
            json!({"outcome":"report","report":" "}),
            json!({"outcome":"error"}),
            json!({"outcome":"quiet","report":"hidden"}),
            json!({"outcome":"quiet","root_run_id":id}),
            json!({"outcome":"other"}),
        ] {
            assert!(policy::complete(&c, &id, &value).is_err());
        }
    }
    app.db
        .event(
            &id,
            "tool_result",
            json!({"failed":true,"text":"Action failed"}),
        )
        .unwrap();
    assert!(policy::complete(&app.db.0.lock().unwrap(), &id, &json!({"outcome":"quiet"})).is_err());
    outcome(&app, &id, "error", "Action failed; needs attention.");
    finish(&app, &id, "completed", "", "");
    assert_eq!(
        app.db.run(&id).unwrap().error,
        "Action failed; needs attention."
    );
}
#[test]
fn recovery_fences_old_callbacks_and_keeps_distinct_occurrences() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    finish(&app, &id, "failed", "", "Synthetic outage");
    let next = app.db.continue_task(&id).unwrap();
    assert_eq!(app.db.continue_task(&id).unwrap(), next);
    outcome(&app, &next, "report", "Same report");
    finish(&app, &next, "completed", "", "");
    finish(&app, &id, "completed", "Late obsolete reply", "");
    assert_eq!(app.db.run(&id).unwrap().status, "failed");
    assert_eq!(results(&app, &next).len(), 1);
    let separate = scheduled(&app, &bot);
    outcome(&app, &separate, "report", "Same report");
    finish(&app, &separate, "completed", "", "");
    assert_eq!(results(&app, &separate).len(), 1);
}
#[test]
fn direct_interactive_completion_and_scheduled_yields_remain_unchanged() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let direct = app.db.queue(&bot.id, "Interactive", 0).unwrap();
    finish(&app, &direct, "completed", "Ordinary reply", "");
    assert_eq!(app.db.run(&direct).unwrap().output, "Ordinary reply");
    let id = scheduled(&app, &bot);
    app.db.event(&id, "process_wait", json!({})).unwrap();
    app.db
        .finish(&id, "completed", "Waiting for command", "")
        .unwrap();
    assert_eq!(app.db.run(&id).unwrap().status, "completed");
}
#[test]
fn changed_recovery_membership_refuses_without_creating_a_retry() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    finish(&app, &id, "failed", "", "Synthetic outage");
    let next = app.db.continue_task(&id).unwrap();
    app.db.finish(&next, "failed", "", "Second outage").unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE routine_runs SET routine_id='different' WHERE run_id=?",
            [&next],
        )
        .unwrap();
    assert!(app.db.continue_task(&next).is_err());
    assert!(policy::current(&app.db.0.lock().unwrap(), &next).is_err());
}
#[tokio::test]
async fn actual_tool_uses_server_identity_and_ends_with_a_structured_report() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(run.id, id);
    let result = runtime::call_tool(
        &app,
        &bot,
        &run,
        "routine_complete",
        json!({"outcome":"report","report":"Verified synthetic update"}),
    )
    .await
    .unwrap();
    assert_eq!(result["routine_complete"], true);
    finish(&app, &id, "completed", "Extra raw provider final", "");
    assert_eq!(app.db.run(&id).unwrap().output, "Verified synthetic update");
    let direct = app.db.queue(&bot.id, "Interactive", 0).unwrap();
    let run = app.db.run(&direct).unwrap();
    let denied = runtime::call_tool(
        &app,
        &bot,
        &run,
        "routine_complete",
        json!({"outcome":"quiet"}),
    )
    .await
    .unwrap();
    assert_eq!(denied["failed"], true);
}
#[test]
fn explicit_reports_are_not_classified_by_their_words() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    outcome(&app, &id, "report", "<finish_quietly />");
    finish(&app, &id, "completed", "", "");
    assert_eq!(results(&app, &id).len(), 1);
    assert_eq!(app.db.run(&id).unwrap().output, "<finish_quietly />");
}
#[test]
fn quiet_does_not_change_chat_attention_sidebar_or_message_page() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    let chat = app.db.run(&id).unwrap().chat_id;
    let before = app.db.attention().unwrap()["chats"][&chat].clone();
    let messages_before = app.db.chat_messages(&chat).unwrap();
    let last = app.db.chat(&chat).unwrap().last_message;
    app.db
        .event(
            &id,
            "assistant",
            json!({"text":"Private synthetic progress"}),
        )
        .unwrap();
    outcome(&app, &id, "quiet", "");
    finish(&app, &id, "completed", "Private final", "");
    assert_eq!(app.db.attention().unwrap()["chats"][&chat], before);
    assert_eq!(app.db.chat(&chat).unwrap().last_message, last);
    let after = app.db.chat_messages(&chat).unwrap();
    let content = |mut rows: Vec<serde_json::Value>| {
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("delivery");
        }
        rows
    };
    assert_eq!(content(after.clone()), content(messages_before));
    assert_eq!(after[0]["delivery"][0]["status"], "completed");
}
#[test]
fn pending_decisions_remain_visible_and_cannot_be_quietly_completed() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(run.id, id);
    app.db
        .ask_question(
            &run,
            crate::questions::QuestionInput {
                topic_key: "synthetic-choice".into(),
                question: "Choose the next action".into(),
                context: "Synthetic context".into(),
                options: vec!["Wait".into(), "Proceed".into()],
            },
        )
        .unwrap();
    assert!(policy::complete(&app.db.0.lock().unwrap(), &id, &json!({"outcome":"quiet"})).is_err());
    app.db.finish(&id, "completed", "", "").unwrap();
    assert_eq!(app.db.run(&id).unwrap().status, "completed");
    assert!(
        app.db
            .chat_messages(&run.chat_id)
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "question")
    );
    let approval = scheduled(&app, &bot);
    let card = app
        .db
        .request_approval(&approval, "synthetic_action", &json!({}))
        .unwrap();
    assert!(
        policy::complete(
            &app.db.0.lock().unwrap(),
            &approval,
            &json!({"outcome":"quiet"})
        )
        .is_err()
    );
    assert_eq!(app.db.approval(&card).unwrap(), "pending");
}
#[test]
fn simultaneous_retry_clicks_have_one_successor_and_one_public_report() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let id = scheduled(&app, &bot);
    finish(&app, &id, "failed", "", "Synthetic outage");
    let successors = std::thread::scope(|scope| {
        let a = scope.spawn(|| app.db.continue_task(&id).unwrap());
        let b = scope.spawn(|| app.db.continue_task(&id).unwrap());
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(successors.0, successors.1);
    outcome(&app, &successors.0, "report", "Verified finding");
    finish(&app, &successors.0, "completed", "", "");
    finish(&app, &successors.0, "failed", "", "Late duplicate callback");
    assert_eq!(app.db.run(&successors.0).unwrap().status, "completed");
    assert_eq!(results(&app, &successors.0).len(), 1);
}
#[test]
fn real_due_scheduler_snapshots_completion_policy() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    app.db
        .save_routine(&db::Routine {
            id: "synthetic-schedule".into(),
            bot_id: bot.id.clone(),
            name: "Synthetic check".into(),
            prompt: "Check synthetic state".into(),
            interval_seconds: 60,
            next_run: db::now() - 1,
            enabled: true,
            schedule: None,
            run_at: None,
        })
        .unwrap();
    app.db.tick(db::now()).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(
        policy::root(&app.db.0.lock().unwrap(), &run.id).unwrap(),
        Some(run.id.clone())
    );
    app.db
        .finish(&run.id, "completed", "Unclassified final", "")
        .unwrap();
    assert_eq!(app.db.run(&run.id).unwrap().status, "failed");
}
#[test]
fn legacy_recovery_without_membership_is_repaired_from_the_server_root() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let root = scheduled(&app, &bot);
    finish(&app, &root, "failed", "", "Synthetic outage");
    let old = app.db.continue_task(&root).unwrap();
    {
        let c = app.db.0.lock().unwrap();
        c.execute("DELETE FROM routine_execution WHERE run_id=?", [&old])
            .unwrap();
        c.execute("DELETE FROM routine_runs WHERE run_id=?", [&old])
            .unwrap();
    }
    app.db
        .finish(&old, "failed", "", "Legacy recovery outage")
        .unwrap();
    let next = app.db.continue_task(&old).unwrap();
    assert_eq!(
        policy::root(&app.db.0.lock().unwrap(), &next).unwrap(),
        Some(root)
    );
    outcome(&app, &next, "quiet", "");
    finish(&app, &next, "completed", "", "");
    assert!(results(&app, &next).is_empty());
}
#[test]
fn occurrence_publication_receipt_prevents_a_second_public_report() {
    let app = tests::app();
    let bot = tests::bot(&app.db, "codex");
    let root = scheduled(&app, &bot);
    outcome(&app, &root, "report", "Exact synthetic finding");
    finish(&app, &root, "completed", "", "");
    // Simulate a server recovery request after an uncertain completion receipt.
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE runs SET status='interrupted' WHERE id=?", [&root])
        .unwrap();
    let next = app.db.continue_task(&root).unwrap();
    outcome(&app, &next, "report", "Exact synthetic finding");
    finish(&app, &next, "completed", "", "");
    assert_eq!(results(&app, &root).len(), 1);
    assert!(results(&app, &next).is_empty());
    assert!(
        !app.db.notifications(Some(0)).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["run_id"] == next)
    );
}
