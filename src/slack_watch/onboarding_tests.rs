use super::onboarding::complete_setup;
use super::*;

fn running(app: &App, bot: &str) -> db::Run {
    app.db
        .queue(
            bot,
            "Watch all urgent requests, mentions everywhere, and DMs to Helpdesk",
            0,
        )
        .unwrap();
    app.db.claim_bot(bot).unwrap().unwrap()
}
fn output(value: &Value) -> Value {
    serde_json::from_str(value["text"].as_str().unwrap()).unwrap()
}
fn write_credentials(app: &App, id: &str) {
    std::fs::write(credential_path(app),json!({format!("slack:{id}"):json!({"app_token":"xapp-synthetic-test","access_token":"xoxp-synthetic-test"}).to_string()}).to_string()).unwrap();
}

#[tokio::test]
async fn bot_can_save_combined_rules_then_pause_them_with_existing_permissions() {
    let (mut app, a, w) = tests::setup();
    let root = std::env::temp_dir().join(format!("kindred-slack-tools-{}", db::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::sync::Arc::get_mut(&mut app).unwrap().config.database =
        root.join("workspace.db").to_string_lossy().into();
    write_credentials(&app, &a.id);
    app.db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM slack_watches", [])
        .unwrap();
    let mut bot = app.db.bot(&w.bot_id).unwrap();
    bot.approval_mode = "full".into();
    app.db.save_bot(&bot).unwrap();
    let run = running(&app, &bot.id);
    let ready = crate::runtime::call_tool(
        &app,
        &bot,
        &run,
        "slack_monitor_setup",
        json!({"topic_key":"helpdesk","account_id":a.id}),
    )
    .await
    .unwrap();
    assert_eq!(output(&ready)["status"], "ready");
    let args = json!({"account_id":a.id,"instructions":"Help with actual requests. Finish quietly when no response is needed.","rules":[{"event":"mentions","scope":"all_channels"},{"event":"all_messages","scope":"direct_messages"}],"enabled":true});
    let saved = crate::runtime::call_tool(&app, &bot, &run, "slack_monitor_save", args.clone())
        .await
        .unwrap();
    assert_ne!(saved["failed"], true, "{saved}");
    let id = output(&saved)["id"].as_str().unwrap().to_owned();
    let retry = crate::runtime::call_tool(&app, &bot, &run, "slack_monitor_save", args)
        .await
        .unwrap();
    assert_eq!(output(&retry)["id"], id);
    assert_eq!(tests::count(&app.db, "slack_watches"), 1);
    let paused = crate::runtime::call_tool(
        &app,
        &bot,
        &run,
        "slack_monitor_control",
        json!({"id":id,"action":"pause"}),
    )
    .await
    .unwrap();
    assert_ne!(paused["failed"], true);
    assert!(!watches(&app.db).unwrap()[0].enabled);
    assert_eq!(tests::count(&app.db, "routines"), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn setup_tool_is_callable_without_a_connection_on_every_provider() {
    for provider in [
        "codex",
        "claude-code",
        "openrouter",
        "kimi-code",
        "opencode",
        "opencode-go",
        "custom-00000000-0000-4000-8000-000000000001",
    ] {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, provider);
        assert!(
            crate::runtime::tool_specs_for(&app, &bot)
                .iter()
                .any(|t| t["name"] == "slack_monitor_setup")
        );
        let run = running(&app, &bot.id);
        let result = crate::runtime::call_tool(
            &app,
            &bot,
            &run,
            "slack_monitor_setup",
            json!({"topic_key":"helpdesk","account_name":"Helpdesk"}),
        )
        .await
        .unwrap();
        assert_eq!(result["deferred_question"], true);
        let data = output(&result);
        let qid = data["question_id"].as_str().unwrap();
        let again = setup_monitor(
            &app,
            &run,
            &json!({"topic_key":"helpdesk","account_name":"Helpdesk"}),
        )
        .unwrap();
        assert_eq!(output(&again)["question_id"], qid);
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        let cards: Vec<_> = messages
            .iter()
            .filter(|m| m["kind"] == "connection_card")
            .collect();
        assert_eq!(cards.len(), 1);
        assert!(!messages.iter().any(|m| m["kind"] == "question"));
        let card: Value = serde_json::from_str(cards[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(card["toolkit"], "slack_events");
        assert_eq!(card["bot_id"], bot.id);
        assert_eq!(card["account_name"], "Helpdesk");
        assert_eq!(tests::count(&app.db, "slack_watches"), 0);
        assert_eq!(tests::count(&app.db, "routines"), 0);
        assert!(setup_monitor(&app, &run, &json!({"topic_key":"bad key"})).is_err());
    }
}

#[test]
fn connected_setup_reuses_existing_rules_and_never_creates_a_polling_routine() {
    let (app, a, w) = tests::setup();
    let run = running(&app, &w.bot_id);
    let data = output(
        &setup_monitor(&app, &run, &json!({"topic_key":"urgent","account_id":a.id})).unwrap(),
    );
    assert_eq!(data["status"], "ready");
    assert_eq!(data["monitoring"]["items"][0]["id"], w.id);
    assert_eq!(
        data["monitoring"]["items"][0]["rules"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(tests::count(&app.db, "questions"), 0);
    assert_eq!(tests::count(&app.db, "slack_watches"), 1);
    assert_eq!(tests::count(&app.db, "routines"), 0);
    let data = output(
        &setup_monitor(
            &app,
            &run,
            &json!({"topic_key":"other","account_name":"Other team"}),
        )
        .unwrap(),
    );
    assert_eq!(
        data["status"], "pending",
        "A different requested account must not silently reuse Helpdesk"
    );
}

#[tokio::test]
async fn verified_connection_resumes_once_with_original_request_and_preserves_rules() {
    let (mut app, a, w) = tests::setup();
    let root = std::env::temp_dir().join(format!("kindred-slack-setup-{}", db::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::sync::Arc::get_mut(&mut app).unwrap().config.database =
        root.join("workspace.db").to_string_lossy().into();
    let run = running(&app, &w.bot_id);
    health(&app.db, &a.id, &a.revision, "connecting", "").unwrap();
    let data = output(
        &setup_monitor(
            &app,
            &run,
            &json!({"topic_key":"helpdesk","account_id":a.id}),
        )
        .unwrap(),
    );
    let qid = data["question_id"].as_str().unwrap();
    assert!(complete_setup(&app, qid, &a.id).await.is_err());
    health(&app.db, &a.id, &a.revision, "connected", "").unwrap();
    assert!(
        complete_setup(&app, qid, &a.id).await.is_err(),
        "Missing credentials must not resume"
    );
    write_credentials(&app, &a.id);
    assert!(complete_setup(&app, qid, "foreign-account").await.is_err());
    let first = complete_setup(&app, qid, &a.id).await.unwrap();
    let second = complete_setup(&app, qid, &a.id).await.unwrap();
    assert_eq!(first, second);
    let next = app
        .db
        .run(first["continuation_run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(next.bot_id, w.bot_id);
    assert!(
        next.prompt
            .contains("urgent requests, mentions everywhere, and DMs to Helpdesk")
    );
    assert!(next.prompt.contains(&a.id));
    assert!(!next.prompt.contains("xapp-"));
    assert!(!next.prompt.contains("xoxp-"));
    let data = output(&setup_monitor(&app, &next, &json!({"topic_key":"helpdesk"})).unwrap());
    assert_eq!(data["is_current_continuation"], true);
    assert_eq!(tests::count(&app.db, "slack_watches"), 1);
    assert_eq!(tests::count(&app.db, "routines"), 0);
    let messages = app.db.chat_messages(&run.chat_id).unwrap();
    let card: Value = serde_json::from_str(
        messages
            .iter()
            .find(|m| m["kind"] == "connection_card")
            .unwrap()["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(card["completed"], true);
    assert_eq!(card["declined"], false);
    let other = crate::tests::app();
    assert!(complete_setup(&other, qid, &a.id).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn declined_or_dismissed_setup_stays_closed_and_cannot_be_completed() {
    for dismissed in [false, true] {
        let (app, a, w) = tests::setup();
        let run = running(&app, &w.bot_id);
        health(&app.db, &a.id, &a.revision, "disconnected", "").unwrap();
        let args = json!({"topic_key":"helpdesk"});
        let result = setup_monitor(&app, &run, &args).unwrap();
        let data = output(&result);
        let qid = data["question_id"].as_str().unwrap();
        if dismissed {
            app.db.dismiss_question(qid).unwrap();
        } else {
            app.db
                .answer_question(
                    qid,
                    crate::questions::Answer {
                        selected: Some(1),
                        custom: None,
                    },
                )
                .unwrap();
        }
        let before = tests::count(&app.db, "runs");
        let again = setup_monitor(&app, &run, &args).unwrap();
        assert_eq!(again["deferred_question"], false);
        assert!(
            output(&again)["instruction"]
                .as_str()
                .unwrap()
                .contains("Finish quietly")
        );
        assert!(complete_setup(&app, qid, &a.id).await.is_err());
        assert_eq!(tests::count(&app.db, "runs"), before);
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        let card: Value = serde_json::from_str(
            messages
                .iter()
                .find(|m| m["kind"] == "connection_card")
                .unwrap()["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(card["dismissed"], dismissed);
        assert_eq!(card["declined"], !dismissed);
        assert_eq!(tests::count(&app.db, "questions"), 1);
    }
}

#[tokio::test]
async fn setup_completion_requires_workspace_auth() {
    use std::future::IntoFuture;
    let app = crate::tests::app();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app)).into_future());
    let response = reqwest::Client::new()
        .post(format!("{base}/api/slack-setup/question/complete"))
        .json(&json!({"account_id":"fake"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    server.abort();
}
