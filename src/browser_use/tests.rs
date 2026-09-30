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
