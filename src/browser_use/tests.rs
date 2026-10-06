use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Selector(Mutex<std::collections::VecDeque<Option<String>>>);
impl Selector {
    fn new(choices: &[Option<&str>]) -> Self {
        Self(Mutex::new(
            choices.iter().map(|c| c.map(str::to_string)).collect(),
        ))
    }
}
impl Decisions for Selector {
    fn choose<'a>(&'a self, _input: &'a DecisionInput) -> Work<'a, String> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("Preview unavailable"))
        })
    }
}
struct FixtureBrowser {
    observations: usize,
    actions: usize,
    lose_response: bool,
    wrong_origin: bool,
}
impl FixtureBrowser {
    fn new() -> Self {
        Self {
            observations: 0,
            actions: 0,
            lose_response: false,
            wrong_origin: false,
        }
    }
}
impl Browser for FixtureBrowser {
    fn observe<'a>(&'a mut self, _task: &'a Task) -> Work<'a, Observation> {
        Box::pin(async move {
            self.observations += 1;
            Ok(Observation {
                snapshot_id: self.observations.to_string(),
                url: if self.wrong_origin {
                    "https://other.example/"
                } else {
                    "https://assessment.example/settings"
                }
                .into(),
                title: "Assessment settings".into(),
                text: "Safe checks: off".into(),
                image: "data:image/png;base64,fixture".into(),
                candidates: vec![
                    Candidate {
                        id: "check".into(),
                        kind: "check".into(),
                        label: "Set Safe checks to checked".into(),
                        value_key: None,
                    },
                    Candidate {
                        id: "finish".into(),
                        kind: "finish".into(),
                        label: "Finish".into(),
                        value_key: None,
                    },
                    Candidate {
                        id: "escalate".into(),
                        kind: "escalate".into(),
                        label: "Return to Codex".into(),
                        value_key: None,
                    },
                ],
            })
        })
    }
    fn act<'a>(
        &'a mut self,
        _observation: &'a Observation,
        _candidate: &'a Candidate,
    ) -> Work<'a, ActionReceipt> {
        Box::pin(async move {
            self.actions += 1;
            ensure!(!self.lose_response, "Lost SSH response after input");
            Ok(ActionReceipt {
                applied: true,
                uncertain: false,
                detail: "Checkbox checked".into(),
            })
        })
    }
}
struct FixtureGuard {
    allow: bool,
    stopped: AtomicBool,
    receipts: Mutex<Vec<bool>>,
}
impl FixtureGuard {
    fn new(allow: bool) -> Self {
        Self {
            allow,
            stopped: AtomicBool::new(false),
            receipts: Mutex::new(vec![]),
        }
    }
}
impl Guard for FixtureGuard {
    fn check(&self) -> Result<()> {
        ensure!(!self.stopped.load(Ordering::SeqCst), "Human took control");
        Ok(())
    }
    fn authorize<'a>(&'a self, _task: &'a Task, _candidate: &'a Candidate) -> Work<'a, bool> {
        Box::pin(async move { Ok(self.allow) })
    }
    fn requested(&self, _candidate: &Candidate) -> Result<String> {
        Ok(db::id())
    }
    fn receipt(&self, _id: &str, _candidate: &Candidate, receipt: &ActionReceipt) -> Result<()> {
        self.receipts.lock().unwrap().push(receipt.uncertain);
        Ok(())
    }
}
fn task() -> Task {
    Task {
        goal: "Enable Safe checks and preserve the supplied targets".into(),
        origin: "https://assessment.example".into(),
        values: BTreeMap::new(),
        max_actions: 2,
    }
}

#[tokio::test]
async fn unavailable_and_invalid_choices_never_execute_browser_actions() {
    for choice in [None, Some("invented-delete-command")] {
        let mut browser = FixtureBrowser::new();
        let outcome = drive(
            &Selector::new(&[choice]),
            &mut browser,
            &FixtureGuard::new(true),
            &task(),
        )
        .await;
        assert_eq!(outcome.status, "fallback");
        assert_eq!(browser.actions, 0);
        assert!(outcome.actions.is_empty());
        assert!(outcome.observation.is_some());
    }
}
#[tokio::test]
async fn successful_actions_are_observed_and_preserved_when_decisions_becomes_unavailable() {
    let mut browser = FixtureBrowser::new();
    let guard = FixtureGuard::new(true);
    let outcome = drive(
        &Selector::new(&[Some("check"), None]),
        &mut browser,
        &guard,
        &task(),
    )
    .await;
    assert_eq!(outcome.status, "fallback");
    assert_eq!(browser.actions, 1);
    assert_eq!(browser.observations, 2);
    assert_eq!(outcome.actions[0]["applied"], true);
    assert!(!outcome.uncertain);
    assert_eq!(*guard.receipts.lock().unwrap(), vec![false]);
}
#[tokio::test]
async fn uncertain_dispatch_stops_without_replay_and_marks_existing_recovery_blocker() {
    let mut browser = FixtureBrowser::new();
    browser.lose_response = true;
    let outcome = drive(
        &Selector::new(&[Some("check"), Some("check")]),
        &mut browser,
        &FixtureGuard::new(true),
        &task(),
    )
    .await;
    assert_eq!(browser.actions, 1);
    assert_eq!(browser.observations, 1);
    assert!(outcome.uncertain);
    assert!(
        outcome.observation.is_none(),
        "Never return the pre-action image as current state"
    );
    assert_eq!(outcome.result()["timed_out"], true);
}
#[tokio::test]
async fn denial_budget_and_origin_change_stop_execution() {
    let mut browser = FixtureBrowser::new();
    let outcome = drive(
        &Selector::new(&[Some("check")]),
        &mut browser,
        &FixtureGuard::new(false),
        &task(),
    )
    .await;
    assert_eq!(outcome.status, "denied");
    assert_eq!(browser.actions, 0);
    assert_eq!(outcome.result()["stopped"], true);
    let mut browser = FixtureBrowser::new();
    let mut limited = task();
    limited.max_actions = 1;
    let outcome = drive(
        &Selector::new(&[Some("check"), Some("check")]),
        &mut browser,
        &FixtureGuard::new(true),
        &limited,
    )
    .await;
    assert_eq!(browser.actions, 1);
    assert_eq!(browser.observations, 2);
    assert_eq!(outcome.status, "fallback");
    let mut browser = FixtureBrowser::new();
    browser.wrong_origin = true;
    drive(
        &Selector::new(&[Some("check")]),
        &mut browser,
        &FixtureGuard::new(true),
        &task(),
    )
    .await;
    assert_eq!(browser.actions, 0);
}
struct Takeover<'a>(&'a FixtureGuard);
impl Decisions for Takeover<'_> {
    fn choose<'a>(&'a self, _input: &'a DecisionInput) -> Work<'a, String> {
        Box::pin(async move {
            self.0.stopped.store(true, Ordering::SeqCst);
            Ok("check".into())
        })
    }
}
#[tokio::test]
async fn takeover_while_awaiting_decisions_prevents_the_selected_action() {
    let mut browser = FixtureBrowser::new();
    let guard = FixtureGuard::new(true);
    let outcome = drive(&Takeover(&guard), &mut browser, &guard, &task()).await;
    assert_eq!(outcome.status, "stopped");
    assert_eq!(browser.actions, 0);
    assert_eq!(outcome.result()["stopped"], true);
}
#[test]
fn task_limits_and_remote_effect_classification_are_enforced() {
    assert!(task().validate().is_ok());
    for origin in [
        "https://user:secret@example.com",
        "https://assessment.example/path",
        "file:///etc/passwd",
    ] {
        let mut invalid = task();
        invalid.origin = origin.into();
        assert!(invalid.validate().is_err());
    }
    let mut invalid = task();
    invalid.values.insert("targets".into(), "x".repeat(16001));
    assert!(invalid.validate().is_err());
    for kind in ["check", "fill", "click", "select"] {
        assert_eq!(
            Candidate {
                id: "a".into(),
                kind: kind.into(),
                label: "x".into(),
                value_key: None
            }
            .scope(),
            "external"
        );
    }
}
#[tokio::test]
async fn runtime_falls_back_without_touching_vm_and_other_providers_keep_their_tools() {
    let mut app = crate::tests::app();
    let codex = crate::tests::bot(&app.db, "codex");
    let id = app
        .db
        .queue(&codex.id, "Configure an assessment", 0)
        .unwrap();
    let run = app.db.claim_bot(&codex.id).unwrap().unwrap();
    assert_eq!(run.id, id);
    let result = crate::runtime::call_tool(
        &app,
        &codex,
        &run,
        "computer_browser_task",
        serde_json::to_value(task()).unwrap(),
    )
    .await
    .unwrap();
    let body: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["browser_worker"]["status"], "unavailable");
    assert!(
        body["browser_worker"]["actions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(app.db.approvals().unwrap().is_empty());
    assert!(!app.desktop_sessions.engaged(&run.id));
    let mut open = json!({"url":"https://assessment.example","_decisions_observation":true});
    prepare_open(&app, &codex, &mut open);
    assert!(open.get("_decisions_observation").is_none());
    assert!(
        !crate::runtime::tool_specs_for(&app, &codex)
            .iter()
            .any(|s| s["name"] == "computer_browser_task")
    );
    let mut previous = vec![];
    for provider in [
        "claude-code",
        "kimi-code",
        "openrouter",
        "opencode",
        "opencode-go",
        "custom-11111111-1111-4111-8111-111111111111",
    ] {
        let bot = crate::tests::bot(&app.db, provider);
        previous.push((bot.clone(), crate::runtime::tool_specs_for(&app, &bot)));
    }
    Arc::get_mut(&mut app).unwrap().decisions = Some(Arc::new(Selector::new(&[])));
    prepare_open(&app, &codex, &mut open);
    assert_eq!(open["_decisions_observation"], true);
    assert!(
        crate::runtime::tool_specs_for(&app, &codex)
            .iter()
            .any(|s| s["name"] == "computer_browser_task")
    );
    for (bot, specs) in previous {
        let mut open = json!({"url":"https://assessment.example","_decisions_observation":true});
        prepare_open(&app, &bot, &mut open);
        assert!(open.get("_decisions_observation").is_none());
        assert_eq!(specs, crate::runtime::tool_specs_for(&app, &bot));
        assert_eq!(instructions(&app, &bot), "");
        assert!(
            call(&app, &bot, &run, serde_json::to_value(task()).unwrap())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn runtime_guard_preserves_real_approval_receipts_and_budget() {
    let mut app = crate::tests::app();
    Arc::get_mut(&mut app).unwrap().config.max_steps = 1;
    let mut bot = crate::tests::bot(&app.db, "codex");
    bot.approval_mode = "auto".into();
    app.db.save_bot(&bot).unwrap();
    app.db.queue(&bot.id, "Configure an assessment", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let guard = RuntimeGuard {
        app: &app,
        bot: &bot,
        run: &run,
        credential: None,
    };
    let candidate = Candidate {
        id: "check".into(),
        kind: "check".into(),
        label: "Enable Safe checks".into(),
        value_key: None,
    };
    assert!(
        crate::runtime::needs_approval(
            &app,
            &bot,
            "computer_browser_task",
            &json!({"action_scope":candidate.scope()})
        )
        .unwrap()
    );
    let mut browser = FixtureBrowser::new();
    let selector = Selector::new(&[Some("check")]);
    let browser_task = task();
    let work = drive(&selector, &mut browser, &guard, &browser_task);
    let refuse = async {
        loop {
            let approvals = app.db.approvals().unwrap();
            if let Some(approval) = approvals.first() {
                app.db
                    .decide(approval["id"].as_str().unwrap(), false)
                    .unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    let (outcome, _) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(work, refuse) })
            .await
            .unwrap();
    assert_eq!(outcome.status, "denied");
    assert_eq!(browser.actions, 0);
    let id = guard.requested(&candidate).unwrap();
    guard
        .receipt(
            &id,
            &candidate,
            &ActionReceipt {
                applied: true,
                uncertain: false,
                detail: "Checked".into(),
            },
        )
        .unwrap();
    assert!(guard.requested(&candidate).is_err());
    assert!(
        app.db
            .events(&run.id)
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "tool_result" && e["body"]["call_id"] == id)
    );
}

#[tokio::test]
async fn foreign_run_bot_cannot_acquire_browser_or_send_observations() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let other = crate::tests::bot(&app.db, "codex");
    app.db.queue(&other.id, "Other task", 0).unwrap();
    let run = app.db.claim_bot(&other.id).unwrap().unwrap();
    let guard = RuntimeGuard {
        app: &app,
        bot: &bot,
        run: &run,
        credential: None,
    };
    assert!(guard.check().is_err());
}
#[tokio::test]
async fn finish_is_only_a_report_from_fresh_observation_not_success_receipt() {
    let mut browser = FixtureBrowser::new();
    let outcome = drive(
        &Selector::new(&[Some("finish")]),
        &mut browser,
        &FixtureGuard::new(true),
        &task(),
    )
    .await;
    assert_eq!(outcome.status, "reported_finished");
    assert_eq!(browser.actions, 0);
    assert_eq!(browser.observations, 1);
    let result = outcome.result();
    let body: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
    assert!(
        body["continuation"]
            .as_str()
            .unwrap()
            .contains("not independent confirmation")
    );
}
struct StalledBrowser {
    action: bool,
}
impl Browser for StalledBrowser {
    fn observe<'a>(&'a mut self, task: &'a Task) -> Work<'a, Observation> {
        Box::pin(async move {
            if self.action {
                FixtureBrowser::new().observe(task).await
            } else {
                std::future::pending().await
            }
        })
    }
    fn act<'a>(&'a mut self, _o: &'a Observation, _c: &'a Candidate) -> Work<'a, ActionReceipt> {
        Box::pin(std::future::pending())
    }
}
#[tokio::test]
async fn total_deadline_read_and_dispatched_write_are_distinguished_without_replay() {
    for action in [false, true] {
        let mut browser = StalledBrowser { action };
        let outcome = drive_until(
            &Selector::new(&[Some("check")]),
            &mut browser,
            &FixtureGuard::new(true),
            &task(),
            tokio::time::Instant::now() + Duration::from_millis(20),
        )
        .await;
        assert_eq!(outcome.actions.len(), usize::from(action));
        assert_eq!(outcome.uncertain, action);
        assert_eq!(outcome.result()["timed_out"] == true, action);
    }
}

/// Runs the production HTTP chooser, Rust loop and real Python/CDP driver on a
/// disposable profile. The harness distinguishes live API results from local
/// schema/fault fixtures; no selected choice is replaced on the live path.
#[tokio::test]
#[ignore = "explicit disposable browser and supported API-key/local wire fixture"]
async fn actual_browser_decisions_worker() {
    let profile = std::env::var("KINDRED_BROWSER_FIXTURE_PROFILE").unwrap();
    let task: Task =
        serde_json::from_str(&std::env::var("KINDRED_BROWSER_FIXTURE_TASK").unwrap()).unwrap();
    task.validate().unwrap();
    let display = std::env::var("KINDRED_BROWSER_FIXTURE_DISPLAY").ok();
    let mut browser = driver::GuestBrowser::local_fixture(&profile, display.as_deref())
        .await
        .unwrap();
    let selector: Arc<dyn Decisions> =
        if let Ok(endpoint) = std::env::var("KINDRED_BROWSER_FIXTURE_ENDPOINT") {
            Arc::new(
                transport::OpenAiDecisions::local_fixture(
                    "sk-fixture-only-not-valid-live".into(),
                    endpoint,
                )
                .unwrap(),
            )
        } else {
            transport::OpenAiDecisions::configured(&crate::config::Decisions {
                enabled: true,
                ..Default::default()
            })
            .expect("A legitimately provisioned Decisions API key is required")
        };
    struct Measured {
        inner: Arc<dyn Decisions>,
        calls: Mutex<Vec<Value>>,
    }
    impl Decisions for Measured {
        fn choose<'a>(&'a self, input: &'a DecisionInput) -> Work<'a, String> {
            Box::pin(async move {
                let start = std::time::Instant::now();
                let result = self.inner.choose(input).await;
                self.calls.lock().unwrap().push(json!({"milliseconds":start.elapsed().as_millis(),"choice":result.as_ref().ok(),"failed":result.is_err(),"snapshot_id":input.observation.snapshot_id}));
                result
            })
        }
    }
    let measured = Measured {
        inner: selector,
        calls: Mutex::new(vec![]),
    };
    let start = std::time::Instant::now();
    let outcome = drive(&measured, &mut browser, &FixtureGuard::new(true), &task).await;
    let mut result = outcome.result();
    result["fixture_metrics"] = json!({"requests":measured.calls.lock().unwrap().clone(),"total_milliseconds":start.elapsed().as_millis(),"live_api":std::env::var("KINDRED_BROWSER_FIXTURE_ENDPOINT").is_err()});
    std::fs::write(
        std::env::var("KINDRED_BROWSER_FIXTURE_RESULT").unwrap(),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn unchanged_submit_click_cannot_be_dispatched_twice() {
    struct ClickBrowser(FixtureBrowser);
    impl Browser for ClickBrowser {
        fn observe<'a>(&'a mut self, task: &'a Task) -> Work<'a, Observation> {
            Box::pin(async move {
                let mut o = self.0.observe(task).await?;
                o.candidates[0].kind = "click".into();
                o.candidates[0].label = "Confirm submission".into();
                Ok(o)
            })
        }
        fn act<'a>(&'a mut self, o: &'a Observation, c: &'a Candidate) -> Work<'a, ActionReceipt> {
            self.0.act(o, c)
        }
    }
    let mut browser = ClickBrowser(FixtureBrowser::new());
    let outcome = drive(
        &Selector::new(&[Some("check"), Some("check")]),
        &mut browser,
        &FixtureGuard::new(true),
        &task(),
    )
    .await;
    assert_eq!(browser.0.actions, 1);
    assert_eq!(browser.0.observations, 2);
    assert!(outcome.uncertain);
    assert_eq!(outcome.result()["timed_out"], true);
}

#[tokio::test]
async fn human_return_requires_desktop_observation_before_browser_lease() {
    let mut app = crate::tests::app();
    Arc::get_mut(&mut app).unwrap().decisions = Some(Arc::new(Selector::new(&[])));
    let bot = crate::tests::bot(&app.db, "codex");
    app.db.queue(&bot.id, "Continue after sign-in", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    app.db
        .event(&run.id, "user_action_done", json!({"outcome":"done"}))
        .unwrap();
    let error = call(&app, &bot, &run, serde_json::to_value(task()).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("fresh computer_screenshot"));
    assert!(!app.desktop_sessions.engaged(&run.id));
    assert!(app.db.approvals().unwrap().is_empty());
    app.db
        .event(
            &run.id,
            "tool_result",
            json!({"tool":"computer_screenshot","has_image":true,"failed":false}),
        )
        .unwrap();
    assert!(!app.db.handoff_needs_observation(&run.id).unwrap());
}

#[test]
fn saved_credential_change_revokes_selected_action_before_input() {
    let mut app = crate::tests::app();
    let root = std::env::temp_dir().join(format!("kindred-decisions-revoke-{}", db::id()));
    std::fs::create_dir_all(&root).unwrap();
    Arc::get_mut(&mut app).unwrap().config.database =
        root.join("fixture.db").to_string_lossy().into_owned();
    let bot = crate::tests::bot(&app.db, "codex");
    app.db.queue(&bot.id, "Fixture", 0).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    let key = "sk-synthetic-revoke-123456789";
    crate::connections::save_decisions(&app, Some(key)).unwrap();
    let credential = std::sync::Mutex::new(Some(
        ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
            .as_ref()
            .to_vec(),
    ));
    let guard = RuntimeGuard {
        app: &app,
        bot: &bot,
        run: &run,
        credential: Some(&credential),
    };
    assert!(guard.check().is_ok());
    crate::connections::save_decisions(&app, Some("sk-synthetic-replacement-12345")).unwrap();
    assert!(
        guard
            .check()
            .unwrap_err()
            .to_string()
            .contains("key changed")
    );
    crate::connections::save_decisions(&app, None).unwrap();
    assert!(guard.check().is_err());
    assert!(app.db.approvals().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Requires the explicitly supplied synthetic fixture environment"]
fn environment_key_is_legacy_only_and_removal_never_resurrects_it() {
    let mut app = crate::tests::app();
    let root = std::env::temp_dir().join(format!("kindred-decisions-env-{}", db::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = &mut Arc::get_mut(&mut app).unwrap().config;
    config.database = root.join("fixture.db").to_string_lossy().into_owned();
    config.decisions.enabled = true;
    config.decisions.api_key_env = "KINDRED_TEST_DECISIONS_ENV_KEY".into();
    assert!(crate::connections::decisions_key(&app).is_some());
    assert_eq!(
        crate::connections::decisions_status(&app)["source"],
        "environment"
    );
    Arc::get_mut(&mut app).unwrap().config.vm.managed_id = db::id();
    assert!(crate::connections::decisions_key(&app).is_none());
    Arc::get_mut(&mut app).unwrap().config.vm.managed_id.clear();
    crate::connections::save_decisions(&app, Some("sk-synthetic-saved-123456789")).unwrap();
    assert_eq!(
        crate::connections::decisions_status(&app)["source"],
        "saved"
    );
    crate::connections::save_decisions(&app, None).unwrap();
    assert!(crate::connections::decisions_key(&app).is_none());
    assert_eq!(crate::connections::decisions_status(&app)["source"], "none");
    std::fs::remove_dir_all(root).unwrap();
}
